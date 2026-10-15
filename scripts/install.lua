-- nvim --headless -l scripts/install.lua
local script_dir = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
vim.opt.runtimepath:prepend(vim.fn.fnamemodify(script_dir, ":h"))
local result, err = require("reviewkit.install").ensure({
  log = function(msg)
    io.stdout:write(msg .. "\n")
  end,
})
if not result then
  io.stderr:write((err or "install failed") .. "\n")
  os.exit(1)
end
