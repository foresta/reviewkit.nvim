local core = require("reviewkit.core")

local M = {}

local api = vim.api

local state = {
  win = nil, ---@type integer?
  buf = nil, ---@type integer?
  thread_id = nil, ---@type string?
  toggling = false,
}

---@param buf integer
---@param line integer
---@return reviewkit.Thread[]
function M.at(buf, line)
  local path, side = require("reviewkit.view.diff").location(buf)
  local found = {}
  if not path then
    return found
  end
  for _, t in ipairs(core.threads()) do
    local last = t.line ~= vim.NIL and t.line or nil
    -- A range may start on the other side, where start_line counts the
    -- other file's lines.
    local start_side = (t.start_side ~= nil and t.start_side ~= vim.NIL) and t.start_side or t.side
    local first = (t.start_line ~= nil and t.start_line ~= vim.NIL and start_side == t.side) and t.start_line or last
    if t.path == path and t.side == side and last and line >= first and line <= last then
      table.insert(found, t)
    end
  end
  return found
end

---@param id string
---@return reviewkit.Thread?
local function find(id)
  for _, t in ipairs(core.threads()) do
    if t.id == id then
      return t
    end
  end
  return nil
end

---@param thread reviewkit.Thread
---@return string
function M.status(thread)
  local parts = {}
  if thread.side == "base" then
    table.insert(parts, "base")
  end
  if thread.resolved then
    table.insert(parts, "resolved")
  end
  if thread.outdated then
    table.insert(parts, "outdated")
  end
  local pending = core.pending_thread_action(thread.id)
  if pending then
    table.insert(parts, pending .. " on submit")
  end
  return table.concat(parts, ", ")
end

---@param thread reviewkit.Thread
---@return string[]
local function render(thread)
  local where = ("%s:%s"):format(thread.path, thread.line or thread.original_line or "?")
  local status = M.status(thread)
  local lines = { where .. (status ~= "" and ("  [" .. status .. "]") or ""), "" }
  for _, c in ipairs(thread.comments) do
    table.insert(lines, ("%s  %s"):format(c.author, c.created_at))
    vim.list_extend(lines, vim.split(c.body, "\n", { plain = true }))
    table.insert(lines, "")
  end
  for _, c in ipairs(core.comments()) do
    if c.reply_to == thread.id then
      table.insert(lines, "(queued reply)")
      vim.list_extend(lines, vim.split(c.body, "\n", { plain = true }))
      table.insert(lines, "")
    end
  end
  return lines
end

function M.is_open()
  return state.win ~= nil and api.nvim_win_is_valid(state.win)
end

function M.close()
  if M.is_open() then
    api.nvim_win_close(state.win, true)
  end
  state.win, state.buf, state.thread_id = nil, nil, nil
end

local function redraw()
  local thread = state.thread_id and find(state.thread_id)
  if not M.is_open() or not state.buf then
    return
  end
  if not thread then
    return M.close()
  end
  vim.bo[state.buf].modifiable = true
  api.nvim_buf_set_lines(state.buf, 0, -1, false, render(thread))
  vim.bo[state.buf].modifiable = false
end

---@param thread reviewkit.Thread
function M.reply(thread)
  require("reviewkit.view.input").open_keeping({
    title = "Reply to " .. (thread.comments[1] and thread.comments[1].author or "thread"),
  }, function(text, done)
    if vim.trim(text) == "" then
      return vim.notify("[reviewkit] empty reply discarded", vim.log.levels.INFO)
    end
    core.add_comment({
      path = thread.path,
      side = thread.side,
      line = thread.line or thread.original_line or 1,
      body = text,
      reply_to = thread.id,
    }, function(err)
      if err then
        return done(err)
      end
      vim.notify("[reviewkit] reply queued", vim.log.levels.INFO)
      redraw()
    end)
  end)
end

--- The direction is read from the cached session, so a second press before
--- the first answer arrives would repeat the first instead of undoing it.
---@param thread reviewkit.Thread
function M.toggle_resolved(thread)
  if state.toggling then
    return
  end
  local pending = core.pending_thread_action(thread.id)
  local resolved_after_submit = pending and pending == "resolve" or (not pending and thread.resolved)
  state.toggling = true
  core.resolve_thread(thread.id, not resolved_after_submit, function(err)
    state.toggling = false
    if err then
      return vim.notify("[reviewkit] " .. err.message, vim.log.levels.ERROR)
    end
    redraw()
  end)
end

---@param thread reviewkit.Thread
function M.show(thread)
  M.close()
  local buf = api.nvim_create_buf(false, true)
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].filetype = "markdown"
  state.buf, state.thread_id = buf, thread.id
  local lines = render(thread)
  api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  local width = math.min(90, vim.o.columns - 8)
  local height = math.max(3, math.min(#lines, vim.o.lines - 8, 20))
  state.win = api.nvim_open_win(buf, true, {
    relative = "cursor",
    row = 1,
    col = 0,
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
    title = " thread ",
    footer = " r reply · R resolve/unresolve · q close ",
    footer_pos = "right",
  })
  vim.wo[state.win].wrap = true
  api.nvim_create_autocmd("WinLeave", {
    buffer = buf,
    callback = function()
      -- The input window for a reply opens from here; the float stays so
      -- the queued reply shows up in it.
      vim.schedule(function()
        local cur = api.nvim_get_current_buf()
        if vim.bo[cur].buftype ~= "acwrite" then
          M.close()
        end
      end)
    end,
  })
  local function current()
    return state.thread_id and find(state.thread_id)
  end
  vim.keymap.set("n", "q", M.close, { buffer = buf, nowait = true })
  vim.keymap.set("n", "<Esc>", M.close, { buffer = buf, nowait = true })
  vim.keymap.set("n", "r", function()
    local t = current()
    if t then
      M.reply(t)
    end
  end, { buffer = buf, nowait = true })
  vim.keymap.set("n", "R", function()
    local t = current()
    if t then
      M.toggle_resolved(t)
    end
  end, { buffer = buf, nowait = true })
end

function M._win()
  return state.win
end

function M.attach()
  local group = api.nvim_create_augroup("reviewkit_thread", { clear = true })
  core.on("ReviewThreadsChanged", redraw, group)
  core.on("ReviewQueueChanged", redraw, group)
  core.on("ReviewSessionClosed", M.close, group)
end

return M
