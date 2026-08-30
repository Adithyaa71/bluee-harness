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
