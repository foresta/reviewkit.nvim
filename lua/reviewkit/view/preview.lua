local M = {}

local api = vim.api

local EVENTS = { approve = "approve", request_changes = "request changes", comment = "comment" }

---@param text string?
---@return string
local function first_line(text)
  return ((text or ""):match("^[^\n]*") or "")
end

---@param comment reviewkit.Comment
---@return string
local function place(comment)
  local range = comment.start_line and ("%d-%d"):format(comment.start_line, comment.line) or tostring(comment.line)
  return ("%s:%s%s"):format(comment.path, range, comment.side == "base" and " (base)" or "")
end

--- Comments are named by id in the preview; their place and text come
--- from the session, which is what the sink reads too.
---@param preview reviewkit.SubmitPreview
---@param session? reviewkit.Session
---@return string[]
function M.lines(preview, session)
  local by_id = {}
  for _, c in ipairs(session and session.comments or {}) do
    by_id[c.id] = c
  end
  local lines = {}
  if session and session.pending_submit then
    table.insert(lines, "An earlier submit was cut off: what it already posted is recognised, not sent again.")
    table.insert(lines, "")
  end
  if preview.decision then
    table.insert(lines, ("Review: %s"):format(EVENTS[preview.decision] or preview.decision))
  end
  if preview.text and vim.trim(preview.text) ~= "" then
    vim.list_extend(lines, vim.split(preview.text, "\n", { plain = true }))
  end
  if #lines > 0 then
    table.insert(lines, "")
  end
  local ids = preview.comment_ids or {}
  if #ids > 0 then
    table.insert(lines, ("Comments (%d)"):format(#ids))
    for _, id in ipairs(ids) do
      local c = by_id[id]
      table.insert(lines, c and ("  %s  %s"):format(place(c), first_line(c.body)) or ("  " .. id))
    end
    table.insert(lines, "")
  end
  local in_body = preview.body_comment_ids or {}
  if #in_body > 0 then
    table.insert(lines, ("Outside the diff, written into the review body above (%d)"):format(#in_body))
    table.insert(lines, "")
  end
  local replies = preview.replies or {}
  if #replies > 0 then
    table.insert(lines, ("Replies (%d)"):format(#replies))
    for _, r in ipairs(replies) do
      local c = by_id[r.comment_id]
      table.insert(lines, "  " .. (c and first_line(c.body) or r.comment_id))
    end
    table.insert(lines, "")
  end
  local actions = preview.thread_actions or {}
  if #actions > 0 then
    table.insert(lines, ("Thread actions (%d)"):format(#actions))
    for _, a in ipairs(actions) do
      table.insert(lines, ("  %s %s"):format(a.action, a.thread_id))
    end
    table.insert(lines, "")
  end
  local skipped = preview.skipped or {}
  if #skipped > 0 then
    table.insert(lines, ("Not sent, stay queued (%d)"):format(#skipped))
    for _, s in ipairs(skipped) do
      local c = by_id[s.comment_id]
      table.insert(lines, ("  %s (%s)"):format(c and place(c) or s.comment_id, s.reason))
    end
  end
  return lines
end

---@param preview reviewkit.SubmitPreview
---@param session? reviewkit.Session
---@param on_confirm fun()
---@return integer buf
function M.show(preview, session, on_confirm)
  local lines = M.lines(preview, session)
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
