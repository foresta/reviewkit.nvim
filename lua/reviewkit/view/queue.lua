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
  local first = comment.body:match("^[^\n]*") or ""
  table.insert(parts, first)
  return table.concat(parts, " ")
end

--- The quickfix list is the queue: one entry per comment, jumping to the
--- head-side file. The list is replaced rather than appended so repeated
--- refreshes do not pile up.
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
    })
  end
  M.replace(items, TITLE .. " queue")
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
  core.on("ReviewViewedChanged", targets_changed, group)
  core.on("ReviewSessionClosed", function()
    if vim.startswith(current_title(), TITLE) then
      M.replace({}, TITLE .. " (closed)")
    end
  end, group)
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
