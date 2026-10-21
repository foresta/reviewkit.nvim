local M = {}

local api = vim.api
local counter = 0

---@class reviewkit.InputOpts
---@field title string
---@field lines? string[]
---@field footer? string
---@field on_save fun(body: string)
---@field on_cancel? fun()
---@field start_insert? boolean
---@field modified? boolean

--- A small floating buffer: `:w` (or <C-s>) saves and closes, `q` discards.
--- It is `acwrite` so `:w` is caught by BufWriteCmd and nothing touches disk.
---@param opts reviewkit.InputOpts
---@return integer buf
---@return integer win
function M.open(opts)
  counter = counter + 1
  local buf = api.nvim_create_buf(false, true)
  api.nvim_buf_set_name(buf, ("reviewkit://input/%d"):format(counter))
  vim.bo[buf].buftype = "acwrite"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  api.nvim_buf_set_lines(buf, 0, -1, false, opts.lines or {})
  -- Text brought back after a failed save is unsaved work: `q` must ask.
  vim.bo[buf].modified = opts.modified == true
  vim.bo[buf].filetype = "markdown"

  local width = math.min(90, vim.o.columns - 8)
  local height = math.min(14, vim.o.lines - 8)
  -- A window that cannot open (E11 and the like) would leave the buffer
  -- behind, hidden and named.
  local opened, win = pcall(api.nvim_open_win, buf, true, {
    relative = "editor",
    row = math.floor((vim.o.lines - height) / 2) - 1,
    col = math.floor((vim.o.columns - width) / 2),
    width = width,
    height = height,
    style = "minimal",
    border = "rounded",
    title = " " .. opts.title .. " ",
    footer = opts.footer or " :w save · q cancel ",
    footer_pos = "right",
  })
  if not opened then
    pcall(api.nvim_buf_delete, buf, { force = true })
    error(win, 0)
  end
  vim.wo[win].wrap = true

  local saved = false
  local function close()
    pcall(api.nvim_win_close, win, true)
  end
  api.nvim_create_autocmd("BufWriteCmd", {
    buffer = buf,
    callback = function()
      -- The window stays until the scheduled close; a second `:w` or <C-s>
      -- typed ahead would save the same text twice.
      if saved then
        vim.bo[buf].modified = false
        return
      end
      local lines = api.nvim_buf_get_lines(buf, 0, -1, false)
      while #lines > 0 and vim.trim(lines[#lines]) == "" do
        table.remove(lines)
      end
      vim.bo[buf].modified = false
      saved = true
      vim.cmd("stopinsert")
      -- Later, not now: the quit half of `:wq` / `:x` / `ZZ` acts on the
      -- current window, which must still be this one.
      vim.schedule(close)
      opts.on_save(table.concat(lines, "\n"))
    end,
  })
  api.nvim_create_autocmd("BufWipeout", {
    buffer = buf,
    once = true,
    callback = function()
      if not saved and opts.on_cancel then
        opts.on_cancel()
      end
    end,
  })
  vim.keymap.set("n", "q", function()
    if vim.bo[buf].modified and vim.fn.confirm("Discard this text?", "&Discard\n&Keep", 2) ~= 1 then
      return
    end
    close()
  end, { buffer = buf, nowait = true })
  vim.keymap.set({ "n", "i" }, "<C-s>", "<Cmd>write<CR>", { buffer = buf })
  if opts.start_insert or #(opts.lines or {}) == 0 then
    vim.cmd("startinsert")
  end
  return buf, win
end

--- The window closes on `:w` before the CLI answers; a save refused then
--- (a submit holding the session, a rejected input) would lose the text.
---@param opts table reviewkit.InputOpts without on_save
---@param save fun(text: string, done: fun(err?: reviewkit.Error))
function M.open_keeping(opts, save)
  M.open(vim.tbl_extend("force", opts, {
    on_save = function(text)
      save(text, function(err)
        if not err then
          return
        end
        require("reviewkit.notify").error(err)
        local again =
          vim.tbl_extend("force", opts, { lines = vim.split(text, "\n", { plain = true }), modified = true })
        local function reopen()
          -- No window can open from the command-line window (E11).
          if vim.fn.getcmdwintype() ~= "" then
            api.nvim_create_autocmd("CmdwinLeave", { once = true, callback = vim.schedule_wrap(reopen) })
            return
          end
          if not pcall(M.open_keeping, again, save) then
            vim.fn.setreg('"', text)
            vim.notify("[reviewkit] the text is kept in the unnamed register", vim.log.levels.WARN)
          end
        end
        -- After the window that was saved from has closed (its close is
        -- scheduled too), or the new one would lose the focus to it.
        vim.schedule(reopen)
      end)
    end,
  }) --[[@as reviewkit.InputOpts]])
end

return M
