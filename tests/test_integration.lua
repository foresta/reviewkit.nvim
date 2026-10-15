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
      assert(vim.wait(10000, function() return _G.done ~= nil end, 10), "callback never ran")
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
        cb(nil, { sent = { { comment_id = session.comments[1].id } }, failed = {}, skipped = {}, decision = 'not_applicable' })
      end,
    })
    require('reviewkit.core').submit('capture', {}, _G.finish)
  ]])
  local submitted = child.lua_get("_G.wait_done()")
  eq(submitted.err, nil)
  eq(child.lua_get("_G.captured"), "why B?")
  eq(submitted.results[1].summary, "1 sent, 0 failed")
  eq(#child.lua_get("require('reviewkit.core').comments()"), 0)
  child.stop()
end

return T
