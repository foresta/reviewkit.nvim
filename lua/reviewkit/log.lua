local M = {}

local MAX = 200
local entries = {} ---@type { at: string, level: string, text: string }[]

---@param level "info"|"warn"|"error"
---@param text string
function M.add(level, text)
  table.insert(entries, { at = os.date("%H:%M:%S"), level = level, text = text })
  if #entries > MAX then
    table.remove(entries, 1)
  end
end

---@return { at: string, level: string, text: string }[]
function M.entries()
  return entries
end

function M.clear()
  entries = {}
end

--- A scratch buffer rather than :messages, so a long JSON error can be
--- searched and yanked.
function M.show()
  local lines = {}
  for _, e in ipairs(entries) do
    table.insert(lines, ("%s [%s] %s"):format(e.at, e.level, e.text))
  end
  if #lines == 0 then
    lines = { "(no reviewkit log entries yet)" }
  end
  local name = "reviewkit://log"
  local buf = vim.fn.bufnr(name)
  if buf == -1 then
    buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_name(buf, name)
    vim.bo[buf].bufhidden = "wipe"
  end
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  local shown = vim.fn.bufwinid(buf)
  if shown == -1 then
    vim.cmd("botright split")
    vim.api.nvim_win_set_buf(0, buf)
  else
    vim.api.nvim_set_current_win(shown)
  end
  vim.api.nvim_win_set_cursor(0, { #lines, 0 })
  vim.keymap.set("n", "q", "<Cmd>close<CR>", { buffer = buf, nowait = true })
end

return M
