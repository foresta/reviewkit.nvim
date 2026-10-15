local M = {}

local function plugin_root()
  local source = debug.getinfo(1, "S").source:sub(2)
  return vim.fn.fnamemodify(source, ":p:h:h:h")
end

---@class reviewkit.InstallResult
---@field path string
---@field built boolean

--- Builds with cargo and copies the release binary to <plugin>/bin/.
---@param opts? { release?: boolean, log?: fun(msg: string) }
---@return reviewkit.InstallResult? result
---@return string? err
function M.ensure(opts)
  opts = opts or {}
  local log = opts.log or function(msg)
    vim.notify("[reviewkit] " .. msg)
  end
  local root = plugin_root()
  if vim.fn.executable("cargo") ~= 1 then
    return nil, "cargo is not installed; install Rust or download a release binary into " .. root .. "/bin/"
  end
  local profile = opts.release == false and "debug" or "release"
  local args = { "cargo", "build", "--manifest-path", root .. "/cli/Cargo.toml" }
  if profile == "release" then
    table.insert(args, "--release")
  end
  log("building reviewkit (" .. profile .. ")…")
  local res = vim.system(args, { text = true }):wait()
  if res.code ~= 0 then
    return nil, "cargo build failed:\n" .. (res.stderr or "")
  end
  local built = root .. "/target/" .. profile .. "/reviewkit"
  if vim.fn.executable(built) ~= 1 then
    built = root .. "/cli/target/" .. profile .. "/reviewkit"
  end
  if vim.fn.executable(built) ~= 1 then
    return nil, "cargo build succeeded but no binary was found under target/"
  end
  local dest_dir = root .. "/bin"
  vim.fn.mkdir(dest_dir, "p")
  local dest = dest_dir .. "/reviewkit"
  local copied = vim.uv.fs_copyfile(built, dest)
  if not copied then
    return nil, "cannot copy " .. built .. " to " .. dest
  end
  vim.uv.fs_chmod(dest, 493) -- 0755
  require("reviewkit.cli").reset()
  log("installed " .. dest)
  return { path = dest, built = true }
end

return M
