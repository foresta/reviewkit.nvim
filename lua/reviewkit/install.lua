local M = {}

local RELEASES = "https://github.com/foresta/reviewkit.nvim/releases"

---@class reviewkit.InstallResult
---@field path string
---@field source "existing"|"download"|"build"
---@field version? string

---@class reviewkit.InstallOpts
---@field prefer? "download"|"build"
---@field root? string plugin checkout
---@field base_url? string directory holding the release assets
---@field target? string|false
---@field cargo? string
---@field curl? string
---@field log? fun(msg: string)

function M.root()
  local source = debug.getinfo(1, "S").source:sub(2)
  return vim.fn.fnamemodify(source, ":p:h:h:h")
end

--- Rosetta reports x86_64, and the x86_64 binary runs there too, so the
--- machine name is taken as is.
---@param uname? { sysname: string, machine: string }
---@return string?
function M.target(uname)
  uname = uname or vim.uv.os_uname()
  local arch = ({ arm64 = "aarch64", aarch64 = "aarch64", x86_64 = "x86_64", amd64 = "x86_64" })[uname.machine]
  if not arch then
    return nil
  end
  if uname.sysname == "Darwin" then
    return arch .. "-apple-darwin"
  elseif uname.sysname == "Linux" then
    return arch .. "-unknown-linux-musl"
  end
  return nil
end

--- The checkout always carries Cargo.toml, so it pins the release without
--- git metadata or the GitHub API.
---@param root string
---@return string?
function M.wanted_version(root)
  local f = io.open(root .. "/Cargo.toml", "r")
  if not f then
    return nil
  end
  local section
  for line in f:lines() do
    local header = line:match("^%s*%[([^%]]+)%]")
    if header then
      section = header
    elseif section == "workspace.package" then
      local v = line:match('^%s*version%s*=%s*"([^"]+)"')
      if v then
        f:close()
        return v
      end
    end
  end
  f:close()
  return nil
end

---@param version string
---@return boolean
function M.is_dev(version)
  return version:find("-", 1, true) ~= nil
end

--- Between releases the version carries `-dev` and has no assets of its
--- own; the latest release is the closest stand-in.
---@param version string
---@param opts reviewkit.InstallOpts
---@return string
function M.base_url(version, opts)
  if opts.base_url then
    return opts.base_url
  end
  local env = vim.env.REVIEWKIT_RELEASE_URL
  if env and env ~= "" then
    return env
  end
  if M.is_dev(version) then
    return RELEASES .. "/latest/download"
  end
  return RELEASES .. "/download/v" .. version
end

---@alias reviewkit.Exec fun(cmd: string[], cb: fun(res: vim.SystemCompleted))

---@param sync boolean
---@return reviewkit.Exec
local function executor(sync)
  return function(cmd, cb)
    local ok, proc = pcall(vim.system, cmd, { text = true }, (not sync) and function(res)
      vim.schedule(function()
        cb(res)
      end)
    end or nil)
    if not ok then
      local res = { code = 127, signal = 0, stdout = "", stderr = tostring(proc) }
      if sync then
        return cb(res)
      end
      return vim.schedule(function()
        cb(res)
      end)
    end
    if sync then
      cb(proc:wait())
    end
  end
end

---@param exec reviewkit.Exec
---@param path string
---@param cb fun(info: { version: string, schema_version: integer }?)
local function probe(exec, path, cb)
  exec({ path, "version" }, function(res)
    if res.code ~= 0 then
      return cb(nil)
    end
    local ok, envelope = pcall(vim.json.decode, res.stdout or "")
    cb(ok and type(envelope) == "table" and envelope.data or nil)
  end)
end

---@param info? { version: string, schema_version: integer, schema_revision?: integer }
---@param wanted string
---@return string? reason nil when the binary fits this checkout
local function mismatch(info, wanted)
  if not info then
    return "the binary does not run"
  end
  local contract = require("reviewkit.cli").contract_mismatch(info)
  if contract then
    return contract
  end
  if not M.is_dev(wanted) and info.version ~= wanted then
    return ("version %s, the plugin needs %s"):format(tostring(info.version), wanted)
  end
  return nil
end

---@param dir string
---@return string
local function temp_in(dir)
  return ("%s/.reviewkit.%d.%d"):format(dir, vim.uv.os_getpid(), math.random(1, 1e9))
end

--- Renamed into place rather than copied over: overwriting the Mach-O of a
--- running binary gets the next exec killed on macOS, and a rename within
--- bin/ is atomic for a concurrent installer.
---@param tmp string
---@param dest string
---@return string? err
local function swap_in(tmp, dest)
  vim.uv.fs_chmod(tmp, 493)
  local ok, err = vim.uv.fs_rename(tmp, dest)
  if not ok then
    os.remove(tmp)
    return "cannot move the binary into " .. dest .. ": " .. tostring(err)
  end
  require("reviewkit.cli").reset()
  return nil
end

--- There is no fallback to skipping the check when no tool is found.
---@param exec reviewkit.Exec
---@param path string
---@param cb fun(sum: string?, err: string?)
local function sha256(exec, path, cb)
  local tools = {
    { "sha256sum", path },
    { "shasum", "-a", "256", path },
    { "openssl", "dgst", "-sha256", "-r", path },
  }
  for _, cmd in ipairs(tools) do
    if vim.fn.executable(cmd[1]) == 1 then
      return exec(cmd, function(res)
        local sum = res.code == 0 and (res.stdout or ""):match("^(%x+)") or nil
        cb(sum and sum:lower(), sum and nil or (cmd[1] .. " failed: " .. (res.stderr or "")))
      end)
    end
  end
  cb(nil, "no sha256 tool found (sha256sum, shasum or openssl)")
end

---@param url string
---@param curl string
---@param dest string
---@return string[]
local function curl_cmd(curl, url, dest)
  local cmd = { curl, "-fsSL", "--connect-timeout", "10", "--max-time", "120", "--retry", "3", "-o", dest }
  if url:match("^https://") then
    vim.list_extend(cmd, { "--proto", "=https", "--proto-redir", "=https" })
  end
  table.insert(cmd, url)
  return cmd
end

---@param ctx table
---@param cb fun(result: reviewkit.InstallResult?, err: string?)
local function download(ctx, cb)
  local opts, exec = ctx.opts, ctx.exec
  local target = opts.target
  if target == nil then
    target = M.target()
  end
  if not target then
    local u = vim.uv.os_uname()
    return cb(nil, ("no prebuilt binary for %s %s"):format(u.sysname, u.machine))
  end
  local curl = opts.curl or "curl"
  if vim.fn.executable(curl) ~= 1 then
    return cb(nil, "curl is not installed")
  end
  local base = M.base_url(ctx.wanted, opts)
  if M.is_dev(ctx.wanted) and not opts.base_url and (vim.env.REVIEWKIT_RELEASE_URL or "") == "" then
    ctx.log(("%s is unreleased; trying the latest release, which may lack newer features"):format(ctx.wanted))
  end
  local asset = "reviewkit-" .. target
  local tmp, sums = temp_in(ctx.bin_dir), temp_in(ctx.bin_dir)
  local function fail(msg)
    os.remove(tmp)
    os.remove(sums)
    cb(nil, msg)
  end
  ctx.log("downloading " .. asset .. " from " .. base)
  exec(curl_cmd(curl, base .. "/SHA256SUMS", sums), function(res)
    if res.code ~= 0 then
      return fail(
        ("cannot fetch %s/SHA256SUMS (a release build may still be running): %s"):format(
          base,
          vim.trim(res.stderr or "")
        )
      )
    end
    local expected
    local f = io.open(sums, "r")
    if not f then
      return fail(base .. "/SHA256SUMS came back empty")
    end
    for line in f:lines() do
      local sum, name = line:match("^(%x+)%s+%*?(%S+)$")
      if name == asset then
        expected = sum:lower()
      end
    end
    f:close()
    os.remove(sums)
    if not expected then
      return fail("SHA256SUMS has no entry for " .. asset)
    end
    exec(curl_cmd(curl, base .. "/" .. asset, tmp), function(dl)
      if dl.code ~= 0 then
        return fail(("cannot fetch %s/%s: %s"):format(base, asset, vim.trim(dl.stderr or "")))
      end
      sha256(exec, tmp, function(sum, sum_err)
        if not sum then
          return fail(sum_err)
        end
        if sum ~= expected then
          return fail(("checksum mismatch for %s: expected %s, got %s"):format(asset, expected, sum))
        end
        vim.uv.fs_chmod(tmp, 493)
        probe(exec, tmp, function(info)
          local why = mismatch(info, ctx.wanted)
          if why then
            return fail("the downloaded binary does not fit this checkout: " .. why)
          end
          local err = swap_in(tmp, ctx.dest)
          if err then
            return fail(err)
          end
          cb({ path = ctx.dest, source = "download", version = info.version })
        end)
      end)
    end)
  end)
end

---@param ctx table
---@param cb fun(result: reviewkit.InstallResult?, err: string?)
local function build(ctx, cb)
  local cargo = ctx.opts.cargo or "cargo"
  if vim.fn.executable(cargo) ~= 1 then
    return cb(nil, "cargo is not installed")
  end
  ctx.log("building reviewkit with cargo…")
  -- Without an explicit target dir, a user's CARGO_TARGET_DIR would put the
  -- binary where this function does not look.
  local cmd = {
    cargo,
    "build",
    "--release",
    "--locked",
    "--manifest-path",
    ctx.root .. "/cli/Cargo.toml",
    "--target-dir",
    ctx.root .. "/target",
  }
  ctx.exec(cmd, function(res)
    if res.code ~= 0 then
      return cb(nil, "cargo build failed:\n" .. (res.stderr or ""))
    end
    local built = ctx.root .. "/target/release/reviewkit"
    if vim.fn.executable(built) ~= 1 then
      return cb(nil, "cargo build succeeded but " .. built .. " is missing")
    end
    local tmp = temp_in(ctx.bin_dir)
    if not vim.uv.fs_copyfile(built, tmp) then
      return cb(nil, "cannot copy " .. built)
    end
    local err = swap_in(tmp, ctx.dest)
    if err then
      return cb(nil, err)
    end
    cb({ path = ctx.dest, source = "build", version = ctx.wanted })
  end)
end

---@param opts reviewkit.InstallOpts
---@param sync boolean
---@param cb fun(result: reviewkit.InstallResult?, err: string?)
local function run(opts, sync, cb)
  local root = opts.root or M.root()
  local wanted = M.wanted_version(root)
  if not wanted then
    return cb(nil, "cannot read the version from " .. root .. "/Cargo.toml")
  end
  local ctx = {
    opts = opts,
    exec = executor(sync),
    root = root,
    wanted = wanted,
    bin_dir = root .. "/bin",
    dest = root .. "/bin/reviewkit",
    log = opts.log or function(msg)
      vim.notify("[reviewkit] " .. msg)
    end,
  }
  vim.fn.mkdir(ctx.bin_dir, "p")
  local prefer = opts.prefer
  if not prefer then
    local dev = require("reviewkit.config").get().cli.dev
    prefer = (dev or M.is_dev(wanted)) and "build" or "download"
  end
  local steps = prefer == "build" and { build, download } or { download, build }
  local reasons = {}
  local function attempt(i)
    if i > #steps then
      return cb(
        nil,
        table.concat(reasons, "\n")
          .. "\nplace a binary at "
          .. ctx.dest
          .. " or set $REVIEWKIT_BIN / setup({ cli = { path = ... } })"
      )
    end
    steps[i](ctx, function(result, err)
      if result then
        return cb(result)
      end
      table.insert(reasons, err)
      attempt(i + 1)
    end)
  end
  -- Every unreleased commit reports the same `-dev` version, so a binary
  -- that fits by version may still predate this checkout; it is only kept
  -- when nothing could replace it.
  local can_refresh = M.is_dev(wanted) and (prefer == "download" or vim.fn.executable(opts.cargo or "cargo") == 1)
  if vim.fn.executable(ctx.dest) ~= 1 or can_refresh then
    return attempt(1)
  end
  probe(ctx.exec, ctx.dest, function(info)
    if not mismatch(info, wanted) then
      return cb({ path = ctx.dest, source = "existing", version = info and info.version })
    end
    attempt(1)
  end)
end

--- Blocking, for build hooks that run headless (lazy.nvim `build`, dein,
--- `PackChanged`).
---@param opts? reviewkit.InstallOpts
---@return reviewkit.InstallResult? result
---@return string? err
function M.ensure(opts)
  local result, err
  run(opts or {}, true, function(r, e)
    result, err = r, e
  end)
  return result, err
end

---@param opts? reviewkit.InstallOpts
---@param cb fun(result: reviewkit.InstallResult?, err: string?)
function M.ensure_async(opts, cb)
  run(opts or {}, false, cb)
end

return M
