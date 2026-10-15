--- The smallest Lua sink: formats the queue as text and puts it on the
--- system clipboard. It exists to prove the `register_sink` + `submit apply`
--- path and as a fallback when no agent is reachable.
local M = {}

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
    local range = c.start_line and c.start_line ~= c.line and ("%d-%d"):format(c.start_line, c.line) or tostring(c.line)
    table.insert(lines, ("%d. %s:%s%s"):format(i, c.path, range, c.side == "base" and " (base)" or ""))
    table.insert(lines, c.body)
    table.insert(lines, "")
  end
  local register = opts.register or "+"
  vim.fn.setreg(register, table.concat(lines, "\n"))
  local sent = {}
  for _, c in ipairs(session.comments) do
    table.insert(sent, { comment_id = c.id })
  end
  cb(nil, {
    sink = "clipboard",
    sent = sent,
    failed = {},
    skipped = {},
    decision = session.decision and (session.decision.body ~= "" and "sent" or "skipped") or "not_applicable",
  })
end

return M
