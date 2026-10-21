local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()
local child, dir, worktree

local function thread(overrides)
  return vim.tbl_deep_extend("force", {
    id = "PRRT_1",
    path = "src/a.rs",
    side = "head",
    line = 4,
    start_line = vim.NIL,
    start_side = vim.NIL,
    original_line = 4,
    resolved = false,
    outdated = false,
    subject_type = "line",
    comments = { { id = "1", author = "alice", body = "why this?\nsecond line", created_at = "t1", url = "u" } },
  }, overrides or {})
end

local function pr_session(overrides)
  local s = helpers.session({
    id = "pr/github.com/o/r/7",
    repo = { root = worktree },
    source = {
      kind = "pr",
      number = 7,
      url = "https://github.com/o/r/pull/7",
      title = "Add k",
      author = "bob",
      state = "OPEN",
      head_ref = "feature",
      base_ref = "main",
      base_oid = string.rep("b", 40),
      node_id = "PR_1",
      worktree = worktree,
    },
    head = { ref = "feature", sha = string.rep("c", 40), dirty = false },
    threads = { thread() },
  })
  return vim.tbl_deep_extend("force", s, overrides or {})
end

local function rules(extra, session)
  return vim.list_extend(extra or {}, {
    { match = { "session open --pr 7" }, body = { ok = true, data = vim.NIL, session = session or pr_session() } },
    {
      match = { "file base" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "l1", "l2", "old3", "l4", "l5", "l6" } },
      },
    },
  })
end

local function wait(cond)
  child.lua(("vim.wait(20000, function() return %s end, 20)"):format(cond))
end

local function call_with(args)
  for _, call in ipairs(helpers.calls(dir)) do
    local joined = table.concat(call.args, " ")
    local all = true
    for _, a in ipairs(args) do
      if not joined:find(a, 1, true) then
        all = false
      end
    end
    if all then
      return call
    end
  end
  return nil
end

local function open_pr(extra, session)
  helpers.respond(dir, rules(extra, session))
  child.cmd("Review pr 7")
  wait("require('reviewkit.view.diff').is_open() and not require('reviewkit.view.diff')._state().showing")
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
end

T["thread"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      dir = helpers.fake_dir()
      worktree = vim.fn.resolve(helpers.fake_dir())
      vim.fn.mkdir(worktree .. "/src", "p")
      vim.fn.writefile({ "l1", "l2", "new3", "l4", "l5", "l6" }, worktree .. "/src/a.rs")
      child = helpers.child(dir)
      child.lua(
        "_G.notices = {}; vim.notify = function(msg, level) table.insert(_G.notices, { msg = msg, level = level }) end"
      )
    end,
    post_case = function()
      child.stop()
    end,
  },
})

T["thread"]["the thread on the cursor line opens in a float with its conversation"] = function()
  open_pr()
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  eq(child.lua_get("require('reviewkit.view.thread').is_open()"), true)
  local lines = child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)")
  eq(lines[1], "src/a.rs:4")
  eq(lines[3], "alice  t1")
  eq(lines[4], "why this?")
  eq(lines[5], "second line")
end

T["thread"]["no thread on the line only notifies"] = function()
  open_pr()
  child.lua("vim.api.nvim_win_set_cursor(0, { 2, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  eq(child.lua_get("require('reviewkit.view.thread').is_open()"), false)
  local notices = child.lua_get("_G.notices")
  eq(notices[#notices].msg:find("no thread on this line", 1, true) ~= nil, true)
end

T["thread"]["R queues a resolve, and R again undoes the pending one"] = function()
  local queued = pr_session({
    rev = 2,
    thread_actions = { { thread_id = "PRRT_1", action = "resolve" } },
    etags = { thread_actions = "ta1" },
  })
  local cleared = pr_session({ rev = 3, etags = { thread_actions = "ta2" } })
  open_pr({
    { match = { "thread resolve", "--undo" }, body = { ok = true, data = vim.NIL, session = cleared } },
    { match = { "thread resolve", "--thread PRRT_1" }, body = { ok = true, data = vim.NIL, session = queued } },
  })
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  child.type_keys("R")
  wait("require('reviewkit.core').pending_thread_action('PRRT_1') == 'resolve'")
  wait("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]:find('resolve on submit', 1, true) ~= nil")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]"), "src/a.rs:4  [resolve on submit]")
  eq(call_with({ "thread resolve", "--undo" }), nil)
  child.type_keys("R")
  wait("require('reviewkit.core').pending_thread_action('PRRT_1') == nil")
  eq(call_with({ "thread resolve", "--undo" }) ~= nil, true)
end

T["thread"]["r queues a reply on the thread's line through the input window"] = function()
  local after = pr_session({
    rev = 2,
    comments = {
      {
        id = "c_1",
        path = "src/a.rs",
        side = "head",
        line = 4,
        kind = "issue",
        body = "because",
        stale = false,
        start_line = vim.NIL,
        reply_to = "PRRT_1",
      },
    },
    etags = { comments = "c1" },
  })
  open_pr({ { match = { "comment add" }, body = { ok = true, data = { id = "c_1" }, session = after } } })
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  child.type_keys("r")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'because' })")
  child.cmd("write")
  wait("#require('reviewkit.core').comments() == 1")
  local input = vim.json.decode(call_with({ "comment add" }).stdin)
  eq({ input.reply_to, input.path, input.side, input.line, input.body }, { "PRRT_1", "src/a.rs", "head", 4, "because" })
  wait(
    "require('reviewkit.view.thread').is_open() and vim.tbl_contains(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.thread')._win()), 0, -1, false), '(queued reply)')"
  )
  eq(
    child.lua_get(
      "vim.tbl_contains(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.thread')._win()), 0, -1, false), '(queued reply)')"
    ),
    true
  )
end

T["thread"]["R on a resolved thread queues an unresolve, and a second press waits for the first"] = function()
  local resolved = pr_session({ threads = { thread({ resolved = true }) } })
  local queued = pr_session({
    rev = 2,
    threads = { thread({ resolved = true }) },
    thread_actions = { { thread_id = "PRRT_1", action = "unresolve" } },
    etags = { thread_actions = "ta1" },
  })
  open_pr({ { match = { "thread resolve" }, body = { ok = true, data = vim.NIL, session = queued } } }, resolved)
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  child.type_keys("R", "R")
  wait("require('reviewkit.core').pending_thread_action('PRRT_1') == 'unresolve'")
  local resolves = 0
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "thread" and call.args[2] == "resolve" then
      resolves = resolves + 1
      eq(vim.tbl_contains(call.args, "--undo"), true)
    end
  end
  eq(resolves, 1)
end

T["thread"]["two threads on one line are offered to pick from"] = function()
  local second =
    thread({ id = "PRRT_2", comments = { { id = "2", author = "carol", body = "nit", created_at = "t2", url = "u" } } })
  open_pr(nil, pr_session({ threads = { thread(), second } }))
  child.lua(
    "vim.ui.select = function(items, opts, cb) _G.offered = vim.tbl_map(opts.format_item, items); cb(items[2]) end"
  )
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  eq(child.lua_get("_G.offered"), { "alice: why this?", "carol: nit" })
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 2, 3, false)[1]"), "carol  t2")
end

T["thread"]["a base-side thread opens from the base window and its reply goes to the base side"] = function()
  local base_thread = thread({ side = "base", line = 3, original_line = 3 })
  open_pr({
    {
      match = { "comment add" },
      body = { ok = true, data = { id = "c_1" }, session = pr_session({ threads = { base_thread }, rev = 2 }) },
    },
  }, pr_session({ threads = { base_thread } }))
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 3, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]"), "src/a.rs:3  [base]")
  child.type_keys("r")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'ok' })")
  child.cmd("write")
  eq(child.lua_get("vim.wait(20000, function() return require('reviewkit.core').session().rev == 2 end, 20)"), true)
  local input
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "comment" and call.args[2] == "add" then
      input = vim.json.decode(call.stdin)
    end
  end
  eq({ input.side, input.line, input.reply_to }, { "base", 3, "PRRT_1" })
end

T["thread"]["leaving the float closes it"] = function()
  open_pr()
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  wait("not require('reviewkit.view.thread').is_open()")
  eq(child.lua_get("require('reviewkit.view.thread').is_open()"), false)
end

T["thread"][":Review threads lists threads in the quickfix, refresh fetches first"] = function()
  local outdated = thread({ id = "PRRT_2", line = vim.NIL, original_line = 2, outdated = true })
  local refreshed = pr_session({ rev = 2, threads = { thread(), outdated }, etags = { threads = "th1" } })
  open_pr({ { match = { "thread list", "--refresh" }, body = { ok = true, data = vim.NIL, session = refreshed } } })
  child.cmd("Review threads")
  eq(child.lua_get("vim.fn.getqflist({ title = 1 }).title"), "reviewkit threads")
  local items = child.lua_get("vim.fn.getqflist()")
  eq(#items, 1)
  eq(items[1].lnum, 4)
  eq(items[1].text, "◆ alice (1) why this?")
  child.cmd("Review threads refresh")
  wait("#vim.fn.getqflist() == 2")
  items = child.lua_get("vim.fn.getqflist()")
  eq(items[2].lnum, 2)
  eq(items[2].text, "◆ alice (1) [outdated] why this?")
end

T["thread"]["K in the thread list opens an outdated thread that has no line"] = function()
  local outdated = thread({ id = "PRRT_2", line = vim.NIL, original_line = 2, outdated = true })
  open_pr(nil, pr_session({ threads = { outdated } }))
  child.cmd("Review threads")
  child.cmd("copen")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.type_keys("K")
  eq(child.lua_get("require('reviewkit.view.thread').is_open()"), true)
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]"), "src/a.rs:2  [outdated]")
end

T["thread"]["<CR> on a base-side entry lands in the diff view's base window"] = function()
  local base_thread = thread({ side = "base", line = 3, original_line = 3 })
  open_pr(nil, pr_session({ threads = { base_thread } }))
  child.cmd("Review threads")
  child.cmd("copen")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.type_keys("<CR>")
  wait("vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().left")
  eq(child.lua_get("vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().left"), true)
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 3)
  eq(child.lua_get("vim.b.reviewkit_side"), "base")
end

T["thread"]["<CR> on a head-side entry keeps the normal quickfix jump"] = function()
  open_pr()
  child.cmd("Review threads")
  child.cmd("copen")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.type_keys("<CR>")
  eq(child.lua_get("vim.bo.buftype"), "")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 4)
end

T["thread"]["a list from another plugin keeps the default <CR>"] = function()
  open_pr()
  child.lua(
    "vim.fn.setqflist({}, ' ', { title = 'grep', items = { { filename = '"
      .. worktree
      .. "/src/a.rs', lnum = 2, text = 'x' } } })"
  )
  child.cmd("copen")
  child.type_keys("<CR>")
  eq(child.lua_get("vim.bo.buftype"), "")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 2)
end

T["thread"]["the annotation shows a pending resolve"] = function()
  local queued = pr_session({ thread_actions = { { thread_id = "PRRT_1", action = "resolve" } } })
  open_pr(nil, queued)
  wait("#vim.api.nvim_buf_get_extmarks(0, vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, {}) > 0")
  local marks = child.lua_get(
    "vim.api.nvim_buf_get_extmarks(0, vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, { details = true })"
  )
  eq(marks[1][4].virt_text[1][1], " ◆ alice (1) resolve on submit")
end

T["thread"]["local sessions have no thread list"] = function()
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    {
      match = { "file base" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = {} } },
    },
  })
  child.cmd("Review diff HEAD")
  wait("require('reviewkit.core').session() ~= nil")
  child.cmd("Review threads")
  local notices = child.lua_get("_G.notices")
  eq(notices[#notices].msg:find("only in pull request sessions", 1, true) ~= nil, true)
end

T["thread"]["a range thread that starts on the other side is found on its own line"] = function()
  open_pr(nil, pr_session({ threads = { thread({ line = 2, start_line = 5, start_side = "base" }) } }))
  child.lua("vim.api.nvim_win_set_cursor(0, { 2, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  eq(child.lua_get("require('reviewkit.view.thread').is_open()"), true)
end

T["thread"]["saving an empty reply says it was discarded and queues nothing"] = function()
  open_pr()
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').thread()")
  child.type_keys("r")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { ' ' })")
  child.cmd("write")
  wait(
    "vim.tbl_contains(vim.tbl_map(function(n) return n.msg:find('discarded', 1, true) ~= nil end, _G.notices), true)"
  )
  eq(
    child.lua_get(
      "vim.tbl_contains(vim.tbl_map(function(n) return n.msg:find('discarded', 1, true) ~= nil end, _G.notices), true)"
    ),
    true
  )
  eq(call_with({ "comment add" }), nil)
end

return T
