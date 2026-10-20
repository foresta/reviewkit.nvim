-- nvim --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua [file ...]
local files = {}
for i = 1, #arg do
  table.insert(files, arg[i])
end
if #files == 0 then
  MiniTest.run()
else
  MiniTest.run({
    collect = {
      find_files = function()
        return files
      end,
    },
  })
end
