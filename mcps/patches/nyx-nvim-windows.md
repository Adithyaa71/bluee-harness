# nyx-nvim on Windows — what had to change, and what still needs changing

Adithya's `nyx-nvim` is written for Linux. Getting its tool layer to answer at
all on this machine needed four config edits and two library shims. None of them
touch the clone in `tools/nyx-nvim` — it stays exactly as he pulls it.

## Config, patched in a copy

The config is copied to `%LOCALAPPDATA%/nyx` and used via `NVIM_APPNAME=nyx`, so
neither the clone nor any existing Neovim config is disturbed. Applied by
`mcps/nvim-lsp` when it starts the shared Neovim.

| File | Upstream | Why it fails here |
|---|---|---|
| `lua/util/paths.lua` | `M.root = "/mnt/Obsidian/nvim"` | an NVMe mount that only exists on his Linux box; every runtime path under it fails |
| `lua/util/paths.lua` | `M.zk_notebook = "/mnt/Obsidian/zk"` | same |
| `lua/core/options.lua` | `serverstart('/tmp/nvim-<pid>.sock')` | a hard error on Windows, and it aborts `init.lua` **before any plugin loads** — which is why `:Lazy` did not even exist |

Also needed: `npm i -g tree-sitter-cli`. Without it nvim-treesitter downloads
every parser and fails to compile all of them.

## Library shims, applied at import

In `mcps/nyx-tools/server.py`, not in his repo:

- **`get_socket()`** honours `NVIM_SOCKET_PATH` first — real foresight — but
  then guards it with `os.path.exists()`, which is false for a Windows named
  pipe, so it falls through to a `.sh` helper that cannot run here.
- **The package name `mcp` collides with the MCP SDK's.** His modules do
  `from mcp.mcp_client import ...`, so the name has to mean his package while
  they import and the SDK's afterwards.

## Still broken — these are fixes to nyx-nvim itself

Verified against Neovim 0.12.5:

- **`tools/internal/_treesitter.py`** requires `nvim-treesitter.ts_utils`,
  which was removed in nvim-treesitter v1.0 / `main`. Anything routing through
  it errors: `current_context`, `current_location`.
- **`tools/internal/_lsp.py`** calls `vim.lsp.util.make_position_params()` with
  no arguments. Neovim 0.11 deprecated that in favour of an explicit position
  encoding, so the request errors inside his `pcall` and the function returns
  `{}` — silently, which is why these look like "no results" rather than a
  failure: `get_file_symbols`, `find_symbol_in_workspace`, `find_todos`.

Working today: `open_file`, `list_open_files`, `goto_line`, `search_in_file`,
`diagnostics_summary`, `module_structure`, `file_diagnostics`, and the
`refactor` writers.
