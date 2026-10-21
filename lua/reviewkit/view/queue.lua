local core = require("reviewkit.core")

local M = {}

local TITLE = "reviewkit"

---@param session reviewkit.Session
---@return string
local function worktree(session)
  return session.source.worktree
end

---@param comment reviewkit.Comment
---@return string
local function label(comment)
  local parts = {}
  if comment.kind ~= "issue" then
    table.insert(parts, "[" .. comment.kind .. "]")
  end
  if comment.side == "base" then
    table.insert(parts, "(base)")
  end
  if comment.stale then
    table.insert(parts, "(stale)")
  end
  if comment.last_error then
    table.insert(parts, "(failed: " .. comment.last_error.code .. ")")
  end
  for _, d in ipairs(comment.deliveries or {}) do
    table.insert(parts, ("(%s %s)"):format(d.mode == "sent" and "→" or d.mode, d.sink))
  end
  local first = comment.body:match("^[^\n]*") or ""
  table.insert(parts, first)
  return table.concat(parts, " ")
end

--- Replaced rather than appended, so repeated refreshes do not pile up.
function M.show_comments()
  local session = core.session()
  if not session then
    return
  end
  local items = {}
  for _, c in ipairs(session.comments) do
    table.insert(items, {
      filename = worktree(session) .. "/" .. c.path,
      lnum = c.line,
      col = 1,
      text = label(c),
      type = c.stale and "W" or "",
      user_data = { path = c.path, side = c.side, line = c.line, comment_id = c.id },
    })
  end
  M.replace(items, TITLE .. " queue")
end

--- Not kept in the session cache (the CLI sends it on request only), so
--- the list is a snapshot until `:ReviewHistory` runs again.
---@param history reviewkit.Comment[]
function M.show_history(history)
  local session = core.session()
  if not session then
    return
  end
  local items = {}
  for _, c in ipairs(history) do
    table.insert(items, {
      filename = worktree(session) .. "/" .. c.path,
      lnum = c.line,
      col = 1,
      text = label(c),
      user_data = { path = c.path, side = c.side, line = c.line, comment_id = c.id },
    })
  end
  M.replace(items, TITLE .. " history")
end

function M.show_targets()
  local session = core.session()
  if not session then
    return
  end
  local items = {}
  for _, t in ipairs(session.targets) do
    local first = t.hunks[1] and math.max(t.hunks[1].head_start, 1) or 1
    table.insert(items, {
      filename = worktree(session) .. "/" .. t.path,
      lnum = first,
      col = 1,
      text = ("%s +%d -%d%s"):format(t.status, t.stats.added, t.stats.removed, t.viewed and " ✓" or ""),
    })
  end
  M.replace(items, TITLE .. " targets")
end

function M.show_threads()
  local session = core.session()
  if not session then
    return
  end
  local status = require("reviewkit.view.thread").status
  local items = {}
  for _, t in ipairs(session.threads) do
    local first = t.comments[1]
    local s = status(t)
    table.insert(items, {
      filename = worktree(session) .. "/" .. t.path,
      lnum = t.line or t.original_line or 1,
      col = 1,
      text = ("◆ %s (%d)%s %s"):format(
        first and first.author or "?",
        #t.comments,
        s ~= "" and (" [" .. s .. "]") or "",
        first and (first.body:match("^[^\n]*") or "") or ""
      ),
      type = (t.resolved or t.outdated) and "" or "I",
      user_data = { path = t.path, side = t.side, line = t.line or t.original_line or 1, thread_id = t.id },
    })
  end
  M.replace(items, TITLE .. " threads")
end

---@param items table[]
---@param title string
function M.replace(items, title)
  local current = vim.fn.getqflist({ title = 1 })
  local ours = type(current.title) == "string" and vim.startswith(current.title, TITLE)
  vim.fn.setqflist({}, ours and "r" or " ", { title = title, items = items })
end

---@return string
local function current_title()
  local title = vim.fn.getqflist({ title = 1 }).title
  return type(title) == "string" and title or ""
end

---@param suffix string?
---@return boolean
local function in_our_list(suffix)
  -- The title is global: a code window must not act on the quickfix
  -- list's lines just because that list is ours.
  local info = vim.fn.getwininfo(vim.api.nvim_get_current_win())[1]
  if not info or info.quickfix ~= 1 or info.loclist == 1 then
    return false
  end
  if suffix then
    return current_title() == TITLE .. " " .. suffix
  end
  return vim.startswith(current_title(), TITLE)
end

--- The keys are mapped in every quickfix buffer (the list's title changes
--- under it); outside our lists they must behave as if unmapped.
---@param key string
local function pass_through(key)
  vim.api.nvim_feedkeys(vim.v.count > 0 and (vim.v.count .. key) or key, "n", false)
end

---@return { path: string, side: reviewkit.Side, line: integer, thread_id?: string, comment_id?: string }?
local function entry_under_cursor()
  if not in_our_list() then
    return nil
  end
  local item = vim.fn.getqflist()[vim.fn.line(".")]
  local data = item and item.user_data
  return type(data) == "table" and data or nil
end

--- `<CR>` on a base-side entry would open the head file at a base line
--- number; those go to the base window of the diff view instead.
function M.open_entry()
  local data = entry_under_cursor()
  local target = data and core.target(data.path)
  local in_view = data and (data.side == "base" or (target and target.status == "deleted"))
  if not data or not in_view then
    local count = vim.v.count > 0 and tostring(vim.v.count) or ""
    return vim.cmd('execute "normal! ' .. count .. '\\<CR>"')
  end
  vim.cmd("wincmd p")
  require("reviewkit.view.diff").reveal(data.path, data.side, data.line)
end

--- The comments on lines `first`..`last` of the queue list; nil when the
--- current window is not that list.
---@param first integer
---@param last integer
---@return string[]?
function M.selected_ids(first, last)
  if not in_our_list("queue") then
    return nil
  end
  local items = vim.fn.getqflist()
  local ids = {}
  for lnum = first, last do
    local data = items[lnum] and items[lnum].user_data
    if type(data) == "table" and data.comment_id then
      table.insert(ids, data.comment_id)
    end
  end
  return ids
end

function M.restore_entry()
  if not in_our_list("history") then
    return pass_through("r")
  end
  local data = entry_under_cursor()
  if not data or not data.comment_id then
    return
  end
  core.restore(data.comment_id, function(err)
    if err then
      local place = ("%s:%d"):format(data.path, data.line)
      local message = err.message:gsub(vim.pesc(data.comment_id), function()
        return place
      end)
      return vim.notify("[reviewkit] " .. message, vim.log.levels.ERROR)
    end
    vim.notify("[reviewkit] back in the queue")
    -- The list is a snapshot; the restored comment must not stay in it.
    core.history(function(list_err, history)
      if not list_err and in_our_list("history") then
        M.show_history(history or {})
      end
    end)
  end)
end

function M.clear_delivered()
  if not in_our_list("queue") then
    return pass_through("D")
  end
  local session = core.session()
  local question = "Move the delivered comments to the history?"
  if session and session.source.kind == "pr" then
    question = question .. " GitHub will not get the ones only another sink received."
  end
  if vim.fn.confirm(question, "&Move\n&Keep", 2) ~= 1 then
    return
  end
  core.clear_delivered(nil, function(err, _, data)
    if err then
      return vim.notify("[reviewkit] " .. err.message, vim.log.levels.ERROR)
    end
    local n = data and data.moved and #data.moved or 0
    vim.notify(("[reviewkit] %d comment%s moved to the history"):format(n, n == 1 and "" or "s"))
  end)
end

function M.preview_thread()
  local data = entry_under_cursor()
  if not data or not data.thread_id then
    return vim.cmd("normal! K")
  end
  for _, t in ipairs(core.threads()) do
    if t.id == data.thread_id then
      return require("reviewkit.view.thread").show(t)
    end
  end
end

--- Keeps whichever of our lists is on screen current, and empties it when
--- the session goes away.
function M.attach()
  local group = vim.api.nvim_create_augroup("reviewkit_queue", { clear = true })
  core.on("ReviewQueueChanged", function()
    if current_title() == TITLE .. " queue" then
      M.show_comments()
    end
  end, group)
  local function targets_changed()
    if current_title() == TITLE .. " targets" then
      M.show_targets()
    end
  end
  core.on("ReviewTargetsChanged", targets_changed, group)
  core.on("ReviewThreadsChanged", function()
    if current_title() == TITLE .. " threads" then
      M.show_threads()
    end
  end, group)
  core.on("ReviewViewedChanged", targets_changed, group)
  core.on("ReviewSessionClosed", function()
    if vim.startswith(current_title(), TITLE) then
      M.replace({}, TITLE .. " (closed)")
    end
  end, group)
  vim.api.nvim_create_autocmd("FileType", {
    group = group,
    pattern = "qf",
    callback = function(args)
      -- Not tied to set_keymaps: these keys act only inside our lists and
      -- are the only way to restore or move entries there. A quickfix
      -- plugin's own buffer-local keys win, since passing a key through
      -- outside our lists would not reach theirs.
      local taken = {}
      for _, map in ipairs(vim.api.nvim_buf_get_keymap(args.buf, "n")) do
        taken[map.lhs] = true
      end
      for _, key in ipairs({
        { "<CR>", M.open_entry, "open entry" },
        { "K", M.preview_thread, "show the thread" },
        { "r", M.restore_entry, "restore to the queue" },
        { "D", M.clear_delivered, "clear delivered" },
      }) do
        if not taken[key[1]] then
          vim.keymap.set("n", key[1], key[2], { buffer = args.buf, desc = "reviewkit: " .. key[3] })
        end
      end
    end,
  })
end

function M.toggle()
  local title = current_title()
  local open = false
  for _, win in ipairs(vim.fn.getwininfo()) do
    if win.quickfix == 1 then
      open = true
    end
  end
  if open and title == TITLE .. " queue" then
    vim.cmd("cclose")
    return
  end
  M.show_comments()
  vim.cmd("botright copen")
end

return M
