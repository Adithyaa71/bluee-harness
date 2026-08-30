"""Neovim as a code-intelligence server (CLAUDE.md §4h-b).

Why Neovim and not more indexing of our own: `codemap.rs` reads what a file
*declares*. That answers "where does X live" and cannot answer "what calls X" or
"what breaks if I change this signature". Those need real resolution - scope,
imports, types - and writing that per language is a compiler front end per
language. An LSP already did it.

So this drives a headless Neovim over its msgpack-RPC socket, attaches the
language server the file's filetype configures, and asks *it*. Definitions and
references come back resolved rather than grepped, which is the entire point:
grep finds the word `run` in forty files, an LSP finds the seven that call this
`run`.

Token efficiency, which is the reason this matters for a harness: the answer to
"who calls this" is a dozen file:line pairs, not a dozen files. The model reads
what it needs after that, instead of being handed a repository.

Requirements, stated plainly because this server is useless without them:

  1. Neovim on PATH            winget install Neovim.Neovim
  2. pynvim in the venv        .venv/Scripts/pip install pynvim
  3. A language server for the language you care about, configured in the
     Neovim you already use (rust-analyzer, pyright, tsserver, ...).

With any of those missing every tool returns a readable `error` saying which,
rather than failing obscurely - the same choice SnareVec's proxy makes when its
daemon has idled out.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from mcp.server.mcpserver import MCPServer

server = MCPServer(
    name="nvim-lsp",
    instructions=(
        "Code intelligence through a headless Neovim and its language servers. "
        "Use find_references before changing a function - it answers 'what "
        "calls this' with resolved call sites rather than text matches. Use "
        "goto_definition to jump from a use to the declaration, and "
        "document_symbols for the shape of one file."
    ),
)

# How long to wait for a language server to attach and index. Cold rust-analyzer
# on a large tree is genuinely slow; better to wait than to answer "no
# references" because nothing had loaded yet.
ATTACH_TIMEOUT = float(os.environ.get("NVIM_LSP_TIMEOUT", "45"))

_nvim: Any = None
_socket: str | None = None
_proc: subprocess.Popen | None = None


def _missing() -> str | None:
    """Which prerequisite is absent, if any."""
    if shutil.which("nvim") is None:
        return (
            "Neovim is not on PATH. Install it with `winget install Neovim.Neovim` "
            "(or scoop/choco), then restart bluee."
        )
    try:
        import pynvim  # noqa: F401
    except ImportError:
        return (
            "pynvim is not installed. Run `.venv/Scripts/pip install pynvim` in the "
            "project folder, then restart bluee."
        )
    return None


def _connect() -> Any:
    """Start a headless Neovim once and keep it, or reuse the running one."""
    global _nvim, _socket, _proc
    if _nvim is not None:
        return _nvim

    import pynvim

    _socket = str(Path(tempfile.gettempdir()) / f"bluee-nvim-{os.getpid()}.sock")
    # --headless so there is no UI, --listen so we can talk to it. The user's
    # own init.lua is loaded on purpose: their LSP setup is the whole value
    # here, and a clean-slate nvim would have no language servers at all.
    _proc = subprocess.Popen(
        ["nvim", "--headless", "--listen", _socket],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    last: Exception | None = None
    for _ in range(60):
        time.sleep(0.25)
        try:
            _nvim = pynvim.attach("socket", path=_socket)
            return _nvim
        except Exception as e:  # noqa: BLE001 - socket not up yet
            last = e
    raise RuntimeError(f"Neovim started but never accepted a connection: {last}")


def _lua(code: str, *args: Any) -> Any:
    return _connect().exec_lua(code, *args)


def _open(path: str) -> dict[str, Any] | None:
    """Load a file and wait for a language server to attach to it."""
    p = Path(path)
    if not p.is_absolute():
        return {"error": f"give an absolute path, got {path!r}"}
    if not p.exists():
        return {"error": f"no such file: {path}"}

    nvim = _connect()
    nvim.command(f"edit {str(p).replace(chr(92), '/')}")

    deadline = time.time() + ATTACH_TIMEOUT
    while time.time() < deadline:
        attached = _lua("return #vim.lsp.get_clients({ bufnr = 0 })")
        if attached and int(attached) > 0:
            return None
        time.sleep(0.2)

    ft = nvim.command_output("set filetype?").strip()
    return {
        "error": (
            f"no language server attached to this file within {ATTACH_TIMEOUT:.0f}s "
            f"({ft}). The file opened fine - what is missing is an LSP configured "
            f"for that filetype in your Neovim config."
        )
    }


def _locations(method: str, path: str, line: int, col: int) -> dict[str, Any]:
    """Run one LSP request and return its locations as file:line pairs."""
    problem = _missing()
    if problem:
        return {"error": problem}
    try:
        bad = _open(path)
        if bad:
            return bad

        result = _lua(
            """
            local method, line, col, timeout = ...
            local params = vim.lsp.util.make_position_params(0, 'utf-8')
            params.position = { line = line - 1, character = col }
            if method == 'textDocument/references' then
              params.context = { includeDeclaration = false }
            end
            local out = {}
            local responses = vim.lsp.buf_request_sync(0, method, params, timeout)
            for _, resp in pairs(responses or {}) do
              local r = resp.result
              if r then
                if r.uri or r.targetUri then r = { r } end
                for _, loc in ipairs(r) do
                  local uri = loc.uri or loc.targetUri
                  local range = loc.range or loc.targetSelectionRange
                  table.insert(out, {
                    file = vim.uri_to_fname(uri),
                    line = range.start.line + 1,
                    col  = range.start.character + 1,
                  })
                end
              end
            end
            return out
            """,
            method,
            line,
            col,
            int(ATTACH_TIMEOUT * 1000),
        )

        hits = result or []
        # The text of each line, because a bare file:line tells the model
        # nothing and it would just have to read every file to find out.
        for h in hits:
            try:
                h["text"] = Path(h["file"]).read_text(
                    encoding="utf-8", errors="replace"
                ).splitlines()[h["line"] - 1].strip()[:200]
            except Exception:  # noqa: BLE001
                h["text"] = ""
        return {"count": len(hits), "locations": hits}
    except Exception as e:  # noqa: BLE001
        return {"error": f"{type(e).__name__}: {e}"}


@server.tool()
def lsp_status() -> dict[str, Any]:
    """Whether this server can actually answer anything, and what is missing.

    Call this first if a code question fails - it distinguishes "Neovim is not
    installed" from "no language server for this filetype" from a real error.
    """
    problem = _missing()
    if problem:
        return {"ready": False, "error": problem}
    try:
        nvim = _connect()
        return {
            "ready": True,
            "nvim": nvim.command_output("version").splitlines()[0],
            "socket": _socket,
            "note": "A language server still has to be configured for the filetype "
                    "you ask about; lsp_status only proves Neovim is reachable.",
        }
    except Exception as e:  # noqa: BLE001
        return {"ready": False, "error": f"{type(e).__name__}: {e}"}


@server.tool()
def find_references(path: str, line: int, col: int = 0) -> dict[str, Any]:
    """Every place that uses the symbol at this position - resolved, not grepped.

    This is the "what breaks if I change this" question. Grep would match the
    same word in unrelated scopes; the language server knows which uses are
    actually this one.

    Args:
        path: Absolute path to the file.
        line: 1-based line number of the symbol.
        col: 0-based column within that line. Default 0 works when the symbol
            starts the line.
    """
    return _locations("textDocument/references", path, line, col)


@server.tool()
def goto_definition(path: str, line: int, col: int = 0) -> dict[str, Any]:
    """Where the symbol at this position is defined.

    Args:
        path: Absolute path to the file.
        line: 1-based line number.
        col: 0-based column.
    """
    return _locations("textDocument/definition", path, line, col)


@server.tool()
def document_symbols(path: str) -> dict[str, Any]:
    """The shape of one file: its functions, types and their line numbers.

    Cheaper than reading the file when you only need to know what is in it, and
    it gives you the positions the other tools need.

    Args:
        path: Absolute path to the file.
    """
    problem = _missing()
    if problem:
        return {"error": problem}
    try:
        bad = _open(path)
        if bad:
            return bad
        result = _lua(
            """
            local timeout = ...
            local out = {}
            local function walk(items, depth)
              for _, sym in ipairs(items or {}) do
                local range = sym.range or (sym.location and sym.location.range)
                table.insert(out, {
                  name = sym.name,
                  kind = vim.lsp.protocol.SymbolKind[sym.kind] or tostring(sym.kind),
                  line = range and (range.start.line + 1) or 0,
                  depth = depth,
                })
                if sym.children then walk(sym.children, depth + 1) end
              end
            end
            local params = { textDocument = vim.lsp.util.make_text_document_params(0) }
            local responses = vim.lsp.buf_request_sync(
              0, 'textDocument/documentSymbol', params, timeout)
            for _, resp in pairs(responses or {}) do walk(resp.result, 0) end
            return out
            """,
            int(ATTACH_TIMEOUT * 1000),
        )
        syms = result or []
        return {"path": path, "count": len(syms), "symbols": syms}
    except Exception as e:  # noqa: BLE001
        return {"error": f"{type(e).__name__}: {e}"}


@server.tool()
def hover(path: str, line: int, col: int = 0) -> dict[str, Any]:
    """The language server's own description of the symbol - its type, its docs.

    Args:
        path: Absolute path to the file.
        line: 1-based line number.
        col: 0-based column.
    """
    problem = _missing()
    if problem:
        return {"error": problem}
    try:
        bad = _open(path)
        if bad:
            return bad
        text = _lua(
            """
            local line, col, timeout = ...
            local params = vim.lsp.util.make_position_params(0, 'utf-8')
            params.position = { line = line - 1, character = col }
            local responses = vim.lsp.buf_request_sync(
              0, 'textDocument/hover', params, timeout)
            for _, resp in pairs(responses or {}) do
              local c = resp.result and resp.result.contents
              if c then
                if type(c) == 'string' then return c end
                if c.value then return c.value end
                if c[1] then return type(c[1]) == 'string' and c[1] or (c[1].value or '') end
              end
            end
            return ''
            """,
            line,
            col,
            int(ATTACH_TIMEOUT * 1000),
        )
        return {"path": path, "line": line, "hover": text or "(nothing)"}
    except Exception as e:  # noqa: BLE001
        return {"error": f"{type(e).__name__}: {e}"}


if __name__ == "__main__":
    server.run()
