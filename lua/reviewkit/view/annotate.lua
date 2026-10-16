local core = require("reviewkit.core")

local M = {}

local ns = vim.api.nvim_create_namespace("reviewkit_annotate")
local ns_range = vim.api.nvim_create_namespace("reviewkit_commentable")

---@param buf integer
---@return string? path
---@return reviewkit.Side side
local function location(buf)
  return require("reviewkit.view.diff").location(buf)
end

--- Redraws the marks of one buffer from the cached queue. Stale comments
--- are drawn dimmed so the reviewer sees that the line may have moved.
---@param buf integer
function M.refresh(buf)
  if not vim.api.nvim_buf_is_valid(buf) then
    return
  end
  vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  vim.api.nvim_buf_clear_namespace(buf, ns_range, 0, -1)
  local session = core.session()
  if not session then
    return
  end
  local path, side = location(buf)
  if not path then
    return
  end
  local last = vim.api.nvim_buf_line_count(buf)
  local target = core.target(path)
  if target and not vim.b[buf].reviewkit_placeholder then
    -- A sign rather than a line highlight: diff mode already colours the
    -- lines, and a second background would hide its add / change colours.
    for _, range in ipairs(target.commentable[side] or {}) do
      for line = range[1], math.min(range[2], last) do
        pcall(vim.api.nvim_buf_set_extmark, buf, ns_range, line - 1, 0, {
          sign_text = "▏",
          sign_hl_group = "DiagnosticHint",
          priority = 1,
        })
      end
    end
  end
  -- Outdated threads have no line; they are left to the thread list
  -- rather than guessed onto one.
  for _, t in ipairs(session.threads or {}) do
    if t.path == path and t.side == side and t.line and t.line <= last then
      local first = t.comments[1]
      local who = first and first.author or "?"
      local hl = t.resolved and "Comment" or "DiagnosticHint"
      local text = ("◆ %s (%d)%s"):format(who, #t.comments, t.resolved and " resolved" or "")
      pcall(vim.api.nvim_buf_set_extmark, buf, ns, t.line - 1, 0, {
        virt_text = { { " " .. text, hl } },
        virt_text_pos = "eol",
        hl_mode = "combine",
        priority = 5,
      })
    end
  end
  for _, c in ipairs(session.comments) do
    if c.path == path and c.side == side and c.line <= last then
      local hl = c.stale and "Comment" or "DiagnosticInfo"
      local text = (c.kind == "issue" and "●" or ("● " .. c.kind)) .. (c.stale and " (stale)" or "")
      pcall(vim.api.nvim_buf_set_extmark, buf, ns, c.line - 1, 0, {
        sign_text = "●",
        sign_hl_group = hl,
        virt_text = { { " " .. text, hl } },
        virt_text_pos = "eol",
        hl_mode = "combine",
      })
      if c.start_line and c.start_line < c.line then
        pcall(vim.api.nvim_buf_set_extmark, buf, ns, c.start_line - 1, 0, {
          end_row = c.line - 1,
          line_hl_group = c.stale and "CursorLine" or "Visual",
          priority = 10,
        })
      end
    end
  end
end

function M.refresh_all()
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) then
      M.refresh(buf)
    end
  end
end

--- ReviewSessionClosed fires while the old session is still cached, so the
--- marks are removed without reading it.
function M.clear_all()
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) then
      vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
      vim.api.nvim_buf_clear_namespace(buf, ns_range, 0, -1)
    end
  end
end

function M.attach()
  local group = vim.api.nvim_create_augroup("reviewkit_annotate", { clear = true })
  core.on("ReviewQueueChanged", M.refresh_all, group)
  core.on("ReviewThreadsChanged", M.refresh_all, group)
  core.on("ReviewTargetsChanged", M.refresh_all, group)
  core.on("ReviewSessionOpened", M.refresh_all, group)
  core.on("ReviewSessionClosed", M.clear_all, group)
  vim.api.nvim_create_autocmd({ "BufWinEnter", "BufReadPost" }, {
    group = group,
    callback = function(args)
      M.refresh(args.buf)
    end,
  })
end

---@param buf integer
---@param line integer
---@return reviewkit.Comment?
function M.comment_at(buf, line)
  local session = core.session()
  if not session then
    return nil
  end
  local path, side = location(buf)
  for _, c in ipairs(session.comments) do
    if c.path == path and c.side == side and line >= (c.start_line or c.line) and line <= c.line then
      return c
    end
  end
  return nil
end

return M
