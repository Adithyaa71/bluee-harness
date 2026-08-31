"""nyx-nvim's tool layer, exposed over MCP (CLAUDE.md §4h-b).

Adithya's `nyx-nvim` repo has a real tool layer - analysis, navigation,
refactor and search built on `pynvim` - but no MCP entry point: at HEAD there is
no tool registration in any language, and its INSTALL.md delegates the protocol
to the third-party npm `mcp-neovim-server`. This file is the missing entry
point, so his own code is what answers rather than a package nobody here has
read.

**It drives the same Neovim `nvim-lsp` already keeps warm.** His `get_socket()`
checks `NVIM_SOCKET_PATH` before anything else - genuine foresight - but it then
calls `os.path.exists()` on it, and a Windows named pipe does not exist as a
path, so the check fails and it falls through to a `.sh` script that cannot run
here. Rather than patch his repo (it is a clone he will pull), the accessor is
replaced at import time. His code is otherwise untouched.

Two of his modules are exposed as-is and one is worth naming:

- `analysis`, `navigation`, `search` - read-only.
- `refactor` - **these edit files.** `safe_edit`, `replace_in_file`,
  `insert_lines_after` and `delete_lines` write through Neovim to whatever
  buffer is open. That is a real capability, deliberately separate from
  `read_source`, which reads and only reads. Turn this server off per workspace
  in **+ menu -> Connectors** if you do not want it in a given folder.
"""

from __future__ import annotations

import importlib.util
import os
import sys
import types
from pathlib import Path
from typing import Any

from mcp.server.mcpserver import MCPServer

# The clone lives in tools/, per §2's layout for software pulled from git.
NYX = Path(__file__).resolve().parents[2] / "tools" / "nyx-nvim"
PIPE = os.environ.get("NVIM_LSP_PIPE", "//./pipe/bluee-nvim")

server = MCPServer(
    name="nyx-tools",
    instructions=(
        "Adithya's own Neovim tool layer: file structure and diagnostics, "
        "project search, navigation, and refactoring. It drives the same "
        "Neovim instance nvim-lsp uses, so the language server is already warm. "
        "The refactor tools WRITE to files - say what you are changing before "
        "you change it."
    ),
)


def _load() -> tuple[dict[str, Any] | None, str | None]:
    """Import his modules, with the socket accessor pointed at our Neovim."""
    if not NYX.is_dir():
        return None, (
            f"nyx-nvim is not cloned at {NYX}. "
            "git clone https://github.com/Adithyaa71/nyx-nvim.git tools/nyx-nvim"
        )
    try:
        import pynvim  # noqa: F401
    except ImportError:
        return None, "pynvim is not installed. `.venv/Scripts/python -m pip install pynvim`"

    # A genuine name collision: his repo has a package called `mcp/`, and so
    # does the MCP SDK this file is written against. His modules do
    # `from mcp.mcp_client import ...`, so that name has to mean HIS package
    # while they import, and the SDK's again afterwards. Swap it for the
    # duration and put it back - his modules bind the functions directly at
    # import time, so they keep working once the name reverts.
    sdk = sys.modules.get("mcp")
    sdk_subs = {k: v for k, v in sys.modules.items() if k.startswith("mcp.")}
    saved_tools = {k: v for k, v in sys.modules.items()
                   if k == "tools" or k.startswith("tools.")}

    try:
        shim = types.ModuleType("mcp")
        shim.__path__ = [str(NYX / "mcp")]  # type: ignore[attr-defined]
        sys.modules["mcp"] = shim
        for k in list(sys.modules):
            if k.startswith("mcp."):
                del sys.modules[k]

        import mcp.mcp_client as client  # his module now, not the SDK

        # His get_socket() honours NVIM_SOCKET_PATH - real foresight - but then
        # guards it with os.path.exists(), which is false for a Windows named
        # pipe, so it falls through to a .sh helper that cannot run here.
        # Replace the accessor rather than patching a clone he will pull.
        client.get_socket = lambda: PIPE  # type: ignore[assignment]

        # Our own repo also has a `tools/` directory, so his has to win the
        # name while these load.
        if str(NYX) not in sys.path:
            sys.path.insert(0, str(NYX))
        for k in list(sys.modules):
            if k == "tools" or k.startswith("tools."):
                del sys.modules[k]

        from tools import analysis, navigation, refactor, search
        mods = {"analysis": analysis, "navigation": navigation,
                "refactor": refactor, "search": search}
    except Exception as e:  # noqa: BLE001
        return None, f"could not import nyx-nvim's tool layer: {type(e).__name__}: {e}"
    finally:
        if sdk is not None:
            sys.modules["mcp"] = sdk
            sys.modules.update(sdk_subs)
        sys.modules.update(saved_tools)

    return mods, None


_MODS, _WHY = _load()


def _ensure_nvim() -> str | None:
    """Start the shared Neovim if nothing is answering on the pipe.

    Each server has to be able to stand this up on its own. The first version
    relied on `nvim_lsp` having been asked something first, which meant every
    tool here failed with a connection error until you happened to use the
    other server - a dependency between two processes that nothing enforced.
    """
    import subprocess
    from pathlib import Path as _P

    lsp = _P(__file__).resolve().parents[1] / "nvim-lsp" / "server.py"
    if not lsp.exists():
        return "nvim-lsp server is missing; cannot start the shared Neovim"
    spec = importlib.util.spec_from_file_location("bluee_nvim_lsp", lsp)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    exe = mod._nvim()
    if not exe:
        return "Neovim is not installed. `winget install Neovim.Neovim`."
    return mod._ensure(exe)


def _call(module: str, fn: str, **kwargs: Any) -> Any:
    if _MODS is None:
        return {"error": _WHY}
    problem = _ensure_nvim()
    if problem:
        return {"error": problem}
    try:
        result = getattr(_MODS[module], fn)(**{k: v for k, v in kwargs.items() if v is not None})
    except Exception as e:  # noqa: BLE001
        # His layer raises NyxConnectionError when Neovim is unreachable, which
        # is the common case worth naming rather than dumping a traceback.
        name = type(e).__name__
        if "Connection" in name:
            return {"error": f"{e}", "hint": "Ask nvim_lsp anything first - that starts "
                                             "the shared Neovim this connects to."}
        return {"error": f"{name}: {e}"}
    return {"result": result}


@server.tool()
def nyx_status() -> dict[str, Any]:
    """Whether nyx-nvim's tool layer is loaded, and what it is talking to."""
    if _MODS is None:
        return {"ready": False, "error": _WHY, "repo": str(NYX)}
    probe = _call("navigation", "list_open_files")
    return {
        "ready": "error" not in probe,
        "repo": str(NYX),
        "neovim_socket": PIPE,
        "modules": sorted(_MODS),
        "probe": probe,
        "note": "Shares the Neovim that nvim-lsp keeps warm, so the language "
                "server does not index twice.",
    }


# ---------------------------------------------------------------- analysis

@server.tool()
def file_symbols(file: str | None = None) -> dict[str, Any]:
    """Symbols declared in a file, via its language server.

    Args:
        file: Absolute path. Omitted means the buffer currently open.
    """
    return _call("analysis", "get_file_symbols", file=file)


@server.tool()
def file_diagnostics(file: str | None = None) -> dict[str, Any]:
    """Errors and warnings the language server reports for a file.

    Args:
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("analysis", "get_file_diagnostics", file=file)


@server.tool()
def module_structure(file: str) -> dict[str, Any]:
    """The shape of a module - its imports, classes and functions.

    Args:
        file: Absolute path.
    """
    return _call("analysis", "get_module_structure", file=file)


@server.tool()
def diagnostics_summary(file: str | None = None) -> dict[str, Any]:
    """Counts of errors and warnings plus the top messages.

    Cheaper than reading every diagnostic when you only need to know whether a
    file is broken.

    Args:
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("analysis", "summarize_diagnostics", file=file)


@server.tool()
def current_context() -> dict[str, Any]:
    """Where the editor is: file, line, and the surrounding symbol."""
    return _call("analysis", "get_current_context")


# ---------------------------------------------------------------- search

@server.tool()
def search_project(pattern: str, path: str | None = None) -> dict[str, Any]:
    """Text search across the project, with file and line for each hit.

    Args:
        pattern: What to search for.
        path: Restrict to this directory. Omitted means the whole project.
    """
    return _call("search", "search_project", pattern=pattern, path=path)


@server.tool()
def find_files_by_name(query: str, path: str | None = None) -> dict[str, Any]:
    """Find files whose name matches a query.

    Args:
        query: Part of a filename.
        path: Restrict to this directory.
    """
    return _call("search", "find_files_by_name", query=query, path=path)


@server.tool()
def find_symbol_in_workspace(query: str) -> dict[str, Any]:
    """Find a symbol anywhere in the workspace, via the language server.

    Resolved rather than text-matched - use this when you know the name but not
    the file.

    Args:
        query: Symbol name or part of one.
    """
    return _call("search", "find_symbol_in_workspace", query=query)


@server.tool()
def search_in_file(pattern: str, file: str | None = None) -> dict[str, Any]:
    """Search within a single file.

    Args:
        pattern: What to search for.
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("search", "search_in_file", pattern=pattern, file=file)


@server.tool()
def find_todos(path: str | None = None) -> dict[str, Any]:
    """Every TODO and FIXME left in the project.

    Args:
        path: Restrict to this directory.
    """
    return _call("search", "find_todos", path=path)


# ---------------------------------------------------------------- navigation

@server.tool()
def open_file(path: str) -> dict[str, Any]:
    """Open a file in the shared Neovim, so later position-based calls act on it.

    Args:
        path: Absolute path.
    """
    return _call("navigation", "open_file", path=path)


@server.tool()
def goto_line(line: int, file: str | None = None) -> dict[str, Any]:
    """Move the cursor to a line, opening the file first if given.

    Args:
        line: 1-based line number.
        file: Absolute path.
    """
    return _call("navigation", "goto_line", line=line, file=file)


@server.tool()
def current_location() -> dict[str, Any]:
    """The file and line the shared Neovim is currently sitting on."""
    return _call("navigation", "get_current_location")


@server.tool()
def list_open_files() -> dict[str, Any]:
    """Which files are open in the shared Neovim."""
    return _call("navigation", "list_open_files")


# ---------------------------------------------------------------- refactor
# These WRITE. Say what you are changing before you change it.

@server.tool()
def safe_edit(file: str, start_line: int, end_line: int, new_content: str) -> dict[str, Any]:
    """Replace a line range in a file. **This writes to disk.**

    Say what you are replacing and why before calling it.

    Args:
        file: Absolute path.
        start_line: First line to replace, 1-based, inclusive.
        end_line: Last line to replace, inclusive.
        new_content: What goes in their place.
    """
    return _call("refactor", "safe_edit", file=file, start_line=start_line,
                 end_line=end_line, new_content=new_content)


@server.tool()
def replace_in_file(pattern: str, replacement: str, file: str | None = None) -> dict[str, Any]:
    """Search and replace within a file. **This writes to disk.**

    Args:
        pattern: What to replace.
        replacement: What to put there.
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("refactor", "replace_in_file", pattern=pattern,
                 replacement=replacement, file=file)


@server.tool()
def insert_lines_after(line: int, new_content: str, file: str | None = None) -> dict[str, Any]:
    """Insert text after a line. **This writes to disk.**

    Args:
        line: Insert after this 1-based line.
        new_content: What to insert.
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("refactor", "insert_lines_after", line=line,
                 new_content=new_content, file=file)


@server.tool()
def delete_lines(start_line: int, end_line: int, file: str | None = None) -> dict[str, Any]:
    """Delete a line range. **This writes to disk.**

    Args:
        start_line: First line to delete, 1-based, inclusive.
        end_line: Last line to delete, inclusive.
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("refactor", "delete_lines", start_line=start_line,
                 end_line=end_line, file=file)


@server.tool()
def format_file(file: str | None = None) -> dict[str, Any]:
    """Run the configured formatter over a file. **This writes to disk.**

    Args:
        file: Absolute path. Omitted means the current buffer.
    """
    return _call("refactor", "format_file", file=file)


if __name__ == "__main__":
    server.run()
