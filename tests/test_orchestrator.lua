local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()
local child, dir, worktree

local function rules(extra)
  local session = helpers.session({ source = { worktree = worktree }, repo = { root = worktree } })
  local list = {
    { match = { "session open --local", "--base HEAD~1" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = session } },
  }
  vim.list_extend(list, extra or {})
  return list
end

T["orchestrator"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      dir = helpers.fake_dir()
      worktree = helpers.fake_dir()
      vim.fn.mkdir(worktree .. "/src", "p")
      vim.fn.writefile({ "l1", "l2", "l3", "l4", "l5", "l6" }, worktree .. "/src/a.rs")
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

local function wait_notice(pattern)
  child.lua(
    ("vim.wait(20000, function() for _, n in ipairs(_G.notices) do if n.msg:find(%q, 1, true) then return true end end return false end, 20)"):format(
      pattern
    )
  )
  return child.lua_get("_G.notices")
end

T["orchestrator"][":Review diff opens a session and lists targets in the quickfix"] = function()
  helpers.respond(dir, rules())
  child.cmd("Review diff HEAD~1")
  local notices = wait_notice("1 file to review")
  eq(#notices >= 1, true)
  eq(child.lua_get("vim.fn.getqflist({ title = 1 }).title"), "reviewkit targets")
  local items = child.lua_get("vim.fn.getqflist()")
  eq(#items, 1)
  eq(items[1].lnum, 3)
  eq(child.lua_get("vim.fn.maparg('<leader>rc', 'n') ~= ''"), true)
end

T["orchestrator"]["comment() reads the cursor line, parses the kind and queues through the CLI"] = function()
  local after = helpers.session({
    source = { worktree = worktree },
    rev = 2,
    comments = {
      {
        id = "c_1",
        path = "src/a.rs",
        side = "head",
        line = 4,
        kind = "nit",
        body = "tighten",
        stale = false,
        start_line = vim.NIL,
      },
    },
    etags = { comments = "c1" },
  })
  helpers.respond(
    dir,
    rules({ { match = { "comment add" }, body = { ok = true, data = { id = "c_1" }, session = after } } })
  )
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'nit: tighten' })")
  child.cmd("write")
  wait_notice("queued src/a.rs:4")
  -- The worktree watcher may interleave a refresh, so the add is searched for.
  local added
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "comment" and call.args[2] == "add" then
      added = call
    end
  end
  if not added then
    error("no comment add call; notices: " .. vim.inspect(child.lua_get("_G.notices")))
  end
  local input = vim.json.decode(added.stdin)
  eq(input.kind, "nit")
  eq(input.body, "tighten")
  eq(input.line, 4)
  eq(input.start_line, nil)
  -- The annotation shows up on the file buffer once the queue changed.
  local annotated = child.lua_get(
    "vim.wait(20000, function() return #vim.api.nvim_buf_get_extmarks(0, vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, {}) > 0 end, 20)"
  )
  eq(annotated, true)
  local marks =
    child.lua_get("#vim.api.nvim_buf_get_extmarks(0, vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, {})")
  eq(marks, 1)
  local text = child.lua_get(
    "vim.api.nvim_buf_get_extmarks(0, vim.api.nvim_create_namespace('reviewkit_annotate'), 0, -1, { details = true })[1][4].virt_text[1][1]"
  )
  eq(text, " ● nit tighten")
end

T["orchestrator"]["ReviewSubmit resolves E_AMBIGUOUS through vim.ui.select and retries with the target"] = function()
  local result = {
    sink = "herdr",
    at = "t",
    url = vim.NIL,
    sent = {},
    failed = {},
    skipped = {},
    thread_actions = {},
    decision = "not_applicable",
    summary = "0 sent, 0 failed",
  }
  helpers.respond(
    dir,
    rules({
      {
        match = { "submit --session", "w9:p3" },
        body = { ok = true, data = result, session = helpers.session({ source = { worktree = worktree }, rev = 3 }) },
      },
      {
        match = { "submit --session" },
        body = {
          ok = false,
          error = {
            code = "E_AMBIGUOUS",
            message = "pick one",
            retryable = false,
            details = { candidates = { { id = "w9:p3", name = "claude", status = "idle" } } },
          },
        },
        exit = 4,
      },
    })
  )
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("vim.ui.select = function(items, _, on_choice) _G.offered = items; on_choice(items[1]) end")
  child.cmd("ReviewSubmit herdr")
  wait_notice("0 sent, 0 failed")
  eq(child.lua_get("_G.offered[1].id"), "w9:p3")
  local calls = helpers.calls(dir)
  local last = calls[#calls]
  eq(vim.json.decode(last.stdin).target, "w9:p3")
end

T["orchestrator"]["ReviewClose! purges and reports"] = function()
  helpers.respond(
    dir,
    rules({ { match = { "session close", "--purge" }, body = { ok = true, data = { closed = true } } } })
  )
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("ReviewClose!")
  wait_notice("review closed")
  eq(child.lua_get("require('reviewkit.core').session()"), vim.NIL)
end

T["orchestrator"]["closing with unsent comments asks first, and keeps the session on no"] = function()
  local session = helpers.session({
    source = { worktree = worktree },
    repo = { root = worktree },
    comments = { { id = "c_1", path = "src/a.rs", side = "head", line = 3, body = "hm" } },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session close" }, body = { ok = true, data = { closed = true } } },
  })
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("_G.asked = nil; vim.fn.confirm = function(msg) _G.asked = msg return 2 end")
  child.cmd("ReviewClose")
  eq(child.lua_get("_G.asked"):find("1 comment still in the queue", 1, true) ~= nil, true)
  local closes = 0
  for _, call in ipairs(helpers.calls(dir)) do
    if table.concat(call.args, " "):find("session close", 1, true) then
      closes = closes + 1
    end
  end
  eq(closes, 0)
  eq(child.lua_get("require('reviewkit.core').session() ~= nil"), true)
  child.lua("vim.fn.confirm = function() return 1 end")
  child.cmd("ReviewClose")
  wait_notice("review closed")
end

T["orchestrator"][":Review without arguments shows the usage"] = function()
  child.cmd("Review")
  eq(child.lua_get("_G.notices[#_G.notices].msg"):find("usage: :Review", 1, true) ~= nil, true)
end

T["orchestrator"][":Review resume completes pull request ids, which are readable"] = function()
  helpers.respond(dir, {
    {
      match = { "session list" },
      body = {
        ok = true,
        data = {
          { id = "local/abc/0123", kind = "local", title = "main..worktree", comments = 0, updated_at = "t" },
          { id = "pr/github.com/o/r/7", kind = "pr", title = "Add k", comments = 1, updated_at = "t" },
          { kind = "pr" },
        },
      },
    },
  })
  eq(child.lua_get("vim.fn.getcompletion('Review resume ', 'cmdline')"), { "pr/github.com/o/r/7" })
end

T["orchestrator"][":Review resume without an id lists sessions by what they compare"] = function()
  helpers.respond(dir, {
    {
      match = { "session list" },
      body = {
        ok = true,
        data = {
          {
            id = "local/abc/0123",
            kind = "local",
            repo_root = "/work/proj",
            title = "main..worktree",
            comments = 2,
            updated_at = "2026-10-20T01:02:03Z",
          },
        },
      },
    },
  })
  child.lua([[vim.ui.select = function(items, opts) _G.shown = opts.format_item(items[1]) end]])
  child.cmd("Review resume")
  child.lua("vim.wait(20000, function() return _G.shown ~= nil end, 20)")
  eq(child.lua_get("_G.shown"):find("proj  main..worktree", 1, true) ~= nil, true)
end

T["orchestrator"]["<Plug> maps exist at plugin load without loading the orchestrator"] = function()
  eq(child.lua_get("vim.fn.maparg('<Plug>(reviewkit-comment)', 'n') ~= ''"), true)
  eq(child.lua_get("package.loaded['reviewkit.orchestrator'] == nil"), true)
end

T["orchestrator"]["a PR refresh reloads worktree buffers whose files changed"] = function()
  local function pr(etag)
    return helpers.session({
      id = "pr/github.com/o/r/7",
      source = {
        kind = "pr",
        number = 7,
        url = "u",
        title = "Add k",
        author = "alice",
        state = "OPEN",
        head_ref = "f",
        base_ref = "main",
        base_oid = string.rep("b", 40),
        node_id = "PR_1",
        worktree = worktree,
      },
      repo = { root = worktree },
      head = { ref = "f", sha = string.rep("c", 40), dirty = false },
      etags = { targets = etag },
    })
  end
  helpers.respond(dir, {
    { match = { "session open --pr 7" }, body = { ok = true, data = vim.NIL, session = pr("t1") } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = pr("t2") } },
  })
  child.cmd("Review pr 7")
  wait_notice("#7 Add k")
  child.cmd("tabnew " .. worktree .. "/src/a.rs")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]"), "l1")
  vim.fn.writefile({ "rewritten" }, worktree .. "/src/a.rs")
  child.lua("vim.uv.fs_utime(vim.fn.expand('%:p'), os.time() + 5, os.time() + 5)")
  child.cmd("Review refresh")
  child.lua("vim.wait(20000, function() return vim.api.nvim_buf_get_lines(0, 0, 1, false)[1] == 'rewritten' end, 20)")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]"), "rewritten")
end

local function history_rules()
  local function comment(id, body)
    return {
      id = id,
      path = "src/a.rs",
      side = "head",
      line = 2,
      kind = "issue",
      body = body,
      deliveries = { { sink = "herdr", at = "t", mode = "sent" } },
    }
  end
  local queued = helpers.session({
    source = { worktree = worktree },
    repo = { root = worktree },
    comments = { comment("c_1", "still queued") },
  })
  return {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = queued } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = queued } },
    {
      match = { "history list" },
      body = { ok = true, data = { comment("c_9", "newest"), comment("c_8", "older") } },
    },
    { match = { "history restore" }, body = { ok = true, data = vim.NIL, session = queued } },
    { match = { "comment clear" }, body = { ok = true, data = { moved = { "c_1" } }, session = queued } },
  }
end

local function calls_with(sub)
  local found = {}
  for _, call in ipairs(helpers.calls(dir)) do
    if table.concat(call.args, " "):find(sub, 1, true) then
      table.insert(found, call.args)
    end
  end
  return found
end

local function wait_for_call(sub)
  vim.wait(20000, function()
    return #calls_with(sub) > 0
  end, 50)
end

T["orchestrator"][":ReviewHistory lists what left the queue and r brings one back"] = function()
  helpers.respond(dir, history_rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("ReviewHistory")
  child.lua("vim.wait(20000, function() return vim.fn.getqflist({ title = 1 }).title == 'reviewkit history' end, 20)")
  local items = child.lua_get("vim.tbl_map(function(i) return i.text end, vim.fn.getqflist())")
  eq(#items, 2)
  eq(items[1]:find("newest", 1, true) ~= nil, true)
  child.cmd("copen")
  child.lua("vim.api.nvim_win_set_cursor(0, { 1, 0 })")
  child.type_keys("r")
  wait_for_call("history restore")
  eq(#calls_with("history restore"), 1)
  local args = calls_with("history restore")[1]
  eq(args[#args], "c_9")
end

T["orchestrator"]["D in the queue list clears delivered comments after asking"] = function()
  helpers.respond(dir, history_rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("require('reviewkit.view.queue').show_comments()")
  child.cmd("copen")
  child.lua("vim.fn.confirm = function() return 2 end")
  child.type_keys("D")
  eq(#calls_with("comment clear"), 0)
  child.lua("vim.fn.confirm = function() return 1 end")
  child.type_keys("D")
  wait_for_call("comment clear")
  eq(calls_with("comment clear")[1], { "comment", "clear", "--session", "local/abc/HEAD..worktree", "--delivered" })
end

T["orchestrator"]["D in a location list leaves the review queue alone"] = function()
  helpers.respond(dir, history_rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("require('reviewkit.view.queue').show_comments()")
  child.lua("vim.fn.setloclist(0, { { filename = 'x', lnum = 1, text = 'diagnostic' } })")
  child.cmd("lopen")
  child.lua("vim.fn.confirm = function() _G.asked = true return 1 end")
  -- Unmapped there, D is Vim's own and fails on the unmodifiable list.
  pcall(child.type_keys, "D")
  eq(child.lua_get("_G.asked == nil"), true)
  eq(#calls_with("comment clear"), 0)
end

local function two_comment_rules()
  local function comment(id, body)
    return { id = id, path = "src/a.rs", side = "head", line = 2, kind = "issue", body = body, deliveries = {} }
  end
  local session = helpers.session({
    source = { worktree = worktree },
    repo = { root = worktree },
    comments = { comment("c_1", "first"), comment("c_2", "second") },
  })
  return {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = session } },
    {
      match = { "submit", "--sink herdr" },
      body = {
        ok = true,
        data = { sink = "herdr", sent = {}, failed = {}, skipped = {}, summary = "1 sent" },
        session = session,
      },
    },
  }
end

T["orchestrator"]["a range of the queue list submits only those comments"] = function()
  helpers.respond(dir, two_comment_rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("require('reviewkit.view.queue').show_comments()")
  child.cmd("copen")
  child.cmd("2ReviewSubmit herdr")
  wait_for_call("--sink herdr")
  local calls = helpers.calls(dir)
  local sent
  for _, call in ipairs(calls) do
    if table.concat(call.args, " "):find("--sink herdr", 1, true) then
      sent = vim.json.decode(call.stdin)
    end
  end
  eq(sent.comment_ids, { "c_2" })
end

T["orchestrator"]["visual <leader>rs in the queue list submits the selected lines"] = function()
  helpers.respond(dir, two_comment_rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("require('reviewkit.view.queue').show_comments()")
  child.cmd("copen")
  child.lua("vim.ui.select = function(items, _, on_choice) on_choice('herdr') end")
  child.lua("vim.api.nvim_win_set_cursor(0, { 2, 0 })")
  child.type_keys("V", "\\rs")
  wait_for_call("--sink herdr")
  local sent
  for _, call in ipairs(helpers.calls(dir)) do
    if table.concat(call.args, " "):find("--sink herdr", 1, true) then
      sent = vim.json.decode(call.stdin)
    end
  end
  eq(sent.comment_ids, { "c_2" })
end

T["orchestrator"]["a range outside the queue list is refused"] = function()
  helpers.respond(dir, two_comment_rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  -- The queue list is open, but the range is taken in a code window.
  child.lua("require('reviewkit.view.queue').show_comments()")
  child.cmd("copen")
  child.cmd("wincmd p")
  child.cmd("1ReviewSubmit herdr")
  child.lua("vim.fn.confirm = function() _G.asked = true return 1 end")
  child.lua("vim.api.nvim_feedkeys = function() end")
  child.lua("require('reviewkit.view.queue').clear_delivered()")
  eq(child.lua_get("_G.asked == nil"), true)
  eq(#calls_with("--sink herdr"), 0)
  eq(child.lua_get("_G.notices[#_G.notices].msg"):find("queue list", 1, true) ~= nil, true)
end

T["orchestrator"]["commands without a session only notify"] = function()
  child.cmd("ReviewSubmit")
  child.cmd("ReviewClose")
  child.cmd("Review refresh")
  local notices = child.lua_get("_G.notices")
  eq(#notices, 3)
  eq(#helpers.calls(dir), 0)
end

T["orchestrator"]["set_keymaps = false leaves the keys alone but keeps <Plug> maps"] = function()
  helpers.respond(dir, rules())
  child.lua("require('reviewkit').setup({ set_keymaps = false })")
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  eq(child.lua_get("vim.fn.maparg('<leader>rc', 'n')"), "")
  eq(child.lua_get("vim.fn.maparg('<Plug>(reviewkit-comment)', 'n') ~= ''"), true)
end

T["orchestrator"]["ReviewSubmit clipboard goes through submit apply and fills the register"] = function()
  local with_comment = helpers.session({
    source = { worktree = worktree },
    rev = 2,
    comments = {
      {
        id = "c_1",
        path = "src/a.rs",
        side = "head",
        line = 4,
        kind = "issue",
        body = "copy me",
        stale = false,
        start_line = vim.NIL,
      },
    },
    etags = { comments = "c1" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = with_comment } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = with_comment } },
    {
      match = { "submit apply" },
      body = {
        ok = true,
        data = { sink = "clipboard", summary = "1 copied, 0 failed" },
        session = helpers.session({ source = { worktree = worktree }, rev = 3 }),
      },
    },
  })
  child.lua("require('reviewkit').setup({ sinks = { clipboard = { register = 'r' } } })")
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("ReviewSubmit clipboard")
  wait_notice("clipboard: 1 copied")
  eq(child.lua_get("vim.fn.getreg('r'):find('1. src/a.rs:4\\ncopy me', 1, true) ~= nil"), true)
  local applied
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "submit" and call.args[2] == "apply" then
      applied = call
    end
  end
  eq(vim.json.decode(applied.stdin).sent[1].comment_id, "c_1")
  eq(vim.json.decode(applied.stdin).mode, "copied")
end

T["orchestrator"]["a clipboard that cannot be written reports a failure and copies nothing"] = function()
  local with_comment = helpers.session({
    source = { worktree = worktree },
    comments = {
      { id = "c_1", path = "src/a.rs", side = "head", line = 4, kind = "nit", body = "x", stale = false },
    },
    etags = { comments = "c1" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = with_comment } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = with_comment } },
    {
      match = { "submit apply" },
      body = { ok = true, data = { sink = "clipboard", summary = "0 copied, 1 failed" }, session = with_comment },
    },
  })
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("vim.fn.getreg = function() return '' end")
  child.cmd("ReviewSubmit clipboard")
  wait_notice("clipboard: 0 copied")
  local applied
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "submit" and call.args[2] == "apply" then
      applied = vim.json.decode(call.stdin)
    end
  end
  eq(#applied.sent, 0)
  eq(applied.failed[1].error.message:find("could not write register", 1, true) ~= nil, true)
end

T["orchestrator"]["decision picker then input window sets the decision"] = function()
  helpers.respond(
    dir,
    rules({
      {
        match = { "decision set" },
        body = {
          ok = true,
          data = vim.NIL,
          session = helpers.session({
            source = { worktree = worktree },
            rev = 2,
            decision = { event = "approve", body = "ship it", updated_at = "t" },
            etags = { decision = "d1" },
          }),
        },
      },
    })
  )
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("vim.ui.select = function(items, _, on_choice) on_choice(items[1]) end")
  child.lua("require('reviewkit.orchestrator').decision()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'ship it' })")
  child.cmd("write")
  child.lua("vim.wait(20000, function() return require('reviewkit.core').decision() ~= nil end, 20)")
  eq(child.lua_get("require('reviewkit.core').decision().event"), "approve")
  local set
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "decision" and call.args[2] == "set" then
      set = call
    end
  end
  eq(vim.json.decode(set.stdin), { event = "approve", body = "ship it" })
end

T["orchestrator"]["close clears annotations and empties our quickfix list"] = function()
  helpers.respond(dir, rules({ { match = { "session close" }, body = { ok = true, data = { closed = true } } } }))
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("ReviewClose")
  wait_notice("review closed")
  eq(child.lua_get("vim.fn.getqflist({ title = 1 }).title"), "reviewkit (closed)")
  eq(#child.lua_get("vim.fn.getqflist()"), 0)
end

T["orchestrator"][":Review pr opens a pull request session and pr_info reads pr.json"] = function()
  local pr_session = helpers.session({
    id = "pr/github.com/o/r/7",
    source = {
      kind = "pr",
      number = 7,
      url = "https://github.com/o/r/pull/7",
      title = "Add k",
      author = "alice",
      state = "OPEN",
      head_ref = "feature",
      base_ref = "main",
      base_oid = string.rep("b", 40),
      node_id = "PR_1",
      worktree = worktree,
    },
    head = { ref = "feature", sha = string.rep("c", 40), dirty = false },
  })
  helpers.respond(dir, {
    { match = { "session open --pr 7" }, body = { ok = true, data = vim.NIL, session = pr_session } },
    { match = { "pr show" }, body = { ok = true, data = { title = "Add k", timeline = {}, checks = {} } } },
  })
  child.cmd("Review pr 7")
  wait_notice("#7 Add k: 1 file to review")
  child.lua("require('reviewkit.core').pr_info(_G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  eq(d.results[1].title, "Add k")
end

T["orchestrator"]["only worktree files, HEAD and refs count as changes for the watcher"] = function()
  local relevant = function(name)
    return child.lua_get(("require('reviewkit.orchestrator')._relevant_change(%q)"):format(name))
  end
  eq(relevant("src/a.rs"), true)
  eq(relevant("src/index"), true)
  eq(relevant(".git/HEAD"), true)
  eq(relevant(".git/refs/heads/main"), true)
  eq(relevant(".git/packed-refs"), true)
  eq(relevant(".git/objects/72/65a9"), false)
  eq(relevant(".git/index"), false)
  eq(relevant(".git/refs/heads/main.lock"), false)
  eq(relevant(".git"), false)
  eq(relevant("vendor/lib/.git/index"), false)
  eq(relevant("vendor/lib/.git"), false)
  eq(relevant(".git/reftable/tables.list"), true)
  eq(relevant("Cargo.lock"), true)
end

local function last_call(sub)
  local found
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "comment" and call.args[2] == sub then
      found = vim.json.decode(call.stdin)
    end
  end
  return found
end

T["orchestrator"]["a comment that cannot be saved while a submit runs reopens with its text"] = function()
  helpers.respond(
    dir,
    rules({
      {
        match = { "comment add" },
        body = {
          ok = false,
          error = {
            code = "E_LOCKED",
            message = "session is locked by another process",
            hint = "a submit may still be running; try again when it finishes",
            retryable = true,
            details = {},
          },
        },
        exit = 5,
      },
    })
  )
  child.lua("require('reviewkit.cli').LOCK_RETRY_MS = {}")
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'nit: keep me', 'second line' })")
  child.cmd("write")
  wait_notice("a submit may still be running")
  child.lua(
    "vim.wait(5000, function() return vim.api.nvim_buf_get_name(0):find('reviewkit://input', 1, true) ~= nil end, 20)"
  )
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "nit: keep me", "second line" })
  eq(child.lua_get("vim.bo.modified"), true)
end

T["orchestrator"]["a save refused twice reopens with the same text each time"] = function()
  child.lua([[
    _G.saves = {}
    require("reviewkit.view.input").open_keeping({ title = "x" }, function(text, done)
      table.insert(_G.saves, text)
      done({ code = "E_NO_COMMENT", message = "gone", retryable = false, details = {} })
    end)
  ]])
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'kept', 'text' })")
  child.cmd("write")
  child.cmd("write")
  eq(child.lua_get("_G.saves"), { "kept\ntext", "kept\ntext" })
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "kept", "text" })
  eq(child.lua_get("vim.bo.modified"), true)
  eq(
    child.lua_get(
      "#vim.tbl_filter(function(w) return vim.api.nvim_win_get_config(w).relative ~= '' end, vim.api.nvim_list_wins())"
    ),
    1
  )
end

T["orchestrator"]["a suggestion starts from the selected lines and is sent apart from the body"] = function()
  helpers.respond(
    dir,
    rules({
      {
        match = { "comment add" },
        body = {
          ok = true,
          data = { id = "c_1" },
          session = helpers.session({ source = { worktree = worktree }, rev = 2 }),
        },
      },
    })
  )
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("require('reviewkit.orchestrator').comment({ range = true, line1 = 2, line2 = 3, suggest = true })")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "", "```suggestion", "l2", "l3", "```" })
  child.lua("vim.api.nvim_buf_set_lines(0, 0, 1, false, { 'nit: tighter' })")
  child.cmd("write")
  wait_notice("queued src/a.rs:2-3")
  local input = last_call("add")
  eq({ input.kind, input.body, input.suggestion, input.start_line, input.line }, { "nit", "tighter", "l2\nl3", 2, 3 })
end

T["orchestrator"]["a suggestion holding a code fence keeps all of it"] = function()
  vim.fn.writefile({ "intro", "```lua", "x()", "```", "outro" }, worktree .. "/src/a.rs")
  helpers.respond(
    dir,
    rules({
      {
        match = { "comment add" },
        body = {
          ok = true,
          data = { id = "c_1" },
          session = helpers.session({ source = { worktree = worktree }, rev = 2 }),
        },
      },
    })
  )
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("require('reviewkit.orchestrator').comment({ range = true, line1 = 2, line2 = 4, suggest = true })")
  eq(
    child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"),
    { "", "````suggestion", "```lua", "x()", "```", "````" }
  )
  child.lua("vim.api.nvim_buf_set_lines(0, 0, 1, false, { 'use a block' })")
  child.cmd("write")
  wait_notice("queued src/a.rs:2-4")
  local input = last_call("add")
  eq({ input.body, input.suggestion }, { "use a block", "```lua\nx()\n```" })
end

T["orchestrator"]["editing a comment shows and saves its kind and suggestion"] = function()
  local with = helpers.session({
    source = { worktree = worktree },
    rev = 2,
    comments = {
      {
        id = "c_1",
        path = "src/a.rs",
        side = "head",
        line = 4,
        kind = "nit",
        body = "tighter",
        suggestion = "x",
        stale = false,
        updated_at = "t",
      },
    },
    etags = { comments = "c1" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = with } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = with } },
    { match = { "comment update" }, body = { ok = true, data = vim.NIL, session = with } },
  })
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').edit_comment()")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "nit: tighter", "```suggestion", "x", "```" })
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'question: why?' })")
  child.cmd("write")
  vim.wait(20000, function()
    return last_call("update") ~= nil
  end, 50)
  local patch = last_call("update")
  eq({ patch.kind, patch.body }, { "question", "why?" })
  eq(patch.suggestion, vim.NIL)
end

T["orchestrator"]["a stale comment is moved to the cursor line"] = function()
  local with = helpers.session({
    source = { worktree = worktree },
    rev = 2,
    comments = {
      { id = "c_1", path = "src/a.rs", side = "head", line = 4, kind = "issue", body = "a", stale = false },
      { id = "c_2", path = "src/a.rs", side = "head", line = 5, kind = "issue", body = "b", stale = true },
    },
    etags = { comments = "c1" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = with } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = with } },
    { match = { "comment update" }, body = { ok = true, data = vim.NIL, session = with } },
  })
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_win_set_cursor(0, { 2, 0 })")
  child.lua("vim.ui.select = function(items, _, cb) _G.first_offered = items[1].id; cb(items[1]) end")
  child.lua("require('reviewkit.orchestrator').move_comment()")
  wait_notice("moved to src/a.rs:2")
  eq(child.lua_get("_G.first_offered"), "c_2")
  local updated
  for _, call in ipairs(helpers.calls(dir)) do
    if call.args[1] == "comment" and call.args[2] == "update" then
      updated = call
    end
  end
  eq(vim.tbl_contains(updated.args, "c_2"), true)
  eq(vim.json.decode(updated.stdin).line, 2)
end

T["orchestrator"]["q asks before throwing away typed text"] = function()
  child.lua("require('reviewkit.view.input').open({ title = 't', on_save = function() end })")
  child.lua("vim.cmd('stopinsert'); vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'draft' })")
  child.lua("vim.fn.confirm = function() return 2 end")
  child.type_keys("q")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)"), { "draft" })
  child.lua("vim.fn.confirm = function() return 1 end")
  child.type_keys("q")
  eq(child.lua_get("vim.bo.buftype"), "")
end

T["orchestrator"][":wq in the input window saves and closes only the input window"] = function()
  child.cmd("vsplit")
  local before = child.lua_get("#vim.api.nvim_tabpage_list_wins(0)")
  child.lua("require('reviewkit.view.input').open({ title = 't', on_save = function(body) _G.saved = body end })")
  child.lua("vim.cmd('stopinsert'); vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'note' })")
  child.cmd("wq")
  child.lua("vim.wait(1000, function() return _G.saved ~= nil end, 10)")
  eq(child.lua_get("_G.saved"), "note")
  eq(child.lua_get("#vim.api.nvim_tabpage_list_wins(0)"), before)
end

T["orchestrator"][":wq in the input window over a single window keeps Neovim running"] = function()
  child.lua("require('reviewkit.view.input').open({ title = 't', on_save = function(body) _G.saved = body end })")
  child.lua("vim.cmd('stopinsert'); vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'note' })")
  child.cmd("wq")
  child.lua("vim.wait(1000, function() return _G.saved ~= nil end, 10)")
  eq(child.lua_get("_G.saved"), "note")
  eq(child.lua_get("#vim.api.nvim_tabpage_list_wins(0)"), 1)
end

T["orchestrator"]["saving twice before the input window closes saves once"] = function()
  child.lua([[
    _G.saves = 0
    require('reviewkit.view.input').open({ title = 't', on_save = function() _G.saves = _G.saves + 1 end })
    vim.cmd('stopinsert')
    vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'note' })
    vim.api.nvim_input('<C-s><C-s>')
  ]])
  child.lua("vim.wait(200, function() return false end, 10)")
  eq(child.lua_get("_G.saves"), 1)
end

T["orchestrator"]["a comment on an unsaved buffer is refused"] = function()
  helpers.respond(dir, rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, 0, false, { 'new1', 'new2' })")
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  wait_notice("save the file first")
  eq(child.lua_get("vim.bo.buftype"), "")
end

T["orchestrator"]["a comment on a file changed on disk since it was read is refused"] = function()
  helpers.respond(dir, rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  vim.fn.writefile({ "new1", "new2", "l1", "l2", "l3", "l4", "l5", "l6" }, worktree .. "/src/a.rs")
  vim.uv.fs_utime(worktree .. "/src/a.rs", os.time() + 10, os.time() + 10)
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  wait_notice("changed on disk")
  eq(child.lua_get("vim.bo.buftype"), "")
end

local function github_rules()
  local pr = helpers.session({
    id = "pr/github.com/o/r/7",
    source = {
      kind = "pr",
      number = 7,
      url = "u",
      title = "Add k",
      author = "alice",
      state = "OPEN",
      head_ref = "f",
      base_ref = "main",
      base_oid = string.rep("b", 40),
      node_id = "PR_1",
      worktree = worktree,
    },
    head = { ref = "f", sha = string.rep("c", 40), dirty = false },
  })
  pr.comments =
    { { id = "c_1", path = "src/a.rs", side = "head", line = 4, kind = "issue", body = "nit: tighter\nmore" } }
  local plan = {
    sink = "github",
    decision = "approve",
    text = "ship it",
    comment_ids = { "c_1" },
    body_comment_ids = {},
    skipped = {},
    replies = { { comment_id = "c_gone", thread_id = "PRRT_2" } },
    thread_actions = { { thread_id = "PRRT_1", action = "resolve" } },
    mode = "sent",
    raw = {},
  }
  local result = {
    sink = "github",
    at = "t",
    url = "https://github.com/o/r/pull/7#pullrequestreview-1",
    sent = { { comment_id = "c_1" } },
    failed = {},
    skipped = {},
    thread_actions = { { thread_id = "PRRT_1", action = "resolve", status = "sent" } },
    decision = "sent",
    summary = "1 sent, 0 failed, decision sent",
  }
  return {
    { match = { "session open --pr 7" }, body = { ok = true, data = vim.NIL, session = pr } },
    { match = { "submit", "--dry-run" }, body = { ok = true, data = { preview = plan } } },
    { match = { "submit", "--sink github" }, body = { ok = true, data = result, session = pr } },
  }
end

local function github_submits()
  local n = 0
  for _, call in ipairs(helpers.calls(dir)) do
    local joined = table.concat(call.args, " ")
    if joined:find("--sink github", 1, true) and not joined:find("--dry-run", 1, true) then
      n = n + 1
    end
  end
  return n
end

T["orchestrator"]["a GitHub submit is previewed and only sent after y"] = function()
  helpers.respond(dir, github_rules())
  child.cmd("Review pr 7")
  wait_notice("#7 Add k")
  child.cmd("ReviewSubmit github")
  child.lua("vim.wait(20000, function() return vim.bo.filetype == 'markdown' end, 20)")
  local lines = child.lua_get("vim.api.nvim_buf_get_lines(0, 0, -1, false)")
  eq(lines[1], "Review: approve")
  eq(lines[2], "ship it")
  eq(vim.tbl_contains(lines, "  src/a.rs:4  nit: tighter"), true)
  eq(vim.tbl_contains(lines, "  resolve PRRT_1"), true)
  eq(vim.tbl_contains(lines, "  c_gone"), true)
  eq(github_submits(), 0)
  child.type_keys("y")
  wait_notice("pullrequestreview-1")
  eq(github_submits(), 1)
  local notices = child.lua_get("_G.notices")
  eq(
    notices[#notices].msg,
    "[reviewkit] github: 1 sent, 0 failed, decision sent · 1/1 thread actions · https://github.com/o/r/pull/7#pullrequestreview-1"
  )
end

T["orchestrator"]["cancelling the preview sends nothing, and the preview can be switched off"] = function()
  helpers.respond(dir, github_rules())
  child.cmd("Review pr 7")
  wait_notice("#7 Add k")
  child.cmd("ReviewSubmit github")
  child.lua("vim.wait(20000, function() return vim.bo.filetype == 'markdown' end, 20)")
  child.type_keys("q")
  eq(github_submits(), 0)
  child.lua("require('reviewkit').setup({ sinks = { github = { confirm = false } } })")
  child.cmd("ReviewSubmit github")
  wait_notice("pullrequestreview-1")
  eq(github_submits(), 1)
end

T["orchestrator"]["a send that differs from the confirmed preview is previewed again"] = function()
  local rules = github_rules()
  rules[2].body.data.preview.digest = "d1"
  table.insert(rules, 2, {
    match = { "submit", "--sink github", '"expect_digest":"d1"' },
    body = {
      ok = false,
      error = {
        code = "E_CONFLICT",
        message = "changed",
        retryable = false,
        details = { reason = "preview_changed" },
      },
    },
    exit = 5,
  })
  helpers.respond(dir, rules)
  child.cmd("Review pr 7")
  wait_notice("#7 Add k")
  child.cmd("ReviewSubmit github")
  child.lua("vim.wait(20000, function() return vim.bo.filetype == 'markdown' end, 20)")
  child.type_keys("y")
  wait_notice("check it again")
  child.lua("vim.wait(20000, function() return vim.bo.filetype == 'markdown' end, 20)")
  eq(#calls_with("--dry-run"), 2)
  eq(child.lua_get("vim.bo.filetype"), "markdown")
end

T["orchestrator"]["the GitHub confirmation previews the same selection it sends"] = function()
  helpers.respond(dir, github_rules())
  child.cmd("Review pr 7")
  wait_notice("#7 Add k")
  child.lua("require('reviewkit.view.queue').show_comments()")
  child.cmd("copen")
  child.cmd("1ReviewSubmit github")
  wait_for_call("--dry-run")
  local preview = calls_with("--dry-run")[1]
  local stdin
  for _, call in ipairs(helpers.calls(dir)) do
    if table.concat(call.args, " "):find("--dry-run", 1, true) then
      stdin = vim.json.decode(call.stdin)
    end
  end
  eq(preview ~= nil, true)
  eq(stdin.comment_ids, { "c_1" })
end

T["orchestrator"]["files of a pull request's worktree cannot be edited"] = function()
  helpers.respond(dir, github_rules())
  child.cmd("Review pr 7")
  wait_notice("#7 Add k")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  eq(child.lua_get("vim.bo.modifiable"), false)
end

T["orchestrator"]["saving an empty comment says it was discarded and queues nothing"] = function()
  helpers.respond(dir, rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { '  ', '' })")
  child.cmd("write")
  local notices = wait_notice("discarded")
  eq(#vim.tbl_filter(function(n)
    return n.msg:find("discarded", 1, true) ~= nil
  end, notices), 1)
  eq(last_call("add"), nil)
end

T["orchestrator"]["saving an edit with no text keeps the comment and says so"] = function()
  local with = helpers.session({
    source = { worktree = worktree },
    rev = 2,
    comments = {
      { id = "c_1", path = "src/a.rs", side = "head", line = 4, kind = "issue", body = "keep", stale = false },
    },
    etags = { comments = "c1" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = with } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = with } },
    { match = { "comment update" }, body = { ok = true, data = vim.NIL, session = with } },
  })
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  child.lua("require('reviewkit.orchestrator').edit_comment()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { '' })")
  child.cmd("write")
  wait_notice("discarded")
  eq(last_call("update"), nil)
end

T["orchestrator"]["a comment on a file whose reload was declined is refused"] = function()
  helpers.respond(dir, rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua("vim.o.autoread = false")
  child.cmd("edit " .. worktree .. "/src/a.rs")
  vim.fn.writefile({ "new1", "new2", "l1", "l2", "l3", "l4", "l5", "l6" }, worktree .. "/src/a.rs")
  vim.uv.fs_utime(worktree .. "/src/a.rs", os.time() + 10, os.time() + 10)
  child.lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  -- The W11 dialog blocks the call; <CR> answers it with its default, OK.
  child.lua_notify("require('reviewkit.orchestrator').comment()")
  child.type_keys("<CR>")
  wait_notice("newer than the buffer")
  eq(child.lua_get("vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]"), "l1")
  eq(child.lua_get("vim.bo.buftype"), "")
end

local function fake_git(out_dir)
  local bin = helpers.fake_dir()
  local log = bin .. "/log"
  vim.fn.writefile({
    "#!/bin/sh",
    ('echo "GIT_DIR=$GIT_DIR GIT_WORK_TREE=$GIT_WORK_TREE $*" >> %s'):format(vim.fn.shellescape(log)),
    ("printf '%%s\\n%%s\\n' %s %s"):format(vim.fn.shellescape(out_dir), vim.fn.shellescape(out_dir)),
  }, bin .. "/git")
  vim.uv.fs_chmod(bin .. "/git", tonumber("755", 8))
  return bin .. "/git", log
end

T["orchestrator"]["the git dir is found with the configured git, outside GIT_DIR, and not watched twice"] = function()
  local real = vim.fn.resolve(helpers.fake_dir())
  vim.fn.mkdir(real .. "/.git", "p")
  local link = helpers.fake_dir() .. "-link"
  vim.uv.fs_symlink(real, link)
  local git, log = fake_git(real .. "/.git")
  local session = helpers.session({ source = { worktree = link }, repo = { root = link } })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = session } },
  })
  child.lua(("vim.env.GIT_DIR = '/elsewhere/.git'; require('reviewkit').setup({ tools = { git = %q } })"):format(git))
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua(("vim.wait(20000, function() return vim.uv.fs_stat(%q) ~= nil end, 20)"):format(log))
  child.lua("vim.wait(300, function() return false end, 20)")
  local logged = table.concat(vim.fn.readfile(log), "\n")
  eq(logged:find("rev-parse", 1, true) ~= nil, true)
  eq(logged:find("GIT_DIR= GIT_WORK_TREE= ", 1, true) ~= nil, true)
  eq(child.lua_get("require('reviewkit.orchestrator')._watching()"), 1)
end

T["orchestrator"]["a commit head on a branch is followed when the branch moves"] = function()
  local git_dir = vim.fn.resolve(helpers.fake_dir())
  vim.fn.mkdir(git_dir .. "/refs/heads", "p")
  local git = fake_git(git_dir)
  local function at(sha, rev)
    return helpers.session({
      source = { worktree = worktree, base_spec = "main", head_spec = "feature" },
      repo = { root = worktree },
      head = { ref = "feature", sha = sha, dirty = false },
      rev = rev,
    })
  end
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = at(string.rep("e", 40), 1) } },
    { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = at(string.rep("f", 40), 2) } },
    {
      match = { "file " },
      body = { ok = true, data = { exists = true, binary = false, too_large = false, lines = {} } },
    },
  })
  child.lua(("require('reviewkit').setup({ tools = { git = %q } })"):format(git))
  child.cmd("Review diff main feature")
  wait_notice("file to review")
  child.lua("vim.wait(20000, function() return require('reviewkit.orchestrator')._watching() > 0 end, 20)")
  eq(child.lua_get("require('reviewkit.orchestrator')._watching() > 0"), true)
  vim.fn.writefile({ string.rep("f", 40) }, git_dir .. "/refs/heads/feature")
  wait_notice("head moved")
  local notices = child.lua_get("_G.notices")
  local found = false
  for _, n in ipairs(notices) do
    found = found or n.msg:find("head moved to ffffffff", 1, true) ~= nil
  end
  eq(found, true)
end

T["orchestrator"]["an explicit refresh asked for during a background one reports its error"] = function()
  helpers.respond(dir, rules())
  child.cmd("Review diff HEAD~1")
  wait_notice("1 file to review")
  child.lua([[
    _G.pending = {}
    require("reviewkit.core").refresh = function(_, cb) table.insert(_G.pending, cb) end
    vim.api.nvim_exec_autocmds("FocusGained", {})
  ]])
  child.lua("vim.wait(20000, function() return #_G.pending == 1 end, 20)")
  child.cmd("Review refresh")
  child.lua("_G.pending[1](nil)")
  child.lua("vim.wait(20000, function() return #_G.pending == 2 end, 20)")
  child.lua([[_G.pending[2]({ code = "E_TOOL", message = "git broke", retryable = false, details = {} })]])
  local notices = wait_notice("git broke")
  local shown = false
  for _, n in ipairs(notices) do
    shown = shown or (n.msg:find("git broke", 1, true) ~= nil and n.level == vim.log.levels.ERROR)
  end
  eq(shown, true)
end

return T
