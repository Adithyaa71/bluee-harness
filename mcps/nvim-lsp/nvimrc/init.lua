-- Minimal Neovim config for code intelligence (CLAUDE.md §4h-b).
--
-- Deliberately no plugin manager. Neovim 0.11+ has a built-in LSP client and
-- `vim.lsp.config` / `vim.lsp.enable`, so asking a language server for
-- references needs no lazy.nvim, no mason, and no network. That matters here:
-- this config is started by a background process to answer one question, and
-- every plugin it loads is latency on every query.
--
-- Used when Adithya's nyx-nvim config is not installed. When it is, the server
-- starts Neovim with his config instead and loads `attach.lua` on top - his
-- config brings the plugins his tool layer needs, and attach.lua adds the same
-- language servers and the same query hook.

vim.opt.swapfile = false
vim.opt.shadafile = 'NONE'
vim.opt.backup = false
vim.opt.writebackup = false

local here = debug.getinfo(1, 'S').source:sub(2):gsub('[^/\\]+$', '')
dofile(here .. 'servers.lua')

-- ---------------------------------------------------------------------------
-- The query entry point.
--
-- Arguments go in via a file and the answer comes out via a file, rather than
-- being threaded through `--remote-expr` as a quoted string. Quoting JSON
-- through a shell, into Vimscript, into Lua is three escaping layers and every
-- one of them is a bug waiting to happen; two temp files are not.
local run = dofile(here .. 'query.lua')

function _G.bluee_query_file(inpath, outpath)
  local ok, res = pcall(function()
    local raw = table.concat(vim.fn.readfile(inpath), '\n')
    return run(vim.json.decode(raw))
  end)
  if not ok then
    res = { error = tostring(res) }
  end
  vim.fn.writefile({ vim.json.encode(res or { error = 'no result' }) }, outpath)
  return 'ok'
end

-- Something to poll for: the pipe answering proves Neovim is up, and this
-- proves the config finished loading.
_G.BLUEE_READY = true
