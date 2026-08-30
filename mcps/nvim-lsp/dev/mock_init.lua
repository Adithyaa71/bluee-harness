-- Test-only config: registers the stdlib mock language server for Rust files,
-- so the whole path can be exercised without a real server installed.
vim.opt.swapfile = false
vim.opt.shadafile = 'NONE'

local py = vim.fn.getcwd() .. '/.venv/Scripts/python.exe'
if vim.fn.executable(py) == 0 then py = 'python' end

vim.lsp.config('mock_lsp', {
  cmd = { py, vim.fn.getcwd() .. '/mcps/nvim-lsp/dev/mock_lsp.py' },
  filetypes = { 'rust' },
  root_markers = { 'Cargo.toml', '.git' },
})
vim.lsp.enable({ 'mock_lsp' })
_G.BLUEE_LSP_ENABLED = { 'mock_lsp' }
