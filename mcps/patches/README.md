# Patches against cloned MCP servers

`mcps/UACC/` and `reference/` are gitignored (they carry their own git
history), so any local fix to them would silently vanish on a fresh clone.
Patches live here so the setup is reproducible.

## Applying

```bash
cd mcps/UACC && git apply ../patches/uacc-artistic-painter-syntax.patch
```

---

## `uacc-tesseract-path.patch`

**Problem:** OCR (`get_screen_info {include_ocr: true}`) either hung or
returned nothing. Two missing pieces and one path:

1. `pytesseract` was not installed in UACC's venv, so UACC fell back to
   EasyOCR - which never returned, and took a whole turn with it for 10+
   minutes (CLAUDE.md §56e). The venv is uv-made and has no pip:
   `python -m uv pip install --python mcps/UACC/.venv/Scripts/python.exe pytesseract`
2. The Tesseract program itself: `winget install -e --id UB-Mannheim.TesseractOCR`
   (installs 5.4 to `C:\Program Files\Tesseract-OCR`).
3. **That installer does not add Tesseract to PATH**, and pytesseract only
   looks on PATH - so even with both installed, every OCR call failed.

The patch points pytesseract at the standard install location when
`tesseract` is not on PATH (`TESSERACT_CMD` overrides). Verified: reads a
rendered test string back at ~200ms instead of hanging.

---

## `uacc-artistic-painter-syntax.patch`

**Problem:** `uacc/actions/artistic_painter.py` does not parse.

```
File "uacc/actions/artistic_painter.py", line 120
    if is_facial:
IndentationError: unexpected indent
```

`uacc_mcp/server.py` imports `ArtisticPainter` unconditionally at line 87, so
this single broken file prevents the **entire 68-tool MCP server** from
starting. Without the patch, `harness tools` reports:

```
[warn] server failed to start - uacc: MCP handshake with uacc: connection closed
```

**Cause:** a bad merge. Lines 115–118 compute `structural_paths` /
`detail_paths` from a length-based split, and then an orphaned
`if / elif / else` block follows whose enclosing `for` loop was deleted. Two
implementations were spliced together and the conflict resolved wrongly.

This is **not a recent regression** — checked every upstream commit that has
ever touched the file (`8c0bb00`, `69bebf9`, `7035ed1`, `95edb5d`, `3ddc2df`,
`85a6dd3`) and it is a SyntaxError in all of them. There is no good upstream
commit to pin to. Verified against `92bba10` (default branch HEAD).

**Fix:** remove the orphaned block, and define `face_feature_paths = []`
because it is otherwise only ever assigned inside that block yet is read at
lines ~129 and ~143 — so deleting alone would trade a SyntaxError for a
NameError.

**Deliberately not fixed:** the per-path facial/structural classification the
orphaned block implemented is *not* reconstructed. Doing so would be guessing
at upstream intent. Only the length-based split remains in effect, which means
facial-feature prioritisation in `ArtisticPainter` is inert.

**Blast radius:** MS Paint drawing only. No GUI-control tool the harness uses
(`click`, `type_text`, `screenshot`, `get_screen_info`, window management…)
touches `ArtisticPainter`.

**Worth reporting upstream** — it's a one-line-class bug that stops the server
booting for everyone.

---

## Note: UACC needs its own venv

Unrelated to the patch, but required. UACC is written against **mcp v1**
(`from mcp.server.fastmcp import FastMCP`) while `mcps/kuzu-graph/` needs
**mcp v2** (`MCPServer`). These cannot share an interpreter, so UACC runs from
`mcps/UACC/.venv`:

```bash
uv venv --python 3.12 mcps/UACC/.venv
uv pip install --python mcps/UACC/.venv/Scripts/python.exe -e mcps/UACC "mcp<2"
```

Separate processes can have separate Pythons — one of the concrete payoffs of
talking to tool servers over MCP rather than importing them.


---

## `uacc-missing-invalidate-tree-cache.patch`

**Problem:** every UACC tool that *acts* on the UI threw `NameError`.

```
Click failed at (936,1060) - Error: NameError: name 'invalidate_tree_cache' is not defined
```

`invalidate_tree_cache` is defined in `uacc/core/accessibility.py` and imported
**inside one function** at `server.py:3680`, but it is *called* at six
module-level sites - 544, 608, 655, 715, 790 and 1454. Those are `click`,
`type_text`, `hotkey`, `scroll` and `drag`: the whole input path. Reading the
screen worked, so the server looked healthy; nothing that touched the UI did.

The module-level import at line 57 pulls in `get_ui_tree` and stops there. The
fix is one word.

```bash
cd mcps/UACC && git apply ../patches/uacc-missing-invalidate-tree-cache.patch
```

Verified after applying: `click_element` by name landed on a real control
(`"clicked": "Large Icons", "how": "by name"`), and `hotkey` round-tripped.
Worth reporting upstream - it is not Windows-specific or setup-specific, so it
presumably breaks for everyone.
