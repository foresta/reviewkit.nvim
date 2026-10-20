local M = {}

local root = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h")
M.root = root
M.stub = root .. "/tests/fixtures/bin/reviewkit"

---@return string dir
function M.fake_dir()
  local dir = vim.fn.tempname()
  vim.fn.mkdir(dir, "p")
  return dir
end

---@param dir string
---@param rules table[]
function M.respond(dir, rules)
  local f = assert(io.open(dir .. "/responses.json", "w"))
  f:write(vim.json.encode(rules))
  f:close()
end

---@param dir string
---@return table[] calls
function M.calls(dir)
  local f = io.open(dir .. "/calls.jsonl", "r")
  if not f then
    return {}
  end
  local calls = {}
  for line in f:lines() do
    table.insert(calls, vim.json.decode(line))
  end
  f:close()
  return calls
end

--- A minimal but complete session envelope, so tests only override the
--- parts they care about.
---@param overrides? table
---@return table
function M.session(overrides)
  local base = {
    schema_version = 1,
    id = "local/abc/HEAD..worktree",
    rev = 1,
    state_path = "/tmp/reviewkit-test/sessions/local/abc/HEAD..worktree/session.json",
    repo = { root = "/repo", host = vim.NIL, owner = vim.NIL, name = vim.NIL, remote_url = vim.NIL },
    source = { kind = "local", base_spec = "HEAD", head_spec = "worktree", worktree = "/repo" },
    base = { ref = "HEAD", sha = string.rep("a", 40) },
    head = { ref = vim.NIL, sha = vim.NIL, dirty = true },
    targets = {
      {
        id = "t_1",
        path = "src/a.rs",
        old_path = vim.NIL,
        status = "modified",
        reason = "changed",
        base_blob = string.rep("b", 40),
        head_blob = string.rep("c", 40),
        binary = false,
        too_large = false,
        viewed = false,
        stats = { added = 1, removed = 1 },
        hunks = { { base_start = 3, base_count = 1, head_start = 3, head_count = 1 } },
        commentable = { base = { { 1, 6 } }, head = { { 1, 6 } } },
        range = vim.NIL,
      },
    },
    comments = {},
    decision = vim.NIL,
    threads = {},
    thread_actions = {},
    viewed = vim.empty_dict(),
    last_submit = vim.NIL,
    etags = { targets = "t1", comments = "c0", decision = "d0", threads = "th0", thread_actions = "ta0", viewed = "v0" },
    created_at = "2026-01-01T00:00:00.000000Z",
    updated_at = "2026-01-01T00:00:00.000000Z",
  }
  return vim.tbl_deep_extend("force", base, overrides or {})
end

--- Starts a child Neovim with the stub binary and the fake dir, and
--- installs an event recorder (`_G.events`).
---@param dir string
---@return table child
function M.child(dir)
  local child = MiniTest.new_child_neovim()
  child.restart({ "-u", root .. "/scripts/minimal_init.lua" })
  child.lua(([[
    vim.env.REVIEWKIT_BIN = %q
    vim.env.FAKE_REVIEWKIT_DIR = %q
    require("reviewkit").setup({})
    _G.events = {}
    vim.api.nvim_create_autocmd("User", {
      pattern = "Review*",
      callback = function(args)
        table.insert(_G.events, { event = args.match, data = args.data })
      end,
    })
    _G.done = nil
    -- RPC cannot carry mixed array/hash tables, so results live under a key.
    _G.finish = function(err, ...)
      _G.done = { err = err, results = { ... } }
    end
    _G.wait_done = function()
      -- Generous: a loaded machine has taken over 5s to spawn the CLI.
      local ok = vim.wait(20000, function() return _G.done ~= nil end, 10)
      local d = _G.done
      _G.done = nil
      if not ok then error("callback never ran") end
      return d
    end
  ]]):format(M.stub, dir))
  return child
end

---@param child table
---@return string[]
function M.event_names(child)
  return vim.tbl_map(function(e)
    return e.event
  end, child.lua_get("_G.events"))
end

return M
