local M = {}

---@param session reviewkit.Session
---@param thread_id string
---@return string
local function reply_note(session, thread_id)
  for _, t in ipairs(session.threads or {}) do
    if t.id == thread_id and t.comments[1] then
      return "reply to " .. t.comments[1].author
    end
  end
  return "reply to a review thread"
end

---@param session reviewkit.Session
---@param c reviewkit.Comment
---@param i integer
---@return string[]
local function block(session, c, i)
  local range = c.start_line and c.start_line ~= c.line and ("%d-%d"):format(c.start_line, c.line) or tostring(c.line)
  local head = ("%d. %s:%s"):format(i, c.path, range)
  if c.kind and c.kind ~= "issue" then
    head = head .. " (" .. c.kind .. ")"
  end
  if c.side == "base" then
    head = head .. " (base)"
  end
  if c.reply_to and c.reply_to ~= vim.NIL then
    head = head .. " (" .. reply_note(session, c.reply_to) .. ")"
  end
  if c.stale then
    head = head .. " (the line may have moved)"
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
    vim.list_extend(lines, block(session, c, i))
  end
  -- An empty copy would overwrite the register and still report success.
  if #lines == 0 then
    return cb({
      code = "E_INVALID",
      message = "nothing to copy: the queue is empty and the decision has no text",
      retryable = false,
      details = { empty = true },
    })
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
