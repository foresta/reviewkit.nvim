local M = {}

---@param err reviewkit.Error
function M.error(err)
  local msg = ("[reviewkit] %s: %s"):format(err.code, err.message)
  if err.hint then
    msg = msg .. "\n" .. err.hint
  end
  vim.notify(msg, vim.log.levels.ERROR)
end

return M
