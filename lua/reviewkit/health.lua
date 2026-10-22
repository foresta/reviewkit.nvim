local cli = require("reviewkit.cli")
local types = require("reviewkit.types")

local M = {}

---@param name string
---@return string?
local function override_of(name)
  if require("reviewkit.config").get().tools[name] then
    return "tools." .. name
  end
  local env = "REVIEWKIT_" .. name:upper()
  if (vim.env[env] or "") ~= "" then
    return env
  end
end

--- Not fs_stat alone: the CLI spawns a bare command name from PATH.
---@param path string
local function exists(path)
  if not path:find("/", 1, true) then
    return vim.fn.exepath(path) ~= ""
  end
  return vim.uv.fs_stat(path) ~= nil
end

function M.check()
  local health = vim.health
  health.start("reviewkit")

  local bin, err = cli.resolve()
  if not bin then
    local e = assert(err)
    health.error(e.message, { e.hint })
    return
  end
  health.ok("binary: " .. bin)

  local version, version_err = cli.run_sync({ "version" })
  if not version then
    local e = assert(version_err)
    health.error("reviewkit version failed: " .. e.message, { e.hint })
    return
  end
  local found = version.data.schema_version
  local install = require("reviewkit.install")
  local wanted = install.wanted_version(install.root())
  local contract = cli.contract_mismatch(version.data)
  if not contract then
    health.ok(
      ("version %s (schema_version %d, revision %d)"):format(
        version.data.version,
        found,
        version.data.schema_revision or 0
      )
    )
    if wanted and not install.is_dev(wanted) and version.data.version ~= wanted then
      health.warn(
        ("the plugin checkout is %s but the binary is %s"):format(wanted, version.data.version),
        { "run :ReviewInstall" }
      )
    end
  else
    health.error(contract, { "update the binary (:ReviewInstall) or the plugin so they match" })
  end

  local info, health_err = cli.run_sync({ "health" })
  if not info then
    local e = assert(health_err)
    health.error("reviewkit health failed: " .. e.message, { e.hint })
    return
  end
  local data = info.data
  for _, name in ipairs({ "git", "gh", "herdr" }) do
    local tool = data.tools[name]
    if tool.version then
      health.ok(name .. ": " .. tool.version)
    elseif tool.path and not exists(tool.path) then
      local by = override_of(name)
      local msg = name .. ": not found at " .. tool.path .. (by and (" (set by " .. by .. ")") or "")
      if name == "git" then
        health.error(msg, { "fix the path or unset the override" })
      else
        health.warn(msg, { "fix the path or unset the override" })
      end
    elseif tool.path then
      health.warn(name .. ": found at " .. tool.path .. " but --version failed")
    elseif name == "git" then
      health.error("git: not found (required)", { "install git or set setup({ tools = { git = ... } })" })
    else
      local advice = name == "gh" and "needed for pull requests and the github sink" or "needed for the herdr sink"
      health.warn(name .. ": not found (" .. advice .. ")")
    end
  end
  if data.gh_auth == false then
    health.warn("gh is not logged in", { "run `gh auth login`" })
  end
  if data.state_dir.writable then
    health.ok("state dir: " .. data.state_dir.path)
  else
    health.error(
      "state dir is not writable: " .. data.state_dir.path,
      { "set --state-dir via setup({ cli = { state_dir = ... } })" }
    )
  end
end

return M
