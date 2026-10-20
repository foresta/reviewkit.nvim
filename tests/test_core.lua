local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()
local child, dir

local function open_rules(extra)
  local rules = {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = helpers.session() } },
  }
  vim.list_extend(rules, extra or {})
  return rules
end

T["core"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      dir = helpers.fake_dir()
      child = helpers.child(dir)
    end,
    post_case = function()
      child.stop()
    end,
  },
})

local function open(child_)
  child_.lua("require('reviewkit.core').open({ kind = 'local', base = 'HEAD' }, _G.finish)")
  return child_.lua_get("_G.wait_done()")
end

T["core"]["open fires Opened then one event per section"] = function()
  helpers.respond(dir, open_rules())
  local d = open(child)
  eq(d.err, nil)
  eq(helpers.event_names(child), {
    "ReviewSessionOpened",
    "ReviewTargetsChanged",
    "ReviewQueueChanged",
    "ReviewDecisionChanged",
    "ReviewThreadsChanged",
    "ReviewViewedChanged",
  })
  local first = child.lua_get("_G.events[1].data")
  eq(first.session_id, "local/abc/HEAD..worktree")
  eq(first.rev, 1)
  eq(child.lua_get("require('reviewkit.core').session().rev"), 1)
  eq(#child.lua_get("require('reviewkit.core').targets()"), 1)
end

T["core"]["add_comment fires ReviewQueueChanged exactly once"] = function()
  local after = helpers.session({
    rev = 2,
    comments = { { id = "c_1", path = "src/a.rs", side = "head", line = 3 } },
    etags = { comments = "c1" },
  })
  helpers.respond(
    dir,
    open_rules({ { match = { "comment add" }, body = { ok = true, data = { id = "c_1" }, session = after } } })
  )
  open(child)
  child.lua("_G.events = {}")
  child.lua(
    "require('reviewkit.core').add_comment({ path = 'src/a.rs', side = 'head', line = 3, body = 'hi' }, _G.finish)"
  )
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  eq(d.results[2].id, "c_1")
  eq(helpers.event_names(child), { "ReviewQueueChanged" })
  eq(child.lua_get("_G.events[1].data.rev"), 2)
  eq(#child.lua_get("require('reviewkit.core').comments()"), 1)
  local call = helpers.calls(dir)
  local last = call[#call]
  eq(last.args, { "comment", "add", "--session", "local/abc/HEAD..worktree" })
  eq(vim.json.decode(last.stdin).body, "hi")
end

T["core"]["an unchanged response fires nothing"] = function()
  helpers.respond(
    dir,
    open_rules({
      { match = { "session refresh" }, body = { ok = true, data = vim.NIL, session = helpers.session({ rev = 2 }) } },
    })
  )
  open(child)
  child.lua("_G.events = {}")
  child.lua("require('reviewkit.core').refresh({}, _G.finish)")
  child.lua_get("_G.wait_done()")
  eq(helpers.event_names(child), {})
  eq(child.lua_get("require('reviewkit.core').session().rev"), 2)
end

T["core"]["is_commentable reads the cached ranges synchronously"] = function()
  helpers.respond(dir, open_rules())
  open(child)
  eq(child.lua_get("require('reviewkit.core').is_commentable('src/a.rs', 'head', 3)"), true)
  eq(child.lua_get("require('reviewkit.core').is_commentable('src/a.rs', 'head', 7)"), false)
  eq(child.lua_get("require('reviewkit.core').is_commentable('other', 'head', 1)"), false)
end

T["core"]["mutations without a session fail with E_NO_SESSION"] = function()
  child.lua("require('reviewkit.core').add_comment({ path = 'x', side = 'head', line = 1, body = 'b' }, _G.finish)")
  eq(child.lua_get("_G.wait_done()").err.code, "E_NO_SESSION")
  eq(#helpers.calls(dir), 0)
end

T["core"]["close fires ReviewSessionClosed and drops the cache"] = function()
  helpers.respond(dir, open_rules({ { match = { "session close" }, body = { ok = true, data = { closed = true } } } }))
  open(child)
  child.lua("_G.events = {}")
  child.lua("require('reviewkit.core').close({}, _G.finish)")
  eq(child.lua_get("_G.wait_done()").err, nil)
  eq(helpers.event_names(child), { "ReviewSessionClosed" })
  eq(child.lua_get("require('reviewkit.core').session()"), vim.NIL)
end

T["core"]["submit through the CLI merges configured sink options and fires ReviewSubmitted"] = function()
  local result = {
    sink = "herdr",
    at = "t",
    url = vim.NIL,
    sent = { { comment_id = "c_1" } },
    failed = {},
    skipped = {},
    thread_actions = {},
    decision = "not_applicable",
    summary = "1 sent, 0 failed",
  }
  helpers.respond(
    dir,
    open_rules({
      {
        match = { "submit --session" },
        body = { ok = true, data = result, session = helpers.session({ rev = 3, etags = { comments = "c2" } }) },
      },
    })
  )
  open(child)
  child.lua("_G.events = {}")
  child.lua("require('reviewkit.core').submit('herdr', { header = 'hey' }, _G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  eq(d.results[1].summary, "1 sent, 0 failed")
  eq(helpers.event_names(child), { "ReviewQueueChanged", "ReviewSubmitted" })
  eq(child.lua_get("_G.events[2].data.result.summary"), "1 sent, 0 failed")
  local calls = helpers.calls(dir)
  local last = calls[#calls]
  eq(last.args, { "submit", "--session", "local/abc/HEAD..worktree", "--sink", "herdr", "--options", "-" })
  local opts = vim.json.decode(last.stdin)
  eq(opts.header, "hey")
  eq(opts.agent_type, "claude")
  eq(opts.match_foreground_cwd, true)
end

T["core"]["a registered Lua sink is applied through submit apply"] = function()
  helpers.respond(
    dir,
    open_rules({
      {
        match = { "submit apply" },
        body = {
          ok = true,
          data = { sink = "clip", summary = "1 sent, 0 failed" },
          session = helpers.session({ rev = 3, etags = { comments = "c9" } }),
        },
      },
    })
  )
  open(child)
  child.lua([[
    require('reviewkit.core').register_sink('clip', {
      submit = function(session, opts, cb)
        _G.sink_saw = { id = session.id, opts = opts }
        cb(nil, { sent = { { comment_id = 'c_1' } }, failed = {}, skipped = {}, decision = 'not_applicable' })
      end,
    })
    require('reviewkit.core').submit('clip', { x = 1 }, _G.finish)
  ]])
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  eq(child.lua_get("_G.sink_saw.id"), "local/abc/HEAD..worktree")
  local calls = helpers.calls(dir)
  local last = calls[#calls]
  eq(last.args, { "submit", "apply", "--session", "local/abc/HEAD..worktree" })
  eq(vim.json.decode(last.stdin).sink, "clip")
end

T["core"]["a delivery waiting to be recorded is drained once per change and holds back Lua sinks"] = function()
  helpers.respond(dir, {
    {
      match = { "session open --local" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ pending_apply = true }) },
    },
    {
      match = { "--drain" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ pending_apply = true }) },
    },
    {
      match = { "session get" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ pending_apply = true }) },
    },
  })
  open(child)
  child.lua("vim.wait(1500, function() return false end, 50)")
  local drains = 0
  for _, call in ipairs(helpers.calls(dir)) do
    if vim.tbl_contains(call.args, "--drain") then
      drains = drains + 1
    end
  end
  eq(drains, 1)
  child.lua([[
    _G.delivered = false
    require('reviewkit.core').register_sink('clip', {
      submit = function(_, _, cb)
        _G.delivered = true
        cb(nil, { sent = {}, failed = {}, skipped = {}, decision = 'not_applicable' })
      end,
    })
    require('reviewkit.core').submit('clip', {}, _G.finish)
  ]])
  local d = child.lua_get("_G.wait_done()")
  eq(d.err.code, "E_CONFLICT")
  eq(child.lua_get("_G.delivered"), false)
end

T["core"]["a Lua sink checks the disk for another instance's waiting delivery first"] = function()
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = helpers.session() } },
    {
      match = { "session get" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ rev = 2, pending_apply = true }) },
    },
    {
      match = { "--drain" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ rev = 3 }) },
    },
    {
      match = { "submit apply" },
      body = { ok = true, data = { sink = "clip", summary = "1 sent" }, session = helpers.session({ rev = 4 }) },
    },
  })
  open(child)
  child.lua([[
    require('reviewkit.core').register_sink('clip', {
      submit = function(_, _, cb)
        cb(nil, { sent = { { comment_id = 'c_1' } }, failed = {}, skipped = {}, decision = 'not_applicable' })
      end,
    })
    require('reviewkit.core').submit('clip', {}, _G.finish)
  ]])
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  local order = {}
  for _, call in ipairs(helpers.calls(dir)) do
    local joined = table.concat(call.args, " ")
    if joined:find("session get", 1, true) then
      table.insert(order, "get")
    elseif joined:find("--drain", 1, true) then
      table.insert(order, "drain")
    elseif joined:find("submit apply", 1, true) then
      table.insert(order, "apply")
    end
  end
  eq(order[1], "get")
  eq(order[#order], "apply")
  eq(vim.tbl_contains(order, "drain"), true)
end

T["core"]["sinks merges the CLI's list with registered Lua sinks, Lua winning"] = function()
  helpers.respond(dir, {
    {
      match = { "sink list" },
      body = {
        ok = true,
        data = {
          { name = "herdr", available = true, reason = vim.NIL, runtime = "cli" },
          { name = "github", available = false, reason = "gh is not installed", runtime = "cli" },
        },
      },
    },
  })
  child.lua([[
    local core = require('reviewkit.core')
    core.register_sink('github', { submit = function() end })
    core.register_sink('clip', {
      submit = function() end,
      available = function() return false, 'no clipboard' end,
    })
    core.sinks(function(err, list) _G.finish(err, list) end)
  ]])
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  local by_name = {}
  for _, s in ipairs(d.results[1]) do
    by_name[s.name] = s
  end
  eq(by_name.herdr.runtime, "cli")
  eq(by_name.github.runtime, "lua")
  eq(by_name.github.available, true)
  eq(by_name.clip.available, false)
  eq(by_name.clip.reason, "no clipboard")
  eq(child.lua_get("require('reviewkit.core').sink_names()"), { "clip", "github", "herdr" })
end

T["core"]["a Lua sink decides consumption and its result names the decision it saw"] = function()
  local with_decision = helpers.session({
    decision = { event = "comment", body = "lgtm", updated_at = "2026-01-01T00:00:00Z" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = with_decision } },
    { match = { "submit apply" }, body = { ok = true, data = { sink = "clip" }, session = with_decision } },
  })
  open(child)
  child.lua([[
    local core = require('reviewkit.core')
    core.register_sink('clip', {
      submit = function(_, _, cb)
        cb(nil, { sent = {}, failed = {}, skipped = {}, decision = 'sent' })
      end,
      consumes = function(_, requested) return requested ~= false end,
    })
    core.submit('clip', {}, _G.finish)
  ]])
  child.lua_get("_G.wait_done()")
  local calls = helpers.calls(dir)
  local last = calls[#calls]
  eq(last.args, { "submit", "apply", "--session", "local/abc/HEAD..worktree", "--consume" })
  eq(vim.json.decode(last.stdin).decision_version, "2026-01-01T00:00:00Z")
  child.lua("require('reviewkit.core').submit('clip', { consume = false }, _G.finish)")
  child.lua_get("_G.wait_done()")
  calls = helpers.calls(dir)
  eq(calls[#calls].args, { "submit", "apply", "--session", "local/abc/HEAD..worktree" })
end

T["core"]["a Lua sink given a selection sees only those comments and no decision"] = function()
  local session = helpers.session({
    comments = {
      { id = "c_1", path = "src/a.rs", side = "head", line = 3, body = "one" },
      { id = "c_2", path = "src/a.rs", side = "head", line = 4, body = "two" },
    },
    decision = { event = "comment", body = "lgtm", updated_at = "t" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    { match = { "submit apply" }, body = { ok = true, data = { sink = "clip" }, session = session } },
  })
  open(child)
  child.lua([[
    require('reviewkit.core').register_sink('clip', {
      submit = function(s, _, cb)
        _G.saw = { ids = vim.tbl_map(function(c) return c.id end, s.comments), decision = s.decision }
        cb(nil, { sent = { { comment_id = 'c_2' } }, failed = {}, skipped = {}, decision = 'not_applicable' })
      end,
    })
    require('reviewkit.core').submit('clip', { comment_ids = { 'c_2' } }, _G.finish)
  ]])
  child.lua_get("_G.wait_done()")
  eq(child.lua_get("_G.saw.ids"), { "c_2" })
  eq(child.lua_get("_G.saw.decision == nil"), true)
  local calls = helpers.calls(dir)
  eq(vim.json.decode(calls[#calls].stdin).attempted, { "c_2" })
end

T["core"]["a Lua sink's selection carries no thread actions, and an unknown id is refused"] = function()
  local session = helpers.session({
    comments = { { id = "c_1", path = "src/a.rs", side = "head", line = 3, body = "one" } },
    thread_actions = { { thread_id = "PRRT_1", action = "resolve" } },
    decision = { event = "comment", body = "lgtm", updated_at = "t" },
  })
  helpers.respond(dir, {
    { match = { "session open --local" }, body = { ok = true, data = vim.NIL, session = session } },
    {
      match = { "history list" },
      body = { ok = true, data = { { id = "c_old", path = "src/a.rs", side = "head", line = 1, body = "sent" } } },
    },
    { match = { "submit apply" }, body = { ok = true, data = { sink = "clip" }, session = session } },
  })
  open(child)
  child.lua([[
    require('reviewkit.core').register_sink('clip', {
      submit = function(s, _, cb)
        _G.saw = { actions = #(s.thread_actions or {}), history = #(s.history or {}) }
        cb(nil, { sent = {}, failed = {}, skipped = {}, decision = 'not_applicable' })
      end,
    })
    require('reviewkit.core').submit('clip', { comment_ids = { 'c_1', 'c_old' } }, _G.finish)
  ]])
  child.lua_get("_G.wait_done()")
  eq(child.lua_get("_G.saw"), { actions = 0, history = 0 })
  local calls = helpers.calls(dir)
  eq(vim.json.decode(calls[#calls].stdin).decision, "skipped")
  child.lua("require('reviewkit.core').submit('clip', { comment_ids = { 'c_nope' } }, _G.finish)")
  eq(child.lua_get("_G.wait_done().err.code"), "E_NO_COMMENT")
end

T["core"]["the clipboard sink trusts an OSC 52 provider it cannot read back"] = function()
  child.lua([[
    vim.g.clipboard = {
      name = "OSC 52",
      copy = { ["+"] = function() end, ["*"] = function() end },
      paste = { ["+"] = function() return {} end, ["*"] = function() return {} end },
    }
    local session = { comments = { { id = "c_1", path = "a", side = "head", line = 1, body = "x" } } }
    require("reviewkit.sink.clipboard").submit(session, { register = "+" }, function(_, result)
      _G.result = result
    end)
  ]])
  eq(child.lua_get("#_G.result.failed"), 0)
  eq(child.lua_get("_G.result.mode"), "copied")
end

T["core"]["a session file rewritten by another process is picked up"] = function()
  local state_dir = helpers.fake_dir()
  local session_dir = state_dir .. "/sessions/local/abc/HEAD..worktree"
  vim.fn.mkdir(session_dir, "p")
  local path = session_dir .. "/session.json"
  helpers.respond(dir, {
    {
      match = { "session open --local" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ state_path = path }) },
    },
    {
      match = { "session get" },
      body = {
        ok = true,
        data = vim.NIL,
        session = helpers.session({ state_path = path, rev = 5, etags = { decision = "d9" } }),
      },
    },
  })
  open(child)
  child.lua("_G.events = {}")
  local f = assert(io.open(path .. ".tmp", "w"))
  f:write("{}")
  f:close()
  os.rename(path .. ".tmp", path)
  child.lua("vim.wait(20000, function() return #_G.events > 0 end, 20)")
  eq(helpers.event_names(child), { "ReviewDecisionChanged" })
  eq(child.lua_get("require('reviewkit.core').session().rev"), 5)
end

T["core"]["opening another session closes the first and moves the watcher"] = function()
  local state_dir = helpers.fake_dir()
  local function session_at(id, extra)
    local dir_ = state_dir .. "/sessions/" .. id
    vim.fn.mkdir(dir_, "p")
    return helpers.session(vim.tbl_deep_extend("force", { id = id, state_path = dir_ .. "/session.json" }, extra or {}))
  end
  helpers.respond(dir, {
    {
      match = { "session open --local", "--base A" },
      body = { ok = true, data = vim.NIL, session = session_at("local/a/x") },
    },
    {
      match = { "session open --local", "--base B" },
      body = { ok = true, data = vim.NIL, session = session_at("local/b/x") },
    },
    {
      match = { "session get", "local/b/x" },
      body = { ok = true, data = vim.NIL, session = session_at("local/b/x", { rev = 7, etags = { viewed = "v9" } }) },
    },
  })
  child.lua("require('reviewkit.core').open({ kind = 'local', base = 'A' }, _G.finish)")
  child.lua_get("_G.wait_done()")
  child.lua("_G.events = {}")
  child.lua("require('reviewkit.core').open({ kind = 'local', base = 'B' }, _G.finish)")
  child.lua_get("_G.wait_done()")
  eq(helpers.event_names(child)[1], "ReviewSessionClosed")
  eq(helpers.event_names(child)[2], "ReviewSessionOpened")
  child.lua("_G.events = {}")
  local path = state_dir .. "/sessions/local/b/x/session.json"
  local f = assert(io.open(path .. ".tmp", "w"))
  f:write("{}")
  f:close()
  os.rename(path .. ".tmp", path)
  child.lua("vim.wait(20000, function() return #_G.events > 0 end, 20)")
  eq(helpers.event_names(child), { "ReviewViewedChanged" })
  eq(child.lua_get("require('reviewkit.core').session().rev"), 7)
end

T["core"]["an older rev from the CLI is ignored and nothing fires after close"] = function()
  local state_dir = helpers.fake_dir()
  local session_dir = state_dir .. "/sessions/local/abc/HEAD..worktree"
  vim.fn.mkdir(session_dir, "p")
  local path = session_dir .. "/session.json"
  helpers.respond(dir, {
    {
      match = { "session open --local" },
      body = { ok = true, data = vim.NIL, session = helpers.session({ state_path = path, rev = 5 }) },
    },
    {
      match = { "session get" },
      body = {
        ok = true,
        data = vim.NIL,
        session = helpers.session({ state_path = path, rev = 3, etags = { decision = "old" } }),
      },
    },
    { match = { "session close" }, body = { ok = true, data = { closed = true } } },
  })
  open(child)
  child.lua("_G.events = {}")
  local f = assert(io.open(path .. ".tmp", "w"))
  f:write("{}")
  f:close()
  os.rename(path .. ".tmp", path)
  child.lua("vim.wait(600, function() return #_G.events > 0 end, 20)")
  eq(helpers.event_names(child), {})
  eq(child.lua_get("require('reviewkit.core').session().rev"), 5)

  child.lua("require('reviewkit.core').close({}, _G.finish)")
  child.lua_get("_G.wait_done()")
  child.lua("_G.events = {}")
  f = assert(io.open(path .. ".tmp", "w"))
  f:write("{}")
  f:close()
  os.rename(path .. ".tmp", path)
  child.lua("vim.wait(600, function() return #_G.events > 0 end, 20)")
  eq(helpers.event_names(child), {})
end

return T
