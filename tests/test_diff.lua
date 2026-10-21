local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()
local child, dir, worktree

local function with_targets(overrides)
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  table.insert(session.targets, {
    id = "t_2",
    path = "src/b.rs",
    old_path = vim.NIL,
    status = "added",
    reason = "changed",
    base_blob = vim.NIL,
    head_blob = string.rep("d", 40),
    binary = false,
    too_large = false,
    viewed = false,
    stats = { added = 2, removed = 0 },
    hunks = { { base_start = 0, base_count = 0, head_start = 1, head_count = 2 } },
    commentable = { base = {}, head = { { 1, 2 } } },
    range = vim.NIL,
  })
  return vim.tbl_deep_extend("force", session, overrides or {})
end

local function rules(session, extra)
  local list = vim.list_extend(extra or {}, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = session } },
    {
      match = { "file base", "--path src/a.rs" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "l1", "l2", "old3", "l4", "l5", "l6" } },
      },
    },
  })
  return list
end

local function wait(cond)
  assert(
    child.lua_get(("vim.wait(20000, function() return %s end, 20)"):format(cond)),
    "timed out waiting for " .. cond
  )
end

local function open_review(session, extra)
  helpers.respond(dir, rules(session or with_targets(), extra))
  child.cmd("Review diff HEAD~1")
  wait("require('reviewkit.view.diff').is_open()")
  wait("require('reviewkit.view.diff')._state().showing == false and require('reviewkit.view.diff').is_open()")
  wait("vim.b[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left)].reviewkit_side == 'base'")
end

local function left_buf_var(name)
  return child.lua_get(
    ("vim.b[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left)].%s"):format(name)
  )
end

local function left_lines()
  return child.lua_get(
    "vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left), 0, -1, false)"
  )
end

local function right_name()
  return child.lua_get(
    "vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right))"
  )
end

T["diff"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      dir = helpers.fake_dir()
      worktree = vim.fn.resolve(helpers.fake_dir())
      vim.fn.mkdir(worktree .. "/src", "p")
      vim.fn.writefile({ "l1", "l2", "new3", "l4", "l5", "l6" }, worktree .. "/src/a.rs")
      vim.fn.writefile({ "b1", "b2" }, worktree .. "/src/b.rs")
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

T["diff"]["opening a review shows base and head side by side in diff mode"] = function()
  open_review()
  eq(left_buf_var("reviewkit_path"), "src/a.rs")
  eq(left_lines(), { "l1", "l2", "old3", "l4", "l5", "l6" })
  eq(vim.fn.resolve(right_name()), worktree .. "/src/a.rs")
  eq(child.lua_get("vim.wo[require('reviewkit.view.diff')._state().left].diff"), true)
  eq(child.lua_get("vim.wo[require('reviewkit.view.diff')._state().right].diff"), true)
  eq(child.lua_get("vim.wo[require('reviewkit.view.diff')._state().left].winbar"):find("BASE", 1, true) ~= nil, true)
  eq(child.lua_get("vim.bo[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left)].modifiable"), false)
end

T["diff"]["a session with a head commit shows that commit, read-only, not the working tree"] = function()
  local session = with_targets({ head = { ref = "feature", sha = string.rep("e", 40), dirty = false } })
  open_review(session, {
    {
      match = { "file head", "--path src/a.rs" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "from", "the", "commit" } },
      },
    },
  })
  wait(
    "vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right), 0, 1, false)[1] == 'from'"
  )
  local right = "vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right)"
  eq(child.lua_get("vim.api.nvim_buf_get_lines(" .. right .. ", 0, -1, false)"), { "from", "the", "commit" })
  eq(child.lua_get("vim.bo[" .. right .. "].modifiable"), false)
  eq(child.lua_get("vim.b[" .. right .. "].reviewkit_side"), "head")
  eq(child.lua_get("vim.b[" .. right .. "].reviewkit_path"), "src/a.rs")
end

local function commit_session(head_blob)
  local session = with_targets({ head = { ref = "feature", sha = string.rep("e", 40), dirty = false } })
  session.targets[1].head_blob = head_blob
  return session
end

local function right_lines()
  return child.lua_get(
    "vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right), 0, -1, false)"
  )
end

T["diff"]["a head commit that moves on refresh shows its new content"] = function()
  local first, second = commit_session(string.rep("1", 40)), commit_session(string.rep("2", 40))
  second.rev = 2
  second.etags = vim.tbl_extend("force", second.etags or {}, { targets = "head-moved" })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = first } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = second } },
    {
      match = { "file base", "--path src/a.rs" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "base" } } },
    },
    {
      match = { "file head", "--path src/a.rs" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "old", "head" } } },
    },
  })
  child.cmd("Review diff HEAD~1")
  wait("require('reviewkit.view.diff').is_open() and not require('reviewkit.view.diff')._state().showing")
  wait("vim.b[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right)].reviewkit_side == 'head'")
  eq(right_lines(), { "old", "head" })
  helpers.respond(dir, {
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = second } },
    {
      match = { "file base", "--path src/a.rs" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "base" } } },
    },
    {
      match = { "file head", "--path src/a.rs" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "new", "head", "line" } },
      },
    },
  })
  child.cmd("Review refresh")
  wait(
    "#vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right), 0, -1, false) == 3"
  )
  eq(right_lines(), { "new", "head", "line" })
end

T["diff"]["reveal lands on a head line of a commit session"] = function()
  open_review(commit_session(string.rep("1", 40)), {
    {
      match = { "file head", "--path src/a.rs" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "a", "b", "c", "d" } } },
    },
  })
  child.lua("require('reviewkit.view.diff').reveal('src/a.rs', 'head', 3)")
  wait("vim.api.nvim_win_get_cursor(0)[1] == 3")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 3)
end

T["diff"]["the working tree's file of a commit session takes no comments"] = function()
  open_review(commit_session(string.rep("1", 40)), {
    {
      match = { "file head", "--path src/a.rs" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "a", "b", "c", "d" } },
      },
    },
  })
  child.cmd("tabnew " .. worktree .. "/src/a.rs")
  eq(child.lua_get("require('reviewkit.view.diff').location(0) == nil"), true)
end

T["diff"]["<CR> on a target of a commit session opens the commit's copy"] = function()
  open_review(commit_session(string.rep("1", 40)), {
    {
      match = { "file head", "--path src/a.rs" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "a", "b", "c", "d" } },
      },
    },
  })
  child.cmd("copen")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.type_keys("<CR>")
  wait("vim.api.nvim_buf_get_name(0):find('reviewkit://head/', 1, true) ~= nil")
end

T["diff"]["the working tree's file opened in a commit session's head window is swapped for the commit's copy"] = function()
  open_review(commit_session(string.rep("1", 40)), {
    {
      match = { "file head", "--path src/a.rs" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "a", "b", "c", "d" } } },
    },
  })
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  wait(
    "vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right)):find('reviewkit://head/', 1, true) ~= nil"
  )
end

T["diff"]["a head commit in another encoding is decoded with fileencodings"] = function()
  child.lua("vim.o.fileencodings = 'ucs-bom,utf-8,latin1'")
  open_review(commit_session(string.rep("1", 40)), {
    {
      match = { "file head", "--path src/a.rs" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "caf\239\191\189" }, raw = "Y2Fm6Qo=" },
      },
    },
  })
  wait("vim.b[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right)].reviewkit_side == 'head'")
  eq(right_lines(), { "café" })
end

T["diff"]["step moves between targets, wraps around and shows added files against a placeholder"] = function()
  open_review()
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(left_lines(), { "(new file)" })
  eq(vim.fn.resolve(right_name()), worktree .. "/src/b.rs")
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/a.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(left_lines(), { "l1", "l2", "old3", "l4", "l5", "l6" })
end

T["diff"]["editing another target in the head window makes the base side follow"] = function()
  open_review()
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.cmd("edit " .. worktree .. "/src/b.rs")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(left_lines(), { "(new file)" })
end

T["diff"]["jump_hunk moves to the hunk start of the cursor's side"] = function()
  open_review()
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.lua("require('reviewkit.view.diff').jump_hunk(1)")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 3)
  child.lua("vim.api.nvim_win_set_cursor(0, { 6, 0 })")
  child.lua("require('reviewkit.view.diff').jump_hunk(-1)")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 3)
end

T["diff"]["]t and [t move between threads across files, skipping outdated ones"] = function()
  local session = with_targets({
    threads = {
      {
        id = "PRRT_b",
        path = "src/b.rs",
        side = "head",
        line = 2,
        start_line = vim.NIL,
        start_side = vim.NIL,
        original_line = 2,
        resolved = false,
        outdated = false,
        subject_type = "line",
        comments = { { id = "1", author = "alice", body = "hm", created_at = "t", url = "u" } },
      },
      {
        id = "PRRT_a",
        path = "src/a.rs",
        side = "head",
        line = 4,
        start_line = vim.NIL,
        start_side = vim.NIL,
        original_line = 2,
        resolved = false,
        outdated = false,
        subject_type = "line",
        comments = { { id = "1", author = "alice", body = "hm", created_at = "t", url = "u" } },
      },
      {
        id = "PRRT_old",
        path = "src/a.rs",
        side = "head",
        line = vim.NIL,
        start_line = vim.NIL,
        start_side = vim.NIL,
        original_line = 2,
        resolved = false,
        outdated = true,
        subject_type = "line",
        comments = { { id = "1", author = "alice", body = "hm", created_at = "t", url = "u" } },
      },
    },
  })
  open_review(session)
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait("vim.api.nvim_win_get_cursor(0)[1] == 4")
  eq(child.lua_get("require('reviewkit.view.diff')._state().path"), "src/a.rs")
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  wait("vim.api.nvim_win_get_cursor(0)[1] == 2")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 2)
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/a.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  wait("vim.api.nvim_win_get_cursor(0)[1] == 4")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 4)
  child.lua("require('reviewkit.view.diff').jump_thread(-1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(child.lua_get("require('reviewkit.view.diff')._state().path"), "src/b.rs")
end

T["diff"]["]t reaches both a base and a head thread on the same line number"] = function()
  local session = with_targets({
    threads = {
      {
        id = "PRRT_head",
        path = "src/a.rs",
        side = "head",
        line = 3,
        start_line = vim.NIL,
        start_side = vim.NIL,
        original_line = 3,
        resolved = false,
        outdated = false,
        subject_type = "line",
        comments = { { id = "1", author = "alice", body = "hm", created_at = "t", url = "u" } },
      },
      {
        id = "PRRT_base",
        path = "src/a.rs",
        side = "base",
        line = 3,
        start_line = vim.NIL,
        start_side = vim.NIL,
        original_line = 3,
        resolved = false,
        outdated = false,
        subject_type = "line",
        comments = { { id = "2", author = "bob", body = "old", created_at = "t", url = "u" } },
      },
    },
  })
  open_review(session)
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  local function where()
    return child.lua_get(
      "{ vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().left and 'base' or 'head', vim.api.nvim_win_get_cursor(0)[1] }"
    )
  end
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait("vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().left")
  eq(where(), { "base", 3 })
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait("vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().right")
  eq(where(), { "head", 3 })
  child.lua("require('reviewkit.view.diff').jump_thread(-1)")
  wait("vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().left")
  eq(where(), { "base", 3 })
end

T["diff"]["]t lands a range thread on its first line only when the range stays on one side"] = function()
  local function range(id, start_side, start_line, line)
    return {
      id = id,
      path = "src/a.rs",
      side = "head",
      line = line,
      start_line = start_line,
      start_side = start_side,
      original_line = line,
      resolved = false,
      outdated = false,
      subject_type = "line",
      comments = { { id = id, author = "alice", body = "hm", created_at = "t", url = "u" } },
    }
  end
  open_review(with_targets({ threads = { range("PRRT_cross", "base", 5, 2) } }))
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait("vim.api.nvim_win_get_cursor(0)[1] == 2")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 2)
end

T["diff"]["]t lands a same-side range thread on its first line"] = function()
  local t = {
    id = "PRRT_r",
    path = "src/a.rs",
    side = "head",
    line = 5,
    start_line = 3,
    start_side = "head",
    original_line = 5,
    resolved = false,
    outdated = false,
    subject_type = "line",
    comments = { { id = "r", author = "alice", body = "hm", created_at = "t", url = "u" } },
  }
  open_review(with_targets({ threads = { t } }))
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.lua("require('reviewkit.view.diff').jump_thread(1)")
  wait("vim.api.nvim_win_get_cursor(0)[1] == 3")
  eq(child.lua_get("vim.api.nvim_win_get_cursor(0)[1]"), 3)
end

T["diff"]["]u starts after the current file and says so when only it is left"] = function()
  local session = with_targets()
  session.targets[2].viewed = true
  open_review(session)
  child.lua("require('reviewkit.view.diff').step_unviewed(1)")
  eq(child.lua_get("_G.notices[#_G.notices].msg"), "[reviewkit] every other file is viewed")
end

T["diff"]["]u moves to the next file not viewed yet, and says when all are"] = function()
  local session = with_targets()
  session.targets[1].viewed = true
  open_review(session)
  child.lua("require('reviewkit.view.diff').step_unviewed(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(child.lua_get("require('reviewkit.view.diff')._state().path"), "src/b.rs")
  child.lua("for _, t in ipairs(require('reviewkit.core').targets()) do t.viewed = true end")
  child.lua("require('reviewkit.view.diff').step_unviewed(1)")
  eq(child.lua_get("_G.notices[#_G.notices].msg"), "[reviewkit] every file is viewed")
end

T["diff"]["a comment from the base window is queued on the base side"] = function()
  local after = with_targets({
    rev = 2,
    comments = {
      {
        id = "c_1",
        path = "src/a.rs",
        side = "base",
        line = 3,
        kind = "issue",
        body = "why remove",
        stale = false,
        start_line = vim.NIL,
      },
    },
    etags = { comments = "c1" },
  })
  open_review(nil, { { match = { "comment add" }, body = { ok = true, data = { id = "c_1" }, session = after } } })
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  child.lua("vim.api.nvim_win_set_cursor(0, { 3, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'why remove' })")
  child.cmd("write")
  wait("#require('reviewkit.core').comments() == 1")
  local added
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "comment" and call.args[2] == "add" then
      added = call
    end
  end
  local input = vim.json.decode(added.stdin)
  eq({ input.path, input.side, input.line }, { "src/a.rs", "base", 3 })
  local ns = "vim.api.nvim_create_namespace('reviewkit_annotate')"
  eq(
    child.lua_get(
      ("#vim.api.nvim_buf_get_extmarks(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left), %s, 0, -1, {})"):format(
        ns
      )
    ),
    1
  )
  eq(
    child.lua_get(
      ("#vim.api.nvim_buf_get_extmarks(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right), %s, 0, -1, {})"):format(
        ns
      )
    ),
    0
  )
end

T["diff"]["toggling viewed from the base window marks the target and the winbar shows it"] = function()
  local viewed = with_targets({ rev = 2, etags = { viewed = "v1", targets = "t2" } })
  viewed.targets[1].viewed = true
  open_review(
    nil,
    { { match = { "target viewed", "--path src/a.rs" }, body = { ok = true, data = vim.NIL, session = viewed } } }
  )
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  child.lua("require('reviewkit.orchestrator').toggle_viewed()")
  wait("require('reviewkit.core').target('src/a.rs').viewed == true")
  wait("vim.wo[require('reviewkit.view.diff')._state().right].winbar:find('viewed', 1, true) ~= nil")
  eq(
    child.lua_get("vim.wo[require('reviewkit.view.diff')._state().right].winbar"):find("✓ viewed", 1, true) ~= nil,
    true
  )
end

T["diff"]["GitHub threads are annotated on their side's line"] = function()
  local session = with_targets({
    threads = {
      {
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
        comments = { { id = "1", author = "alice", body = "hm", created_at = "t", url = "u" } },
      },
      {
        id = "PRRT_2",
        path = "src/a.rs",
        side = "head",
        line = vim.NIL,
        start_line = vim.NIL,
        start_side = vim.NIL,
        original_line = 2,
        resolved = false,
        outdated = true,
        subject_type = "line",
        comments = { { id = "2", author = "bob", body = "old", created_at = "t", url = "u" } },
      },
    },
  })
  open_review(session)
  wait(
    "#vim.api.nvim_buf_get_extmarks(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right), vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, {}) > 0"
  )
  local marks = child.lua_get(
    "vim.api.nvim_buf_get_extmarks(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right), vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, { details = true })"
  )
  eq(#marks, 1)
  eq(marks[1][2], 3)
  eq(marks[1][4].virt_text[1][1], " ◆ alice (1)")
  local left_marks = child.lua_get(
    "#vim.api.nvim_buf_get_extmarks(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left), vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, {})"
  )
  eq(left_marks, 0)
end

local function extra_target(path, status)
  return {
    id = "t_" .. path,
    path = path,
    old_path = vim.NIL,
    status = status or "modified",
    reason = "changed",
    base_blob = string.rep("e", 39) .. #path,
    head_blob = string.rep("f", 40),
    binary = false,
    too_large = false,
    viewed = false,
    stats = { added = 1, removed = 1 },
    hunks = { { base_start = 1, base_count = 1, head_start = 1, head_count = 1 } },
    commentable = { base = { { 1, 1 } }, head = { { 1, 1 } } },
    range = vim.NIL,
  }
end

T["diff"]["targets whose names prefix each other keep their own base buffers"] = function()
  vim.fn.writefile({ "x" }, worktree .. "/src/a.rs.orig")
  local session = with_targets()
  local orig = extra_target("src/a.rs.orig")
  orig.base_blob = session.targets[1].base_blob
  table.insert(session.targets, 1, orig)
  open_review(session, {
    {
      match = { "file base", "--path src/a.rs.orig" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "orig" } } },
    },
  })
  eq(left_lines(), { "orig" })
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/a.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(left_lines(), { "l1", "l2", "old3", "l4", "l5", "l6" })
  child.lua("require('reviewkit.view.diff').step(-1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/a.rs.orig' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(left_lines(), { "orig" })
  eq(left_buf_var("reviewkit_path"), "src/a.rs.orig")
end

T["diff"]["stepping onto the modified file already shown keeps the view working"] = function()
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  open_review(session)
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, 1, false, { 'edited' })")
  child.lua("require('reviewkit.view.diff').step(1)")
  wait("not require('reviewkit.view.diff')._state().showing")
  eq(child.lua_get("require('reviewkit.view.diff')._state().showing"), false)
  eq(child.lua_get("vim.bo.modified"), true)
  eq(child.lua_get("vim.wo[require('reviewkit.view.diff')._state().left].diff"), true)
end

T["diff"]["a failed base read is retried the next time the target is shown"] = function()
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = session } },
    {
      match = { "file base" },
      body = { ok = false, error = { code = "E_GIT", message = "boom", retryable = true, details = {} } },
      exit = 4,
    },
  })
  child.cmd("Review diff HEAD~1")
  wait("require('reviewkit.view.diff').is_open()")
  wait("vim.b[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left)].reviewkit_side == 'base'")
  eq(left_lines()[1]:find("boom", 1, true) ~= nil, true)
  helpers.respond(dir, rules(session))
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left), 0, 1, false)[1] == 'l1'"
  )
  eq(left_lines(), { "l1", "l2", "old3", "l4", "l5", "l6" })
end

T["diff"]["a refresh that changes the shown target redraws its base side"] = function()
  local session = with_targets()
  local refreshed = with_targets({ rev = 2, etags = { targets = "t2" } })
  refreshed.targets[2] = extra_target("src/b.rs")
  open_review(session, {
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = refreshed } },
    {
      match = { "file base", "--path src/b.rs" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "b0" } } },
    },
  })
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(left_lines(), { "(new file)" })
  child.cmd("Review refresh")
  wait("require('reviewkit.core').session().rev == 2")
  wait(
    "vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left), 0, 1, false)[1] == 'b0'"
  )
  eq(left_lines(), { "b0" })
end

T["diff"]["more than eight targets in a row stay within Vim's diff limit"] = function()
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  for i = 1, 10 do
    local path = ("src/f%02d.rs"):format(i)
    vim.fn.writefile({ "new" }, worktree .. "/" .. path)
    table.insert(session.targets, extra_target(path))
  end
  open_review(session, {
    {
      match = { "file base", "--path src/f" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = { "old" } } },
    },
  })
  for i = 1, 10 do
    child.lua("require('reviewkit.view.diff').step(1)")
    wait(
      ("require('reviewkit.view.diff')._state().path == 'src/f%02d.rs' and not require('reviewkit.view.diff')._state().showing"):format(
        i
      )
    )
  end
  eq(child.lua_get("require('reviewkit.view.diff')._state().path"), "src/f10.rs")
  eq(child.lua_get("vim.wo[require('reviewkit.view.diff')._state().left].diff"), true)
end

T["diff"]["navigation keys are mapped only in the diff view's buffers"] = function()
  open_review()
  eq(child.lua_get("vim.fn.maparg(']f', 'n') ~= ''"), true)
  child.cmd("tabnew")
  eq(child.lua_get("vim.fn.maparg(']f', 'n')"), "")
  eq(child.lua_get("vim.fn.maparg('[h', 'n')"), "")
end

T["diff"]["closing the diff tab by hand removes the navigation keys"] = function()
  open_review()
  child.cmd("tabclose")
  wait("not require('reviewkit.view.diff').is_open()")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  eq(child.lua_get("vim.fn.maparg(']f', 'n')"), "")
end

T["diff"]["closing the review removes the navigation keys from worktree buffers"] = function()
  open_review(nil, { { match = { "session close" }, body = { ok = true, data = { closed = true } } } })
  child.cmd("ReviewClose")
  wait("not require('reviewkit.view.diff').is_open()")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  eq(child.lua_get("vim.fn.maparg(']f', 'n')"), "")
end

T["diff"]["a foreign buffer-local map is kept whichever window has the cursor"] = function()
  open_review()
  local right = "require('reviewkit.view.diff')._state().right"
  child.lua(
    ("vim.keymap.set('n', ']h', function() _G.foreign = true end, { buffer = vim.api.nvim_win_get_buf(%s), desc = 'signs' })"):format(
      right
    )
  )
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/a.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  child.lua(("vim.api.nvim_set_current_win(%s)"):format(right))
  eq(child.lua_get("vim.fn.maparg(']h', 'n', false, true).desc"), "signs")
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  eq(child.lua_get("vim.fn.maparg(']h', 'n', false, true).desc"), "reviewkit: next hunk")
end

T["diff"]["a refresh that keeps the shown pair leaves the reviewer's folds alone"] = function()
  local long = {}
  for i = 1, 40 do
    long[i] = "line" .. i
  end
  vim.fn.writefile(long, worktree .. "/src/a.rs")
  local base = vim.deepcopy(long)
  base[20] = "old20"
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  session.targets[1].hunks = { { base_start = 20, base_count = 1, head_start = 20, head_count = 1 } }
  local refreshed = vim.deepcopy(session)
  refreshed.rev = 2
  refreshed.etags.targets = "t2"
  refreshed.targets[1].stats = { added = 2, removed = 1 }
  open_review(session, {
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = refreshed } },
    {
      match = { "file base" },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = base } },
    },
  })
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().right)")
  eq(child.lua_get("vim.fn.foldclosed(1)"), 1)
  child.cmd("normal! zR")
  eq(child.lua_get("vim.fn.foldclosed(1)"), -1)
  child.cmd("Review refresh")
  wait("require('reviewkit.core').session().rev == 2 and not require('reviewkit.view.diff')._state().showing")
  eq(child.lua_get("vim.fn.foldclosed(1)"), -1)
end

T["diff"]["a deleted file restored by a refresh replaces the placeholder with the real file"] = function()
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  session.targets[1].status = "deleted"
  local refreshed = helpers.session({ source = { worktree = worktree }, repo = { root = worktree }, rev = 2 })
  refreshed.etags.targets = "t2"
  open_review(session, {
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = refreshed } },
  })
  eq(right_name():find("reviewkit://deleted/", 1, true) ~= nil, true)
  child.cmd("Review refresh")
  wait("require('reviewkit.core').session().rev == 2")
  wait(
    "vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right)):find('reviewkit://', 1, true) == nil"
  )
  eq(vim.fn.resolve(right_name()), worktree .. "/src/a.rs")
end

T["diff"]["the placeholder of an added file's base side refuses comments"] = function()
  open_review()
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  child.lua("vim.api.nvim_set_current_win(require('reviewkit.view.diff')._state().left)")
  child.lua("require('reviewkit.orchestrator').comment()")
  eq(child.lua_get("vim.api.nvim_get_current_win() == require('reviewkit.view.diff')._state().left"), true)
  local notices = child.lua_get("_G.notices")
  eq(notices[#notices].msg:find("no file to comment on", 1, true) ~= nil, true)
end

T["diff"]["commentable lines carry a sign on each side, placeholders none"] = function()
  open_review()
  local ns = "vim.api.nvim_create_namespace('reviewkit_commentable')"
  local rows = ("vim.tbl_map(function(m) return m[2] end, vim.api.nvim_buf_get_extmarks(vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().%%s), %s, 0, -1, {}))"):format(
    ns
  )
  eq(child.lua_get(rows:format("right")), { 0, 1, 2, 3, 4, 5 })
  eq(child.lua_get(rows:format("left")), { 0, 1, 2, 3, 4, 5 })
  child.lua("require('reviewkit.view.diff').step(1)")
  wait(
    "require('reviewkit.view.diff')._state().path == 'src/b.rs' and not require('reviewkit.view.diff')._state().showing"
  )
  eq(child.lua_get(rows:format("left")), {})
  eq(child.lua_get(rows:format("right")), { 0, 1 })
end

T["diff"]["a base side in another encoding is decoded like the head file"] = function()
  local f = assert(io.open(worktree .. "/src/a.rs", "wb"))
  f:write("caf\233!\n")
  f:close()
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    {
      match = { "file base" },
      body = {
        ok = true,
        data = { exists = true, binary = false, too_large = false, lines = { "caf\239\191\189" }, raw = "Y2Fm6Qo=" },
      },
    },
  })
  child.cmd("Review diff HEAD~1")
  wait("require('reviewkit.view.diff').is_open() and not require('reviewkit.view.diff')._state().showing")
  wait("vim.b[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().left)].reviewkit_side == 'base'")
  eq(
    child.lua_get("vim.bo[vim.api.nvim_win_get_buf(require('reviewkit.view.diff')._state().right)].fileencoding"),
    "latin1"
  )
  eq(left_lines(), { "café" })
end

T["diff"]["closing the review closes the diff tab"] = function()
  open_review(nil, { { match = { "session close" }, body = { ok = true, data = { closed = true } } } })
  eq(child.lua_get("#vim.api.nvim_list_tabpages()"), 2)
  child.cmd("ReviewClose")
  wait("not require('reviewkit.view.diff').is_open()")
  eq(child.lua_get("#vim.api.nvim_list_tabpages()"), 1)
end

return T
