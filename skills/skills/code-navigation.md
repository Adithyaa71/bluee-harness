# Code navigation

> Find, understand and safely edit code with Neovim's language servers (nvim_lsp) and the nyx editor tools.

- category: skills
- tools: nvim_lsp__lsp_status, nvim_lsp__find_references, nvim_lsp__goto_definition, nvim_lsp__hover, nvim_lsp__document_symbols, nyx_tools__nyx_status, nyx_tools__search_project, nyx_tools__find_symbol_in_workspace, nyx_tools__file_symbols, nyx_tools__file_diagnostics, nyx_tools__open_file, nyx_tools__safe_edit
- triggers: find references, who calls, goto definition, refactor, rust-analyzer, codebase
- created: 2026-10-07T05:30:00+00:00
- updated: 2026-10-07T05:30:00+00:00

---

1. `nvim_lsp__lsp_status` - which language servers actually run.
2. Locate: `nyx_tools__search_project` (text) or `nyx_tools__find_symbol_in_workspace` (symbol).
3. Understand: `nvim_lsp__hover`, `nvim_lsp__goto_definition`, `nvim_lsp__document_symbols`.
4. Impact: `nvim_lsp__find_references` - resolved by the language server, so `system::run`
   and `reduce::run` are not confused the way grep confuses them.
5. The first call can take ~30s while rust-analyzer indexes; an empty answer flagged
   `indexing: true` means "ask again shortly", not "nothing found".
6. Edit only with `nyx_tools__safe_edit`, then `nyx_tools__file_diagnostics` to confirm
   it still compiles. Only edit inside folders the user granted.
