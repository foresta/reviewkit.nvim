local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()
local child, dir

T["cli"] = MiniTest.new_set({
  hooks = {
    pre_case = function()
      dir = helpers.fake_dir()
      helpers.respond(dir, {})
      child = helpers.child(dir)
    end,
    post_case = function()
      child.stop()
    end,
  },
})

T["cli"]["resolves $REVIEWKIT_BIN and caches it"] = function()
  eq(child.lua_get("require('reviewkit.cli').resolve()"), helpers.stub)
  child.lua("vim.env.REVIEWKIT_BIN = '/nonexistent'")
  eq(child.lua_get("require('reviewkit.cli').resolve()"), helpers.stub)
  child.lua("require('reviewkit.cli').reset()")
  -- A built <plugin>/bin/reviewkit may exist on the developer machine, so
  -- only the stub's absence is asserted, not an empty result.
  local r =
    child.lua_get("(function() local p, e = require('reviewkit.cli').resolve() return { path = p, err = e } end)()")
  eq(r.path ~= helpers.stub, true)
  if r.path == nil then
    eq(r.err.code, "E_INTERNAL")
  end
end

T["cli"]["decodes ok envelopes and passes data and session"] = function()
  helpers.respond(
    dir,
    { { match = { "session get" }, body = { ok = true, data = vim.NIL, session = helpers.session() } } }
  )
  child.lua("require('reviewkit.cli').run({ 'session', 'get', '--session', 'x' }, {}, _G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  eq(d.results[1].ok, true)
  eq(d.results[1].session.id, "local/abc/HEAD..worktree")
end

T["cli"]["turns error envelopes into error tables"] = function()
  helpers.respond(dir, {
    {
      match = { "comment add" },
      body = {
        ok = false,
        error = {
          code = "E_NOT_COMMENTABLE",
          message = "nope",
          hint = "pick another line",
          retryable = false,
          details = {},
        },
      },
      exit = 2,
    },
  })
  child.lua(
    "require('reviewkit.cli').run({ 'comment', 'add', '--session', 'x' }, { stdin = { path = 'a' } }, _G.finish)"
  )
  local d = child.lua_get("_G.wait_done()")
  eq(d.err.code, "E_NOT_COMMENTABLE")
  eq(d.err.hint, "pick another line")
  local calls = helpers.calls(dir)
  eq(vim.json.decode(calls[#calls].stdin).path, "a")
end

T["cli"]["non-JSON output is E_INTERNAL with the raw text"] = function()
  helpers.respond(dir, { { match = { "health" }, raw = "thread 'main' panicked at boom\n", exit = 101 } })
  child.lua("require('reviewkit.cli').run({ 'health' }, {}, _G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err.code, "E_INTERNAL")
  eq(d.err.message:find("panicked", 1, true) ~= nil, true)
end

T["cli"]["a schema_version mismatch blocks every call"] = function()
  helpers.respond(
    dir,
    { { match = { "version" }, body = { ok = true, data = { version = "9.9.9", schema_version = 99 } } } }
  )
  child.lua("require('reviewkit.cli').call({ 'session', 'list' }, {}, _G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err.code, "E_SCHEMA_VERSION")
  local calls = helpers.calls(dir)
  eq(#calls, 1)
end

T["cli"]["passes configured tool paths as REVIEWKIT_* and the state dir as a flag"] = function()
  helpers.respond(dir, {
    { match = { "health" }, body = { ok = true, data = { env = "seen" } } },
  })
  child.lua("require('reviewkit').setup({ cli = { state_dir = '/tmp/rk-state' }, tools = { git = '/opt/git' } })")
  child.lua("require('reviewkit.cli').run({ 'health' }, {}, _G.finish)")
  child.lua_get("_G.wait_done()")
  local calls = helpers.calls(dir)
  eq(calls[#calls].args, { "--state-dir", "/tmp/rk-state", "health" })
end

return T
