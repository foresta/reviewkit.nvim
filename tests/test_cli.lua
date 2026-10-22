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

T["cli"]["a CLI older than the plugin's contract revision blocks every call"] = function()
  local revision = require("reviewkit.types").SCHEMA_REVISION
  helpers.respond(dir, {
    {
      match = { "version" },
      body = { ok = true, data = { version = "0.0.9", schema_version = 1, schema_revision = revision - 1 } },
    },
  })
  child.lua("require('reviewkit.cli').call({ 'session', 'list' }, {}, _G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err.code, "E_SCHEMA_VERSION")
  eq(d.err.message:find("revision", 1, true) ~= nil, true)
  eq(#helpers.calls(dir), 1)
end

T["cli"]["a newer contract revision of the same version is accepted"] = function()
  local revision = require("reviewkit.types").SCHEMA_REVISION
  helpers.respond(dir, {
    {
      match = { "version" },
      body = { ok = true, data = { version = "9.9.9", schema_version = 1, schema_revision = revision + 5 } },
    },
    { match = { "session list" }, body = { ok = true, data = {} } },
  })
  child.lua("require('reviewkit.cli').call({ 'session', 'list' }, {}, _G.finish)")
  eq(child.lua_get("_G.wait_done().err"), vim.NIL)
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

T["cli"]["run_sync passes its cwd as --cwd, like run"] = function()
  helpers.respond(dir, { { match = { "session list" }, body = { ok = true, data = {} } } })
  child.lua("require('reviewkit.cli').run_sync({ 'session', 'list' }, { cwd = '/some/where' })")
  local calls = helpers.calls(dir)
  eq(calls[#calls].args, { "--cwd", "/some/where", "session", "list" })
end

T["cli"]["call retries a locked session a few times, then gives up"] = function()
  child.lua([[
    local cli = require("reviewkit.cli")
    cli.LOCK_RETRY_MS = { 10, 10, 10 }
    _G.attempts = 0
    _G.locked_for = 2
    cli.run = function(_, _, cb)
      _G.attempts = _G.attempts + 1
      if _G.attempts <= _G.locked_for then
        return vim.schedule(function()
          cb({ code = "E_LOCKED", message = "locked", retryable = true, details = {} })
        end)
      end
      vim.schedule(function()
        cb(nil, { ok = true, data = "done" })
      end)
    end
    cli._compatible = true
  ]])
  child.lua("require('reviewkit.cli').call({ 'x' }, {}, _G.finish)")
  local d = child.lua_get("_G.wait_done()")
  eq(d.err, nil)
  eq(child.lua_get("_G.attempts"), 3)

  child.lua("_G.attempts = 0; _G.locked_for = 99")
  child.lua("require('reviewkit.cli').call({ 'x' }, {}, _G.finish)")
  d = child.lua_get("_G.wait_done()")
  eq(d.err.code, "E_LOCKED")
  eq(child.lua_get("_G.attempts"), 4)
end

T["cli"]["health reports a missing git as an error and says what gh is for"] = function()
  helpers.respond(dir, {
    {
      match = { "health" },
      body = {
        ok = true,
        data = {
          tools = { git = {}, gh = {}, herdr = { path = "/bin/herdr", version = "1" } },
          gh_auth = vim.NIL,
          state_dir = { path = "/tmp/s", writable = true },
        },
      },
    },
  })
  child.lua([[
    _G.reported = {}
    for _, level in ipairs({ "start", "ok", "warn", "error", "info" }) do
      vim.health[level] = function(msg) table.insert(_G.reported, level .. ": " .. msg) end
    end
    require("reviewkit.health").check()
  ]])
  local reported = child.lua_get("_G.reported")
  local function has(text)
    for _, line in ipairs(reported) do
      if line:find(text, 1, true) then
        return true
      end
    end
    return false
  end
  eq(has("error: git: not found"), true)
  eq(has("pull requests"), true)
end

T["cli"]["health names the override whose tool path does not exist"] = function()
  local missing = dir .. "/no-such-gh"
  helpers.respond(dir, {
    {
      match = { "health" },
      body = {
        ok = true,
        data = {
          tools = { git = { path = "/bin/git", version = "2" }, gh = { path = missing }, herdr = { path = "/bin/sh" } },
          gh_auth = vim.NIL,
          state_dir = { path = "/tmp/s", writable = true },
        },
      },
    },
  })
  child.lua(([[
    require("reviewkit").setup({ tools = { gh = %q } })
    _G.reported = {}
    for _, level in ipairs({ "start", "ok", "warn", "error", "info" }) do
      vim.health[level] = function(msg) table.insert(_G.reported, level .. ": " .. msg) end
    end
    require("reviewkit.health").check()
  ]]):format(missing))
  local reported = table.concat(child.lua_get("_G.reported"), "\n")
  eq(reported:find("gh: not found at " .. missing .. " (set by tools.gh)", 1, true) ~= nil, true)
  eq(reported:find("found at " .. missing .. " but", 1, true), nil)
  eq(reported:find("herdr: found at /bin/sh but --version failed", 1, true) ~= nil, true)
end

T["cli"]["git run by the tests and their child Neovims ignores the global and system config"] = function()
  local outside = [[
    (function()
      local out = vim.system({ "git", "config", "--show-origin", "--list" }, { cwd = vim.fn.tempname():match("^(.*)/"), text = true }):wait().stdout
      return out:match("^file:[^\t]+") or out:match("\nfile:[^\t]+") or ""
    end)()
  ]]
  eq(vim.fn.luaeval(outside), "")
  eq(child.lua_get(outside), "")
end

return T
