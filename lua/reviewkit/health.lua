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
