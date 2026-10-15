local M = {}

---@param opts? table
function M.setup(opts)
  require("reviewkit.config").set(opts)
  -- A new cli.path must win over a path resolved before setup ran.
  require("reviewkit.cli").reset()
end

return M
