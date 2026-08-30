"""A tiny language server that answers with fixed data, for testing the pipeline.

Why this exists: verifying the Neovim path needs *a* language server, and a real
one (rust-analyzer, pyright) is a network install away. This one is stdlib-only
and answers instantly, so the harness plumbing - nvim attaches, the request goes
out, the response comes back, the JSON is shaped - can be proven independently
of whether any real server is installed yet.

It is a test fixture and nothing else. It never ships in servers.json.

    nvim --headless -u dev/mock_init.lua -l query.lua '{...}'
"""

from __future__ import annotations

import json
import sys


def read_message() -> dict | None:
    """One LSP frame: Content-Length header, blank line, JSON body."""
    length = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if not length:
        return None
    return json.loads(sys.stdin.buffer.read(length))


def send(payload: dict) -> None:
    body = json.dumps(payload).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def uri(path: str) -> str:
    p = path.replace("\\", "/")
    return "file:///" + p.lstrip("/")


def main() -> None:
    root = ""
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        mid = msg.get("id")

        if method == "initialize":
            root = (msg.get("params") or {}).get("rootUri") or ""
            send({
                "jsonrpc": "2.0", "id": mid,
                "result": {
                    "capabilities": {
                        "textDocumentSync": 1,
                        "referencesProvider": True,
                        "definitionProvider": True,
                        "documentSymbolProvider": True,
                        "hoverProvider": True,
                    },
                    "serverInfo": {"name": "mock-lsp", "version": "1"},
                },
            })
        elif method == "shutdown":
            send({"jsonrpc": "2.0", "id": mid, "result": None})
        elif method == "exit":
            return
        elif mid is not None:
            doc = ((msg.get("params") or {}).get("textDocument") or {}).get("uri", root)
            if method == "textDocument/references":
                send({"jsonrpc": "2.0", "id": mid, "result": [
                    {"uri": doc, "range": {"start": {"line": 0, "character": 0},
                                           "end": {"line": 0, "character": 4}}},
                    {"uri": doc, "range": {"start": {"line": 2, "character": 0},
                                           "end": {"line": 2, "character": 4}}},
                ]})
            elif method == "textDocument/definition":
                send({"jsonrpc": "2.0", "id": mid, "result": [
                    {"uri": doc, "range": {"start": {"line": 1, "character": 0},
                                           "end": {"line": 1, "character": 4}}},
                ]})
            elif method == "textDocument/documentSymbol":
                send({"jsonrpc": "2.0", "id": mid, "result": [
                    {"name": "mock_symbol", "kind": 12,
                     "range": {"start": {"line": 0, "character": 0},
                               "end": {"line": 0, "character": 4}},
                     "selectionRange": {"start": {"line": 0, "character": 0},
                                        "end": {"line": 0, "character": 4}}},
                ]})
            elif method == "textDocument/hover":
                send({"jsonrpc": "2.0", "id": mid,
                      "result": {"contents": {"kind": "plaintext",
                                              "value": "mock hover text"}}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "result": None})
        # notifications need no reply


if __name__ == "__main__":
    main()
