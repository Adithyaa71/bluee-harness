-- Minimal Neovim config for code intelligence (CLAUDE.md §4h-b).
--
-- Deliberately no plugin manager. Neovim 0.11+ has a built-in LSP client and
-- `vim.lsp.config` / `vim.lsp.enable`, so asking a language server for
-- references needs no lazy.nvim, no mason, and no network. That matters here:
-- this config is started by a background process to answer one question, and
-- every plugin it loads is latency on every query.
--
-- Adithya's own nyx-nvim config is the one he edits and uses; this one exists
-- so the harness never depends on that config being present, correct for
-- Windows (its paths.lua hardcodes /mnt/Obsidian/nvim), or finished bootstrapping.

vim.opt.swapfile = false
vim.opt.shadafile = 'NONE'
vim.opt.backup = false
vim.opt.writebackup = false

-- Language servers, registered only if the binary is actually there. A server
-- configured but missing produces a confusing "no client attached" rather than
-- an honest "that language server is not installed".
local servers = {
  rust_analyzer = {
    cmd = { 'rust-analyzer' },
    filetypes = { 'rust' },
    root_markers = { 'Cargo.toml', 'rust-project.json', '.git' },
    -- rust-analyzer will tell us when it has actually finished loading the
    -- workspace, if asked. Without this the only readiness signal is progress
    -- reports, which go quiet between phases - and asking during a gap returns
    -- an empty list that reads as "nothing calls this". That false zero is far
    -- worse than a wait, so take the real signal.
    capabilities = {
      experimental = { serverStatusNotification = true },
    },
    handlers = {
      ['experimental/serverStatus'] = function(_, res)
        _G.BLUEE_RA_READY = res and res.quiescent or false
        return true
      end,
    },
    settings = {
      ['rust-analyzer'] = {
        -- The harness asks structural questions; running clippy on every query
        -- would triple the wait for nothing.
        checkOnSave = false,
        cargo = { allFeatures = false },
      },
    },
  },
  pyright = {
    cmd = { 'pyright-langserver', '--stdio' },
    filetypes = { 'python' },
    root_markers = { 'pyproject.toml', 'setup.py', 'requirements.txt', '.git' },
  },
  basedpyright = {
    cmd = { 'basedpyright-langserver', '--stdio' },
    filetypes = { 'python' },
    root_markers = { 'pyproject.toml', 'setup.py', '.git' },
  },
  ts_ls = {
    cmd = { 'typescript-language-server', '--stdio' },
    filetypes = { 'javascript', 'typescript', 'javascriptreact', 'typescriptreact' },
    root_markers = { 'package.json', 'tsconfig.json', '.git' },
  },
  lua_ls = {
    cmd = { 'lua-language-server' },
    filetypes = { 'lua' },
    root_markers = { '.luarc.json', '.git' },
  },
  clangd = {
    cmd = { 'clangd' },
    filetypes = { 'c', 'cpp' },
    root_markers = { 'compile_commands.json', '.git' },
  },
  gopls = {
    cmd = { 'gopls' },
    filetypes = { 'go' },
    root_markers = { 'go.mod', '.git' },
  },
}

local enabled = {}
for name, conf in pairs(servers) do
  if vim.fn.executable(conf.cmd[1]) == 1 then
    vim.lsp.config(name, conf)
    table.insert(enabled, name)
  end
end
if #enabled > 0 then
  vim.lsp.enable(enabled)
end

-- Readable from the query script, so "no answer" can distinguish "no language
-- server installed for this filetype" from "the server had nothing to say".
_G.BLUEE_LSP_ENABLED = enabled

-- ---------------------------------------------------------------------------
-- The query entry point, for the persistent-server mode.
--
-- Arguments go in via a file and the answer comes out via a file, rather than
-- being threaded through `--remote-expr` as a quoted string. Quoting JSON
-- through a shell, into Vimscript, into Lua, is three escaping layers and every
-- one of them is a bug waiting to happen; two temp files are not.
local here = debug.getinfo(1, 'S').source:sub(2):gsub('[^/\\]+$', '')
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
