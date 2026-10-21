---@diagnostic disable: need-check-nil
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()
local root, release, fakebin, logs

local function write(path, lines, exec)
  vim.fn.mkdir(vim.fn.fnamemodify(path, ":h"), "p")
  vim.fn.writefile(lines, path)
  if exec then
    vim.uv.fs_chmod(path, 493)
  end
end

local function binary(version, schema)
  return {
    "#!/bin/sh",
    ('echo \'{"ok":true,"data":{"version":"%s","schema_version":%d,"schema_revision":%d}}\''):format(
      version,
      schema or 1,
      require("reviewkit.types").SCHEMA_REVISION
    ),
  }
end

local function sha(path)
  local out = vim.system({ "shasum", "-a", "256", path }, { text = true }):wait().stdout
  return out:match("^(%x+)")
end

local function publish(version, schema, opts)
  opts = opts or {}
  local name = "reviewkit-" .. (opts.target or "test-target")
  local asset = release .. "/" .. name
  write(asset, binary(version, schema), true)
  local sum = opts.wrong_sum and string.rep("0", 64) or sha(asset)
  local lines = {}
  if not opts.no_entry then
    table.insert(lines, sum .. (opts.binary_marker and " *" or "  ") .. name)
  end
  table.insert(lines, string.rep("1", 64) .. "  reviewkit-other")
  write(release .. "/SHA256SUMS", lines)
end

local function checkout(version)
  write(
    root .. "/Cargo.toml",
    { "[workspace]", 'members = ["cli"]', "", "[workspace.package]", ('version = "%s"'):format(version) }
  )
end

--- Like cargo: the binary lands under target/<triple>/ when a build target
--- is configured, and the artifact line names where.
local function fake_cargo(version, schema, triple)
  local out = root .. "/target/" .. (triple and (triple .. "/") or "") .. "release"
  write(fakebin .. "/cargo", {
    "#!/bin/sh",
    'echo "$@" > "' .. fakebin .. '/cargo.args"',
    'mkdir -p "' .. out .. '"',
    'cat > "' .. out .. "/reviewkit\" <<'BIN'",
    binary(version, schema)[1],
    binary(version, schema)[2],
    "BIN",
    'chmod +x "' .. out .. '/reviewkit"',
    "cat <<'JSON'",
    vim.json.encode({
      reason = "compiler-artifact",
      target = { name = "reviewkit", kind = { "lib" } },
      executable = vim.NIL,
    }),
    vim.json.encode({
      reason = "compiler-artifact",
      target = { name = "reviewkit", kind = { "bin" } },
      executable = out .. "/reviewkit",
    }),
    vim.json.encode({ reason = "build-finished", success = true }),
    "JSON",
  }, true)
  return fakebin .. "/cargo"
end

local function opts(extra)
  local o = vim.tbl_extend("force", {
    root = root,
    base_url = "file://" .. release,
    target = "test-target",
    cargo = fakebin .. "/no-cargo",
    log = function(msg)
      table.insert(logs, msg)
    end,
  }, extra or {})
  if o.real_target then
    o.target, o.real_target = nil, nil
  end
  return o
end

local function installed()
  local res = vim.system({ root .. "/bin/reviewkit", "version" }, { text = true }):wait()
  return res.code == 0 and vim.json.decode(res.stdout).data.version or nil
end

T["install"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      root = vim.fn.tempname()
      release = vim.fn.tempname()
      fakebin = vim.fn.tempname()
      vim.fn.mkdir(fakebin, "p")
      logs = {}
      package.loaded["reviewkit.install"] = nil
    end,
  },
})

T["install"]["target names follow the release assets"] = function()
  local target = require("reviewkit.install").target
  eq(target({ sysname = "Darwin", machine = "arm64" }), "aarch64-apple-darwin")
  eq(target({ sysname = "Darwin", machine = "x86_64" }), "x86_64-apple-darwin")
  eq(target({ sysname = "Linux", machine = "aarch64" }), "aarch64-unknown-linux-musl")
  eq(target({ sysname = "Linux", machine = "x86_64" }), "x86_64-unknown-linux-musl")
  eq(target({ sysname = "Windows_NT", machine = "x86_64" }), nil)
end

T["install"]["the release is picked from the checkout's Cargo.toml"] = function()
  local install = require("reviewkit.install")
  checkout("1.2.3")
  eq(install.wanted_version(root), "1.2.3")
  eq(install.base_url("1.2.3", {}), "https://github.com/foresta/reviewkit.nvim/releases/download/v1.2.3")
  eq(install.base_url("1.3.0-dev", {}), "https://github.com/foresta/reviewkit.nvim/releases/latest/download")
end

T["install"]["a verified release binary is moved into bin/"] = function()
  checkout("1.2.3")
  publish("1.2.3")
  local result, err = require("reviewkit.install").ensure(opts())
  eq(err, nil)
  eq({ result.source, result.version }, { "download", "1.2.3" })
  eq(installed(), "1.2.3")
  eq(vim.fn.glob(root .. "/bin/.reviewkit.*"), "")
end

T["install"]["a binary that already fits is kept without touching the network"] = function()
  checkout("1.2.3")
  write(root .. "/bin/reviewkit", binary("1.2.3"), true)
  local result = require("reviewkit.install").ensure(opts({ base_url = "file:///nonexistent" }))
  eq(result.source, "existing")
end

T["install"]["a checksum mismatch is refused and the old binary stays"] = function()
  checkout("1.2.3")
  publish("1.2.3", 1, { wrong_sum = true })
  write(root .. "/bin/reviewkit", binary("1.0.0"), true)
  local result, err = require("reviewkit.install").ensure(opts())
  eq(result, nil)
  eq(err:find("checksum mismatch", 1, true) ~= nil, true)
  eq(err:find("cargo is not installed", 1, true) ~= nil, true)
  eq(err:find("place a binary at", 1, true) ~= nil, true)
  eq(installed(), "1.0.0")
  eq(vim.fn.glob(root .. "/bin/.reviewkit.*"), "")
end

T["install"]["a release without an entry for this target is refused"] = function()
  checkout("1.2.3")
  publish("1.2.3", 1, { no_entry = true })
  local _, err = require("reviewkit.install").ensure(opts())
  eq(err:find("SHA256SUMS has no entry for reviewkit-test-target", 1, true) ~= nil, true)
end

T["install"]["a binary for another schema falls back to cargo"] = function()
  checkout("1.2.3")
  publish("1.2.3", 2)
  local result, err = require("reviewkit.install").ensure(opts({ cargo = fake_cargo("1.2.3") }))
  eq(err, nil)
  eq(result.source, "build")
  eq(installed(), "1.2.3")
  eq(vim.fn.readfile(fakebin .. "/cargo.args")[1]:find("--release --locked", 1, true) ~= nil, true)
end

T["install"]["a missing release falls back to cargo"] = function()
  checkout("1.2.3")
  local result = require("reviewkit.install").ensure(opts({ cargo = fake_cargo("1.2.3") }))
  eq(result.source, "build")
end

T["install"]["an unreleased checkout builds first, and without cargo takes the latest release with a warning"] = function()
  checkout("1.3.0-dev")
  publish("1.2.3")
  local built = require("reviewkit.install").ensure(opts({ cargo = fake_cargo("1.3.0-dev") }))
  eq(built.source, "build")
  vim.fn.delete(root .. "/bin/reviewkit")
  vim.fn.delete(fakebin .. "/cargo")
  -- The warning is only for the real latest release, so its URL is kept and
  -- a fake curl answers it from the local release.
  write(fakebin .. "/curl", {
    "#!/bin/sh",
    'while [ $# -gt 1 ]; do [ "$1" = -o ] && { shift; out=$1; }; shift; done',
    'echo "$1" >> "' .. fakebin .. '/curl.urls"',
    'cp "' .. release .. '/$(basename "$1")" "$out"',
  }, true)
  local o = opts({ cargo = fakebin .. "/cargo", curl = fakebin .. "/curl" })
  o.base_url = nil
  local downloaded = require("reviewkit.install").ensure(o)
  eq(downloaded.source, "download")
  eq(installed(), "1.2.3")
  eq(
    vim.fn.readfile(fakebin .. "/curl.urls")[1],
    "https://github.com/foresta/reviewkit.nvim/releases/latest/download/SHA256SUMS"
  )
  local warned = vim.tbl_filter(function(m)
    return m:find("may lack newer features", 1, true) ~= nil
  end, logs)
  eq(#warned, 1)
end

T["install"]["an unreleased checkout rebuilds even when the binary claims the same -dev version"] = function()
  checkout("1.3.0-dev")
  write(root .. "/bin/reviewkit", binary("1.3.0-dev"), true)
  local result = require("reviewkit.install").ensure(opts({ cargo = fake_cargo("1.3.0-dev") }))
  eq(result.source, "build")
  vim.fn.delete(fakebin .. "/cargo")
  local kept =
    require("reviewkit.install").ensure(opts({ cargo = fakebin .. "/cargo", base_url = "file:///nonexistent" }))
  eq(kept.source, "existing")
end

T["install"]["the asset for this machine is named by target(), and `*` marked sums are read"] = function()
  local install = require("reviewkit.install")
  checkout("1.2.3")
  publish("1.2.3", 1, { target = install.target(), binary_marker = true })
  local result, err = install.ensure(opts({ real_target = true }))
  eq(err, nil)
  eq(result.source, "download")
end

T["install"]["a cargo configured with a build target still installs what it built"] = function()
  checkout("1.2.3")
  local result, err =
    require("reviewkit.install").ensure(opts({ cargo = fake_cargo("1.2.3", 1, "x86_64-unknown-linux-gnu") }))
  eq(err, nil)
  eq(result.source, "build")
  eq(installed(), "1.2.3")
end

T["install"]["cargo is told where to put the binary"] = function()
  checkout("1.2.3")
  require("reviewkit.install").ensure(opts({ cargo = fake_cargo("1.2.3") }))
  local args = vim.fn.readfile(fakebin .. "/cargo.args")[1]
  eq(args:find("--target-dir " .. root .. "/target", 1, true) ~= nil, true)
end

T["install"]["ensure_async reports through the callback"] = function()
  checkout("1.2.3")
  publish("1.2.3")
  local done
  require("reviewkit.install").ensure_async(opts(), function(result, err)
    done = { result = result, err = err }
  end)
  vim.wait(20000, function()
    return done ~= nil
  end, 20)
  eq(done.err, nil)
  eq(done.result.source, "download")
end

return T
