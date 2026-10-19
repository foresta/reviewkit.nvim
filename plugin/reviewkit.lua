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
  nargs = "+",
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
    return {}
  end,
})

vim.api.nvim_create_user_command("ReviewSubmit", function(cmd)
  orchestrator().submit(cmd.fargs[1])
end, {
  nargs = "?",
  desc = "reviewkit: send the queue through a sink (default: herdr)",
  complete = function()
    return { "herdr", "github", "clipboard" }
  end,
})

vim.api.nvim_create_user_command("ReviewClose", function(cmd)
  orchestrator().close({ purge = cmd.bang })
end, { bang = true, desc = "reviewkit: close the session (! also deletes its state)" })

vim.api.nvim_create_user_command("ReviewInstall", function()
  orchestrator().install()
end, { desc = "reviewkit: build the CLI from the plugin checkout" })

vim.api.nvim_create_user_command("ReviewLog", function()
  require("reviewkit.log").show()
end, { desc = "reviewkit: show recent CLI calls and errors" })
