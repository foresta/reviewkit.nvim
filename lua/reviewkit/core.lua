local cli = require("reviewkit.cli")
local config = require("reviewkit.config")

local M = {}

---@alias reviewkit.Event
---| "ReviewSessionOpened"
---| "ReviewTargetsChanged"
---| "ReviewQueueChanged"
---| "ReviewDecisionChanged"
---| "ReviewThreadsChanged"
---| "ReviewViewedChanged"
---| "ReviewSubmitted"
---| "ReviewSessionClosed"

---@class reviewkit.LuaSink
---@field submit fun(session: reviewkit.Session, opts: table, cb: fun(err: reviewkit.Error?, result: reviewkit.SinkResult?))
---@field available? fun(): boolean, string?
---@field consumes? fun(session: reviewkit.Session, requested: boolean?): boolean

local state = {
  session = nil, ---@type reviewkit.Session?
  watcher = nil, ---@type uv.uv_fs_event_t?
  debounce = nil, ---@type uv.uv_timer_t?
  watched_id = nil, ---@type string?
  sinks = {}, ---@type table<string, reviewkit.LuaSink>
  sink_list = nil, ---@type reviewkit.SinkInfo[]?
}

local WATCH_DEBOUNCE_MS = 150

---@param event reviewkit.Event
---@param data? table
local function fire(event, data)
  local payload = vim.tbl_extend("force", {
    session_id = state.session and state.session.id or nil,
    rev = state.session and state.session.rev or nil,
  }, data or {})
  vim.api.nvim_exec_autocmds("User", { pattern = event, data = payload, modeline = false })
end

local sections = {
  { etag = "targets", event = "ReviewTargetsChanged" },
  { etag = "comments", event = "ReviewQueueChanged" },
  { etag = "decision", event = "ReviewDecisionChanged" },
  { etag = "threads", event = "ReviewThreadsChanged" },
  { etag = "thread_actions", event = "ReviewThreadsChanged" },
  { etag = "viewed", event = "ReviewViewedChanged" },
}

--- Threads and thread actions share an event, so it fires at most once per
--- response.
--- A response older than the cache (a slow mutation answered after the
--- watcher already pulled a newer rev) is dropped so events never run
--- backwards.
---@param session reviewkit.Session
---@param extra? { submitted?: reviewkit.SinkResult }
---@return boolean accepted
local function accept(session, extra)
  local previous = state.session
  -- The same id with a different created_at is a session purged and
  -- reopened elsewhere; its rev restarted, so it counts as a new session.
  local opened = previous == nil or previous.id ~= session.id or previous.created_at ~= session.created_at
  if previous and not opened and session.rev < previous.rev then
    return false
  end
  if previous and opened then
    fire("ReviewSessionClosed")
  end
  state.session = session
  if opened then
    fire("ReviewSessionOpened")
  end
  local fired = {}
  for _, section in ipairs(sections) do
    local before = previous and not opened and previous.etags[section.etag] or nil
    if before ~= session.etags[section.etag] and not fired[section.event] then
      fired[section.event] = true
      fire(section.event)
    end
  end
  if extra and extra.submitted then
    fire("ReviewSubmitted", { result = extra.submitted })
  end
  return true
end

local function stop_watching()
  state.watched_id = nil
  if state.watcher then
    state.watcher:stop()
    state.watcher:close()
    state.watcher = nil
  end
  if state.debounce then
    state.debounce:stop()
    state.debounce:close()
    state.debounce = nil
  end
end

--- Other Neovim instances write the same session.json (temp + rename), so
--- the directory is watched rather than the file, whose inode changes on
--- every save. Bursts are debounced before one `session get`.
local function watch(session)
  stop_watching()
  local dir = vim.fs.dirname(session.state_path)
  local watcher = vim.uv.new_fs_event()
  local debounce = vim.uv.new_timer()
  if not watcher or not debounce then
    return
  end
  state.watcher = watcher
  state.debounce = debounce
  state.watched_id = session.id
  local id = session.id
  local ok, start_err = watcher:start(dir, {}, function(err, filename)
    if err or (filename and filename ~= "session.json") then
      return
    end
    debounce:stop()
    debounce:start(WATCH_DEBOUNCE_MS, 0, function()
      vim.schedule(function()
        if not state.session or state.session.id ~= id then
          return
        end
        cli.run({ "session", "get", "--session", id }, {}, function(get_err, envelope)
          if get_err or not envelope or not envelope.session then
            return
          end
          if state.session and state.session.id == id then
            accept(envelope.session)
          end
        end)
      end)
    end)
  end)
  if not ok then
    stop_watching()
    vim.notify_once("[reviewkit] cannot watch " .. dir .. ": " .. tostring(start_err), vim.log.levels.WARN)
  end
end

---@param err reviewkit.Error?
---@param envelope reviewkit.Envelope?
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
---@param extra? { submitted?: reviewkit.SinkResult }
local function take_session(err, envelope, cb, extra)
  if err then
    return cb(err)
  end
  local session = envelope and envelope.session
  if not session then
    return cb({ code = "E_INTERNAL", message = "the CLI returned no session", retryable = false, details = {} })
  end
  -- A dropped (older) response still reports the submit it carried; the
  -- caller always receives what the cache holds now, never a stale copy.
  if not accept(session, extra) and extra and extra.submitted then
    fire("ReviewSubmitted", { result = extra.submitted })
  end
  if state.watched_id ~= state.session.id then
    watch(state.session)
  end
  cb(nil, state.session)
end

local function require_session()
  if not state.session then
    return nil,
      {
        code = "E_NO_SESSION",
        message = "no review session is open",
        hint = ":Review diff or :Review pr first",
        retryable = false,
        details = {},
      }
  end
  return state.session
end

---@class reviewkit.OpenSpec
---@field kind "local"|"pr"
---@field base? string
---@field head? string
---@field pr? string|integer number or URL
---@field cwd? string

---@param spec reviewkit.OpenSpec
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.open(spec, cb)
  local args
  if spec.kind == "pr" then
    args = { "session", "open", "--pr", tostring(spec.pr) }
  else
    args = { "session", "open", "--local", "--base", spec.base or "HEAD" }
    if spec.head then
      vim.list_extend(args, { "--head", spec.head })
    end
  end
  cli.call(args, { cwd = spec.cwd }, function(err, envelope)
    take_session(err, envelope, cb)
  end)
end

---@param id string
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.resume(id, cb)
  cli.call({ "session", "open", "--session", id }, {}, function(err, envelope)
    take_session(err, envelope, cb)
  end)
end

---@param opts? table
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.refresh(opts, cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  cli.call({ "session", "refresh", "--session", session.id }, {}, function(run_err, envelope)
    take_session(run_err, envelope, cb)
  end)
end

---@param opts? { purge?: boolean }
---@param cb fun(err: reviewkit.Error?)
function M.close(opts, cb)
  opts = opts or {}
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  local args = { "session", "close", "--session", session.id }
  if opts.purge then
    table.insert(args, "--purge")
  end
  cli.call(args, {}, function(run_err)
    if run_err then
      return cb(run_err)
    end
    stop_watching()
    fire("ReviewSessionClosed")
    state.session = nil
    cb(nil)
  end)
end

---@return reviewkit.Session?
function M.session()
  return state.session
end

---@return reviewkit.Target[]
function M.targets()
  return state.session and state.session.targets or {}
end

---@return reviewkit.Comment[]
function M.comments()
  return state.session and state.session.comments or {}
end

---@return reviewkit.Decision?
function M.decision()
  return state.session and state.session.decision or nil
end

---@return reviewkit.Thread[]
function M.threads()
  return state.session and state.session.threads or {}
end

---@param path string
---@return reviewkit.Target?
function M.target(path)
  for _, target in ipairs(M.targets()) do
    if target.path == path then
      return target
    end
  end
  return nil
end

--- The session path of a buffer, or nil when it is outside the worktree.
--- Both sides are resolved through realpath because `:edit` resolves
--- symlinks (macOS `/var` → `/private/var`) while a worktree recorded from
--- user input may not be.
---@param name string absolute buffer name
---@return string? path
function M.relpath(name)
  local session = state.session
  if not session or name == "" then
    return nil
  end
  local worktree = session.source.worktree
  local real_root = vim.uv.fs_realpath(worktree) or worktree
  local real_name = vim.uv.fs_realpath(name) or name
  return vim.fs.relpath(real_root, real_name) or vim.fs.relpath(worktree, name)
end

--- Synchronous, from the cached commentable ranges: the view calls this on
--- every cursor move, so it must not spawn anything.
---@param path string
---@param side reviewkit.Side
---@param line integer
---@return boolean
function M.is_commentable(path, side, line)
  local target = M.target(path)
  if not target or target.binary then
    return false
  end
  for _, range in ipairs(target.commentable[side] or {}) do
    if line >= range[1] and line <= range[2] then
      return true
    end
  end
  return false
end

local function mutate(args, stdin, cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  local full = { args[1], args[2], "--session", session.id }
  for i = 3, #args do
    table.insert(full, args[i])
  end
  cli.call(full, { stdin = stdin }, function(run_err, envelope)
    take_session(run_err, envelope, function(take_err, taken)
      cb(take_err, taken, envelope and envelope.data or nil)
    end)
  end)
end

--- Newest first; not cached, since the session is sent without it.
---@param cb fun(err: reviewkit.Error?, history: reviewkit.Comment[]?)
function M.history(cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  cli.call({ "history", "list", "--session", session.id }, {}, function(run_err, envelope)
    cb(run_err, envelope and envelope.data or nil)
  end)
end

---@param id string
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.restore(id, cb)
  mutate({ "history", "restore", "--id", id }, nil, cb)
end

---@param sink string?
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?, data?: { moved: string[] })
function M.clear_delivered(sink, cb)
  local args = { "comment", "clear", "--delivered" }
  if sink then
    vim.list_extend(args, { "--sink", sink })
  end
  mutate(args, nil, cb)
end

---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?, data?: { removed: integer })
function M.clear_history(cb)
  mutate({ "history", "clear" }, nil, cb)
end

---@param input reviewkit.CommentInput
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?, data?: { id: string })
function M.add_comment(input, cb)
  mutate({ "comment", "add" }, input, cb)
end

---@param id string
---@param patch reviewkit.CommentPatch
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.update_comment(id, patch, cb)
  mutate({ "comment", "update", "--id", id }, patch, cb)
end

---@param id string
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.remove_comment(id, cb)
  mutate({ "comment", "remove", "--id", id }, nil, cb)
end

---@param decision reviewkit.DecisionInput
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.set_decision(decision, cb)
  mutate({ "decision", "set" }, decision, cb)
end

---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.clear_decision(cb)
  mutate({ "decision", "clear" }, nil, cb)
end

---@param path string
---@param on boolean
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.set_viewed(path, on, cb)
  local args = { "target", "viewed", "--path", path }
  if not on then
    table.insert(args, "--undo")
  end
  mutate(args, nil, cb)
end

---@param id string
---@param resolved boolean
---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.resolve_thread(id, resolved, cb)
  local args = { "thread", "resolve", "--thread", id }
  if not resolved then
    table.insert(args, "--undo")
  end
  mutate(args, nil, cb)
end

---@param cb fun(err: reviewkit.Error?, session: reviewkit.Session?)
function M.refresh_threads(cb)
  mutate({ "thread", "list", "--refresh" }, nil, cb)
end

---@param thread_id string
---@return reviewkit.ThreadActionKind?
function M.pending_thread_action(thread_id)
  for _, action in ipairs(state.session and state.session.thread_actions or {}) do
    if action.thread_id == thread_id then
      return action.action
    end
  end
  return nil
end

--- `nil` for local sessions, which have no pull request.
---@param cb fun(err: reviewkit.Error?, info: reviewkit.PrInfo?)
function M.pr_info(cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  cli.call({ "pr", "show", "--session", session.id }, {}, function(run_err, envelope)
    if run_err then
      return cb(run_err)
    end
    cb(nil, envelope and envelope.data or nil)
  end)
end

---@param path string
---@param cb fun(err: reviewkit.Error?, data: table?)
function M.base_file(path, cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  cli.call({ "file", "base", "--session", session.id, "--path", path }, {}, function(run_err, envelope)
    if run_err then
      return cb(run_err)
    end
    cb(nil, envelope and envelope.data or {})
  end)
end

--- A sink registered here runs in Lua (clipboard, a custom integration);
--- its result goes through `submit apply` so the queue is updated by the
--- same rules as for the built-in sinks.
---@param name string
---@param impl reviewkit.LuaSink
function M.register_sink(name, impl)
  state.sinks[name] = impl
end

--- A registered sink shadows a built-in one of the same name, as it does
--- in `submit`. A failed `sink list` still yields the registered sinks,
--- with the error.
---@param cb fun(err: reviewkit.Error?, sinks: reviewkit.SinkInfo[])
function M.sinks(cb)
  cli.call({ "sink", "list" }, {}, function(err, envelope)
    local list = {}
    for _, info in ipairs(not err and envelope and envelope.data or {}) do
      if not state.sinks[info.name] then
        table.insert(list, info)
      end
    end
    local names = vim.tbl_keys(state.sinks)
    table.sort(names)
    for _, name in ipairs(names) do
      local impl = state.sinks[name]
      local ok, reason = true, nil
      if impl.available then
        ok, reason = impl.available()
      end
      table.insert(list, { name = name, available = ok ~= false, reason = reason, runtime = "lua" })
    end
    state.sink_list = list
    cb(err, list)
  end)
end

--- Synchronous so completion never waits on the CLI; before any `sinks`
--- call the CLI's names are unknown, and the ones it has always had stand in.
---@return string[]
function M.sink_names()
  local seen, names = {}, {}
  for _, info in ipairs(state.sink_list or { { name = "herdr" }, { name = "github" } }) do
    seen[info.name] = true
    table.insert(names, info.name)
  end
  for name in pairs(state.sinks) do
    if not seen[name] then
      table.insert(names, name)
    end
  end
  table.sort(names)
  return names
end

---@param session reviewkit.Session
---@param ids string[]
---@param cb fun(err: reviewkit.Error?, view: reviewkit.Session?, decision_left: boolean?)
local function narrowed(session, ids, cb)
  if #ids == 0 then
    return vim.schedule(function()
      cb({ code = "E_INVALID", message = "nothing selected: comment_ids is empty", retryable = false, details = {} })
    end)
  end
  local queued = {}
  for _, c in ipairs(session.comments) do
    queued[c.id] = true
  end
  local function finish(sent)
    -- After an async history lookup the cache may be newer; the sink must
    -- see the comments as they are now, and only of this session.
    local current = state.session
    if not current or current.id ~= session.id then
      return cb({ code = "E_NO_SESSION", message = "the review was closed meanwhile", retryable = false, details = {} })
    end
    session = current
    queued = {}
    for _, c in ipairs(session.comments) do
      queued[c.id] = true
    end
    for _, id in ipairs(ids) do
      if not queued[id] and not sent[id] then
        return cb({ code = "E_NO_COMMENT", message = "no such comment: " .. id, retryable = false, details = {} })
      end
    end
    local chosen = {}
    for _, id in ipairs(ids) do
      chosen[id] = true
    end
    local view = vim.deepcopy(session)
    view.comments = vim.tbl_filter(function(c)
      return chosen[c.id]
    end, view.comments)
    view.decision = nil
    view.thread_actions = {}
    view.history = {}
    cb(nil, view, session.decision ~= nil)
  end
  local missing = vim.tbl_filter(function(id)
    return not queued[id]
  end, ids)
  if #missing == 0 then
    return vim.schedule(function()
      finish({})
    end)
  end
  -- Like the CLI: an id already in the history was sent and is dropped.
  M.history(function(err, history)
    if err then
      return cb(err)
    end
    local sent = {}
    for _, c in ipairs(history or {}) do
      sent[c.id] = true
    end
    finish(sent)
  end)
end

---@param session reviewkit.Session
---@param sink string
---@param lua_sink reviewkit.LuaSink
---@param opts table
---@param cb fun(err: reviewkit.Error?, result: reviewkit.SinkResult?, session: reviewkit.Session?)
---@param decision_left boolean
local function submit_lua(session, sink, lua_sink, opts, cb, decision_left)
  local merged = vim.tbl_extend("force", config.get().sinks[sink] or {}, opts)
  lua_sink.submit(session, merged, function(sink_err, result)
    if sink_err then
      return cb(sink_err)
    end
    if type(result) ~= "table" then
      return cb({
        code = "E_INVALID",
        message = "sink " .. sink .. " returned no result",
        retryable = false,
        details = {},
      })
    end
    result.sink = result.sink or sink
    -- The sink worked from this snapshot; stamping it here lets the CLI
    -- keep comments edited while the sink was running, whatever the sink.
    local version = {}
    local attempted = {}
    for _, c in ipairs(session.comments) do
      version[c.id] = c.updated_at
      table.insert(attempted, c.id)
    end
    for _, s in ipairs(result.sent or {}) do
      s.version = s.version or version[s.comment_id]
    end
    result.attempted = result.attempted or attempted
    -- "" matches no decision: one set while a sink that saw none was
    -- running must not be recorded as delivered.
    result.decision_version = result.decision_version or (session.decision and session.decision.updated_at or "")
    if decision_left and result.decision == "not_applicable" then
      result.decision = "skipped"
    end
    local consume = merged.consume
    if consume == nil and lua_sink.consumes then
      consume = lua_sink.consumes(session, nil)
    end
    local args = { "submit", "apply", "--session", session.id }
    if consume then
      table.insert(args, "--consume")
    end
    cli.call(args, { stdin = result }, function(run_err, envelope)
      take_session(run_err, envelope, function(take_err, taken)
        cb(take_err, envelope and envelope.data or nil, taken)
      end, { submitted = envelope and envelope.data or nil })
    end)
  end)
end

---@param sink string
---@param opts? table
---@param cb fun(err: reviewkit.Error?, result: reviewkit.SinkResult?, session: reviewkit.Session?)
function M.submit(sink, opts, cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  opts = opts or {}
  local lua_sink = state.sinks[sink]
  if lua_sink and opts.comment_ids then
    -- The same view the CLI gives its own sinks: the chosen comments only,
    -- without the decision and thread actions.
    return narrowed(session, opts.comment_ids, function(narrow_err, view, decision_left)
      if narrow_err or not view then
        return cb(narrow_err)
      end
      if #view.comments == 0 then
        return cb({
          code = "E_INVALID",
          message = "nothing to submit: the selection was already sent",
          retryable = false,
          details = {},
        })
      end
      submit_lua(view, sink, lua_sink, opts, cb, decision_left == true)
    end)
  end
  if lua_sink then
    return submit_lua(session, sink, lua_sink, opts, cb, false)
  end
  local args = { "submit", "--session", session.id, "--sink", sink }
  local stdin = nil
  local sink_defaults = config.get().sinks[sink]
  local merged = vim.tbl_extend("force", sink_defaults or {}, opts)
  merged.confirm = nil
  if next(merged) ~= nil then
    vim.list_extend(args, { "--options", "-" })
    stdin = merged
  end
  cli.call(args, { stdin = stdin }, function(run_err, envelope)
    take_session(run_err, envelope, function(take_err, taken)
      cb(take_err, envelope and envelope.data or nil, taken)
    end, { submitted = envelope and envelope.data or nil })
  end)
end

---@param sink string
---@param opts? table
---@param cb fun(err: reviewkit.Error?, preview: reviewkit.SubmitPreview?)
function M.preview(sink, opts, cb)
  local session, err = require_session()
  if not session then
    return vim.schedule(function()
      cb(err)
    end)
  end
  local args = { "submit", "--session", session.id, "--sink", sink, "--dry-run" }
  local merged = vim.tbl_extend("force", config.get().sinks[sink] or {}, opts or {})
  merged.confirm = nil
  local stdin = nil
  if next(merged) ~= nil then
    vim.list_extend(args, { "--options", "-" })
    stdin = merged
  end
  cli.call(args, { stdin = stdin }, function(run_err, envelope)
    if run_err then
      return cb(run_err)
    end
    cb(nil, envelope and envelope.data and envelope.data.preview or nil)
  end)
end

---@param event reviewkit.Event
---@param fn fun(args: { data: { session_id?: string, rev?: integer, result?: reviewkit.SinkResult } })
---@param group? integer
---@return integer
function M.on(event, fn, group)
  return vim.api.nvim_create_autocmd("User", { pattern = event, group = group, callback = fn })
end

--- Test seam: unlike `close`, it never talks to the CLI.
function M._reset()
  stop_watching()
  state.session = nil
  state.sinks = {}
  state.sink_list = nil
end

return M
