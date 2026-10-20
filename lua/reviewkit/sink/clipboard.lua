local M = {}

---@param c reviewkit.Comment
---@param i integer
---@return string[]
local function block(c, i)
  local range = c.start_line and c.start_line ~= c.line and ("%d-%d"):format(c.start_line, c.line) or tostring(c.line)
  local head = ("%d. %s:%s"):format(i, c.path, range)
  if c.kind and c.kind ~= "issue" then
    head = head .. " (" .. c.kind .. ")"
  end
  if c.side == "base" then
    head = head .. " (base)"
  end
  local lines = { head, c.body }
  if c.suggestion and c.suggestion ~= "" then
    table.insert(lines, "Suggested replacement:")
    table.insert(lines, "```")
    vim.list_extend(lines, vim.split(c.suggestion, "\n", { plain = true }))
    table.insert(lines, "```")
  end
  table.insert(lines, "")
  return lines
end

---@param session reviewkit.Session
---@return reviewkit.SinkResult
local function copied(session)
  local sent = {}
  for _, c in ipairs(session.comments) do
    table.insert(sent, { comment_id = c.id })
  end
  return {
    sink = "clipboard",
    mode = "copied",
    sent = sent,
    failed = {},
    skipped = {},
    decision = session.decision and (session.decision.body ~= "" and "sent" or "skipped") or "not_applicable",
  }
end

--- Reading an OSC 52 clipboard asks the terminal and waits, and many
--- terminals never answer; such a provider is trusted instead.
---@return boolean
local function osc52()
  local configured = vim.g.clipboard
  local name = type(configured) == "table" and configured.name or configured
  if type(name) ~= "string" then
    local ok, detected = pcall(vim.fn["provider#clipboard#Executable"])
    name = ok and detected or ""
  end
  return name:lower():gsub("%s", ""):find("osc52", 1, true) ~= nil
end

--- `setreg` does not fail when there is no clipboard provider, so the
--- register is read back before anything is reported as copied. `copied`
--- rather than a delivery: a copy must not consume the queue.
---@param session reviewkit.Session
---@param opts table
---@param cb fun(err: reviewkit.Error?, result: reviewkit.SinkResult?)
function M.submit(session, opts, cb)
  local lines = {}
  if session.decision and session.decision.body ~= "" then
    table.insert(lines, session.decision.body)
    table.insert(lines, "")
  end
  for i, c in ipairs(session.comments) do
    vim.list_extend(lines, block(c, i))
  end
  local text = table.concat(lines, "\n")
  local register = opts.register or "+"
  local written = pcall(vim.fn.setreg, register, text)
  if written and (register == "+" or register == "*") and osc52() then
    return cb(nil, copied(session))
  end
  local ok, back = pcall(vim.fn.getreg, register)
  if not ok or back ~= text then
    return cb(nil, {
      sink = "clipboard",
      mode = "copied",
      sent = {},
      failed = {
        {
          comment_id = nil,
          error = {
            code = "E_INVALID",
            message = ("could not write register %s (no clipboard provider?)"):format(register),
            retryable = false,
            details = vim.empty_dict(),
          },
        },
      },
      skipped = {},
      decision = session.decision and "failed" or "not_applicable",
    })
  end
  cb(nil, copied(session))
end

return M
