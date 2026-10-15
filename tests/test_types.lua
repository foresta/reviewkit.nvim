local helpers = require("tests.helpers")
local eq = MiniTest.expect.equality

local T = MiniTest.new_set()

T["types.lua matches the binary's schema"] = function()
  local bin = vim.env.REVIEWKIT_REAL_BIN
  if not bin or bin == "" then
    MiniTest.skip("REVIEWKIT_REAL_BIN is not set")
  end
  local res = vim
    .system({ vim.v.progpath, "-l", helpers.root .. "/scripts/gen-types.lua", "--check", "--bin", bin }, { text = true })
    :wait()
  if res.code ~= 0 then
    error(res.stderr)
  end
  eq(res.code, 0)
end

return T
