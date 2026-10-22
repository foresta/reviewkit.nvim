local config = require("reviewkit.config")
local log = require("reviewkit.log")
local types = require("reviewkit.types")

local M = {}

---@class reviewkit.CliError : reviewkit.Error

---@param code reviewkit.ErrorCode
---@param message string
---@param hint? string
---@return reviewkit.CliError
local function make_error(code, message, hint)
  return { code = code, message = message, hint = hint, retryable = false, details = {} }
end

local function plugin_root()
  local source = debug.getinfo(1, "S").source:sub(2)
  return vim.fn.fnamemodify(source, ":p:h:h:h")
end

--- Cached after the first success so the lookups do not run on every call.
---@return string? path
---@return reviewkit.CliError? err
function M.resolve()
  if M._resolved then
    return M._resolved
  end
  local cfg = config.get().cli
  local candidates = {}
  if cfg.path and cfg.path ~= "" then
    table.insert(candidates, cfg.path)
  end
  local env = vim.env.REVIEWKIT_BIN
  if env and env ~= "" then
    table.insert(candidates, env)
  end
  local root = plugin_root()
  if cfg.dev then
    -- cargo builds there instead when it is set; the fixed paths below would
    -- then hold a stale binary from before it was set.
    local target_dir = vim.env.CARGO_TARGET_DIR
    if target_dir and target_dir ~= "" then
      target_dir = vim.fs.normalize(target_dir:sub(1, 1) == "/" and target_dir or root .. "/" .. target_dir)
      table.insert(candidates, target_dir .. "/debug/reviewkit")
      table.insert(candidates, target_dir .. "/release/reviewkit")
    end
    table.insert(candidates, root .. "/cli/target/debug/reviewkit")
    table.insert(candidates, root .. "/cli/target/release/reviewkit")
    table.insert(candidates, root .. "/target/debug/reviewkit")
    table.insert(candidates, root .. "/target/release/reviewkit")
  end
  table.insert(candidates, root .. "/bin/reviewkit")
  local on_path = vim.fn.exepath("reviewkit")
  if on_path ~= "" then
    table.insert(candidates, on_path)
  end
  for _, candidate in ipairs(candidates) do
    if vim.fn.executable(candidate) == 1 then
      M._resolved = candidate
      return candidate
    end
  end
  return nil, make_error("E_INTERNAL", "reviewkit binary not found", "run :ReviewInstall or set cli.path")
end

function M.reset()
  M._resolved = nil
  M._compatible = nil
end

local function tool_env()
  local tools = config.get().tools
  local env = {}
  if tools.git then
    env.REVIEWKIT_GIT = tools.git
  end
  if tools.gh then
    env.REVIEWKIT_GH = tools.gh
  end
  if tools.herdr then
    env.REVIEWKIT_HERDR = tools.herdr
  end
  return env
end

---@class reviewkit.Envelope
---@field ok boolean
---@field data any
---@field session? reviewkit.Session
---@field error? reviewkit.Error

--- Non-envelope output (a panic, a wrong binary) keeps its raw text in the
--- error rather than a bare parse failure, so the user can see what spoke.
---@param res vim.SystemCompleted
---@return reviewkit.Envelope? envelope
---@return reviewkit.CliError? err
local function decode(res)
  local stdout = res.stdout or ""
  local ok, parsed = pcall(vim.json.decode, stdout, { luanil = { object = true, array = true } })
  if not ok or type(parsed) ~= "table" or parsed.ok == nil then
    local snippet = vim.trim(stdout ~= "" and stdout or (res.stderr or ""))
    return nil,
      make_error("E_INTERNAL", ("reviewkit exited with %d and no envelope: %s"):format(res.code, snippet:sub(1, 400)))
  end
  if parsed.ok then
    return parsed
  end
  local err = parsed.error or make_error("E_INTERNAL", "envelope without error")
  err.details = err.details or {}
  return nil, err
end

--- The callback runs on the main loop, so it may touch buffers and windows.
---@param args string[]
---@param opts? { stdin?: string|table, cwd?: string, timeout?: integer }
---@param cb fun(err: reviewkit.CliError?, envelope: reviewkit.Envelope?)
function M.run(args, opts, cb)
  opts = opts or {}
  local bin, err = M.resolve()
  if not bin then
    return vim.schedule(function()
      cb(err)
    end)
  end
  local argv = { bin }
  local state_dir = config.get().cli.state_dir
  if state_dir then
    vim.list_extend(argv, { "--state-dir", state_dir })
  end
  if opts.cwd then
    vim.list_extend(argv, { "--cwd", opts.cwd })
  end
  vim.list_extend(argv, args)
  local stdin = opts.stdin
  if type(stdin) == "table" then
    stdin = vim.json.encode(stdin)
  end
  vim.system(argv, {
    text = true,
    stdin = stdin,
    env = tool_env(),
    timeout = opts.timeout,
  }, function(res)
    vim.schedule(function()
      local envelope, decode_err = decode(res)
      if decode_err then
        log.add("error", table.concat(args, " ") .. " -> " .. decode_err.code .. ": " .. decode_err.message)
        return cb(decode_err)
      end
      log.add("info", table.concat(args, " ") .. " -> ok")
      cb(nil, envelope)
    end)
  end)
end

--- Blocks the editor: keep the timeout short where the user is waiting on it.
---@param args string[]
---@param opts? { stdin?: string|table, cwd?: string, timeout?: integer }
---@return reviewkit.Envelope? envelope
---@return reviewkit.CliError? err
function M.run_sync(args, opts)
  opts = opts or {}
  local bin, err = M.resolve()
  if not bin then
    return nil, err
  end
  local argv = { bin }
  local state_dir = config.get().cli.state_dir
  if state_dir then
    vim.list_extend(argv, { "--state-dir", state_dir })
  end
  if opts.cwd then
    vim.list_extend(argv, { "--cwd", opts.cwd })
  end
  vim.list_extend(argv, args)
  local stdin = opts.stdin
  if type(stdin) == "table" then
    stdin = vim.json.encode(stdin)
  end
  local res = vim
    .system(argv, {
      text = true,
      stdin = stdin,
      env = tool_env(),
      timeout = opts.timeout or 30000,
    })
    :wait()
  return decode(res)
end

--- Why the CLI cannot serve this plugin, or nil. The revision must be at
--- least ours: an older CLI rejects (or once ignored) inputs we send.
---@param info? { schema_version?: integer, schema_revision?: integer }
---@return string?
function M.contract_mismatch(info)
  local found = info and info.schema_version
  if found ~= types.SCHEMA_VERSION then
    return ("reviewkit speaks schema_version %s, this plugin expects %s"):format(
      tostring(found),
      tostring(types.SCHEMA_VERSION)
    )
  end
  local revision = (info and info.schema_revision) or 0
  if revision < types.SCHEMA_REVISION then
    return ("reviewkit is at contract revision %d, this plugin needs %d or later"):format(
      revision,
      types.SCHEMA_REVISION
    )
  end
  return nil
end

--- The contract version is checked once per session; a mismatch stops every
--- later call instead of failing on some field halfway through a review.
---@param cb fun(err: reviewkit.CliError?)
function M.ensure_compatible(cb)
  if M._compatible == true then
    return vim.schedule(function()
      cb(nil)
    end)
  end
  M.run({ "version" }, {}, function(err, envelope)
    if err then
      return cb(err)
    end
    local reason = M.contract_mismatch(envelope and envelope.data)
    if reason then
      return cb(
        make_error("E_SCHEMA_VERSION", reason, "update the binary with :ReviewInstall (see :checkhealth reviewkit)")
      )
    end
    M._compatible = true
    cb(nil)
  end)
end

--- Waits between attempts when another process holds the session lock;
--- the CLI itself only waits briefly so the editor never hangs on it.
M.LOCK_RETRY_MS = { 100, 250, 500, 1000, 2000 }

---@param args string[]
---@param opts? { stdin?: string|table, cwd?: string, timeout?: integer }
---@param cb fun(err: reviewkit.CliError?, envelope: reviewkit.Envelope?)
function M.call(args, opts, cb)
  M.ensure_compatible(function(err)
    if err then
      return cb(err)
    end
    local attempt = 0
    local function try()
      M.run(args, opts, function(run_err, envelope)
        attempt = attempt + 1
        local delay = M.LOCK_RETRY_MS[attempt]
        if run_err and run_err.code == "E_LOCKED" and delay then
          vim.defer_fn(try, delay)
        else
          cb(run_err, envelope)
        end
      end)
    end
    try()
  end)
end

return M
