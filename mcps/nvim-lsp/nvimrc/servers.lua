-- Language server registration, shared by both entry points.
--
-- Loaded by init.lua (the minimal standalone config) AND by attach.lua (which
-- runs on top of Adithya's nyx-nvim config). His config configures servers
-- through mason and lspconfig, but on this machine nothing attached - the
-- buffer came back with zero clients and every LSP-backed tool returned an
-- empty list that read as "no symbols". Registering here as well means the
-- language server is attached whichever config is in play.

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

-- Readable from the query script, so an empty answer can distinguish 'no
-- language server installed for this filetype' from 'the server had nothing
-- to say'.
_G.BLUEE_LSP_ENABLED = enabled
