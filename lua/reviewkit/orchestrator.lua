local config = require("reviewkit.config")
local core = require("reviewkit.core")

local M = {}

-- On top of cli.call's own LOCK_RETRY_MS: a lock held across a GitHub
-- submit outlasts those, and the user's action should still land.
M.LOCKED_ROUNDS = 5

local KINDS = { "issue", "suggestion", "question", "praise", "nit" }
local REFRESH_DEBOUNCE_MS = 300

local state = {
  attached = false,
  watchers = {}, ---@type uv.uv_fs_event_t[]
  debounce = nil, ---@type uv.uv_timer_t?
  refreshing = false,
  refresh_again = false,
  locked_rounds = 0,
  retry_pending = false,
  previewing = false,
  mapped_bufs = {}, ---@type table<integer, true>
}

---@param err reviewkit.Error
local function notify_error(err)
  local msg = ("[reviewkit] %s: %s"):format(err.code, err.message)
  if err.hint then
    msg = msg .. "\n" .. err.hint
  end
  vim.notify(msg, vim.log.levels.ERROR)
end

---@param msg string
local function notify(msg)
  vim.notify("[reviewkit] " .. msg, vim.log.levels.INFO)
end

--- The reviewer's edits and the agent's edits both arrive as file writes,
--- so BufWritePost alone would miss the agent. The worktree is watched too,
--- bursts are debounced, and a refresh that lands mid-refresh runs again
--- afterwards instead of racing it.
---@param quiet? boolean background refreshes log instead of interrupting
local function request_refresh(quiet)
  if not core.session() then
    return
  end
  if state.refreshing then
    state.refresh_again = true
    return
  end
  state.refreshing = true
  core.refresh({}, function(err)
    state.refreshing = false
    if err and err.code == "E_LOCKED" then
      -- Still locked after the retries in cli.call (a long GitHub submit): the
      -- change that asked for this refresh must not be lost, but a lock that
      -- never frees must not keep the editor retrying forever.
      state.locked_rounds = state.locked_rounds + 1
      if state.locked_rounds > M.LOCKED_ROUNDS then
        state.locked_rounds = 0
        vim.notify_once("[reviewkit] the session stays locked; refresh skipped (see :ReviewLog)", vim.log.levels.WARN)
      elseif not state.retry_pending then
        state.retry_pending = true
        vim.defer_fn(function()
          state.retry_pending = false
          request_refresh(quiet)
        end, 2000)
      end
      return
    end
    state.locked_rounds = 0
    if err then
      if quiet then
        vim.notify_once(
          ("[reviewkit] background refresh failed: %s (see :ReviewLog)"):format(err.code),
          vim.log.levels.WARN
        )
      else
        notify_error(err)
      end
    end
    if state.refresh_again then
      state.refresh_again = false
      request_refresh(quiet)
    end
  end)
end

--- Every file-change signal (watcher, BufWritePost, FocusGained) goes
--- through one timer, so a save that the watcher also sees costs one refresh.
local function schedule_refresh()
  if not state.debounce then
    return request_refresh(true)
  end
  state.debounce:stop()
  state.debounce:start(REFRESH_DEBOUNCE_MS, 0, function()
    vim.schedule(function()
      request_refresh(true)
    end)
  end)
end

local function stop_watching()
  for _, watcher in ipairs(state.watchers) do
    watcher:stop()
    watcher:close()
  end
  state.watchers = {}
  if state.debounce then
    state.debounce:stop()
    state.debounce:close()
    state.debounce = nil
  end
end

---@param dir string
---@param recursive boolean
---@param relevant fun(filename: string): boolean
local function add_watcher(dir, recursive, relevant)
  local watcher = vim.uv.new_fs_event()
  if not watcher then
    return
  end
  local ok = watcher:start(dir, { recursive = recursive }, function(err, filename)
    if err then
      return
    end
    if filename and filename ~= "" and not relevant(filename) then
      return
    end
    schedule_refresh()
  end)
  if ok then
    table.insert(state.watchers, watcher)
  else
    watcher:close()
  end
end

---@param session reviewkit.Session
local function watch_worktree(session)
  stop_watching()
  state.debounce = vim.uv.new_timer()
  -- `recursive` is honoured on macOS; Linux inotify watches only the root,
  -- where BufWritePost and FocusGained still cover the reviewer's own edits.
  add_watcher(session.source.worktree, true, M._relevant_change)
  -- A linked worktree (`git worktree add`) keeps HEAD and refs outside the
  -- worktree, where the recursive watch above never looks.
  local worktree = session.source.worktree
  vim.system(
    { "git", "-C", worktree, "rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir" },
    { text = true },
    function(res)
      local dirs = res.code == 0 and vim.split(vim.trim(res.stdout or ""), "\n") or {}
      vim.schedule(function()
        local git_dir, common = dirs[1], dirs[2]
        local still_watching = state.debounce ~= nil and #state.watchers > 0
        if not still_watching or not git_dir or not common or vim.startswith(git_dir, worktree .. "/") then
          return
        end
        add_watcher(git_dir, false, function(name)
          return name == "HEAD"
        end)
        add_watcher(common, false, function(name)
          return name == "packed-refs"
        end)
        local not_lock = function(name)
          return not name:match("%.lock$")
        end
        add_watcher(common .. "/refs", true, not_lock)
        -- Linux ignores `recursive`; plain branch names live right here.
        add_watcher(common .. "/refs/heads", false, not_lock)
      end)
    end
  )
end

--- Inside a `.git` directory only a moved HEAD or ref changes what is
--- reviewed. Objects, the index and lock files change on every git command,
--- including the ones a refresh runs itself, so reacting to them would
--- refresh forever. Nested repositories have their own `.git` directories.
---@param filename string path relative to the worktree
---@return boolean
function M._relevant_change(filename)
  local name = filename:gsub("\\", "/")
  if name == ".git" or name:match("/%.git$") then
    return false
  end
  local inside = name:match("^%.git/(.*)$") or name:match("/%.git/(.*)$")
  if not inside then
    return true
  end
  if inside:match("%.lock$") then
    return false
  end
  return inside == "HEAD"
    or inside == "packed-refs"
    or inside:match("^refs/") ~= nil
    or inside:match("^reftable/") ~= nil
end

local function attach()
  if state.attached then
    return
  end
  state.attached = true
  local group = vim.api.nvim_create_augroup("reviewkit_orchestrator", { clear = true })
  require("reviewkit.view.queue").attach()
  require("reviewkit.view.annotate").attach()
  local diff = require("reviewkit.view.diff")
  diff.hooks.buffer = M.set_buffer_keymaps
  diff.hooks.close = M.clear_buffer_keymaps
  diff.attach()
  require("reviewkit.view.thread").attach()
  core.register_sink("clipboard", require("reviewkit.sink.clipboard") --[[@as reviewkit.LuaSink]])
  core.on("ReviewSessionOpened", function()
    -- Fills the :ReviewSubmit completion; nothing waits on it.
    core.sinks(function() end)
    local session = core.session()
    if session and session.head.sha == nil then
      watch_worktree(session)
    else
      stop_watching()
    end
  end, group)
  core.on("ReviewSessionClosed", stop_watching, group)
  -- A PR refresh checks the new head out under open buffers; Vim only
  -- notices on focus, so the reviewer would comment on stale text.
  core.on("ReviewTargetsChanged", function()
    local session = core.session()
    if not session or session.source.kind ~= "pr" then
      return
    end
    for _, buf in ipairs(vim.api.nvim_list_bufs()) do
      if vim.api.nvim_buf_is_loaded(buf) and vim.bo[buf].buftype == "" then
        local name = vim.api.nvim_buf_get_name(buf)
        if name ~= "" and core.relpath(name) then
          vim.cmd("checktime " .. buf)
        end
      end
    end
  end, group)
  vim.api.nvim_create_autocmd({ "BufWritePost", "FocusGained" }, {
    group = group,
    callback = function(args)
      local session = core.session()
      if not session or session.head.sha ~= nil then
        return
      end
      if args.event == "BufWritePost" and not core.relpath(vim.api.nvim_buf_get_name(args.buf)) then
        return
      end
      schedule_refresh()
    end,
  })
  if config.get().set_keymaps then
    M.set_keymaps()
  end
end

---@param session reviewkit.Session
local function announce(session)
  local n = #session.targets
  local what
  if session.source.kind == "local" then
    local base = session.source.base_spec
    what = base .. (vim.endswith(base, "...") and "" or "..") .. session.source.head_spec
  else
    what = ("#%d %s"):format(session.source.number, session.source.title)
  end
  notify(("%s: %d file%s to review"):format(what, n, n == 1 and "" or "s"))
  if session.pending_submit then
    vim.notify(
      "[reviewkit] an earlier GitHub submit was cut off; :ReviewSubmit github checks GitHub before sending again",
      vim.log.levels.WARN
    )
  end
end

---@param spec reviewkit.OpenSpec
local function open(spec)
  attach()
  core.open(spec, function(err, session)
    if err then
      return notify_error(err)
    end
    announce(session)
    require("reviewkit.view.diff").open()
    require("reviewkit.view.queue").show_targets()
    vim.cmd("botright copen")
    vim.cmd("wincmd p")
  end)
end

---@param base string
---@param head? string
function M.review_diff(base, head)
  open({ kind = "local", base = base, head = head, cwd = vim.fn.getcwd() })
end

---@param pr string number or URL
function M.review_pr(pr)
  open({ kind = "pr", pr = pr, cwd = vim.fn.getcwd() })
end

---@param id? string
function M.resume(id)
  attach()
  if id then
    return core.resume(id, function(err, session)
      if err then
        return notify_error(err)
      end
      announce(session)
      require("reviewkit.view.diff").open()
      require("reviewkit.view.queue").show_targets()
    end)
  end
  require("reviewkit.cli").call({ "session", "list" }, {}, function(err, envelope)
    if err then
      return notify_error(err)
    end
    local summaries = envelope and envelope.data or {}
    if #summaries == 0 then
      return notify("no saved sessions")
    end
    vim.ui.select(summaries, {
      prompt = "Resume review session",
      format_item = function(s)
        return ("%s  %s  (%d comment%s, %s)"):format(
          vim.fs.basename(s.repo_root or ""),
          s.title or s.id,
          s.comments or 0,
          s.comments == 1 and "" or "s",
          (s.updated_at or ""):sub(1, 16)
        )
      end,
    }, function(choice)
      if choice then
        M.resume(choice.id)
      end
    end)
  end)
end

function M.refresh()
  if not core.session() then
    return notify("no review session is open")
  end
  request_refresh(false)
end

---@param buf integer
---@return string? path
---@return reviewkit.Side side
local function location(buf)
  if not core.session() then
    notify("no review session is open")
    return nil, "head"
  end
  local path, side = require("reviewkit.view.diff").location(buf)
  if not path then
    notify("this buffer is not inside the review worktree")
  end
  return path, side
end

--- `kind:` on the first line sets the kind, so one input window covers
--- every kind without a second prompt.
---@param body string
---@return string body
---@return string kind
local function split_kind(body)
  local word, rest = body:match("^(%a+):%s*(.*)$")
  if word and vim.tbl_contains(KINDS, word:lower()) then
    return rest, word:lower()
  end
  return body, "issue"
end

--- The fence is parsed out rather than kept in the body so an agent gets
--- the replacement as such, and GitHub gets exactly one fence.
---@param text string
---@return string body
---@return string? suggestion
local function split_suggestion(text)
  local before, inner, after = text:match("^(.-)```suggestion[ \t\r]*\n(.-)\r?\n?```(.*)$")
  if not inner then
    return text, nil
  end
  local body = vim.trim(vim.trim(before or "") .. "\n" .. vim.trim(after or ""))
  return body, inner
end

---@param text string
---@return string body
---@return string kind
---@return string? suggestion
local function compose(text)
  local body, kind = split_kind(text)
  local rest, suggestion = split_suggestion(body)
  return rest, kind, suggestion
end

---@param comment reviewkit.Comment
---@return string[]
local function render(comment)
  local first = comment.kind ~= "issue" and (comment.kind .. ": ") or ""
  local lines = vim.split(first .. comment.body, "\n", { plain = true })
  if comment.suggestion then
    table.insert(lines, "```suggestion")
    -- An empty suggestion deletes the lines; a blank line inside the fence
    -- would turn it into "replace with an empty line".
    if comment.suggestion ~= "" then
      vim.list_extend(lines, vim.split(comment.suggestion, "\n", { plain = true }))
    end
    table.insert(lines, "```")
  end
  return lines
end

---@param opts? { range?: boolean, line1?: integer, line2?: integer }
---@return integer line
---@return integer? start_line
local function cursor_range(opts)
  opts = opts or {}
  local ranged = opts.range and opts.line1 and opts.line2
  local line = ranged and math.max(opts.line1, opts.line2) or vim.api.nvim_win_get_cursor(0)[1]
  local start_line = ranged and math.min(opts.line1, opts.line2) or nil
  if start_line == line then
    start_line = nil
  end
  return line, start_line
end

---@param opts? { range?: boolean, line1?: integer, line2?: integer, suggest?: boolean }
function M.comment(opts)
  opts = opts or {}
  local buf = vim.api.nvim_get_current_buf()
  local path, side = location(buf)
  if not path then
    return
  end
  if require("reviewkit.view.diff").is_placeholder(buf) then
    return notify("this side has no file to comment on")
  end
  if opts.suggest and side == "base" then
    return notify("a suggestion replaces head lines; comment on the head side")
  end
  local line, start_line = cursor_range(opts)
  local where = start_line and ("%s:%d-%d"):format(path, start_line, line) or ("%s:%d"):format(path, line)
  if side == "base" then
    where = where .. " (base)"
  end
  local outside = not core.is_commentable(path, side, line)
    or (start_line ~= nil and not core.is_commentable(path, side, start_line))
  local lines = nil
  if opts.suggest then
    lines = { "", "```suggestion" }
    vim.list_extend(lines, vim.api.nvim_buf_get_lines(buf, (start_line or line) - 1, line, false))
    table.insert(lines, "```")
  end
  require("reviewkit.view.input").open({
    title = "Comment " .. where .. (outside and " (outside the diff; agent only)" or ""),
    lines = lines,
    start_insert = true,
    footer = " :w save · q cancel · first line `nit:` sets the kind ",
    on_save = function(text)
      if vim.trim(text) == "" then
        return
      end
      local body, kind, suggestion = compose(text)
      core.add_comment({
        path = path,
        side = side,
        line = line,
        start_line = start_line,
        kind = kind,
        body = body,
        suggestion = suggestion,
      }, function(err)
        if err then
          return notify_error(err)
        end
        notify("queued " .. where)
      end)
    end,
  })
end

function M.edit_comment()
  local buf = vim.api.nvim_get_current_buf()
  local comment = require("reviewkit.view.annotate").comment_at(buf, vim.api.nvim_win_get_cursor(0)[1])
  if not comment then
    return notify("no comment on this line")
  end
  require("reviewkit.view.input").open({
    title = ("Edit %s:%d"):format(comment.path, comment.line),
    lines = render(comment),
    on_save = function(text)
      local body, kind, suggestion = compose(text)
      core.update_comment(comment.id, { body = body, kind = kind, suggestion = suggestion or vim.NIL }, function(err)
        if err then
          notify_error(err)
        end
      end)
    end,
  })
end

--- Stale comments are offered first: re-anchoring them is what this is for.
---@param opts? { range?: boolean, line1?: integer, line2?: integer }
function M.move_comment(opts)
  local buf = vim.api.nvim_get_current_buf()
  local path, side = location(buf)
  if not path then
    return
  end
  if require("reviewkit.view.diff").is_placeholder(buf) then
    return notify("this side has no file to move a comment to")
  end
  local line, start_line = cursor_range(opts)
  local candidates = vim.tbl_filter(function(c)
    return c.path == path and c.side == side and not c.reply_to
  end, core.comments())
  table.sort(candidates, function(a, b)
    if a.stale ~= b.stale then
      return a.stale
    end
    return a.line < b.line
  end)
  if #candidates == 0 then
    return notify("no comment on this file to move")
  end
  local function move(c)
    core.update_comment(c.id, { line = line, start_line = start_line or vim.NIL }, function(err)
      if err then
        return notify_error(err)
      end
      notify(("moved to %s:%d"):format(path, line))
    end)
  end
  if #candidates == 1 then
    return move(candidates[1])
  end
  vim.ui.select(candidates, {
    prompt = "Move which comment here",
    format_item = function(c)
      return ("%s:%d%s %s"):format(c.path, c.line, c.stale and " (stale)" or "", (c.body:match("^[^\n]*") or ""))
    end,
  }, function(choice)
    if choice then
      move(choice)
    end
  end)
end

function M.remove_comment()
  local buf = vim.api.nvim_get_current_buf()
  local comment = require("reviewkit.view.annotate").comment_at(buf, vim.api.nvim_win_get_cursor(0)[1])
  if not comment then
    return notify("no comment on this line")
  end
  core.remove_comment(comment.id, function(err)
    if err then
      return notify_error(err)
    end
    notify("removed " .. comment.path .. ":" .. comment.line)
  end)
end

function M.decision()
  if not core.session() then
    return notify("no review session is open")
  end
  vim.ui.select({ "approve", "request_changes", "comment", "(clear)" }, { prompt = "Review decision" }, function(choice)
    if not choice then
      return
    end
    if choice == "(clear)" then
      return core.clear_decision(function(err)
        if err then
          notify_error(err)
        end
      end)
    end
    local current = core.decision()
    require("reviewkit.view.input").open({
      title = "Decision: " .. choice,
      lines = current and vim.split(current.body, "\n") or {},
      on_save = function(body)
        core.set_decision({ event = choice, body = body }, function(err)
          if err then
            notify_error(err)
          end
        end)
      end,
    })
  end)
end

function M.toggle_viewed()
  local buf = vim.api.nvim_get_current_buf()
  local path = location(buf)
  if not path then
    return
  end
  local target = core.target(path)
  if not target then
    return notify(path .. " is not a review target")
  end
  core.set_viewed(path, not target.viewed, function(err)
    if err then
      return notify_error(err)
    end
    notify((target.viewed and "unviewed " or "viewed ") .. path)
  end)
end

---@param result reviewkit.SinkResult
local function report(result)
  local level = #result.failed > 0 and vim.log.levels.WARN or vim.log.levels.INFO
  local parts = { result.summary }
  local actions = result.thread_actions or {}
  if #actions > 0 then
    local done = #vim.tbl_filter(function(a)
      return a.status == "sent"
    end, actions)
    table.insert(parts, ("%d/%d thread actions"):format(done, #actions))
  end
  if result.url then
    table.insert(parts, result.url)
  end
  vim.notify("[reviewkit] " .. result.sink .. ": " .. table.concat(parts, " · "), level)
end

--- E_AMBIGUOUS carries the candidate panes; the user picks one and the same
--- submit is retried with an explicit target.
---@param sink? string
---@param opts? table
function M.submit(sink, opts)
  sink = sink or "herdr"
  opts = opts or {}
  if not core.session() then
    return notify("no review session is open")
  end
  -- Posting to GitHub is public and cannot be taken back (an APPROVE
  -- included), so it is shown before it is sent unless switched off.
  if sink == "github" and not opts.confirmed and config.get().sinks.github.confirm ~= false then
    if state.previewing then
      return
    end
    state.previewing = true
    return core.preview(sink, {}, function(err, plan)
      state.previewing = false
      if err then
        return notify_error(err)
      end
      require("reviewkit.view.preview").show(plan, core.session(), function()
        M.submit(sink, vim.tbl_extend("force", opts, { confirmed = true }))
      end)
    end)
  end
  local send_opts = vim.deepcopy(opts)
  send_opts.confirmed = nil
  core.submit(sink, send_opts, function(err, result)
    if err and err.code == "E_AMBIGUOUS" and not opts.target then
      local candidates = err.details and err.details.candidates or {}
      return vim.ui.select(candidates, {
        prompt = err.message,
        ---@param c reviewkit.SinkTarget
        format_item = function(c)
          return ("%s  [%s %s]  %s"):format(c.id, c.name or "-", c.status or "?", c.title or c.cwd or "")
        end,
      }, function(choice)
        if choice then
          M.submit(sink, vim.tbl_extend("force", opts, { target = choice.id }))
        end
      end)
    end
    if err then
      return notify_error(err)
    end
    if result then
      report(result)
    end
  end)
end

---@param opts? { purge?: boolean }
function M.close(opts)
  if not core.session() then
    return notify("no review session is open")
  end
  opts = opts or {}
  local unsent = #core.comments()
  if unsent > 0 then
    local what = ("%d comment%s still in the queue"):format(unsent, unsent == 1 and "" or "s")
    local question = opts.purge and (what .. ". Close and delete them?")
      or (what .. ". Close anyway? (they stay saved and come back with :Review resume)")
    if vim.fn.confirm(question, "&Close\n&Keep", 2) ~= 1 then
      return
    end
  end
  core.close(opts, function(err)
    if err then
      return notify_error(err)
    end
    notify(opts.purge and "review closed and its state deleted" or "review closed")
  end)
end

function M.history()
  if not core.session() then
    return notify("no review session is open")
  end
  core.history(function(err, history)
    if err then
      return notify_error(err)
    end
    require("reviewkit.view.queue").show_history(history or {})
    vim.cmd("botright copen")
  end)
end

function M.install()
  require("reviewkit.install").ensure_async({ log = notify }, function(result, err)
    if not result then
      return vim.notify("[reviewkit] " .. (err or "install failed"), vim.log.levels.ERROR)
    end
    if result.source == "existing" then
      return notify(("%s is up to date (%s)"):format(result.path, result.version or "?"))
    end
    notify(("installed %s %s (%s)"):format(result.path, result.version or "", result.source))
  end)
end

function M.thread()
  if not core.session() then
    return notify("no review session is open")
  end
  local view = require("reviewkit.view.thread")
  local threads = view.at(vim.api.nvim_get_current_buf(), vim.api.nvim_win_get_cursor(0)[1])
  if #threads == 0 then
    return notify("no thread on this line")
  end
  if #threads == 1 then
    return view.show(threads[1])
  end
  vim.ui.select(threads, {
    prompt = "Thread",
    format_item = function(t)
      local first = t.comments[1]
      return ("%s: %s"):format(first and first.author or "?", first and first.body:match("^[^\n]*") or "")
    end,
  }, function(choice)
    if choice then
      view.show(choice)
    end
  end)
end

---@param refresh? boolean fetch the pull request again first
function M.threads(refresh)
  local session = core.session()
  if not session then
    return notify("no review session is open")
  end
  if session.source.kind ~= "pr" then
    return notify("threads exist only in pull request sessions")
  end
  local function show()
    require("reviewkit.view.queue").show_threads()
    vim.cmd("botright copen")
  end
  if not refresh then
    return show()
  end
  core.refresh_threads(function(err)
    if err then
      return notify_error(err)
    end
    show()
  end)
end

---@param args string[]
function M.command(args)
  local sub = args[1]
  if sub == "diff" then
    M.review_diff(args[2] or "HEAD", args[3])
  elseif sub == "resume" then
    M.resume(args[2])
  elseif sub == "refresh" then
    M.refresh()
  elseif sub == "pr" and args[2] then
    M.review_pr(args[2])
  elseif sub == "threads" then
    M.threads(args[2] == "refresh")
  else
    notify("usage: :Review diff <base> [<head>] | pr <number|url> | resume [id] | refresh | threads [refresh]")
  end
end

local maps = require("reviewkit.maps")

local actions = {
  comment = function()
    M.comment()
  end,
  suggest = function()
    M.comment({ suggest = true })
  end,
  suggest_range = function()
    vim.cmd("normal! \27")
    M.comment({ range = true, line1 = vim.fn.line("'<"), line2 = vim.fn.line("'>"), suggest = true })
  end,
  move_comment = function()
    M.move_comment()
  end,
  move_comment_range = function()
    vim.cmd("normal! \27")
    M.move_comment({ range = true, line1 = vim.fn.line("'<"), line2 = vim.fn.line("'>") })
  end,
  comment_range = function()
    -- Leave visual mode first so '< and '> are set.
    vim.cmd("normal! \27")
    M.comment({ range = true, line1 = vim.fn.line("'<"), line2 = vim.fn.line("'>") })
  end,
  edit_comment = M.edit_comment,
  remove_comment = M.remove_comment,
  decision = M.decision,
  thread = M.thread,
  threads = function()
    M.threads(false)
  end,
  toggle_queue = function()
    require("reviewkit.view.queue").toggle()
  end,
  toggle_viewed = M.toggle_viewed,
  submit_default = function()
    M.submit("herdr")
  end,
  open_diff = function()
    require("reviewkit.view.diff").open()
  end,
  next_file = function()
    require("reviewkit.view.diff").step(1)
  end,
  prev_file = function()
    require("reviewkit.view.diff").step(-1)
  end,
  next_hunk = function()
    require("reviewkit.view.diff").jump_hunk(1)
  end,
  next_thread = function()
    require("reviewkit.view.diff").jump_thread(1)
  end,
  prev_thread = function()
    require("reviewkit.view.diff").jump_thread(-1)
  end,
  next_unviewed = function()
    require("reviewkit.view.diff").step_unviewed(1)
  end,
  prev_hunk = function()
    require("reviewkit.view.diff").jump_hunk(-1)
  end,
}

---@param action string
function M.run_action(action)
  actions[action]()
end

--- Global rather than buffer-local so they work in any buffer of the
--- worktree, not only the diff view's.
function M.set_keymaps()
  local skipped = {}
  for _, map in ipairs(maps) do
    local mode, lhs, action, desc, local_only = map[1], map[2], map[3], map[4], map[5]
    local free = vim.fn.maparg(lhs, mode) == ""
    if not local_only and free then
      vim.keymap.set(mode, lhs, actions[action], { desc = desc })
    elseif not local_only then
      table.insert(skipped, mode .. " " .. lhs)
    end
  end
  if #skipped > 0 then
    vim.notify_once(
      "[reviewkit] keymaps already taken, left alone: " .. table.concat(skipped, ", "),
      vim.log.levels.INFO
    )
  end
end

--- Navigation keys only exist in the diff view's buffers: globally they
--- would shadow `[f` and the `]h` of git sign plugins everywhere. A
--- buffer-local map another plugin set is left alone.
---@param buf integer
function M.set_buffer_keymaps(buf)
  if not config.get().set_keymaps or not vim.api.nvim_buf_is_valid(buf) then
    return
  end
  for _, map in ipairs(maps) do
    local mode, lhs, action, desc, local_only = map[1], map[2], map[3], map[4], map[5]
    if local_only then
      local foreign = false
      for _, existing in ipairs(vim.api.nvim_buf_get_keymap(buf, mode)) do
        if existing.lhs == lhs and existing.desc ~= desc then
          foreign = true
        end
      end
      if not foreign then
        vim.keymap.set(mode, lhs, actions[action], { buffer = buf, desc = desc })
        state.mapped_bufs[buf] = true
      end
    end
  end
end

--- Worktree buffers outlive the review; left mapped, they would keep
--- shadowing the user's global `]h` after it closed.
function M.clear_buffer_keymaps()
  for buf in pairs(state.mapped_bufs) do
    if vim.api.nvim_buf_is_valid(buf) then
      for _, map in ipairs(maps) do
        if map[5] then
          for _, existing in ipairs(vim.api.nvim_buf_get_keymap(buf, map[1])) do
            if existing.lhs == map[2] and existing.desc == map[4] then
              vim.keymap.del(map[1], map[2], { buffer = buf })
            end
          end
        end
      end
    end
  end
  state.mapped_bufs = {}
end

--- Test seam.
function M._reset()
  stop_watching()
  state.attached = false
  state.refreshing = false
  state.refresh_again = false
end

return M
