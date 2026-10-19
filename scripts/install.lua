-- nvim --headless -l scripts/install.lua [download|build]
local script_dir = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
vim.opt.runtimepath:prepend(vim.fn.fnamemodify(script_dir, ":h"))
local prefer = (arg[1] == "download" or arg[1] == "build") and arg[1] or nil
local result, err = require("reviewkit.install").ensure({
  prefer = prefer,
  log = function(msg)
    io.stdout:write(msg .. "\n")
  end,
})
if not result then
  io.stderr:write((err or "install failed") .. "\n")
  os.exit(1)
end
io.stdout:write(("%s %s (%s)\n"):format(result.path, result.version or "?", result.source))
