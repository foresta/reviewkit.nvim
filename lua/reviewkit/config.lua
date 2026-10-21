local M = {}

---@class reviewkit.Config
---@field cli { path?: string, dev: boolean, state_dir?: string }
---@field tools { git?: string, gh?: string, herdr?: string }
---@field set_keymaps boolean
---@field sinks { herdr: { agent_type: "claude"|"plain", header?: string, match_foreground_cwd: boolean, consume?: boolean }, github: { confirm: boolean, consume?: boolean }, clipboard: { register: string, consume?: boolean } }

---@type reviewkit.Config
local defaults = {
  cli = { path = nil, dev = false, state_dir = nil },
  tools = { git = nil, gh = nil, herdr = nil },
  set_keymaps = true,
  sinks = {
    herdr = { agent_type = "claude", header = nil, match_foreground_cwd = true },
    github = { confirm = true },
    clipboard = { register = "+" },
  },
}

local current = vim.deepcopy(defaults)

---@param opts? table
function M.set(opts)
  current = vim.tbl_deep_extend("force", vim.deepcopy(defaults), opts or {})
end

---@return reviewkit.Config
function M.get()
  return current
end

return M
