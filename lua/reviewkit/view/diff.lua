local cli = require("reviewkit.cli")
local core = require("reviewkit.core")

local M = {}

local api = vim.api

local state = {
  tab = nil, ---@type integer?
  left = nil, ---@type integer?
  right = nil, ---@type integer?
  path = nil, ---@type string?
  base_bufs = {}, ---@type table<string, integer>
  showing = false,
  shape = nil, ---@type string?
  retarget = false,
}

--- The parts of a target that decide what the pair shows; a refresh that
--- keeps them only needs a `diffupdate`, so the reviewer's folds survive.
---@param target reviewkit.Target
---@return string
local function shape(target)
  return table.concat({
    target.path,
    target.status,
    target.base_blob or "",
    target.old_path or "",
    tostring(target.binary),
    tostring(target.too_large),
  }, "\0")
end

---@param buf integer
---@return string? path
---@return reviewkit.Side side
function M.location(buf)
  local tagged = vim.b[buf].reviewkit_path
  if tagged then
    return tagged, vim.b[buf].reviewkit_side or "head"
  end
  return core.relpath(api.nvim_buf_get_name(buf)), "head"
end

--- Placeholders stand in for a side that has no file, so they carry a
--- location for viewed toggles but must not take comments.
---@param buf integer
---@return boolean
function M.is_placeholder(buf)
  return vim.b[buf].reviewkit_placeholder == true
end

function M.is_open()
  return state.tab ~= nil
    and api.nvim_tabpage_is_valid(state.tab)
    and state.left ~= nil
    and api.nvim_win_is_valid(state.left)
    and state.right ~= nil
    and api.nvim_win_is_valid(state.right)
end

--- `bufnr()` falls back to a prefix match, which would hand `a.rs` the
--- buffer of `a.rs.orig`; only an exact name is reused.
---@param name string
---@return integer?
local function buf_named(name)
  for _, buf in ipairs(api.nvim_list_bufs()) do
    if api.nvim_buf_get_name(buf) == name then
      return buf
    end
  end
  return nil
end

---@param name string
---@param lines string[]
---@param path string
---@param side reviewkit.Side
---@param placeholder boolean
---@return integer
local function scratch(name, lines, path, side, placeholder)
  local buf = buf_named(name)
  if not buf then
    buf = api.nvim_create_buf(false, true)
    api.nvim_buf_set_name(buf, name)
  end
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "hide"
  vim.bo[buf].swapfile = false
  vim.bo[buf].modifiable = true
  api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  vim.b[buf].reviewkit_path = path
  vim.b[buf].reviewkit_side = side
  vim.b[buf].reviewkit_placeholder = placeholder
  local ft = not placeholder and vim.filetype.match({ filename = path }) or nil
  if ft then
    vim.bo[buf].filetype = ft
  end
  return buf
end

--- Keyed by blob rather than base sha: a refresh that keeps the base sha
--- can still change which file the target's base side is (a new rename).
---@param target reviewkit.Target
---@return string
local function base_key(target)
  return (target.base_blob or "") .. ":" .. (target.old_path or target.path)
end

---@param session reviewkit.Session
---@param target reviewkit.Target
---@param cb fun(buf: integer)
local function base_buffer(session, target, cb)
  local key = base_key(target)
  local cached = state.base_bufs[key]
  if cached and api.nvim_buf_is_valid(cached) then
    return cb(cached)
  end
  local name = ("reviewkit://base/%s/%s"):format((target.base_blob or ""):sub(1, 8), target.path)
  cli.call({ "file", "base", "--session", session.id, "--path", target.path }, {}, function(err, envelope)
    if err then
      -- Not cached: a transient CLI failure must not stick for the session.
      return cb(scratch(name, { "(cannot read the base side: " .. err.message .. ")" }, target.path, "base", true))
    end
    local data = envelope and envelope.data or {}
    local buf
    if not data.exists then
      buf = scratch(name, { "(not present in base)" }, target.path, "base", true)
    elseif data.binary then
      buf = scratch(name, { "(binary file)" }, target.path, "base", true)
    elseif data.too_large then
      buf = scratch(name, { "(too large to show)" }, target.path, "base", true)
    else
      buf = scratch(name, data.lines or {}, target.path, "base", false)
    end
    state.base_bufs[key] = buf
    cb(buf)
  end)
end

---@param win integer
---@param on boolean
local function set_diff(win, on)
  api.nvim_win_call(win, function()
    if on and not vim.wo.diff then
      vim.cmd("diffthis")
    elseif not on and vim.wo.diff then
      vim.cmd("diffoff")
    end
  end)
end

---@param s string
local function bar(s)
  return (s:gsub("%%", "%%%%"))
end

---@param session reviewkit.Session
---@param target reviewkit.Target
local function winbars(session, target)
  local base_label = session.base.ref or session.base.sha:sub(1, 8)
  vim.wo[state.left].winbar = ("%%#DiffDelete# BASE %%* %s  %s"):format(
    bar(base_label),
    bar(target.old_path or target.path)
  )
  vim.wo[state.right].winbar = ("%%#DiffAdd# HEAD %%* %s%s"):format(
    bar(target.path),
    target.viewed and "  ✓ viewed" or ""
  )
end

---@param buf integer
local function keymaps(buf)
  require("reviewkit.orchestrator").set_buffer_keymaps(buf)
end

---@param session reviewkit.Session
---@param target reviewkit.Target
---@return boolean ok
local function show_head(session, target)
  if target.status == "deleted" then
    local buf = scratch("reviewkit://deleted/" .. target.path, { "(deleted in head)" }, target.path, "head", true)
    api.nvim_win_set_buf(state.right, buf)
    return true
  end
  local file = session.source.worktree .. "/" .. target.path
  local shown = api.nvim_win_get_buf(state.right)
  if M.location(shown) == target.path and not M.is_placeholder(shown) then
    -- Re-editing the shown file would fail with E37 once it is modified.
    return true
  end
  local ok, err = pcall(api.nvim_win_call, state.right, function()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
  end)
  if not ok then
    vim.notify("[reviewkit] " .. tostring(err), vim.log.levels.WARN)
  end
  return ok
end

---@param path string
function M.show(path)
  local session = core.session()
  local target = core.target(path)
  if not session or not target or not M.is_open() then
    return
  end
  state.showing = true
  -- Buffers once diffed stay in Vim's diff set after they are hidden, and
  -- the ninth one is E96; the pair is reset whenever it changes.
  api.nvim_win_call(state.right, function()
    vim.cmd("diffoff!")
  end)
  if not show_head(session, target) then
    state.showing = false
    local previous = state.path and core.target(state.path)
    if previous and not previous.binary and not previous.too_large then
      set_diff(state.left, true)
      set_diff(state.right, true)
    end
    return
  end
  state.path = path
  state.shape = shape(target)
  local function finish(left_buf)
    if not M.is_open() or state.path ~= path then
      return
    end
    api.nvim_win_set_buf(state.left, left_buf)
    if not target.binary and not target.too_large then
      set_diff(state.left, true)
      set_diff(state.right, true)
    end
    winbars(session, target)
    keymaps(left_buf)
    keymaps(api.nvim_win_get_buf(state.right))
    state.showing = false
    if state.retarget then
      state.retarget = false
      vim.schedule(M.retarget)
    end
  end
  if target.status == "added" or target.status == "untracked" then
    finish(scratch("reviewkit://added/" .. path, { "(new file)" }, path, "base", true))
  else
    base_buffer(session, target, finish)
  end
end

---@param path? string target to show first; the first target otherwise
function M.open(path)
  local targets = core.targets()
  if #targets == 0 then
    return
  end
  if not M.is_open() then
    vim.cmd("tabnew")
    state.tab = api.nvim_get_current_tabpage()
    state.right = api.nvim_get_current_win()
    vim.cmd("leftabove vsplit")
    state.left = api.nvim_get_current_win()
    api.nvim_set_current_win(state.right)
  else
    api.nvim_set_current_tabpage(state.tab)
  end
  M.show(path or state.path or targets[1].path)
end

function M.retarget()
  if not M.is_open() then
    return
  end
  if state.showing then
    state.retarget = true
    return
  end
  local session = core.session()
  local targets = core.targets()
  if not session or #targets == 0 then
    return
  end
  local target = state.path and core.target(state.path)
  if not target then
    return M.show(targets[1].path)
  end
  if shape(target) ~= state.shape then
    return M.show(target.path)
  end
  winbars(session, target)
  if not target.binary and not target.too_large then
    api.nvim_win_call(state.right, function()
      vim.cmd("diffupdate")
    end)
  end
end

function M.close()
  require("reviewkit.orchestrator").clear_buffer_keymaps()
  if state.tab and api.nvim_tabpage_is_valid(state.tab) and #api.nvim_list_tabpages() > 1 then
    local nr = api.nvim_tabpage_get_number(state.tab)
    vim.cmd(nr .. "tabclose")
  end
  state.tab, state.left, state.right, state.path = nil, nil, nil, nil
  state.showing, state.shape, state.retarget = false, nil, false
end

---@param delta integer
function M.step(delta)
  local targets = core.targets()
  if #targets == 0 or not M.is_open() then
    return
  end
  local index = 1
  for i, t in ipairs(targets) do
    if t.path == state.path then
      index = i
    end
  end
  local next_index = ((index - 1 + delta) % #targets) + 1
  M.show(targets[next_index].path)
end

---@param delta integer
function M.jump_hunk(delta)
  local buf = api.nvim_get_current_buf()
  local path, side = M.location(buf)
  local target = path and core.target(path)
  if not target or #target.hunks == 0 then
    return
  end
  local starts = {}
  for _, h in ipairs(target.hunks) do
    local start = side == "base" and h.base_start or h.head_start
    table.insert(starts, math.max(start, 1))
  end
  local cursor = api.nvim_win_get_cursor(0)[1]
  local pick
  if delta > 0 then
    for _, s in ipairs(starts) do
      if s > cursor then
        pick = s
        break
      end
    end
    pick = pick or starts[1]
  else
    for i = #starts, 1, -1 do
      if starts[i] < cursor then
        pick = starts[i]
        break
      end
    end
    pick = pick or starts[#starts]
  end
  api.nvim_win_set_cursor(0, { math.min(pick, api.nvim_buf_line_count(buf)), 0 })
end

function M.attach()
  local group = api.nvim_create_augroup("reviewkit_diff", { clear = true })
  api.nvim_create_autocmd("BufWinEnter", {
    group = group,
    callback = function(args)
      if state.showing or not M.is_open() or api.nvim_get_current_win() ~= state.right then
        return
      end
      local path = M.location(args.buf)
      if path and path ~= state.path and core.target(path) then
        vim.schedule(function()
          M.show(path)
        end)
      end
    end,
  })
  api.nvim_create_autocmd("TabClosed", {
    group = group,
    callback = function()
      if state.tab and not api.nvim_tabpage_is_valid(state.tab) then
        M.close()
      end
    end,
  })
  core.on("ReviewTargetsChanged", M.retarget, group)
  core.on("ReviewViewedChanged", function()
    local target = state.path and core.target(state.path)
    local session = core.session()
    if target and session and M.is_open() then
      winbars(session, target)
    end
  end, group)
  core.on("ReviewSessionClosed", function()
    M.close()
    state.base_bufs = {}
  end, group)
end

function M._state()
  return state
end

return M
