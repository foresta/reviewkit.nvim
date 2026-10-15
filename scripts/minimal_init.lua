local script_dir = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
local root = vim.fn.fnamemodify(script_dir, ":h")
vim.opt.runtimepath:prepend(root)
vim.opt.runtimepath:prepend(root .. "/.tests/deps/mini.nvim")
vim.opt.swapfile = false
vim.opt.shadafile = "NONE"
require("mini.test").setup()
