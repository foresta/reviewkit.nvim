local M = {}

local api = vim.api

local EVENTS = { APPROVE = "approve", REQUEST_CHANGES = "request changes", COMMENT = "comment" }

---@param text string?
---@return string
local function first_line(text)
  return ((text or ""):match("^[^\n]*") or "")
end

--- The plan is the GraphQL request itself, so what is listed is exactly
--- what GitHub will receive.
---@param plan table
---@param session? reviewkit.Session
---@return string[]
function M.lines(plan, session)
  local vars = plan.variables or {}
  local lines = {}
  if session and session.pending_submit then
    table.insert(lines, "An earlier submit was cut off: what it already posted is recognised, not sent again.")
    table.insert(lines, "")
  end
  if plan.event then
    table.insert(lines, ("Review: %s"):format(EVENTS[plan.event] or plan.event))
    local body = vars.body or ""
    if vim.trim(body) ~= "" then
      vim.list_extend(lines, vim.split(body, "\n", { plain = true }))
    end
    table.insert(lines, "")
  end
  local threads = vars.threads or {}
  if #threads > 0 then
    table.insert(lines, ("Comments (%d)"):format(#threads))
    for _, t in ipairs(threads) do
      local range = t.startLine and ("%d-%d"):format(t.startLine, t.line) or tostring(t.line)
      local side = t.side == "LEFT" and " (base)" or ""
      table.insert(lines, ("  %s:%s%s  %s"):format(t.path, range, side, first_line(t.body)))
    end
    table.insert(lines, "")
  end
  local in_body = plan.body_comment_ids or {}
  if #in_body > 0 then
    table.insert(lines, ("Outside the diff, written into the review body above (%d)"):format(#in_body))
    table.insert(lines, "")
  end
  local replies = plan.replies or {}
  if #replies > 0 then
    table.insert(lines, ("Replies (%d)"):format(#replies))
    for _, r in ipairs(replies) do
      table.insert(lines, "  " .. first_line(r.body))
    end
    table.insert(lines, "")
  end
  local actions = plan.actions or {}
  if #actions > 0 then
    table.insert(lines, ("Thread actions (%d)"):format(#actions))
    for _, a in ipairs(actions) do
      table.insert(lines, ("  %s %s"):format(a.action, a.thread_id))
    end
    table.insert(lines, "")
  end
  local skipped = plan.skipped or {}
  if #skipped > 0 then
    table.insert(lines, ("Not sent, stay queued (%d)"):format(#skipped))
    local where = {}
    for _, c in ipairs(session and session.comments or {}) do
      where[c.id] = ("%s:%d"):format(c.path, c.line)
    end
    for _, s in ipairs(skipped) do
      table.insert(lines, ("  %s (%s)"):format(where[s.comment_id] or s.comment_id, s.reason))
    end
  end
  return lines
end

---@param plan table
---@param session? reviewkit.Session
---@param on_confirm fun()
---@return integer buf
function M.show(plan, session, on_confirm)
  local lines = M.lines(plan, session)
  local buf = api.nvim_create_buf(false, true)
  vim.bo[buf].bufhidden = "wipe"
  api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  vim.bo[buf].filetype = "markdown"
  local width = math.min(100, vim.o.columns - 8)
  local height = math.max(3, math.min(#lines, vim.o.lines - 8))
  local win = api.nvim_open_win(buf, true, {
    relative = "editor",
    row = math.floor((vim.o.lines - height) / 2) - 1,
    col = math.floor((vim.o.columns - width) / 2),
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
    title = " Submit to GitHub ",
    footer = " y send · q cancel ",
    footer_pos = "right",
  })
  vim.wo[win].wrap = true
  local function close()
    pcall(api.nvim_win_close, win, true)
  end
  local function confirm()
    close()
    on_confirm()
  end
  for _, key in ipairs({ "y", "<CR>" }) do
    vim.keymap.set("n", key, confirm, { buffer = buf, nowait = true })
  end
  for _, key in ipairs({ "q", "n", "<Esc>" }) do
    vim.keymap.set("n", key, close, { buffer = buf, nowait = true })
  end
  return buf
end

return M
