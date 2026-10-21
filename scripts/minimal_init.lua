-- Not in the Makefile: a direct `nvim -l` run and every child Neovim (which
-- loads this file too) must also keep the developer's global and system git
-- config (signing, hooks) away from the tests.
vim.env.GIT_CONFIG_GLOBAL = "/dev/null"
vim.env.GIT_CONFIG_NOSYSTEM = "1"
local script_dir = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
local root = vim.fn.fnamemodify(script_dir, ":h")
vim.opt.runtimepath:prepend(root)
vim.opt.runtimepath:prepend(root .. "/.tests/deps/mini.nvim")
vim.opt.swapfile = false
vim.opt.shadafile = "NONE"
require("mini.test").setup()
