if vim.g.loaded_reviewkit then
  return
end
vim.g.loaded_reviewkit = true

local function orchestrator()
  return require("reviewkit.orchestrator")
end

vim.api.nvim_create_user_command("Review", function(cmd)
  orchestrator().command(cmd.fargs)
end, {
  nargs = "*",
  desc = "reviewkit: diff <base> [<head>] | resume [id] | refresh | pr <number> | threads [refresh]",
  complete = function(arg_lead, line)
    local words = vim.split(line, "%s+")
    if #words == 2 then
      return vim.tbl_filter(function(w)
        return vim.startswith(w, arg_lead)
      end, { "diff", "resume", "refresh", "pr", "threads" })
    end
    if #words == 3 and words[2] == "threads" then
      return { "refresh" }
    end
    if #words == 3 and words[2] == "resume" then
      -- Only PR ids read as something; local ids are hashes, and
      -- `:Review resume` without an id lists those by what they compare.
      local envelope = require("reviewkit.cli").run_sync({ "session", "list" }, { timeout = 2000 })
      local ids = {}
      for _, s in ipairs(envelope and envelope.data or {}) do
        if s.kind == "pr" and type(s.id) == "string" and vim.startswith(s.id, arg_lead) then
          table.insert(ids, s.id)
        end
      end
      return ids
    end
    return {}
  end,
})

vim.api.nvim_create_user_command("ReviewSubmit", function(cmd)
  if cmd.range > 0 then
    return orchestrator().submit_range(cmd.fargs[1], cmd.line1, cmd.line2)
  end
  orchestrator().submit(cmd.fargs[1])
end, {
  nargs = "?",
  range = true,
  desc = "reviewkit: send the queue through a sink (default: github for a pull request, else herdr); with a range, the selected comments of the queue list",
  complete = function()
    return require("reviewkit.core").sink_names()
  end,
})

vim.api.nvim_create_user_command("ReviewClose", function(cmd)
  orchestrator().close({ purge = cmd.bang })
end, { bang = true, desc = "reviewkit: close the session (! also deletes its state)" })

vim.api.nvim_create_user_command("ReviewInstall", function()
  orchestrator().install()
end, { desc = "reviewkit: install or update the CLI (release download, else cargo build)" })

-- Defined here so a user's own `nmap` to them works before any review.
for _, map in ipairs(require("reviewkit.maps")) do
  local mode, action, desc = map[1], map[3], map[4]
  vim.keymap.set(mode, "<Plug>(reviewkit-" .. action:gsub("_", "-") .. ")", function()
    orchestrator().run_action(action)
  end, { desc = desc })
end

vim.api.nvim_create_user_command("ReviewHistory", function()
  orchestrator().history()
end, { desc = "reviewkit: list comments that left the queue (r puts one back)" })

vim.api.nvim_create_user_command("ReviewLog", function()
  require("reviewkit.log").show()
end, { desc = "reviewkit: show recent CLI calls and errors" })
