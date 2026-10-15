-- nvim --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua [file]
local target = arg[1]
if target then
  MiniTest.run_file(target)
else
  MiniTest.run()
end
