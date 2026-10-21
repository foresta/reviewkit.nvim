--- A Markdown fence longer than any run of backticks in the lines, so a
--- code block inside does not close it early.
---@param lines string[]
---@return string
return function(lines)
  local longest = 2
  for _, line in ipairs(lines) do
    for run in line:gmatch("`+") do
      longest = math.max(longest, #run)
    end
  end
  return string.rep("`", longest + 1)
end
