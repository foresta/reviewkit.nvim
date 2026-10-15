local M = {}

local api = vim.api
local counter = 0

---@class reviewkit.InputOpts
---@field title string
---@field lines? string[]
---@field footer? string
---@field on_save fun(body: string)
---@field on_cancel? fun()

--- A small floating buffer: `:w` (or <C-s>) saves and closes, `q` discards.
--- It is `acwrite` so `:w` is caught by BufWriteCmd and nothing touches disk.
---@param opts reviewkit.InputOpts
---@return integer buf
---@return integer win
function M.open(opts)
  counter = counter + 1
  local buf = api.nvim_create_buf(false, true)
  api.nvim_buf_set_name(buf, ("reviewkit://input/%d"):format(counter))
  vim.bo[buf].buftype = "acwrite"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  api.nvim_buf_set_lines(buf, 0, -1, false, opts.lines or {})
  vim.bo[buf].modified = false
  vim.bo[buf].filetype = "markdown"

  local width = math.min(90, vim.o.columns - 8)
  local height = math.min(14, vim.o.lines - 8)
  local win = api.nvim_open_win(buf, true, {
    relative = "editor",
    row = math.floor((vim.o.lines - height) / 2) - 1,
    col = math.floor((vim.o.columns - width) / 2),
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
    title = " " .. opts.title .. " ",
    footer = opts.footer or " :w save · q cancel ",
    footer_pos = "right",
  })
  vim.wo[win].wrap = true

  local saved = false
  local function close()
    pcall(api.nvim_win_close, win, true)
  end
  api.nvim_create_autocmd("BufWriteCmd", {
    buffer = buf,
    callback = function()
      local lines = api.nvim_buf_get_lines(buf, 0, -1, false)
      while #lines > 0 and vim.trim(lines[#lines]) == "" do
        table.remove(lines)
      end
      vim.bo[buf].modified = false
      saved = true
      vim.cmd("stopinsert")
      close()
      opts.on_save(table.concat(lines, "\n"))
    end,
  })
  api.nvim_create_autocmd("BufWipeout", {
    buffer = buf,
    once = true,
    callback = function()
      if not saved and opts.on_cancel then
        opts.on_cancel()
      end
    end,
  })
  vim.keymap.set("n", "q", close, { buffer = buf, nowait = true })
  vim.keymap.set({ "n", "i" }, "<C-s>", "<Cmd>write<CR>", { buffer = buf })
  if #(opts.lines or {}) == 0 then
    vim.cmd("startinsert")
  end
  return buf, win
end

return M
