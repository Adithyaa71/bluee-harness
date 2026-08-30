"""Neovim as a code-intelligence server (CLAUDE.md §4h-b).

Why Neovim and not more indexing of our own: `codemap.rs` reads what a file
*declares*. That answers "where does X live" and cannot answer "what calls X" or
"what breaks if I change this signature". Those need real resolution - scope,
imports, types - and writing that per language is a compiler front end per
language. A language server already did it, so this asks the language server.

The difference in practice: grep finds the word `run` in forty files; an LSP
finds the seven that call *this* `run`. And the answer is a dozen file:line
pairs rather than a dozen files, which is the token argument for putting it in
a harness at all.

**No Python dependencies.** It shells out to `nvim --headless -l`, one query per
invocation, and parses JSON from stdout. The obvious alternative - a long-lived
Neovim on a socket - needs `pynvim` or a msgpack implementation, and this
harness should not gain a dependency to ask a question. SnareVec's proxy makes
the same trade for the same reason.

The cost of one-shot is a language-server cold start per query. That is real:
rust-analyzer on a large tree takes tens of seconds the first time. Every answer
reports its own elapsed seconds so the cost is visible rather than mysterious,
and `lsp_status` says plainly what is installed before you spend it.

Requirements:
  1. Neovim 0.11+ on PATH.  Installed: winget install Neovim.Neovim
  2. A language server for the language you care about, on PATH. The bundled
     config registers only the ones that are actually present, so an absent
     server produces an honest message rather than a silent empty answer.
       rust     rustup component add rust-analyzer
       python   pip install pyright        (or basedpyright)
       ts/js    npm i -g typescript-language-server typescript
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any

from mcp.server.mcpserver import MCPServer

HERE = Path(__file__).resolve().parent
INIT = HERE / "nvimrc" / "init.lua"
QUERY = HERE / "query.lua"

# Cold-start budget. rust-analyzer indexing a large workspace genuinely takes
# this long; answering early would return an empty list, which reads as
# "nothing calls this" and is worse than waiting.
TIMEOUT = int(os.environ.get("NVIM_LSP_TIMEOUT", "90"))

server = MCPServer(
    name="nvim-lsp",
    instructions=(
        "Code intelligence through Neovim's language servers. Use "
        "find_references before changing a function - it answers 'what calls "
        "this' with resolved call sites, not text matches. goto_definition "
        "jumps from a use to its declaration; document_symbols gives the shape "
        "of one file. These are slow (a language server has to start and "
        "index) so reach for search_memory and read_source first, and use "
        "these when you specifically need resolution."
    ),
)


def _nvim() -> str | None:
    exe = shutil.which("nvim")
    if exe:
        return exe
    # winget installs here and does not always refresh PATH for a running process.
    fallback = Path(os.environ.get("PROGRAMFILES", r"C:\Program Files")) / "Neovim" / "bin" / "nvim.exe"
    return str(fallback) if fallback.exists() else None


def _ask(**args: Any) -> dict[str, Any]:
    exe = _nvim()
    if not exe:
        return {
            "error": "Neovim is not installed or not on PATH. "
                     "`winget install Neovim.Neovim`, then restart bluee."
        }

    path = args.get("path", "")
    if path and not Path(path).is_absolute():
        return {"error": f"give an absolute path, got {path!r}"}

    args.setdefault("timeout", TIMEOUT)
    try:
        proc = subprocess.run(
            [exe, "--headless", "-u", str(INIT), "-l", str(QUERY), json.dumps(args)],
            capture_output=True,
            text=True,
            # A little past the Lua-side budget, so the inner timeout reports a
            # useful message rather than being killed from outside first.
            timeout=args["timeout"] + 30,
            cwd=str(Path(path).parent) if path else None,
        )
    except subprocess.TimeoutExpired:
        return {"error": f"Neovim did not answer within {args['timeout'] + 30}s"}

    out = (proc.stdout or "").strip()
    # Neovim prints unrelated notices to stdout; the payload is the last JSON
    # object on it, so take from the final opening brace.
    start = out.rfind('{"')
    if start == -1:
        return {
            "error": "Neovim returned nothing usable",
            "stdout": out[-400:],
            "stderr": (proc.stderr or "")[-400:],
        }
    try:
        return json.loads(out[start:])
    except json.JSONDecodeError as e:
        return {"error": f"could not parse the answer: {e}", "stdout": out[-400:]}


@server.tool()
def lsp_status() -> dict[str, Any]:
    """Whether this can answer anything, and which language servers are present.

    Call this first when a code question fails: it separates "Neovim is not
    installed" from "no language server for that language" from a real error.
    """
    exe = _nvim()
    if not exe:
        return {
            "ready": False,
            "error": "Neovim is not installed. `winget install Neovim.Neovim`.",
        }
    known = {
        "rust-analyzer": "rust     rustup component add rust-analyzer",
        "pyright-langserver": "python   pip install pyright",
        "basedpyright-langserver": "python   pip install basedpyright",
        "typescript-language-server": "ts/js    npm i -g typescript-language-server typescript",
        "lua-language-server": "lua      winget install LuaLS.lua-language-server",
        "clangd": "c/c++    winget install LLVM.LLVM",
        "gopls": "go       go install golang.org/x/tools/gopls@latest",
    }
    # Presence on PATH is not the same as working. rust-analyzer in .cargo/bin
    # is a rustup *shim*: it exists, and it exits 1 with "Unknown binary" when
    # the component is not installed. Reporting that as "installed" would be a
    # confident lie, so each candidate is actually run.
    found, broken = [], {}
    for b in known:
        if not shutil.which(b):
            continue
        try:
            probe = subprocess.run([b, "--version"], capture_output=True,
                                   text=True, timeout=20)
            if probe.returncode == 0:
                found.append(b)
            else:
                broken[b] = ((probe.stderr or probe.stdout) or "").strip()[:200]
        except Exception as e:  # noqa: BLE001
            broken[b] = f"{type(e).__name__}: {e}"

    missing = {b: how for b, how in known.items() if b not in found}
    out = {
        "ready": bool(found),
        "nvim": exe,
        "language_servers_working": found,
        "how_to_add": missing,
        "note": "Without a working language server for the file's language these "
                "tools cannot answer. Nothing here guesses - an absent or broken "
                "server gives a message saying so.",
    }
    if broken:
        out["on_path_but_not_working"] = broken
        out["hint"] = ("A binary that is present but fails is usually a stub. "
                       "rust-analyzer in .cargo/bin is a rustup shim - install "
                       "the real component with `rustup component add rust-analyzer`.")
    return out


@server.tool()
def find_references(path: str, line: int, col: int = 0,
                    include_declaration: bool = False) -> dict[str, Any]:
    """Every place that uses the symbol at this position - resolved, not grepped.

    This is the "what breaks if I change this" question. Text search matches the
    same word in unrelated scopes; the language server knows which uses are
    actually this symbol.

    Args:
        path: Absolute path to the file.
        line: 1-based line number of the symbol.
        col: 0-based column within that line. 0 works when the symbol starts it.
        include_declaration: Whether to include the definition itself.
    """
    return _ask(path=path, line=line, col=col,
                method="textDocument/references",
                include_declaration=include_declaration)


@server.tool()
def goto_definition(path: str, line: int, col: int = 0) -> dict[str, Any]:
    """Where the symbol at this position is defined.

    Args:
        path: Absolute path to the file.
        line: 1-based line number.
        col: 0-based column.
    """
    return _ask(path=path, line=line, col=col, method="textDocument/definition")


@server.tool()
def document_symbols(path: str) -> dict[str, Any]:
    """The shape of one file: its functions and types, with line numbers.

    Cheaper than reading the whole file when you only need to know what is in
    it, and it gives you the positions the other tools want.

    Args:
        path: Absolute path to the file.
    """
    return _ask(path=path, method="textDocument/documentSymbol")


@server.tool()
def hover(path: str, line: int, col: int = 0) -> dict[str, Any]:
    """The language server's own description of a symbol - its type and docs.

    Args:
        path: Absolute path to the file.
        line: 1-based line number.
        col: 0-based column.
    """
    return _ask(path=path, line=line, col=col, method="textDocument/hover")


if __name__ == "__main__":
    server.run()
