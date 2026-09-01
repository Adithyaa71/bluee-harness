# bluee

A personal AI assistant harness. Rust core, cloud LLM for reasoning, tools over
MCP, and memory that is built to be inspectable rather than magic.

It is a single binary that opens as a desktop app: chat on the left, a real
terminal, a graph of everything it knows, a playground where it builds working
pages, and a log showing every tool call it made and why.

**Status: usable daily driver, actively built.** Text is the interface. Voice
is not built — see [Adding voice](#adding-voice-stt--tts).

---

## The one idea worth knowing

**The append-only event log is the source of truth. Everything else is derived
from it and is fully rebuildable.**

Every user message, assistant reply, tool call, tool result, error and system
event is appended to a JSONL file in order, and nothing is ever overwritten.
Vector memory and the graph are *projections* of that log — delete them, run
`bluee reduce`, and you get identical stores back.

That single rule is what makes "why did it do that?" answerable: you read the
log. It is also why the model can query the graph but never write to it — a
fact written directly into a derived store would either vanish on the next
rebuild or survive with no evidence behind it. Both are bugs.

## Memory, in three layers

| Layer | What it is | How it is populated |
|---|---|---|
| **Event log** | Append-only JSONL, one event per line, monotonic sequence | Written live by the turn loop |
| **Vector** | `fastembed` (MiniLM, 384-dim) as SQLite BLOBs, brute-force cosine | Derived — whole conversational turns, not lines |
| **Graph** | Kuzu, entities and typed relations | Derived — only what the log *structurally* proves |

There is no vector database. At this scale one is not needed: 100k chunks at
384 dims is ~153 MB resident, and scoring all of them is single-digit
milliseconds in Rust. That removed a dependency rather than compromising on one.

Graph extraction is deliberately shallow. It records what actually happened —
which tools ran, in what order, on which server — plus a code index of this
repo's own source. It does not guess at people, projects or intent. A small
true graph beats a large invented one.

## What is in the box

- **Chat** with streaming turns, session resume, rename, and `/compact` to fold
  a long conversation into searchable memory without losing anything
- **Memory and graph pages** — search, and a pan/zoom canvas over ~1,000 nodes
- **Playground** — the assistant writes real HTML artifacts that render live,
  saved per topic and restorable ("right, let's do trading")
- **Granted folders** — point it at a directory and it can read and work there.
  There is deliberately no tool that grants a folder; only you can
- **Shared terminal** — PowerShell, cmd, bash or `ssh`, in tabs, surviving
  disconnect, and visible to both of you
- **Settings** — persona editor, provider chain with fallbacks, per-workspace
  MCP toggles, skills
- **Screen vision**, three modes, gated: in OFF and PASSIVE the vision tool is
  not in the toolset at all, so the model cannot reach for it

## Requirements

- **Rust** 1.97+, MSVC toolchain on Windows
- **Python 3.12** for the MCP tool servers (3.13 works; **3.14 does not** —
  kuzu publishes no Windows wheel for it)
- **Node**, only if you want to run the UI checks in `dev/`
- An **OpenAI-compatible LLM endpoint** and a key. Any of them — OpenRouter, a
  proxy, a local server. The harness is provider-agnostic
- Optional: **Neovim 0.11+** for code intelligence. Without it that server is
  skipped and nothing else changes

Built and run on Windows 11. The Rust core is portable; the Python paths in
`mcps/servers.example.json` say `.venv/Scripts/python.exe` and would need to be
`.venv/bin/python` elsewhere.

## Setup

```bash
git clone https://github.com/Adithyaa71/bluee-harness
cd bluee-harness
```

**1. The tool-server venv.** This is for the Python MCP servers, not for the
harness itself.

```bash
uv python install 3.12
uv venv --python 3.12
uv pip install kuzu mcp
```

**2. Your provider.**

```bash
cp .env.example .env
```

Fill in `LLM_BASE_URL`, `LLM_API_KEY` and `LLM_MODEL`. Then `bluee models`
lists what your provider actually offers — copy the exact id rather than
guessing it.

**3. Build.** The first one is slow (~7 min cold): `fastembed` and `ort` pull
an ONNX runtime.

```bash
cargo build --release
```

**4. Run.**

```bash
cargo run --release -- app
```

`app` opens the desktop window; `dash 7788` serves the same UI to a browser
instead. On Windows, `bluee.cmd` wraps both once built.

`mcps/servers.json` is created for you on first run from
`mcps/servers.example.json`. Two servers are enabled by default — the graph and
the Neovim LSP bridge, both of which live in this repo. The other three need
repos you clone yourself, and are listed there disabled with what each one
needs. **A server that fails to start is reported and skipped**, never fatal.

### If your checkout path has spaces or a tilde

Copy `.cargo/config.toml.example` to `.cargo/config.toml` and point
`target-dir` somewhere short. Windows' 260-character `MAX_PATH` breaks
cmake-based native dependencies on long paths, and this project's own path is
why that file exists. If your path is ordinary, skip it.

## The CLI

```
bluee app                     desktop window
bluee dash 7788               serve the UI to a browser
bluee chat                    plain terminal chat, no UI
bluee reduce                  rebuild vector + graph memory from the event log
bluee search "..."            query vector memory
bluee tools                   list every connected tool
bluee call <server__tool>     call one directly, logged like any other call
bluee log                     print the last session's raw trace
bluee models                  list model ids your provider offers
```

## Persona

Four Markdown files in `persona/`, loaded in this order at session start and
never mutated mid-conversation, so the prompt prefix stays cache-friendly:

- `SOUL.md` — who it is. Keep it short; 200-500 words. **The name lives here**,
  as a one-line edit
- `AGENTS.md` — how it works. Procedure, kept separate from identity on purpose
- `TOOLS.md` — tool policy, and the operational knowledge that was expensive to
  learn
- `USER.md` — facts about you. Seed it; an empty one means every session starts
  from zero

`USER.md` may be updated automatically. **`SOUL.md` is human-gated** and should
stay that way — a silently drifted persona core is a permanently hijacked agent.

## Cost — the one lever that matters

Tool schemas are paid on **every turn**, before you have said anything.

```
129 tools (all servers)   ~22,300 prompt tokens
 54 tools (playground)     ~9,300 prompt tokens
 28 tools (a code folder)  ~4,800 prompt tokens
```

Set them per granted folder in **+ menu, Connectors**. `null` means every
server and `[]` means none — kept distinct from unset, because both are real
choices. This matters far more than conversation length; `/compact` is for when
a chat genuinely gets long.

## Adding voice (STT / TTS)

Not built, and nothing depends on it — which is the point. The harness takes
text and returns text, so **voice is additive, not a parallel implementation**.

The whole seam is one function in `src/agent.rs`:

```rust
pub async fn turn_with(&mut self, input: &str, sink: Option<&EventSink>) -> Vec<TurnEvent>
```

Text in, and out a stream of `TurnEvent::{ToolCall, ToolResult, Reply, Error}`.
The CLI, the dashboard websocket and any voice loop all drive this same
function. Do not write a second turn loop.

- **STT** produces a string, then calls `turn_with`. Nothing else changes.
  `whisper.cpp` with `base` or `small` (not `tiny`) is the intended path;
  benchmark both and pick whichever hits real-time on your CPU
- **TTS** consumes `TurnEvent::Reply { text }`. Piper is fast and CPU-native
- **Wake word** via OpenWakeWord, with push-to-talk as the documented fallback
  if continuous listening ends up competing for CPU with everything else

Everything a voice loop says and hears still lands in the event log, so memory,
the graph and the dashboard keep working unchanged.

## Testing UI changes — do not skip this

`dash/index.html` is one large file compiled into the binary with
`include_str!`, so a syntax error in it does not fail the build. It blanks the
window at runtime. Two things catch that:

```bash
cargo test
node dev/uicheck.mjs 7788
```

`cargo test` runs `node --check` over the page's inline script. `uicheck.mjs`
drives real headless Chrome over CDP and needs nothing installed — Chrome ships
with Windows and CDP is plain WebSocket. It clicks things, screenshots to
`dev/shots/`, and fails on any console error.

Every UI bug that ever reached a user here got there because a check ran
against a DOM stub. A stub proves code executes; it cannot prove a click does
anything.

## Not built, deliberately

- **Voice** — see above
- **Writing its own source.** It can *read* the repo, via `read_source` and a
  code index in both memory layers. Writing is a separate decision with its own
  guardrails, and should be made on purpose rather than arrive as a side effect
- **Automatic skill mining.** Explicit "save that as a skill" works;
  `skills/proposed/` is the safety catch that would make mining safe to enable

## A word on safety

`run_command` gives the model a shell. Its working directory is always a folder
you granted, and it cannot grant itself one, so it chooses *what* runs and never
*where from*. **But a shell reaches the whole machine regardless of cwd. This is
not a sandbox and should not be described as one.** What it gives instead is
visibility: every command is a logged tool call carrying its full text, exit
code and output.

Machine-ending commands are refused unconditionally, with no override on the
tool. An earlier version took a `confirm: true` flag — asked to run
`format C: /q`, the model set the flag itself and ran it. A confirmation the
model can grant itself is decoration.

## Docs

- **[`USER_GUIDE.md`](USER_GUIDE.md)** — how to use it, in plain English. Start
  here
- **[`CLAUDE.md`](CLAUDE.md)** — the build log. Every decision, what was
  measured, and the bugs, including the embarrassing ones. Long, and the most
  useful thing here if you want to know *why* anything is shaped the way it is

## Licence

MIT — see [`LICENSE`](LICENSE). `dash/vendor/` holds unmodified xterm.js
builds, also MIT; attribution is in `dash/vendor/README.md`.
