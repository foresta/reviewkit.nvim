local cli = require("reviewkit.cli")
local types = require("reviewkit.types")

local M = {}

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
  if found == types.SCHEMA_VERSION then
    health.ok(("version %s (schema_version %d)"):format(version.data.version, found))
  else
    health.error(
      ("schema_version mismatch: binary %s, plugin %s"):format(tostring(found), tostring(types.SCHEMA_VERSION)),
      { "update the binary (:ReviewInstall) or the plugin so they match" }
    )
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
    elseif tool.path then
      health.warn(name .. ": found at " .. tool.path .. " but --version failed")
    else
      local advice = name == "git" and "required"
        or ("needed for the " .. (name == "gh" and "github" or "herdr") .. " sink")
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
