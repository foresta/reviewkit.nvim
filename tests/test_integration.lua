local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()

--- A throwaway git repository with one commit and one uncommitted edit.
local function temp_repo()
  local dir = vim.fn.tempname()
  vim.fn.mkdir(dir, "p")
  local function git(args)
    local res = vim
      .system(
        vim.list_extend({ "git", "-c", "user.name=t", "-c", "user.email=t@example.com" }, args),
        { cwd = dir, text = true }
      )
      :wait()
    assert(res.code == 0, res.stderr)
  end
  git({ "init", "-q", "-b", "main" })
  vim.fn.writefile({ "a", "b", "c" }, dir .. "/f.txt")
  git({ "add", "f.txt" })
  git({ "commit", "-q", "-m", "base" })
  vim.fn.writefile({ "a", "B", "c" }, dir .. "/f.txt")
  return dir
end

T["real binary: open, comment, submit through a Lua sink"] = function()
  local bin = vim.env.REVIEWKIT_REAL_BIN
  if not bin or bin == "" then
    MiniTest.skip("REVIEWKIT_REAL_BIN is not set")
  end
  local repo = temp_repo()
  local state_dir = helpers.fake_dir()
  local child = MiniTest.new_child_neovim()
  child.restart({ "-u", helpers.root .. "/scripts/minimal_init.lua" })
  child.lua(([[
    vim.env.REVIEWKIT_BIN = %q
    require("reviewkit").setup({ cli = { state_dir = %q } })
    _G.done = nil
    _G.finish = function(err, ...) _G.done = { err = err, results = { ... } } end
    _G.wait_done = function()
      assert(vim.wait(30000, function() return _G.done ~= nil end, 10), "callback never ran")
      local d = _G.done
      _G.done = nil
      return d
    end
  ]]):format(bin, state_dir))
  child.lua(("require('reviewkit.core').open({ kind = 'local', base = 'HEAD', cwd = %q }, _G.finish)"):format(repo))
  local opened = child.lua_get("_G.wait_done()")
  eq(opened.err, nil)
  eq(opened.results[1].targets[1].path, "f.txt")
  eq(child.lua_get("require('reviewkit.core').is_commentable('f.txt', 'head', 2)"), true)

  child.lua(
    "require('reviewkit.core').add_comment({ path = 'f.txt', side = 'head', line = 2, body = 'why B?' }, _G.finish)"
  )
  local added = child.lua_get("_G.wait_done()")
  eq(added.err, nil)
  eq(#added.results[1].comments, 1)
  eq(added.results[1].comments[1].commentable.github, true)

  child.lua([[
    require('reviewkit.core').register_sink('capture', {
      submit = function(session, opts, cb)
        _G.captured = session.comments[1].body
        cb(nil, { sent = { { comment_id = session.comments[1].id } } })
      end,
    })
    require('reviewkit.core').submit('capture', {}, _G.finish)
  ]])
  local submitted = child.lua_get("_G.wait_done()")
  eq(submitted.err, nil)
  eq(child.lua_get("_G.captured"), "why B?")
  eq(submitted.results[1].summary, "1 sent, 0 failed")
  eq(#child.lua_get("require('reviewkit.core').comments()"), 1)
  eq(child.lua_get("require('reviewkit.core').comments()[1].deliveries[1].sink"), "capture")
  child.stop()
end

T["real binary: :Review diff → comment → :ReviewSubmit herdr reaches a fake herdr"] = function()
  local bin = vim.env.REVIEWKIT_REAL_BIN
  if not bin or bin == "" then
    MiniTest.skip("REVIEWKIT_REAL_BIN is not set")
  end
  local repo = temp_repo()
  local state_dir = helpers.fake_dir()
  local herdr_dir = helpers.fake_dir()
  local root = vim.uv.fs_realpath(repo)
  local f = assert(io.open(herdr_dir .. "/agents.json", "w"))
  f:write(vim.json.encode({
    result = {
      agents = { { pane_id = "w1:p1", agent = "claude", agent_status = "idle", cwd = root, foreground_cwd = root } },
    },
  }))
  f:close()
  local child = MiniTest.new_child_neovim()
  child.restart({ "-u", helpers.root .. "/scripts/minimal_init.lua" })
  child.lua(([[
    vim.env.REVIEWKIT_BIN = %q
    vim.env.REVIEWKIT_HERDR = %q
    vim.env.FAKE_HERDR_DIR = %q
    require("reviewkit").setup({ cli = { state_dir = %q } })
    _G.notices = {}
    vim.notify = function(msg) table.insert(_G.notices, msg) end
    vim.fn.chdir(%q)
  ]]):format(bin, helpers.root .. "/cli/tests/fixtures/bin/herdr", herdr_dir, state_dir, repo))
  local function wait_notice(pattern)
    child.lua(
      ("vim.wait(30000, function() for _, n in ipairs(_G.notices) do if n:find(%q, 1, true) then return true end end return false end, 20)"):format(
        pattern
      )
    )
  end
  child.cmd("Review diff HEAD")
  wait_notice("1 file to review")
  child.cmd("edit " .. repo .. "/f.txt")
  child.lua("vim.api.nvim_win_set_cursor(0, { 2, 0 })")
  child.lua("require('reviewkit.orchestrator').comment()")
  child.lua("vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'question: why B?' })")
  child.cmd("write")
  wait_notice("queued f.txt:2")
  child.cmd("ReviewSubmit herdr")
  wait_notice("1 sent")
  local sent = io.open(herdr_dir .. "/last_text", "r")
  local text = sent and sent:read("*a") or ""
  if sent then
    sent:close()
  end
  eq(text:find("1. @f.txt#L2 (question)\nwhy B?", 1, true) ~= nil, true)
  eq(#child.lua_get("require('reviewkit.core').comments()"), 0)
  child.stop()
end

T["real binary: a queued comment does not make the worktree watcher refresh by itself"] = function()
  local bin = vim.env.REVIEWKIT_REAL_BIN
  if not bin or bin == "" then
    MiniTest.skip("REVIEWKIT_REAL_BIN is not set")
  end
  if vim.uv.os_uname().sysname ~= "Darwin" then
    MiniTest.skip("only macOS watches the worktree recursively")
  end
  local repo = vim.fn.resolve(temp_repo())
  local child = MiniTest.new_child_neovim()
  child.restart({ "-u", helpers.root .. "/scripts/minimal_init.lua" })
  child.lua(([[
    vim.env.REVIEWKIT_BIN = %q
    require("reviewkit").setup({ cli = { state_dir = %q } })
    vim.cmd.cd(%q)
    vim.notify = function() end
    local cli = require("reviewkit.cli")
    local call = cli.call
    _G.refreshes = 0
    cli.call = function(args, ...)
      if args[1] == "session" and args[2] == "refresh" then
        _G.refreshes = _G.refreshes + 1
      end
      return call(args, ...)
    end
  ]]):format(bin, helpers.fake_dir(), repo))
  child.cmd("Review diff HEAD")
  assert(child.lua_get("vim.wait(30000, function() return require('reviewkit.core').session() ~= nil end, 20)"))
  child.lua(
    "require('reviewkit.core').add_comment({ path = 'f.txt', side = 'head', line = 2, body = 'b' }, function() _G.added = true end)"
  )
  assert(child.lua_get("vim.wait(30000, function() return _G.added end, 20)"))
  child.lua([[
    local last, stable_since = -1, vim.uv.now()
    vim.wait(30000, function()
      if _G.refreshes ~= last then
        last, stable_since = _G.refreshes, vim.uv.now()
      end
      return vim.uv.now() - stable_since > 1000
    end, 50)
  ]])
  child.lua("_G.refreshes = 0")
  child.lua("vim.wait(3000)")
  eq(child.lua_get("_G.refreshes"), 0)
  child.stop()
end

T["real binary: a commit in a linked worktree refreshes the session"] = function()
  local bin = vim.env.REVIEWKIT_REAL_BIN
  if not bin or bin == "" then
    MiniTest.skip("REVIEWKIT_REAL_BIN is not set")
  end
  local main = vim.fn.resolve(temp_repo())
  local linked = vim.fn.resolve(vim.fn.tempname())
  local function git(dir, args)
    local res = vim
      .system(
        vim.list_extend({ "git", "-c", "user.name=t", "-c", "user.email=t@example.com" }, args),
        { cwd = dir, text = true }
      )
      :wait()
    assert(res.code == 0, res.stderr)
  end
  git(main, { "worktree", "add", "-q", "-b", "feature", linked })
  vim.fn.writefile({ "a", "b", "c", "d" }, linked .. "/f.txt")
  local child = MiniTest.new_child_neovim()
  child.restart({ "-u", helpers.root .. "/scripts/minimal_init.lua" })
  child.lua(([[
    vim.env.REVIEWKIT_BIN = %q
    require("reviewkit").setup({ cli = { state_dir = %q } })
    vim.cmd.cd(%q)
    vim.notify = function() end
  ]]):format(bin, helpers.fake_dir(), linked))
  child.cmd("Review diff HEAD")
  assert(child.lua_get("vim.wait(30000, function() return require('reviewkit.core').session() ~= nil end, 20)"))
  eq(child.lua_get("#require('reviewkit.core').targets()"), 1)
  -- The linked worktree's own watchers come in one go after the root's.
  assert(child.lua_get("vim.wait(30000, function() return require('reviewkit.orchestrator')._watching() >= 3 end, 20)"))
  git(linked, { "commit", "-q", "-am", "agent" })
  assert(child.lua_get("vim.wait(30000, function() return #require('reviewkit.core').targets() == 0 end, 50)"))
  child.stop()
end

return T
