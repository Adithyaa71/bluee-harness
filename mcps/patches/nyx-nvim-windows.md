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

## Fixed in the clone — three bugs, one of which caused all the others

Patched version-tolerantly, so his Linux setup keeps working.

**1. `mcp/mcp_client.py` — `eval_lua` never evaluated anything.**
It called `nvim.exec_lua(expr)`. `exec_lua` runs a *chunk*, not an expression:
`1+1` does not parse as a chunk, and `(function() ... end)()` parses but
discards its return value. So **every** tool routed through `eval_lua` got
`None` back and returned an empty list. Reproduced directly:
`eval_lua("1+1")` raised `unexpected symbol near '1'` - and his own docstring
gives `eval_lua("vim.lsp.get_clients()")` as the example, which fails the same
way. It now prepends `return` and falls back to running the text as given for
callers that really do pass a multi-statement chunk.

This single bug is why `file_symbols`, `find_symbol_in_workspace` and
`find_todos` all looked like "no results". After the fix, `file_symbols`
returns 15 symbols for `src/roots.rs`.

**2. `tools/internal/_lsp.py` — `make_position_params()` with no arguments.**
Neovim 0.11 requires `(window, position_encoding)`; guessing the encoding was
the source of off-by-one columns on non-ASCII lines. Called bare it raises, and
the surrounding `pcall` turned that into an empty result. Now passes
`(0, 'utf-8')` with a fallback.

**3. `tools/internal/_treesitter.py` — `nvim-treesitter.ts_utils`.**
Removed in nvim-treesitter v1.0 / `main`. The node under the cursor has been in
core as `vim.treesitter.get_node()` since 0.9 - which this same file already
used elsewhere - so it now prefers core and keeps `ts_utils` as the fallback.

## Still limited — needs his plugin stack, not a code fix

`module_structure`'s function list and `find_todos` go through treesitter and
telescope. They need his plugins *and* a compiled parser for the language in
question.

Running the shared Neovim on his full config was tried and measured rather than
assumed: **44s cold instead of 28s, `file_symbols` regressed to 0, and the
treesitter tools still returned nothing** (no Rust parser built). So the minimal
config stays the default; `NVIM_LSP_USE_NYX=1` switches to his.

One characteristic worth knowing: his tools use a 2s LSP timeout and no
readiness wait, so they return empty against a cold rust-analyzer. Ask
`nvim_lsp` anything first - it waits on the server's own `serverStatus` signal -
and his tools are correct from then on.

## Superseded note



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
