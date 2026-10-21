local core = require("reviewkit.core")

local M = {}

--- Set by the orchestrator, which owns the keymaps: the view only says
--- which buffers it shows and when it goes away.
---@type { buffer: fun(buf: integer)?, close: fun()? }
M.hooks = {}

local api = vim.api

local state = {
  tab = nil, ---@type integer?
  left = nil, ---@type integer?
  right = nil, ---@type integer?
  path = nil, ---@type string?
  base_bufs = {}, ---@type table<string, integer>
  head_bufs = {}, ---@type table<string, integer>
  show_seq = 0,
  showing = false,
  shape = nil, ---@type string?
  retarget = false,
}

--- The parts of a target that decide what the pair shows; a refresh that
--- keeps them only needs a `diffupdate`, so the reviewer's folds survive.
---@param session reviewkit.Session
---@return boolean
local function commit_head(session)
  return session.source.kind == "local" and session.head.sha ~= nil and session.head.sha ~= vim.NIL
end

--- A working-tree head is the file itself and changes under the reviewer;
--- a commit head is a copy, so its blob decides the pair too.
---@param session reviewkit.Session
---@param target reviewkit.Target
---@return string
local function shape(session, target)
  return table.concat({
    target.path,
    target.status,
    target.base_blob or "",
    target.old_path or "",
    tostring(target.binary),
    tostring(target.too_large),
    commit_head(session) and (target.head_blob or "") or "",
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
  -- The working tree may hold another revision of a commit head's file;
  -- only the commit's own copy is a place on the reviewed lines.
  local session = core.session()
  if session and commit_head(session) then
    return nil, "head"
  end
  return core.relpath(api.nvim_buf_get_name(buf)), "head"
end

---@param session reviewkit.Session?
---@return boolean
function M.reviews_commit(session)
  return session ~= nil and commit_head(session)
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

--- The head buffer was decoded by Neovim's `fileencodings`; the base side
--- has to use the same encoding or diff mode marks every non-ASCII line.
---@param raw string base64
---@param encoding string
---@return string[]?
local function decode(raw, encoding)
  local ok, bytes = pcall(vim.base64.decode, raw)
  if not ok or encoding == "" or encoding == "utf-8" then
    return nil
  end
  local text = vim.iconv(bytes, encoding, "utf-8")
  if not text then
    return nil
  end
  local lines = vim.split((text:gsub("\r\n", "\n")), "\n", { plain = true })
  if lines[#lines] == "" then
    table.remove(lines)
  end
  return lines
end

---@param target reviewkit.Target
---@param cb fun(buf: integer)
local function base_buffer(target, cb)
  local right = state.right
  local right_buf = right and api.nvim_win_is_valid(right) and api.nvim_win_get_buf(right)
  local encoding = right_buf and (vim.b[right_buf].reviewkit_encoding or vim.bo[right_buf].fileencoding) or ""
  local key = base_key(target) .. ":" .. encoding
  local cached = state.base_bufs[key]
  if cached and api.nvim_buf_is_valid(cached) then
    return cb(cached)
  end
  local name = ("reviewkit://base/%s/%s"):format((target.base_blob or ""):sub(1, 8), target.path)
  core.base_file(target.path, function(err, data)
    if err or not data then
      -- Not cached: a transient CLI failure must not stick for the session.
      return cb(
        scratch(
          name,
          { "(cannot read the base side: " .. (err and err.message or "no data") .. ")" },
          target.path,
          "base",
          true
        )
      )
    end
    local buf
    if not data.exists then
      buf = scratch(name, { "(not present in base)" }, target.path, "base", true)
    elseif data.binary then
      buf = scratch(name, { "(binary file)" }, target.path, "base", true)
    elseif data.too_large then
      buf = scratch(name, { "(too large to show)" }, target.path, "base", true)
    else
      local lines = data.raw and decode(data.raw, encoding) or data.lines or {}
      buf = scratch(name, lines, target.path, "base", false)
    end
    state.base_bufs[key] = buf
    cb(buf)
  end)
end

--- A commit's file has no editor to pick its encoding, so the first of
--- 'fileencodings' that converts is used, as when Vim reads a file.
---@param raw string
---@return string[]? lines
---@return string? encoding
local function decode_guessing(raw)
  -- `raw` only comes when the bytes are not UTF-8.
  for _, enc in ipairs(vim.opt.fileencodings:get()) do
    if enc ~= "ucs-bom" and enc ~= "default" and enc ~= "utf-8" then
      local lines = decode(raw, enc)
      if lines then
        return lines, enc
      end
    end
  end
  return nil, nil
end

---@param session reviewkit.Session
---@param target reviewkit.Target
---@param cb fun(buf: integer)
local function head_buffer(session, target, cb)
  local key = (target.head_blob or session.head.sha) .. ":" .. target.path
  local cached = state.head_bufs[key]
  if cached and api.nvim_buf_is_valid(cached) then
    return cb(cached)
  end
  local name = ("reviewkit://head/%s/%s"):format((target.head_blob or session.head.sha):sub(1, 8), target.path)
  core.side_file("head", target.path, function(err, data)
    if err or not data then
      return cb(
        scratch(
          name,
          { "(cannot read the head side: " .. (err and err.message or "no data") .. ")" },
          target.path,
          "head",
          true
        )
      )
    end
    local buf
    if not data.exists then
      buf = scratch(name, { "(not present in head)" }, target.path, "head", true)
    elseif data.binary then
      buf = scratch(name, { "(binary file)" }, target.path, "head", true)
    elseif data.too_large then
      buf = scratch(name, { "(too large to show)" }, target.path, "head", true)
    else
      local lines, encoding = nil, nil
      if data.raw then
        lines, encoding = decode_guessing(data.raw)
      end
      buf = scratch(name, lines or data.lines or {}, target.path, "head", false)
      vim.b[buf].reviewkit_encoding = encoding
    end
    state.head_bufs[key] = buf
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
  if M.hooks.buffer then
    M.hooks.buffer(buf)
  end
end

---@param session reviewkit.Session
---@param target reviewkit.Target
---@param done fun(ok: boolean)
---@param current fun(): boolean false once a later show took over
local function show_head(session, target, done, current)
  if target.status == "deleted" then
    local buf = scratch("reviewkit://deleted/" .. target.path, { "(deleted in head)" }, target.path, "head", true)
    api.nvim_win_set_buf(state.right, buf)
    return done(true)
  end
  -- Comments of such a session belong to the commit's lines; the working
  -- tree may be another branch or carry edits that shift them.
  if commit_head(session) then
    return head_buffer(session, target, function(buf)
      -- A later show owns the window now; this one must not touch it.
      if not M.is_open() or not current() then
        return
      end
      api.nvim_win_set_buf(state.right, buf)
      done(true)
    end)
  end
  local file = session.source.worktree .. "/" .. target.path
  local shown = api.nvim_win_get_buf(state.right)
  if M.location(shown) == target.path and not M.is_placeholder(shown) then
    -- Re-editing the shown file would fail with E37 once it is modified.
    return done(true)
  end
  local ok, err = pcall(api.nvim_win_call, state.right, function()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
  end)
  if not ok then
    vim.notify("[reviewkit] " .. tostring(err), vim.log.levels.WARN)
  end
  done(ok)
end

---@param path string
---@param on_shown? fun() runs once the base side is in place
function M.show(path, on_shown)
  local session = core.session()
  local target = core.target(path)
  if not session or not target or not M.is_open() then
    return
  end
  state.showing = true
  state.show_seq = state.show_seq + 1
  local seq = state.show_seq
  -- Buffers once diffed stay in Vim's diff set after they are hidden, and
  -- the ninth one is E96; the pair is reset whenever it changes.
  api.nvim_win_call(state.right, function()
    vim.cmd("diffoff!")
  end)
  local function current()
    return seq == state.show_seq
  end
  show_head(session, target, function(ok)
    if not current() then
      return
    end
    if not ok then
      state.showing = false
      local previous = state.path and core.target(state.path)
      if previous and not previous.binary and not previous.too_large then
        set_diff(state.left, true)
        set_diff(state.right, true)
      end
      return
    end
    state.path = path
    state.shape = shape(session, target)
    local function finish(left_buf)
      if not M.is_open() or state.path ~= path or not current() then
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
      if on_shown then
        pcall(on_shown)
      end
      if state.retarget then
        state.retarget = false
        vim.schedule(M.retarget)
      end
    end
    if target.status == "added" or target.status == "untracked" then
      finish(scratch("reviewkit://added/" .. path, { "(new file)" }, path, "base", true))
    else
      base_buffer(target, finish)
    end
  end, current)
end

---@param path? string target to show first; the first target otherwise
---@param on_shown? fun()
function M.open(path, on_shown)
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
  M.show(path or state.path or targets[1].path, on_shown)
end

--- Places the cursor on a line of either side. The base side only exists
--- in this view, so jumps to base-side comments and threads come here.
---@param path string
---@param side reviewkit.Side
---@param line integer
function M.reveal(path, side, line)
  if not core.target(path) then
    return
  end
  M.open(path, function()
    local win = side == "base" and state.left or state.right
    if not win then
      return
    end
    api.nvim_set_current_win(win)
    local last = api.nvim_buf_line_count(api.nvim_win_get_buf(win))
    api.nvim_win_set_cursor(win, { math.max(1, math.min(line, last)), 0 })
  end)
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
  if shape(session, target) ~= state.shape then
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
  if M.hooks.close then
    M.hooks.close()
  end
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
function M.step_unviewed(delta)
  local targets = core.targets()
  if #targets == 0 or not M.is_open() then
    return
  end
  local index
  for i, t in ipairs(targets) do
    if t.path == state.path then
      index = i
    end
  end
  local others = index and #targets - 1 or #targets
  for n = 1, others do
    local t = targets[(((index or 0) - 1 + delta * n) % #targets) + 1]
    if not t.viewed then
      return M.show(t.path)
    end
  end
  local current = index and targets[index]
  local message = current and not current.viewed and "every other file is viewed" or "every file is viewed"
  vim.notify("[reviewkit] " .. message, vim.log.levels.INFO)
end

--- Threads in target order, then by line, base before head on the same
--- line number (the two sides number lines independently). Outdated and
--- file-level threads have no line to land on and are reached through the
--- thread list. A range thread is landed on at its first line when the
--- range stays on one side.
---@param delta integer
function M.jump_thread(delta)
  local order = {}
  for i, t in ipairs(core.targets()) do
    order[t.path] = i
  end
  local SIDE = { base = 0, head = 1 }
  local function key(rank, line, side)
    return { rank, line, SIDE[side] or 1 }
  end
  local function less(a, b)
    for k = 1, 3 do
      if a[k] ~= b[k] then
        return a[k] < b[k]
      end
    end
    return false
  end
  local spots = {}
  for _, t in ipairs(core.threads()) do
    if t.line ~= nil and t.line ~= vim.NIL and order[t.path] then
      -- A range may start on the other side, where start_line counts the
      -- other file's lines.
      local start_side = (t.start_side ~= nil and t.start_side ~= vim.NIL) and t.start_side or t.side
      local has_start = t.start_line ~= nil and t.start_line ~= vim.NIL
      local line = (has_start and start_side == t.side) and t.start_line or t.line
      table.insert(spots, { path = t.path, side = t.side, line = line, key = key(order[t.path], line, t.side) })
    end
  end
  if #spots == 0 then
    return vim.notify("[reviewkit] no thread to move to", vim.log.levels.INFO)
  end
  table.sort(spots, function(a, b)
    return less(a.key, b.key)
  end)
  local win = api.nvim_get_current_win()
  local here = key(order[state.path or ""] or 0, api.nvim_win_get_cursor(0)[1], win == state.left and "base" or "head")
  local pick
  if delta > 0 then
    for _, s in ipairs(spots) do
      if less(here, s.key) then
        pick = s
        break
      end
    end
    pick = pick or spots[1]
  else
    for i = #spots, 1, -1 do
      if less(spots[i].key, here) then
        pick = spots[i]
        break
      end
    end
    pick = pick or spots[#spots]
  end
  M.reveal(pick.path, pick.side, pick.line)
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
      -- A commit head's working-tree file (from :cnext, :edit) is swapped
      -- for the commit's copy, even when it is the file already shown.
      local worktree_of_commit = not path and M.reviews_commit(core.session())
      if worktree_of_commit then
        path = core.relpath(api.nvim_buf_get_name(args.buf))
      end
      if path and (path ~= state.path or worktree_of_commit) and core.target(path) then
        local win = state.right --[[@as integer]]
        vim.schedule(function()
          -- Read now, not at BufWinEnter: `:edit +N` and `:cnext` place the
          -- cursor after the event.
          local line = worktree_of_commit
            and win
            and api.nvim_win_is_valid(win)
            and api.nvim_win_get_buf(win) == args.buf
            and api.nvim_win_get_cursor(win)[1]
          M.show(path, line and function()
            if line and win == state.right and api.nvim_win_is_valid(win) then
              local last = api.nvim_buf_line_count(api.nvim_win_get_buf(win))
              api.nvim_win_set_cursor(win, { math.max(1, math.min(line, last)), 0 })
            end
          end or nil)
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
    state.head_bufs = {}
  end, group)
end

function M._state()
  return state
end

return M
