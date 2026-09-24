# Streaming into a Neovim split

Requires Neovim 0.10+ and `mdtrans` on Neovim's `PATH`. Add this mapping to your `init.lua` or keymaps file; no plugin or changes to `mdtrans` configuration are needed.

`<leader>mt` translates the **saved file on disk**, opens a vertical split immediately, and appends stdout fragments as they arrive. The original buffer is untouched. Progress and failures use virtual lines, so they remain visible without becoming part of the Markdown or being written by `:w`.

```lua
vim.keymap.set("n", "<leader>mt", function()
  local path = vim.api.nvim_buf_get_name(0)
  if vim.fn.executable("mdtrans") == 0 then
    vim.notify("mdtrans is not in PATH", vim.log.levels.ERROR)
    return
  end
  if vim.fn.filereadable(path) == 0 or vim.bo.modified then
    vim.notify("Save the current file before translating", vim.log.levels.WARN)
    return
  end

  vim.cmd("botright vnew")
  local buf = vim.api.nvim_get_current_buf()
  local ns = vim.api.nvim_create_namespace("mdtrans")
  local process, read_error
  local done, has_text, ends_with_newline = false, false, false
  vim.bo[buf].filetype = "markdown"
  vim.bo[buf].modifiable = false

  local function alive()
    return vim.api.nvim_buf_is_valid(buf) and vim.api.nvim_buf_is_loaded(buf)
  end

  local function banner(text, highlight)
    if not alive() then return end
    vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
    local lines = {}
    for _, line in ipairs(vim.split(text, "\n", { plain = true, trimempty = true })) do
      lines[#lines + 1] = { { line, highlight } }
    end
    vim.api.nvim_buf_set_extmark(buf, ns, 0, 0, {
      virt_lines = lines,
      virt_lines_above = true,
    })
  end

  banner("Translating… waiting for the provider.", "Comment")
  vim.api.nvim_create_autocmd("BufWipeout", {
    buffer = buf,
    once = true,
    callback = function()
      if process and not done then process:kill(15) end
    end,
  })

  local function finish(result)
    done = true
    if not alive() then return end
    vim.bo[buf].modifiable = true
    -- Remove the placeholder line created by a final newline, preserving EOL.
    if ends_with_newline then
      vim.api.nvim_buf_set_lines(buf, -2, -1, false, {})
    end
    vim.bo[buf].endofline = ends_with_newline
    vim.bo[buf].fixendofline = false
    if result.code ~= 0 or result.signal ~= 0 or read_error then
      local title = has_text
        and "Translation interrupted — this output is incomplete."
        or "Translation failed."
      banner(title .. "\n" .. (read_error or result.stderr or ""), "ErrorMsg")
    else
      banner("Translation complete. Use :w <new-path> to save.", "Comment")
    end
  end

  local ok, job = pcall(vim.system,
    { "mdtrans", path, "--stdout" },
    {
      text = true,
      timeout = 130000,
      stdout = vim.schedule_wrap(function(err, data)
        read_error = read_error or err
        if not alive() or not data or data == "" then return end
        local lines = vim.split(data, "\n", { plain = true })
        local last = vim.api.nvim_buf_get_lines(buf, -2, -1, false)[1] or ""
        lines[1] = last .. lines[1]
        vim.bo[buf].modifiable = true
        vim.api.nvim_buf_set_lines(buf, -2, -1, false, lines)
        vim.bo[buf].modifiable = false
        has_text = true
        ends_with_newline = data:sub(-1) == "\n"
        banner("Translating… output is incomplete until completion.", "Comment")
      end),
    },
    vim.schedule_wrap(finish)
  )
  if ok then
    process = job
  else
    finish({ code = -1, signal = 0, stderr = tostring(job) })
  end
end, { desc = "Stream Markdown translation into a vertical split" })
```

- The mapping uses the configured language. Add `"--lang", "en"` to the command arguments to override it.
- The split is not editable until the process finishes. A failed translation leaves partial text available for inspection, with an `ErrorMsg` banner (red in standard themes). Do not treat it as complete.
- Save a successful translation with `:w README-en-translate.md`, never to the source file. This is a normal unnamed buffer, not a managed temporary file.
- `:bwipeout!` on the output buffer cancels its subprocess. Cancelling does not guarantee that the provider stops processing or billing the request immediately.
- Requests have a 120-second timeout in `mdtrans`; the mapping adds a 130-second process timeout as a safeguard. A 503 can still happen before any text arrives.
- `--stdout` is explicit for compatibility. Do not add `--no-stream` if you want incremental text; that flag intentionally buffers the response. Neither mode requires `$EDITOR`.
- For errors that occurred before a split was opened, check `:messages` (or your notification plugin's history). Running `mdtrans preview` instead launches another editor process, not a split in the current instance.
