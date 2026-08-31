"""Neovim as a code-intelligence server (CLAUDE.md §4h-b).

Why Neovim rather than more indexing of our own: `codemap.rs` reads what a file
*declares*. That answers "where does X live" and cannot answer "what calls X" or
"what breaks if I change this signature". Those need real resolution - scope,
imports, types - and writing that per language is a compiler front end per
language. A language server already did it, so this asks the language server.

The difference in practice, measured on this repo: `roots::pretty` has six call
sites, and `find_references` returns exactly those six with their source lines.
Grep for `pretty` would also match the word in comments and in unrelated
scopes. The answer is six file:line pairs rather than six files, which is the
token argument for putting it in a harness at all.

**No Python dependencies.** One long-lived `nvim --headless --listen <pipe>`,
queried with `nvim --headless --server <pipe> --remote-expr`. Neovim speaks
msgpack to Neovim; this module only shells out and reads JSON. `pynvim` would
work too and is not worth the dependency.

**Persistent, not one-shot** - and this is the whole performance story. A fresh
Neovim per question throws away rust-analyzer's index every time, and indexing
this workspace takes ~28 seconds. Keeping one alive makes the first question
slow and every one after it about 2 seconds. Measured, both numbers.

Requirements:
  1. Neovim 0.11+ on PATH.   winget install Neovim.Neovim
  2. A language server per language, on PATH. The config registers only the ones
     actually present, and `lsp_status` runs each rather than trusting PATH -
     `rust-analyzer` in `.cargo/bin` is a rustup shim that exists and fails
     until you run `rustup component add rust-analyzer`.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any

from mcp.server.mcpserver import MCPServer

HERE = Path(__file__).resolve().parent
NVIMRC = HERE / "nvimrc"

# One named pipe for the whole machine, so a second bluee reuses the warm
# Neovim instead of paying the indexing cost again.
PIPE = os.environ.get("NVIM_LSP_PIPE", "//./pipe/bluee-nvim")

# Cold-start budget. rust-analyzer loading a large workspace genuinely takes
# tens of seconds; the query waits on the server's own readiness signal rather
# than answering early with an empty list.
TIMEOUT = int(os.environ.get("NVIM_LSP_TIMEOUT", "180"))

# Temp files carry arguments in and answers out. Quoting JSON through a shell,
# into Vimscript, into Lua is three escaping layers; two files are none.
SCRATCH = Path(tempfile.gettempdir()) / "bluee-nvim"

server = MCPServer(
    name="nvim-lsp",
    instructions=(
        "Code intelligence through Neovim's language servers. find_references "
        "answers 'what calls this' with resolved call sites rather than text "
        "matches - use it before changing a function. goto_definition jumps "
        "from a use to its declaration; document_symbols gives the shape of a "
        "file. The first call after startup waits for the language server to "
        "index (tens of seconds); later ones are quick, so prefer "
        "search_memory and read_source for cheap questions and come here when "
        "you specifically need resolution."
    ),
)


def _config_dir() -> Path:
    """A copy of the config at a path Neovim will not mangle.

    Neovim expands `~` inside a `-u` argument. This project lives at
    `D:/Conceptual Project ~ clg`, so passing the config path directly silently
    turned it into a home-directory expansion and Neovim started with no config
    at all - visible only as every query answering "no language server". Copying
    to a scratch directory with a plain name removes the whole class of problem,
    and refreshing on each start means edits to the real config still apply.
    """
    dest = SCRATCH / "config"
    dest.mkdir(parents=True, exist_ok=True)
    for f in NVIMRC.glob("*.lua"):
        target = dest / f.name
        if not target.exists() or target.stat().st_mtime < f.stat().st_mtime:
            target.write_bytes(f.read_bytes())
    return dest


def _nvim() -> str | None:
    exe = shutil.which("nvim")
    if exe:
        return exe
    # winget installs here and does not always refresh PATH for a live process.
    fallback = Path(os.environ.get("PROGRAMFILES", r"C:\Program Files")) / "Neovim" / "bin" / "nvim.exe"
    return str(fallback) if fallback.exists() else None


def _expr(exe: str, lua: str, timeout: int = 30) -> subprocess.CompletedProcess:
    return subprocess.run(
        [exe, "--headless", "--server", PIPE, "--remote-expr", f'luaeval("{lua}")'],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _alive(exe: str) -> bool:
    try:
        r = _expr(exe, "tostring(_G.BLUEE_READY)", timeout=15)
        return "true" in (r.stdout or "")
    except Exception:  # noqa: BLE001
        return False


def _ensure(exe: str) -> str | None:
    """Start the persistent Neovim if it is not already answering."""
    if _alive(exe):
        return None
    SCRATCH.mkdir(parents=True, exist_ok=True)
    cfg = _config_dir()
    env = dict(os.environ)

    # Prefer Adithya's own config when it is installed: his nyx-nvim brings the
    # plugins (treesitter, telescope) that his tool layer needs, and without
    # them those tools return empty. The harness then adds only its query hook
    # on top. Falls back to the minimal config beside this file, which needs no
    # plugins at all.
    nyx = Path(env.get("LOCALAPPDATA", "")) / "nyx"
    if (nyx / "init.lua").exists():
        env["NVIM_APPNAME"] = "nyx"
        argv = [exe, "--headless", "--listen", PIPE,
                "-c", "luafile " + (cfg / "attach.lua").as_posix()]
    else:
        argv = [exe, "--headless", "-u", (cfg / "init.lua").as_posix(), "--listen", PIPE]

    subprocess.Popen(
        argv,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        # Detached, so it outlives this MCP server process and stays warm
        # across bluee restarts - which is the point of it being persistent.
        creationflags=getattr(subprocess, "DETACHED_PROCESS", 0)
        | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0),
    )
    for _ in range(40):
        time.sleep(0.5)
        if _alive(exe):
            return None
    return "Neovim started but never answered on its pipe"


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
    # Forward slashes throughout: a Windows backslash inside a Lua string
    # literal is an escape, and that silently corrupts the path.
    if path:
        args["path"] = str(path).replace("\\", "/")

    problem = _ensure(exe)
    if problem:
        return {"error": problem}

    args.setdefault("timeout", TIMEOUT)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    tag = uuid.uuid4().hex[:12]
    qin = (SCRATCH / f"{tag}.in.json").as_posix()
    qout = (SCRATCH / f"{tag}.out.json").as_posix()
    Path(qin).write_text(json.dumps(args), encoding="utf-8")

    try:
        r = _expr(exe, f"_G.bluee_query_file('{qin}','{qout}')", timeout=args["timeout"] + 60)
    except subprocess.TimeoutExpired:
        return {"error": f"Neovim did not answer within {args['timeout'] + 60}s"}
    finally:
        Path(qin).unlink(missing_ok=True)

    try:
        raw = Path(qout).read_text(encoding="utf-8").strip()
    except OSError:
        return {
            "error": "Neovim produced no answer file",
            "stderr": (r.stderr or "")[-400:],
        }
    finally:
        Path(qout).unlink(missing_ok=True)

    try:
        return json.loads(raw)
    except json.JSONDecodeError as e:
        return {"error": f"could not parse the answer: {e}", "raw": raw[:400]}


@server.tool()
def lsp_status() -> dict[str, Any]:
    """Whether this can answer anything, and which language servers work.

    Call this first when a code question fails: it separates "Neovim is not
    installed" from "no language server for that language" from "the binary is
    there but broken".
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
    # Presence on PATH is not the same as working: rust-analyzer in .cargo/bin
    # is a rustup shim that exists and exits 1 until the component is added.
    # Reporting that as installed would be a confident lie, so each is run.
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

    out = {
        "ready": bool(found),
        "nvim": exe,
        "language_servers_working": found,
        "how_to_add": {b: how for b, how in known.items() if b not in found},
        "neovim_running": _alive(exe),
        "note": "The first query after startup waits for the language server to "
                "index the project - tens of seconds on a large one. Neovim then "
                "stays warm and later queries take about two seconds.",
    }
    if broken:
        out["on_path_but_not_working"] = broken
        out["hint"] = ("A binary present but failing is usually a stub. "
                       "rust-analyzer in .cargo/bin is a rustup shim - "
                       "`rustup component add rust-analyzer`.")
    return out


@server.tool()
def find_references(path: str, line: int, col: int = 0,
                    include_declaration: bool = False) -> dict[str, Any]:
    """Every place that uses the symbol at this position - resolved, not grepped.

    This is the "what breaks if I change this" question. Text search matches the
    same word in unrelated scopes; the language server knows which uses are
    actually this symbol.

    If the answer comes back empty with `indexing: true`, the language server
    had not finished loading - that is not a real zero, ask again.

    Args:
        path: Absolute path to the file.
        line: 1-based line number of the symbol.
        col: 0-based column within that line. 0 works when the symbol starts it.
        include_declaration: Include the definition itself as well.
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
