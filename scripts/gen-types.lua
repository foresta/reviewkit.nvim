-- Generates lua/reviewkit/types.lua from `reviewkit schema`.
--
--   nvim -l scripts/gen-types.lua [--check] [--bin PATH]
--
-- --check exits non-zero when the committed file differs from what the
-- binary describes, so CI catches a contract change that forgot the types.

local function fail(msg)
  io.stderr:write("gen-types: " .. msg .. "\n")
  os.exit(1)
end

local args = { check = false, bin = os.getenv("REVIEWKIT_BIN") }
local i = 1
while i <= #arg do
  if arg[i] == "--check" then
    args.check = true
  elseif arg[i] == "--bin" then
    i = i + 1
    args.bin = arg[i]
  else
    fail("unknown argument: " .. tostring(arg[i]))
  end
  i = i + 1
end

local script_dir = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
local root = vim.fn.fnamemodify(script_dir, ":h")
local out_path = root .. "/lua/reviewkit/types.lua"

if not args.bin then
  local candidates = {}
  -- cargo builds there instead when it is set; the fixed paths below would
  -- then hold a stale binary from before it was set.
  local target_dir = os.getenv("CARGO_TARGET_DIR")
  if target_dir and target_dir ~= "" then
    target_dir = target_dir:sub(1, 1) == "/" and target_dir or root .. "/" .. target_dir
    table.insert(candidates, target_dir .. "/debug/reviewkit")
    table.insert(candidates, target_dir .. "/release/reviewkit")
  end
  vim.list_extend(candidates, {
    root .. "/cli/target/debug/reviewkit",
    root .. "/cli/target/release/reviewkit",
    root .. "/target/debug/reviewkit",
    root .. "/target/release/reviewkit",
  })
  for _, candidate in ipairs(candidates) do
    if vim.fn.executable(candidate) == 1 then
      args.bin = candidate
      break
    end
  end
end
if not args.bin then
  fail("no reviewkit binary found; pass --bin or set REVIEWKIT_BIN")
end

local res = vim.system({ args.bin, "schema" }, { text = true }):wait()
if res.code ~= 0 then
  fail("reviewkit schema failed: " .. (res.stderr or ""))
end
local envelope = vim.json.decode(res.stdout)
if not envelope.ok then
  fail("reviewkit schema returned an error: " .. vim.inspect(envelope.error))
end
local schema_version = envelope.data.schema_version
local defs = envelope.data.schema["$defs"]

local function ref_name(ref)
  return "reviewkit." .. ref:gsub("^#/%$defs/", "")
end

local lua_type

local function is_nullable(prop)
  if type(prop) ~= "table" then
    return false
  end
  if type(prop.type) == "table" then
    return vim.tbl_contains(prop.type, "null")
  end
  if prop.anyOf then
    for _, alt in ipairs(prop.anyOf) do
      if alt.type == "null" then
        return true
      end
    end
  end
  return false
end

local function non_null(prop)
  if type(prop.type) == "table" then
    local copy = vim.deepcopy(prop)
    copy.type = vim.tbl_filter(function(t)
      return t ~= "null"
    end, prop.type)
    if #copy.type == 1 then
      copy.type = copy.type[1]
    end
    return copy
  end
  if prop.anyOf then
    for _, alt in ipairs(prop.anyOf) do
      if alt.type ~= "null" then
        return alt
      end
    end
  end
  return prop
end

lua_type = function(prop)
  -- `true` is the JSON Schema for "anything".
  if type(prop) ~= "table" then
    return "any"
  end
  if prop["$ref"] then
    return ref_name(prop["$ref"])
  end
  if prop.enum then
    local parts = {}
    for _, v in ipairs(prop.enum) do
      table.insert(parts, string.format("%q", v))
    end
    return table.concat(parts, "|")
  end
  if prop.const then
    return string.format("%q", prop.const)
  end
  if prop.anyOf then
    return lua_type(non_null(prop))
  end
  local t = prop.type
  if type(t) == "table" then
    return lua_type(non_null(prop))
  end
  if t == "string" then
    return "string"
  elseif t == "integer" then
    return "integer"
  elseif t == "number" then
    return "number"
  elseif t == "boolean" then
    return "boolean"
  elseif t == "array" then
    if prop.prefixItems then
      local parts = {}
      for _, item in ipairs(prop.prefixItems) do
        table.insert(parts, lua_type(item))
      end
      return "[" .. table.concat(parts, ", ") .. "]"
    end
    if prop.minItems and prop.minItems == prop.maxItems and prop.minItems <= 4 then
      local item = lua_type(prop.items or {})
      local parts = {}
      for _ = 1, prop.minItems do
        table.insert(parts, item)
      end
      return "[" .. table.concat(parts, ", ") .. "]"
    end
    return lua_type(prop.items or {}) .. "[]"
  elseif t == "object" then
    if prop.additionalProperties and type(prop.additionalProperties) == "table" then
      return "table<string, " .. lua_type(prop.additionalProperties) .. ">"
    end
    return "table"
  end
  -- Untyped (serde_json::Value) fields documented as objects are tables.
  if prop.description and prop.description:lower():find("object", 1, true) then
    return "table"
  end
  return "any"
end

local function doc_lines(text, indent)
  local lines = {}
  if not text then
    return lines
  end
  for line in text:gmatch("[^\n]+") do
    table.insert(lines, (indent or "") .. "--- " .. line)
  end
  return lines
end

--- Only patch types carry Option<Option<T>> fields (omitted = unchanged,
--- null = cleared); the schema cannot tell them from plain nullable output
--- fields, so the naming convention decides.
local function is_patch(name)
  return name:match("Patch$") ~= nil
end

local function emit_class(out, name, def, extra_fields)
  vim.list_extend(out, doc_lines(def.description))
  table.insert(out, "---@class " .. name)
  local required = {}
  for _, r in ipairs(def.required or {}) do
    required[r] = true
  end
  local names = vim.tbl_keys(def.properties or {})
  table.sort(names)
  for _, field in ipairs(names) do
    local prop = def.properties[field]
    local optional = (not required[field]) or is_nullable(prop)
    local comment = prop.description and (" " .. prop.description:gsub("\n", " ")) or ""
    local field_type = lua_type(prop)
    -- An optional, nullable field without a default is serde's
    -- Option<Option<T>>: sending null clears it, which from Lua is vim.NIL.
    if is_patch(name) and not required[field] and is_nullable(prop) and prop.default == nil then
      field_type = field_type .. "|vim.NIL"
    end
    table.insert(out, string.format("---@field %s%s %s%s", field, optional and "?" or "", field_type, comment))
  end
  for _, line in ipairs(extra_fields or {}) do
    table.insert(out, line)
  end
  table.insert(out, "")
end

local out = {
  "-- Generated by scripts/gen-types.lua from `reviewkit schema`. Do not edit.",
  "-- Regenerate: nvim -l scripts/gen-types.lua",
  "",
  "local M = {}",
  "",
  "M.SCHEMA_VERSION = " .. tostring(schema_version),
  "M.SCHEMA_REVISION = " .. tostring(envelope.data.schema_revision or 0),
  "",
}

local names = vim.tbl_keys(defs)
table.sort(names)
for _, name in ipairs(names) do
  local def = defs[name]
  local lua_name = "reviewkit." .. name
  if def.enum then
    vim.list_extend(out, doc_lines(def.description))
    table.insert(out, "---@alias " .. lua_name .. " " .. lua_type(def))
    table.insert(out, "")
  elseif def.oneOf then
    local variants = {}
    for _, variant in ipairs(def.oneOf) do
      local tag = variant.properties and variant.properties.kind and variant.properties.kind.const
      local variant_name = lua_name .. "." .. (tag and (tag:sub(1, 1):upper() .. tag:sub(2)) or "Variant")
      table.insert(variants, variant_name)
      emit_class(out, variant_name, variant)
    end
    vim.list_extend(out, doc_lines(def.description))
    table.insert(out, "---@alias " .. lua_name .. " " .. table.concat(variants, "|"))
    table.insert(out, "")
  else
    emit_class(out, lua_name, def)
  end
end

table.insert(out, "return M")
table.insert(out, "")
local text = table.concat(out, "\n")

if args.check then
  local f = io.open(out_path, "r")
  local current = f and f:read("*a") or ""
  if f then
    f:close()
  end
  if current ~= text then
    fail("lua/reviewkit/types.lua is out of date; run `nvim -l scripts/gen-types.lua`")
  end
  print("types.lua is up to date")
  return
end

local f = assert(io.open(out_path, "w"))
f:write(text)
f:close()
print("wrote " .. out_path)
