# Personal AI Assistant — Solo Build (name: configurable, see §5)

**Owner:** Adithya | **Window:** Fri Aug 28 (evening) → Sun Aug 30 morning (~30 hrs)
**Mode:** Solo build, Claude Code as pair programmer. LLM reasoning = cloud API (no local brain model).
**This is v1** — a genuinely usable daily-driver assistant to live with for a while, not the end goal. The end goal (stated by owner) is an eventual Linux kernel-level AI-OS harness; this build is intentionally scoped below that, and §10 notes what to study later without attempting now.

---

## 0. What this is

A personal AI assistant harness that:
- Takes input as **text by default** — a real chat/CLI interface to the harness — and reasons (cloud LLM API) and acts (OS tools + GUI control + browser control) via MCP tool-calling. **Voice (wake word, STT, TTS) is an optional input/output module bolted on afterward, not the primary interface and not a requirement for the harness to be "working."** Build and prove the core loop in text mode first; add voice as a module once that's solid, since it's genuinely a separate, swappable layer, not core plumbing.
- Has real memory: vector RAG (semantic recall) + graph DB (relationships/entities) + an append-only session event log — this log doubles as the tool-usage log you asked for, since every tool call and result is an event in it
- Has a live dashboard to inspect memory (graph + vector + event log) as it grows, and to control screen-vision mode
- Understands your screen in three configurable modes — off, passive (default), or active — never continuous VLM by default
- Has a persona split across multiple config files (not one blob, not hardcoded into prompts), name configurable
- Can self-evolve in a scoped way: it can be told to save a procedure as a reusable skill, and (stretch) can propose new skills itself from patterns in its own event log

**Build priority for this pass, in the order you asked for:** core harness (MCP tool-calling loop, text-driven) → vector + graph memory → tool-usage/event log → dashboard. Everything below this point in the doc is organized around that order, not voice-first.

**Current scope decisions for this build window — not permanent limits, entirely yours to revisit at any point:**

These are calls made to fit a 30-hour solo window without the whole budget going into infrastructure instead of a working assistant. None of them are "you can't" — they're "not this pass, here's why, revisit whenever you want":

- Kernel-level work (drivers, syscall hooks) — sized as its own project (§10), genuinely weeks of work on its own; folding it into this pass would mean nothing else ships.
- ~~Full Rust/Java rewrite of tool functions~~ — **REVISED: the harness core is Rust.** The original reasoning (LLM latency dominates, so a rewrite buys no felt speed) still holds *on performance grounds* — that was never why this changed. What changed is the deadline: it was confirmed soft, which removes the only real argument against. The actual reasons to go Rust are (a) a single binary is a materially better thing to live with daily than a venv carrying a 3.12 interpreter + torch, and (b) §10's end goal is a kernel-level AI-OS harness, where Rust is the language you want to already be fluent in. Note the cost honestly: the owner does not write Rust, so long-term maintainability depends on tooling assistance. That tradeoff was raised and accepted.
- Continuous local VLM screen analysis — no hardware here makes this cheap or fast; the gated toggle in §6 gets you agentic vision without the always-on cost. Change this later if you get a GPU or want to eat the API cost.
- Local LLM as the main reasoning brain — this laptop's CPU-only silicon makes it too slow for a responsive loop right now; nothing stops you from pointing the harness at a local model later if that changes (better hardware, a smaller model that's actually fast enough) — the harness is provider-agnostic by design (§3).
- Prime Agent–style persistent kernel / Odysseus-scale multi-surface workspace — both real, both referenced in §10 as where this could grow, both multi-week builds on their own.
- SOUL.md self-modification stays human-gated (§5c) — this one's a safety call more than a scope call, and it's worth keeping even after everything else above gets revisited.

---

## 1. Hardware inventory & role assignment

**No Pi 5 in the loop — no LAN connection to it available. Everything runs on the laptop.**

| Device | Specs | Role |
|---|---|---|
| **Laptop ("Grey")** | i5-11300H, 32GB RAM, Iris Xe (no dGPU) | Everything: wake word, STT, LLM orchestration (API calls), local TTS, memory (vector+graph+event-log), dashboard, tool-calling MCP servers, GUI/OS/browser control |

**Voice stack (optional module, added after the text-driven harness works — see §7 phase ordering):**
- **Wake word:** OpenWakeWord — CPU-only, low latency, custom trigger trainable in ~20 min. If it competes for CPU with everything else, fall back to push-to-talk.
- **STT:** whisper.cpp — use `base` or `small` (not `tiny`); benchmark both when you get to this module and pick whichever hits real-time comfortably on this i5.
- **TTS:** Piper — fast, CPU-native, no GPU needed.
- These three run alongside chromadb + Kùzu + the event log + the dashboard backend fine within 32GB RAM whenever voice gets added — nothing about the core harness design assumes voice is present.

**Kill criterion (applies once voice is added):** if running wake-word + STT + LLM calls + TTS + all memory layers + dashboard simultaneously causes noticeable lag, cut *continuous* wake-word listening first — fall back to push-to-talk, which removes the always-on audio processing overhead entirely.

---

## 2. Repos in play

| Repo | Role | Link |
|---|---|---|
| **This repo — the harness** | Fresh Rust core, written here. **Not** a fork of anything. See §2a for why. | (this directory) |
| **RustFox** — *reference only, cloned to `reference/`* | Single-binary Rust Telegram assistant: sandboxed tools, scheduling, SQLite + vector RAG, MCP integration, skills + sub-agents, and SOUL.md/AGENTS.md/USER.md auto-injected as persistent identity — an independent implementation converging on the same persona split as §5, found after the fact. **Evaluated as a base and rejected (§2a).** Read it for how it does persona injection, skill loading, and the scheduler; take the ideas, not the code. | https://github.com/chinkan/RustFox |
| **friday-tony-stark-demo** — *reference only, cloned to `reference/`* | Kept for its FastMCP tool-calling pattern as a conceptual reference (not code to port — `rmcp` handles this natively) and later as a reference for LiveKit's wiring when Phase 5 starts. LiveKit has an official Rust SDK, so voice doesn't force a language boundary either; confirm the crate when Phase 5 begins, not now. | https://github.com/SAGAR-TAMANG/friday-tony-stark-demo |
| **UACC** — *cloned to `mcps/UACC`* | Pixel-level GUI control — **70 tools connected**, `safe_mode=True` by default. Unaffected by the Rust move: it's a Python MCP server either way. Runs from its **own venv** (`mcps/UACC/.venv`, mcp v1) and needs one upstream syntax patch — both documented in `mcps/patches/README.md`. Otherwise used as-is, not rewritten. | https://github.com/chrisjaron03/UACC |
| **SnareVec** | Local capture → embed → search. MCP server at **`<snarevec>/pro/mcp/snarevec_mcp.py`** — **31 tools, 16 of them `browser_*` CDP tools** for real GUI-supported browsing (uses the user's own session and cookies, so logged-in pages load as themselves). Stdlib-only stateless proxy to a daemon on `127.0.0.1:8756`; **the daemon idles out and is restarted from the workbench.** Repo at `C:\Users\adity\Desktop\blueeClaw\git\broswer extension\snarevec`, data at `~\.snarevec\`. | (local repo) |

**Directory layout:** `mcps/` holds MCP servers the harness connects to. `tools/` holds CLI/agent tool software pulled from git. `reference/` holds read-only repos we learn from but never build.

**Architecture shape:** this repo is the harness (Rust: MCP client + persona loader + event log + memory, with an optional voice loop layered on later). UACC, SnareVec, and the graph server are MCP tool servers it connects to — same protocol, no rewrite needed. **This is what makes the language choice reversible and the tool servers language-agnostic.**

### 2a. Why a fresh core instead of extending RustFox

RustFox's feature list maps closely onto §5 and parts of §4, and extending it looked like the obvious shortcut. Measured against the actual codebase, it isn't:

- **It's a Telegram bot.** There is a `src/platform/` abstraction, so a CLI front-end is the intended extension point — but **22 of its 73 source files reference Telegram**. The abstraction is partial.
- **Its memory model contradicts §4a.** RustFox is SQLite conversation history + embeddings. Our load-bearing decision is append-only-log-as-source-of-truth with everything else *derived*. Retrofitting that means rewriting `src/memory/` — its largest subsystem, 7 files — which is the exact part you'd want to inherit.
- **No graph layer at all.** §4c would be built from scratch regardless.
- **27,882 lines, 5 stars, effectively one author, on `rmcp 0.15` against a current `3.1.4`** — several breaking majors behind. No community, no docs past the README.
- **Decisive factor:** the owner does not read Rust. Inheriting 27.8k lines nobody in the loop has read is a liability, not a head start. A purpose-built core that matches this document's architecture exactly is navigable; someone else's Telegram bot is not.

What it would have given free is the persona files — §5, Phase 3, the cheapest phase in the plan. Not worth the rest.

---

## 3. LLM / Provider config

- **LLM (the brain): cloud API only.** Use whatever's available via the repo's existing `LLM_PROVIDER` pattern; extend `.env` switching if needed.
- **STT / TTS / wake word:** local, per §1.

---

## 4. Memory architecture — three layers, one source of truth

The key architectural decision here (borrowed deliberately from how DeepSeek's own agent harness is built — see §9 research notes) is: **don't treat memory as several independent stores that each try to be authoritative. Make the event log the source of truth, and derive everything else from it.**

### 4a. Session event log (foundational — build this FIRST, everything else reads from it)
- An **append-only** log of every event in a session: user messages, assistant messages, tool calls, tool results, errors, system events — in order, nothing overwritten, nothing silently mutated.
- This is what gives you the "transparency" you were after — you can always answer "why did it do that" by reading the log backward, and sessions become genuinely replayable/debuggable instead of a black box.
- Store as JSONL (one event per line) on disk; this is your audit trail and also the substrate that the vector/graph layers and the skill-extraction step (§5c) are built from.
- **This is where TOON earns its place** (see §4d) — not as a database format, but as the encoding used when a chunk of this log (or a memory-query result) gets serialized back into the LLM's context window.

### 4b. Vector memory (semantic recall) — derived
- **`fastembed` 6.0.2** (MiniLM-class via ONNX, same model family as the original sentence-transformers plan) — **verified building on this machine.**
- **No vector database.** chromadb has no real Rust equivalent, and at this scale one isn't needed: 100k event-log chunks at 384 dims is ~153 MB resident, and scoring all of them is ~38M multiply-adds — single-digit milliseconds in Rust. Vectors are stored as BLOBs in the harness's SQLite and scored by brute-force cosine. This removes a dependency rather than compromising; it matches the §9 "small core" philosophy taken from Pi. Revisit only if the corpus grows past ~1M chunks.
- Populated by processing the event log (and SnareVec captures) into embedded chunks — not written to directly and separately, so it never drifts from what actually happened.
- **Chunking note:** MiniLM scored two clearly-related short phrases at only ~0.22 cosine during Phase 0 testing. Short abstract fragments embed badly — chunk the event log into fuller, context-carrying units rather than per-line.
- Tool exposed to LLM: `search_memory(query)`

### 4c. Graph memory (relationships) — derived
- **Kùzu** — embedded graph DB, single local file, no server process.
- Nodes: entities (apps, people, projects, files, preferences, screen-context snapshots). Edges: relationships (`used_with`, `mentioned_in`, `prefers`, `part_of`, `opened_after`).
- Also populated from the event log where relationship-worthy patterns appear (e.g. two tool calls in sequence → `used_with` edge), plus anything explicitly told to it.
- Tool exposed to LLM: `query_graph(entity, relation)`

**RESOLVED — kùzu runs as a Python MCP server (`mcps/kuzu-graph/`). Built and verified.**

*The problem:* the `kuzu` Rust crate does not link on this machine. Measured, not assumed — the C++ compiles fully (1,060 object files, `kuzu.lib` + `kuzu_rs.lib` + `libkuzu_rs.a` all produced) but the final link fails with **113 unresolved `kuzu_rs$cxxbridge1$*` symbols**. Root cause is a stale-crate-meets-current-toolchain mismatch: crate last published 2025-10-10, pins `cxx 1.0.138` against a current 1.0.199, on rustc 1.97.1. Two earlier failures on the way there were separate and are fixed — missing Ninja generator, then Windows `MAX_PATH` (project path is long and contains spaces and a tilde, hence `.cargo/config.toml` redirecting `target-dir` to `D:/tgt/harness`).

*The fix:* kùzu 0.11.3 works fine from Python, and the harness already speaks MCP to UACC and SnareVec — so one more MCP server costs nothing architecturally and keeps Cypher and kùzu exactly as specified above. **This is the payoff of §2's "everything is an MCP server" shape, and it retroactively justifies the 3.12 venv work.** Rejected alternatives: a SQLite edges table in the Rust core (loses Cypher, hand-written traversal), and fighting the linker (unbounded time in someone else's `build.rs`, and even winning leaves a pinned-fragile setup — a bad foundation for a daily driver the owner can't debug alone).

*Verified:* 5 tools registered and exercised over **real MCP stdio transport** (handshake, `tools/list`, `query_graph` round-trip), not just as importable Python. Repeat observations raise edge weight rather than duplicating edges. `cypher()` rejects writes, so the graph stays derived.

*Schema decision:* one `Entity` node table plus one `Rel` edge table carrying a `type` property, rather than a REL TABLE per relation type. Makes `query_graph` a single parameterised query instead of dynamic table-name building, and lets the reducer add relation types without a migration. Tradeoff: no per-type endpoint constraints — acceptable, the reducer is the only writer.

*Note on the MCP SDK:* the venv has **mcp 2.1.1**, where `FastMCP` was renamed `MCPServer`. The FastMCP pattern in the friday reference repo is v1 API — do not copy it verbatim.

### 4d. TOON — where it actually fits (correction on scope)
TOON (Token-Oriented Object Notation) is **not a storage format for chromadb or Kùzu** — both already have their own efficient native storage, and swapping their internals for TOON would be pure wasted effort with zero benefit. What TOON actually is: a compact, schema-aware way to *serialize JSON-shaped data for LLM prompt input*, reaching 30–60% fewer tokens than raw JSON on uniform/tabular data (e.g. a list of memory-search hits, a batch of graph query rows, a stretch of the event log) while staying human-readable.

**Use it at exactly one boundary:** whenever `search_memory`, `query_graph`, or a chunk of the event log gets formatted to go back into the LLM's context, encode that payload as TOON instead of JSON before it hits the prompt. In Rust this is a hand-rolled encoder of roughly 100 lines over `serde_json::Value` — a thin serialization step, not a new subsystem, and not worth hunting for a crate. Payoff: real token/cost savings on every single tool-result round trip, for near-zero build effort. Skip it if you're behind schedule — it's a cheap optimization, not a dependency for anything else working.

### 4e. Dashboard
A local web app to inspect all three memory layers live, and to control screen-vision mode (§6):
- **Backend:** `axum`, thin read layer over the event log + the SQLite vector store + the graph. Served by the same single binary as the harness — no second process, no second language, which is one of the concrete wins of the Rust core.
- **Frontend:** single-page app —
  - Graph view: Cytoscape.js or vis-network.js rendering Kùzu nodes/edges, filterable
  - Vector view: recent memory entries + a live `search_memory` box
  - Event log view: scrollable, readable transcript of the raw session log (this is your transparency window — literally show the append-only log, don't hide it behind a summary)
  - Screen-vision toggle (§6): the OFF / PASSIVE / ACTIVE control lives here
- Runs locally (`localhost:PORT`), no auth needed for solo use

### 4f. Workspaces + shared terminal — requested scope expansion

**Status: agreed in direction, not yet sized into a phase.** This is a genuine
increase over §4e's three read-only views, and it should be planned honestly
rather than folded in silently. §4e is a *window onto memory*; this is an
*operating surface*. Shape borrowed from the Claude Code GUI's four-pane
layout (terminal / editor / browser / menu).

**One adaptive surface, NOT separate workspace profiles.** This was revised by
the owner and the revision is better than the original design. Rather than N
siloed workspaces each with its own profile, there is **one main page that the
agent reconfigures on demand**. You name a topic; it composes the view —
pulling up the relevant tools, that topic's stats, and the work area where you
left off.

The reasoning is the important part: **siloed workspaces would hide the
interconnections between your work**, which is precisely what the graph exists
to reveal. Partitioning the memory into profiles would fight the memory model.
One continuous graph, many composed views of it, is the right shape.

Two consequences worth noting:
- **This removes the event-schema problem.** An earlier draft of this section
  wanted a `workspace` field on every event so the reducer could learn
  membership — a change to an append-only log, which is expensive to decide
  late. The adaptive design needs no such field: a "workspace" is a *query
  against the graph*, computed at view time, not a stored partition. Nothing
  about the event schema has to change.
- Tool scoping still applies, and still comes free: when the agent composes a
  view for a topic, it exposes only that topic's tools. Same mechanism as §6's
  vision gate and `HARNESS_TOOL_SERVERS`. This keeps the tool count sane —
  106 in one flat list is past what most models choose well from.

**Also on the dashboard (requested):**
- **Persona editing page** — edit SOUL/AGENTS/USER/TOOLS from the UI. Note
  §5c's rule still holds: USER.md may be written automatically, SOUL.md is
  human-gated. A UI editor is the human gate, so this is consistent.
- **LLM provider page** — configure *multiple* providers, set one default and
  an ordered fallback chain (fallback 1, 2, 3 …), each with its own model.
  This is a change to §3, which assumed a single provider. The `Provider`
  trait already abstracts this; what's new is a chain that retries down the
  list on failure. Worth building — a daily driver that dies because one
  provider 500s is not a daily driver.

**Shared terminal — confirmed in scope, not optional.** One terminal inside the
dashboard that both Adithya and the assistant can see and type into, with
tmux-style persistence so reopening after days resumes the same session.

- **Must not be PowerShell-only.** cmd, PowerShell, bash, and notably
  **`ssh` to the Pi 5** all have to work. This is free: a PTY runs whatever
  binary you hand it, so "which shell" is just the command it launches.
  **Note this reverses §1's "no Pi 5 in the loop"** — the Pi returns, over SSH
  from the terminal, rather than as a LAN service the harness depends on.
- `portable-pty` + `xterm.js` is the well-trodden path. For persistence,
  **wrapping real tmux is cheaper and more reliable** than reimplementing
  session persistence, which is its own project.
- **Safety:** a terminal the model can type into is strictly more dangerous
  than the MCP tools, which at least have typed schemas and UACC's `safe_mode`.
  The owner has stated he is building the guards for this himself, so the
  harness side should focus on making the model's actions *visible* — echo
  everything it types before it runs — rather than on inventing its own policy
  layer that would duplicate or fight his.

### 4f-b. Layout, as revised by the owner

The first dashboard put everything in one right-hand panel. Revised:

- **Left rail = pages.** Memory and Graph become their own left-side pages
  (toggled from the rail), not right-panel tabs.
- **Right panel = only Log and Tools.** Nothing else.
- **Stats: removed.** Not wanted.
- **Log shows the CURRENT session only**, not previous sessions. The full
  history stays available, but the working view is "what is happening now".
- **Tools: collapsed by server** — `kuzu_graph`, `snarevec`, `uacc` — expanding
  one shows its tools. 106 flat rows is unreadable.
- Possibly a browsing page later; the owner may just use his normal browser,
  so this is explicitly undecided and not being built yet.

### 4f-c. The playground — agent-built work surfaces

The most substantial new idea, and the concrete form of §4f's adaptive page.

A left-rail **playground** page: a work area where the assistant *builds things
that run*. Not chat output — artifacts. HTML dashboards, small live-fetching
services, charts wired to a real API. The stated example: give it trading API
credentials, ask for a live candle chart plus the news context you discussed,
and it writes the code and the surface renders there.

**The part that makes it more than a scratchpad: these get saved into memory.**
Say "we're doing trading" and it restores that surface — the charts it built
last time, the artifacts, and the context of where you left off. Same for a Git
project, or anything else. A *work profile* that resumes, rather than a blank
page each session.

This is why §4f rejects siloed workspace profiles: the graph holds one
continuous picture, and a "workspace" is a **view composed from it on demand**,
including the artifacts previously built for that topic. Artifacts therefore
need to be first-class: stored, associated with topics in the graph, and
re-openable.

Open questions to settle before building: where artifacts are stored (files on
disk vs blobs), how a running artifact-server is sandboxed, and how "restore
the workspace" is triggered (explicit command vs the model deciding).

### 4f-d. Requested next (recorded, not yet built)

Captured so none of it is lost. Ordered by my read of value, not by the order raised.

**1. Session access — BUILT.** Sessions page in the left rail: every conversation,
newest first, each showing its opening line, message and tool-call counts, and
when it last ran. Three actions per session — **Open & resume**, **Read only**,
**Delete**.
- `Agent::resume` reopens the existing event log and *appends* to it, so the
  session id stays the same and history is genuinely continued rather than
  copied. Verified: resumed a session and asked "what number did you just tell
  me?" — it answered `17`, from a turn in a previous process.
- **Only user/assistant text is replayed into the prompt**, not the
  tool_call/tool_result pairs. Reconstructing those means re-emitting matching
  ids in exactly the shape the provider expects, and one mismatch gets the whole
  request rejected. Nothing is lost — the tool detail stays in the event log and
  is reachable via `search_memory` — it just doesn't go back verbatim.
- Delete is **two-click armed** and one-at-a-time, with no bulk sweep, because it
  destroys source-of-truth data. It also clears the live agent if that session
  was the active one, and says to re-run `reduce` to drop it from memory too.

**2. Right panel is Log + Tasks — BUILT.** `/api/tasks` + a TASKS panel replacing
TOOLS. Current session only: every call with its **duration**, a one-line
**reason**, a status dot (done / failed / running), and click-to-expand showing
full arguments and result. Header summarises count, total time, failures and
anything still running. Refreshes live as events stream in.
- **Duration needed no schema change** — `tool_call` and `tool_result` are
  timestamped and paired by `call_id`, so it is a subtraction.
- **Reason required one.** The model routinely explains itself before calling,
  but that text only ever went into the prompt, never the log — so the record
  showed *what* ran with no trace of *why*. The agent now logs it. It is a true
  thing the assistant said, not a generated justification, and calls with no
  explanation show "no stated reason" rather than an invented one.
- First implementation consumed the reason on the first call of a batch, leaving
  the rest looking unexplained when the model explains once and then calls
  several tools together. The reason now applies to the whole batch and is
  replaced only when the assistant speaks again.
- **The panel toggle moved to the top bar**, right of "new chat".
- The *tool catalogue* that used to live here moves to the Settings → Tools &
  Skills page (item 4); this panel is about activity, not inventory.

**3. Settings: MCP page — BUILT.** Every configured server with command, args,
env and an enabled toggle, plus **whether it is actually connected right now**
and how many tools it contributed. Configured and connected are different things
- a server can be enabled in the file and still have failed to start - and the
page is far less useful if it cannot tell you which. Add/remove/save writes
`mcps/servers.json`; changes apply on restart, which the page says plainly.
- **Saving preserves the `$comment` keys.** First version lost them: the Rust
  side carried unknown keys through `#[serde(flatten)]`, but the UI is only
  shown editable fields, so it had nothing to round-trip and pressing Save
  quietly deleted the notes explaining why each server is configured as it is
  (verified: 4 comments went to 1). Now the merge happens **server-side** when
  writing, so the client never needs to know they exist. Re-verified 4 → 4.

**4. Skills themselves — BUILT (§5c Tier 1). The settings *page* is not.**
`src/skills.rs` + `save_skill` / `list_skills` / `run_skill` + `/skills`.
- **A skill is a recipe, not code.** `run_skill` executes nothing — it returns
  the steps and the model carries them out with its normal tools. A skill that
  ran itself would be a second, invisible way for things to happen, and the log
  would show one opaque call instead of the real actions. This way every action
  a skill causes is still a logged tool call. The tool result says so explicitly
  so the model doesn't report a skill as "done" merely for fetching it.
- Stored as plain Markdown, one file per skill, readable and editable outside
  the app. Four categories as directories: `skills/`, `recorded/`, `toolkit/`,
  `proposed/`. Re-saving into a different category **moves** the file rather
  than leaving a duplicate (covered by a test).
- **`proposed/` is the safety catch** that would make §5c Tier 2 mining safe to
  switch on: candidates land there and nothing is offered to the model until
  promoted into `skills/`.
- Lives at the repo root, not under `data/` — skills are meant to be read,
  edited and version-controlled, unlike derived state.
- **Verified with the real model:** told it a recovery procedure and said
  "remember that as a skill"; it called `save_skill`, wrote
  `skills/skills/recover-snarevec-search-failure.md`, `/skills` listed it, and
  `run_skill` returned the steps.

**The settings page is now built too.** Category-grouped list on the left,
editor on the right: name, category, one-line description, tools used, and the
Markdown body. Save / Copy / Delete (delete is two-click armed). Moving a skill
between categories moves the file rather than duplicating it. Dropping `.md`
files onto the list imports them into `toolkit`.

**5. Slash commands — BUILT.** Handled by the harness, never sent to the model:
they are about the session itself, and paying a round trip to be told what
`/help` says would be silly. Typing `/` opens a menu in the composer with
arrow-key/Tab selection.
`/compact`, `/context`, `/sessions`, `/mcp`, `/reduce`, `/help`; `/skills` exists
and says honestly that skills are not built yet rather than failing.

**6. `/compact` — session memory — BUILT and proven.**
Folds the current session into searchable memory and shrinks the prompt. Nothing
is summarised and nothing is lost: the event log already holds every character,
so "lossless" is true by construction — compaction is about what stays in the
*prompt*, not what is kept on disk.
- Vector chunks gained a **`scope`** column: `global` (rebuilt wholesale by the
  reducer) vs `session` (written by `/compact`). `reduce` now clears only
  `global`, so a rebuild cannot wipe what a live conversation is leaning on.
  Re-compacting replaces that session's chunks rather than duplicating them.
- The prompt is trimmed to the system message, a marker telling the model *why*
  its history looks short and to use `search_memory`, then the last `keep`
  messages. The tail never starts on a `tool` message — that would be a result
  with no matching call and the provider would reject the request.
- **Proven end to end.** Stored a fact, ran `/compact 0` so *nothing* of the
  conversation stayed in the prompt (7 messages out, ~5835 → ~3710 tokens), then
  asked for the fact back. It called `search_memory` and returned
  `CINDER-77-OAK` exactly — recall from session memory, not from context.
- **Honest finding: at this scale the tool schemas dominate the prompt, not the
  conversation.** With one MCP server the schemas are ~3.5k tokens on their own,
  so compaction barely moves the number on a short chat. `/compact` is what
  saves a *long* conversation; the lever for the baseline is tool scoping
  (`HARNESS_TOOL_SERVERS`, and eventually §4f's per-topic tool exposure).
- A context meter sits in the top bar, amber at 70% and red at 80%, clicking it
  pre-fills `/compact`. The budget is advisory — the real ceiling is model
  specific and this is a ~4-chars-per-token estimate.
- Compaction is written to the event log as a new `System` event kind, so the
  record shows it happened.

**7. UI/UX pass at the end**, using Framer for components, transitions and
animation. Explicitly deferred — current components are placeholders and known to
be weak; do not over-polish before then.

**8. `USER_GUIDE.md` — WRITTEN.** The owner said, fairly, that he understood how
the commands were built but not how to *use* them. Everything had been documented
as build rationale (this file) and nothing as usage. The guide covers starting
it, every page and panel, all slash commands with `/compact` given the most
space, how memory actually behaves (including the non-obvious bit: search only
covers what `reduce` has indexed), sessions, playground, skills, the terminal,
settings, troubleshooting, where files live, and a plain list of what is not
built yet. Keep it current — it is the thing that makes this usable by someone
who did not write it.

**9. Provider settings beyond the model id — BUILT.** The Providers page now
carries **context length**, **temperature**, **top_p** and **request timeout**
per provider, alongside the existing reply cap.

- **The two numbers people conflate are now separated on the page, with the
  reason written next to them.** `max_tokens` caps the *reply*; `context_window`
  is how much the model holds at once. Setting the cap to the full window is the
  exact mistake that produced the earlier `402 … you requested up to 100000
  tokens, but can only afford 8048`, because the provider reserves the cap up
  front. The context window is **advisory only** — it drives the meter and
  nothing else, and is never sent in a request.
- **Temperature and top_p left blank are omitted from the request entirely**
  rather than defaulted to a number the harness invented, so the model's own
  tuning stands. `#[serde(skip_serializing_if = "Option::is_none")]` on both.
- **Timeout is per provider and per request**, and it is what makes the fallback
  chain mean anything: a provider that hangs now falls through to the next one
  instead of freezing the turn indefinitely.
- **`detect` reads the real figure off the endpoint** (`GET /v1/models` via a new
  `/api/models`) rather than asking you to look it up. aicredits.in publishes
  `context_length`; a provider that does not gets an honest "this provider does
  not publish a context length for it" instead of a filled-in guess.

**10. The context meter now measures against the real window — 1,000,000.**
Verified against the provider's own model list, not assumed:
`qwen/qwen3.8-27b` reports `"context_length": 1000000`. The meter had a
**hardcoded 32,000** in `current_session`, so it was reporting ~72% full when the
session was using under 3% of what the model can actually hold — and prompting
`/compact` for no reason. It now comes from the active provider's configured
window. `LLM_CONTEXT_WINDOW` is the `.env` fallback; it defaults to a
conservative 32,000 when unset rather than guessing high.

**Worth being straight about what this does and does not change:** it removes a
false alarm, it does not make long prompts free. A 1M-token prompt costs 1M
tokens. `/compact` is still the right move on a long session, just no longer at
23k.

**11. Session titles — BUILT.** Shown top left next to the name, click to
rename; also renameable from the Sessions page, where the title now heads each
card with the opening line beneath it.

- **A title is an event, not a stored field.** `EventKind::SessionTitle` is
  appended, so renaming writes a new one and the last wins. Nothing is
  overwritten and the rename history stays in the log — the §4a property holds
  for this the same as everything else.
- Untitled sessions fall back to the first line of the opening message, so the
  list is readable without anyone naming anything.
- The rename goes through the live agent when it owns that session, and opens
  the log directly when it does not — two writers on one file would break the
  monotonic sequence.

**12. Playground prompt box — BUILT.** A composer at the bottom of the
Playground page, so you can build and revise artifacts without switching to
Chat.

- **It is the same conversation, not a second one.** It routes through the same
  websocket, agent, session and event log; what you say there is context in Chat
  and every tool call is still logged exactly once. A separate playground
  conversation would have meant a second history the graph and the reducer knew
  nothing about.
- The strip above it mirrors the turn — tool calls as they happen, then the first
  line of the reply — and the artifact list reloads when the turn finishes, so
  something built appears without pressing refresh.

**13. The `500 Internal Server Error` — DIAGNOSED AND FIXED. Root cause: the
provider cuts every non-streamed request off at ~30 seconds of wall clock.**

Reported as "it crawled a site, ran five searches, then died, and the next
message died too". The error text was `all 1 provider(s) failed: … provider
returned 500 Internal Server Error`, which says nothing.

*What the log gave away:* both failures took **exactly 30 seconds** —
20:16:41→20:17:11 and 20:18:38→20:19:08. Twice in a row to the second is a
deadline, not a fault.

*Measured against the live endpoint, not assumed:*

| request | result |
|---|---|
| tiny prompt, short answer | 1.4s, 200 |
| tiny prompt, **4000-token answer** | **30.4s, 500** — three for three |
| the real failing request, 118 KB, 106 tools | 30.5s, 500 — three for three |
| the same request, `stream: true` | **49.4s, 200** |
| tiny prompt, 3000-word essay, `stream: true` | **362.8s, 200**, 20,306 chars |

Every failure landed at 30.3–30.6s; every success came in under 30s. A *tiny*
prompt with a long answer fails too, so **this was never about context size** —
it is a wall-clock deadline on the whole request. The five searches only made it
likely by making generation slow enough to cross the line.

*Fix, in `src/llm.rs`:*
- **`stream: true` by default.** This is not a UI nicety here — it is what makes
  a long turn possible at all. Note the provider still buffers: on the 362s
  request the first and last tokens arrived 0.1s apart at the very end. Nothing
  is gained in responsiveness; what is gained is that the request survives.
- **SSE reassembly is index-based**, because several tool calls in one turn
  interleave their fragments, and the arguments arrive in pieces that must be
  concatenated rather than replaced. Unparseable frames are skipped rather than
  fatal — providers sprinkle keepalives through these streams and one unknown
  line must not lose a turn.
- **Transient failures retry against the same provider** before the chain falls
  through. 5xx/429/timeout retry; 401 and 402 do not, since those will not fix
  themselves. Errors now carry elapsed time and attempt number.
- Both are per-provider settings on the Providers page, with `stream` an escape
  hatch rather than a preference.

*Verified through the harness, not just by curl:* a tool call round-tripped over
streaming, three parallel tool calls came back with correct distinct ids and
names, and a 55-second answer completed where the same shape used to 500. Four
unit tests cover the reassembly against frames captured verbatim from the
provider, including the split-arguments and `"content": null` cases.

**Still true and still worth doing: cut the tool count.** The cost data makes it
concrete — 106 tool schemas are **~18,300 prompt tokens on every single turn**,
about **₹0.93–1.06 per call** before you have said anything. 30 tools costs
₹0.28. This is now the top item.

### 4g. Desktop app, not a web page

Requested, and correct: the dashboard should be a real local desktop
application. **Use Tauri.** Rust backend (the harness already is one), web
frontend, ships as a genuine desktop app at ~10 MB rather than Electron's
~150 MB, and keeps everything one language and one binary — which was a stated
reason for choosing Rust in the first place.

Rejected: a pure-Rust GUI toolkit (`egui`/`iced`). It would mean giving up
`xterm.js` for the terminal and Cytoscape for the graph, and hand-rolling a
terminal emulator. Bad trade for a solo build.

**Sequencing recommendation:** §4e's read-only views first, then the adaptive
main page, then the terminal. Each is independently useful, and the terminal is
the one carrying real safety design.

**Validation checkpoint:** once all three memory layers have real entries, spend 10 minutes actually querying them for things you'd realistically ask. If retrieval quality is bad, fix chunking/embedding before dashboard polish — a pretty dashboard over bad retrieval is validation theater.

---

## 5. Persona — split across files, OpenClaw-pattern, name configurable

Researched OpenClaw's actual file-split (SOUL.md / AGENTS.md / USER.md / TOOLS.md / MEMORY.md) because it's a genuinely well-tested pattern for exactly this problem — not reinventing it, adopting it with one deliberate change explained below.

### 5a. The files (all plain Markdown, all loaded into system prompt at session start)

**`SOUL.md`** — personality, tone, values, hard boundaries. The character sheet. Keep this **short — 200-500 words is the right size**, per how OpenClaw's own community guidance frames it; longer dilutes instruction-following, not strengthens it. Include a `name:` field at the top so the assistant's name is a one-line edit, not a rename-everywhere refactor.

```markdown
# SOUL.md
## Who You Are
Name: [configurable — pick anything, not locked to "FRIDAY"]
You're a personal assistant for Adithya — direct, competent, dry wit, never
performatively cheerful. You have opinions; you're allowed to disagree.

## Tone
Concise unless asked to elaborate. Reference past context naturally — never
narrate "recalling memory" or "searching database."

## Hard Limits
- Never execute destructive shell commands (rm, format, kill -9 on unknown
  processes) without explicit confirmation.
- Never fabricate system state — check via tools first, always.
- Never volunteer screen contents unprompted; mention only when relevant.
```

**`AGENTS.md`** — operating instructions, task-handling workflow, how to use tools together. This is "how to work," separate from "who you are" — mixing procedure into SOUL.md is the exact anti-pattern OpenClaw's own docs warn against, because it makes both files harder to maintain independently.

**`USER.md`** — facts about Adithya: hardware (this laptop's specs), working style, standing preferences. Pre-seed this at setup rather than leaving it empty — an empty USER.md means every session starts from zero context about you, and five minutes filling it in now saves re-explaining things for weeks.

**`TOOLS.md`** — tool usage policy: which tools exist (UACC, SnareVec, custom OS tools, memory tools), when to prefer which, and the safety notes that apply across all of them (mirrors the guardrails already defined for UACC/SnareVec individually, collected here so the LLM sees one coherent policy).

**No separate static `MEMORY.md`.** Confirmed this is the right call for this build, not a shortcut — OpenClaw's own version of this file is a bootstrap/seed mechanism for harnesses that don't have a real DB-backed memory system; since §4 gives this build actual vector + graph + event-log memory, a static Markdown memory file would just be a second, unsynced source of truth. The one thing worth borrowing from OpenClaw's MEMORY.md advice: **seed the graph DB with a handful of known facts at setup** (timezone, naming conventions, key projects) rather than starting from an empty graph — same benefit, right layer.

### 5b. Loading order
SOUL.md first (persona should dominate early attention), then AGENTS.md, then TOOLS.md, then USER.md, then relevant memory pulled in per-turn. Build once at session start; don't mutate the assembled prompt mid-conversation (this also keeps you compatible with prompt caching if your provider supports it — Hermes's harness docs specifically call this out as an economic reason, not just an aesthetic one).

### 5c. Self-evolution — scoped, not automatic-by-default
Two tiers, build the first, treat the second as a stretch goal:

- **Tier 1 (build this): explicit skill-saving.** When told something like "remember this as a skill" or "save this sequence for later," write the relevant recent slice of the event log into a `skills/` folder as a named, reusable procedure (a markdown file describing the steps + which tools it calls). Expose a `list_skills()` / `run_skill(name)` tool pair so future sessions can invoke saved procedures directly instead of re-deriving them. This mirrors UACC's own `create_workflow`/`run_workflow` pattern — consistent with a tool the LLM already knows how to think about.
- **Tier 2 (stretch, only if ahead of schedule): automatic pattern mining.** Periodically scan the event log for repeated successful tool-call sequences and propose new skills unprompted — this is the pattern Hermes's harness implements as a full "generate → recall → optimize" loop, and it's genuinely the most complete version of this idea in the open-source ecosystem right now, but it's real infrastructure (pattern detection, dedup, a review step) — don't attempt it until Tier 1 and the rest of Phase 1-2 are solid.
- **Persona changes stay human-gated.** USER.md can be updated automatically as the assistant learns preferences (low risk — it's facts, not values). **SOUL.md should not self-modify.** OpenClaw's own documentation flags SOUL.md as the #1 attacker target in that ecosystem — a compromised or silently-drifted persona-core file is a permanently hijacked agent. If you want the assistant to be able to suggest persona changes, have it propose the diff and require you to apply it manually (or via a real git commit you review), never write to SOUL.md directly at runtime.

---

## 6. Screen understanding — three modes, dashboard-controlled

**Design principle (unchanged from earlier decision, now made explicit as a control, not just a default):** continuous VLM analysis is never the default and never silent. The dashboard (§4e) exposes a toggle with three states:

- **OFF** — no capture at all.
- **PASSIVE (default)** — periodic screenshot + OS accessibility tree read (reuse UACC's `get_screen_info`/`get_screen_info_enhanced`) + OCR fallback. Text-only, no vision model, cheap, local. Indexed into the event log → vector memory as timestamped "screen context." This is what's running unless you change it.
- **ACTIVE** — unlocks the VLM path, and *within* Active there are two ways it gets used, both opt-in:
  - **Manual trigger:** you ask "what am I looking at" / hit a dashboard button → one screenshot → cloud VLM call → real visual read. Expensive path, used on demand.
  - **Agentic trigger:** the LLM itself is given an `analyze_screen_vlm()` tool it can call when it decides passive text context isn't enough — but this tool is only present in the exposed toolset when Active mode is on. In Passive or Off mode, the tool doesn't exist for the LLM to call at all, so there's no chance of it reaching for vision when you haven't authorized the cost/latency for that session.

This gets you both things you asked for — an agent-usable vision tool, and a human-controlled toggle — without them conflicting: the toggle is the gate, agentic use is what happens inside the gate when it's open.

**Kill criterion:** if Passive capture meaningfully impacts responsiveness of anything else, drop capture frequency (e.g. every 30–60s) before cutting the feature.

---

## 7. Build phases — text-first harness, memory, logs, dashboard, in that order

Voice is not in Phase 0-3 at all. It's Phase 5, explicitly optional, and nothing in Phases 0-4 depends on it existing.

### Phase 0 — Environment + minimal text-driven harness
- [x] **Python version — RESOLVED, measured not assumed.** The laptop's system Python is 3.14, and the concern was that `chromadb`, `sentence-transformers`/torch, `kuzu`, and `onnxruntime` might lack wheels for a brand-new CPython minor. Tested directly: **three of the four are fine on 3.14** (chromadb ships a `cp39-abi3` wheel, torch 2.13.0 and onnxruntime 1.29.0 both have cp314 wheels). **`kuzu` is the sole blocker** — kuzu 0.11.3 publishes cp314 wheels for Linux/macOS but its **Windows** wheels stop at `cp313`, and there is no sdist to fall back on, so `pip install kuzu` on Windows 3.14 fails with "no matching distribution."
  - **Resolution:** `uv` installed (via `pip install uv`), CPython **3.12.14** installed through it, project venv at `.venv`. Chose 3.12 over 3.13 (kùzu supports both) because it's where the whole torch/chromadb/sentence-transformers stack is most heavily exercised.
  - **Verified working in `.venv`:** chromadb 1.5.9 (add + query round-trip), kuzu 0.11.3 (node table + rel table + Cypher traversal), onnxruntime 1.29.0 (CPUExecutionProvider present), torch 2.13.0+cpu, sentence-transformers (all-MiniLM-L6-v2, 384-dim, cached locally), anthropic 1.2.0, mcp, fastapi 0.141.1.
  - **Note for the §4e retrieval checkpoint:** MiniLM scored two semantically-related short phrases at only ~0.22 cosine. Short abstract fragments embed poorly — this is an argument for chunking the event log into fuller, context-carrying chunks rather than per-line, and worth re-checking when retrieval quality gets validated.
  - `uv python install` prints a "Missing expected target directory for Python minor version link" warning on Windows — cosmetic, the interpreter installs and the venv builds from it correctly.
  - **The 3.12 venv is still needed — its job changed.** It is no longer the harness environment; it is the *tool-server* environment. UACC is a Python MCP server, SnareVec is a Python MCP proxy, and the graph server (§4c option 1) would be Python too. None of that work is wasted.
- [x] **Rust toolchain verified.** rustc/cargo 1.97.1, `x86_64-pc-windows-msvc`, VS 2022 BuildTools present (`cl.exe 14.44.35207`). Ninja installed via `pip install ninja` (lands in `%APPDATA%\Python\Python314\Scripts`, not on PATH by default — export it for any cmake-based crate).
- [x] **Rust dependency de-risking — done before writing code, deliberately.** `fastembed 6.0.2` + `ort 2.0.0-rc.13` + `rmcp 3.1.4` + `tokio` all compile clean (~7 min cold). `kuzu` does **not** link — see §4c. Note `ort` is a release candidate, not stable; it's what 1.76M recent downloads run on, but it is the one pre-1.0 link in the chain.
- [x] `git init`, `.gitignore`, `mcps/` + `tools/` + `reference/` created, RustFox + friday-tony-stark-demo + UACC cloned
- [x] **Event log (§4a) built first, ahead of the turn loop** — deliberate reordering. It's ~200 lines and having the loop emit events from its very first run means nothing gets retrofitted. `src/eventlog.rs`: append-only JSONL, per-event flush, monotonic seq, sequence resumption across restart, replay-from-disk. 2/2 tests passing.
- [x] **Text-in / text-out loop working, and then some.** Provider is `https://aicredits.in/v1` (OpenAI-compatible, **458 models**), key in `.env` (gitignored). Note "Qwen 3.8 27b" does not exist on it; currently running `qwen/qwen3-30b-a3b-instruct-2507` — **confirm the intended model.**
  - Because both aicredits.in and OpenRouter speak the OpenAI dialect, the eventual OpenRouter move is a `.env` edit, not a code change.
- [x] **Model-driven tool calling verified — the §11 core loop.** Asked "how many entities and relations are in my graph memory?"; the model chose `kuzu_graph__graph_stats` itself, called it, and answered correctly (8 entities, 5 relations, 5 tools + 3 servers). The event log captured the whole trace: user message → tool call → tool result → reply.
  - Tool loop is capped at 8 rounds — a runaway loop burns money silently, so it is bounded rather than trusted. Hitting the cap is logged, not swallowed.
  - A failed tool is reported back to the model rather than aborting the turn, so it can recover by trying another.
  - `HARNESS_TOOL_SERVERS` narrows the exposed toolset. **106 tools in one flat list is past what most models choose well from, and their schemas are a large slice of every prompt.** This is the same gating mechanism §4f (adaptive workspace) and §6 (vision gate) will use.
- [x] **MCP tool-calling wired via `rmcp` 3.1.4 and round-tripped** — `src/mcp.rs`. The Rust harness spawns the Python kùzu server as a child process, handshakes, lists tools, and calls them: `harness tools` and `harness call <server__tool> '<json>'`. Verified against a real server, not a toy one, and **proven independently of the LLM** — which is why this checkbox closed while credentials were still outstanding. Every direct call is written to the event log exactly as a model-driven call would be, so the log stays a complete record of what touched the system.
  - Tools are namespaced `server__tool`. Not cosmetic: UACC alone exposes 68 tools and bare names will collide once several servers connect. `__` keeps ids inside the `^[a-zA-Z0-9_-]+$` charset providers require.
  - A server that fails to start is reported and skipped rather than aborting startup — one broken tool server shouldn't take the assistant down.
  - Servers are declared in `mcps/servers.json` (kùzu enabled; UACC and SnareVec present but disabled pending their install steps).
  - **Still to do:** hand the tool list to the model and let *it* choose calls. That needs the provider, so it lands with the chat loop.
- [x] **UACC registered — 70 tools.** Two blockers, both now fixed and documented in `mcps/patches/README.md`:
  - **UACC needs its own venv.** It is written against **mcp v1** (`FastMCP`) while `mcps/kuzu-graph/` needs **mcp v2** (`MCPServer`); they cannot share an interpreter. UACC therefore runs from `mcps/UACC/.venv` (which is what UACC's own `uacc-mcp.bat` already expects). Separate processes, separate Pythons — a concrete payoff of the MCP boundary.
  - **Upstream syntax error.** `uacc/actions/artistic_painter.py` does not parse (orphaned `if is_facial:` from a bad merge), and `uacc_mcp/server.py` imports it unconditionally — so one broken file took the whole 68-tool server down. Broken in *every* upstream commit that ever touched it, so there was no good commit to pin to. Patched minimally and reproducibly; the lost classification logic is deliberately **not** reconstructed. Blast radius is MS Paint drawing only. Worth reporting upstream.
- [x] **SnareVec registered — 31 tools, 16 of them `browser_*` CDP tools** (navigate, click, type, fill_form, execute_js, query, screenshot, scroll, wait_for, tab management). Runs from the harness `.venv`; it is stdlib-only so it needs no packages.
  - **Runtime caveat:** it is a stateless proxy to a daemon on `127.0.0.1:8756` that **idles out and is restarted from the SnareVec workbench**. When the daemon is down the server still starts and still lists tools — only calls fail, and they fail *well*, returning a readable `status: NOT RUNNING …` tool result rather than a transport error. TOOLS.md (§5a) should tell the assistant to check `snarevec__snarevec_status` before concluding a search capability is broken.

**Total connected: 3 servers, 106 tools** (kuzu_graph 5 + uacc 70 + snarevec 31), verified via `harness tools`, with a real read-only call round-tripped through each.

**Checkpoint:** you can type something that requires a tool call and get a correct result back, entirely in text. That's "the harness" working — everything after this is additive.

### Phase 1 — Memory: event log, vector, graph
- [x] ~~Append-only event log writer~~ — **built in Phase 0**, see above. It is also the tool-usage log; no separate logging system, same file.
- [x] **The reducer** (`src/reduce.rs`) — **built and verified.** Reads the event log, rebuilds vector + graph. It is a **full projection, not an incremental update**: every run clears both derived stores and rebuilds from scratch, so "delete the derived stores and re-run" reproduces them exactly. Verified by doing precisely that — entity/relation counts identical across three runs including a from-scratch rebuild.
  - Cost of that choice: rebuild time grows with the log. Fine at personal scale; when it stops being fine, add a watermark for incremental runs but keep the full rebuild to check the incremental path honest.
  - **Honest limit on graph extraction:** deterministically we can only see what the log *structurally* records — which tools ran, on which server, in what order. That gives `tool`/`server` entities, `part_of`, and §4c's own example of consecutive calls → `used_with` (repeats raise weight). Richer entities (people, projects, preferences) need the LLM or explicit teaching. We deliberately do **not** guess at them: a graph of hallucinated entities is worse than a small true one.
- [x] **`fastembed` + SQLite BLOB vectors + brute-force cosine** (`src/memory.rs`) — built, 5/5 tests passing. Vectors are L2-normalised on insert so cosine is a plain dot product at query time. `harness search "..."` works; ranking is sensible (a graph-related query returns the graph calls at 0.58, a monitor query returns `list_monitors` at 0.37).
  - **Chunking follows the §4b note:** chunks are whole *turns* (user message + assistant reply + the tool calls between), not single lines, because short fragments embed badly.
- [x] **`search_memory` exposed to the model and verified.** `src/tools.rs` holds the harness's *native* (in-process) tools — memory lives inside the harness, so routing it back out over MCP to read the same SQLite would add a process boundary for nothing. MCP is for external capability; this is for the harness's own. Verified: asked "what did I ask you about append-only event logs earlier?", the model called `search_memory` itself and recalled the exchange **from a different session**, quoting both the question and its own prior answer.
  - Native calls are logged with server `harness`, so the event log records them exactly like any MCP call and the trace stays complete.
  - The embedding model loads lazily on first search — most turns never search, so there's no reason to pay that pause at session start.
- [x] **Graph writes withheld from the model** (`WITHHELD_FROM_MODEL` in `src/tools.rs`). The model gets `query_graph`, `graph_stats` and `cypher`, but **not** `upsert_entity` / `upsert_relation`. The reducer is the graph's only writer; if the model could write directly, those facts would either be erased by the next rebuild or survive with no evidence behind them in the log — and "the event log is the source of truth" would stop being true. `cypher` is safe to expose because the server rejects writes by construction.
- [x] ~~Graph schema + `query_graph` tool~~ — **built** as `mcps/kuzu-graph/`, verified over stdio MCP. Still to do: have the reducer *populate* it from the event log (the server is the store; the reducer is what fills it).
- [ ] TOON serialization applied at the tool-result → context boundary (§4d) — quick add once the above work, skip if behind schedule

**Checkpoint:** ask it something that requires each memory layer; verify retrieval is sensible and the event log genuinely shows the full turn-by-turn trace, including tool calls, when you read it back.

### Phase 2 — Dashboard
- [x] **`axum` backend** (`src/dash.rs`) reading the event log + the SQLite vector store + the graph. `harness dash [port]`, default 7777. Routes: `/api/stats`, `/api/sessions`, `/api/events`, `/api/search`, `/api/graph`. Verified live against real data (13 sessions, 50 events, 9 chunks, 8 entities, 5 relations).
  - Only the graph server is spawned for the dashboard — booting UACC and SnareVec just to render read-only views would be slow and pointless.
  - Errors return a JSON body, not a bare status code: a dashboard that says "500" teaches you nothing about your own system.
- [x] **Frontend** (`dash/index.html`) — four views: overview cards, event log (colour-coded by event kind, session picker), memory search, and the graph.
  - **No CDN.** The graph is a hand-rolled force layout on `<canvas>` (~60 lines) rather than Cytoscape/vis-network, because this has to keep working offline once wrapped as a desktop app (§4g). Single self-contained HTML file, `include_str!`'d into the binary — still one binary, no static file serving.
- [x] **Rebuilt as a workspace, not a memory viewer.** The first version was §4e as literally written — three read-only panels — and it was correctly called out as weak: no chat, no input, no send, no voice, no attachments. §4e's spec predated everything decided since. The rebuild puts **chat first**, with an icon rail, a collapsible side panel (stats / memory / graph / log / tools) and a terminal drawer.
  - Composer: textarea, send, file attach (text files — images need vision, Phase 6), and a mic button **rendered disabled with a tooltip** rather than faked, because a button that silently does nothing is worse than an honest one.
  - `src/agent.rs` extracts the turn loop from the CLI so the dashboard, the CLI and later voice all drive the **same** loop rather than three parallel implementations. §11 requires exactly this for voice; the same argument applies to the dashboard.
- [x] **Shared terminal (§4f) built and proven.** `src/pty.rs` + `portable-pty` + xterm.js over a websocket.
  - **Any shell:** powershell, cmd, bash, or a custom command — the shell string is split on whitespace, so `ssh pi@host` works as a terminal entry. This is how the Pi 5 comes back.
  - **Sessions survive disconnect**, which is the whole point. Verified decisively: a loop was at TICK-4 when the socket closed and at TICK-12 when it reattached, having kept running with nobody watching, with full scrollback replay (256 KB ring).
  - Two real bugs found and fixed: the child process handle was bound to `let _child` and dropped at the end of `spawn()`, and ConPTY blocks on an unanswered `ESC[6n` cursor query — xterm.js answers it automatically, a naive client does not.
  - xterm.js is **vendored locally** (`dash/vendor/`), not pulled from a CDN, so the desktop app works offline.
- [x] **`max_tokens` now sent explicitly** (`LLM_MAX_TOKENS`, default 4096). Without it the provider reserves the model's whole context and a low credit balance refuses the request outright: *"You requested up to 100000 tokens, but can only afford 8048."* This is a real operational trap on a metered account, not a niceties fix.
- [x] **Wrapped in Tauri — `harness app` opens a real desktop window** (`src/app.rs`). Verified running: window process alive, page served, MCP servers connected, seeded graph live.
  - `dash::serve_on()` takes an already-bound listener so the desktop shell can **bind port 0 first**, learn the real port, then point the window at it. The CLI `harness dash` path still binds a fixed port.
  - **Ephemeral port, not 7777.** Loopback was never internet-reachable, but a fixed known port is findable by other local programs; the OS-assigned one is not.
  - Tauri owns the main thread (OS event loops require it); the server runs on its own tokio runtime in a background thread.
  - `tauri-build` requires `icons/icon.ico` on Windows even with minimal bundling — generated one rather than disabling the bundle.
- [x] **Layout restructure (§4f-b) — done.**
  - Left rail is now **pages**: Chat / Memory / Graph, plus toggles for the terminal and the side panel. Memory and Graph get the whole main area instead of a cramped 340px column.
  - Right panel is **Log and Tools only**. Stats panel removed entirely.
  - **Log is scoped to the current session.** Needed a new `/api/session` endpoint: the agent is created lazily on first message, so before that there is genuinely no current session and the panel says so rather than showing stale history.
  - **Tools collapse per server** — `kuzu_graph 5` / `snarevec 31` / `uacc 70`, expanding one to reveal its tools. 106 flat rows was unreadable.
  - Graph nodes are now coloured by entity kind (person / machine / project / preference / tool / server) with a generated legend, which is what makes the seeded facts legible at a glance.
- [x] **Playground (§4f-c) — built and working end to end.** `src/artifacts.rs` + three native tools + a left-rail page.
  - **Artifacts are plain files** (`data/artifacts/<id>/index.html` + `meta.json`), not blobs — openable, editable, diffable, deletable without this program. Same name + topic **updates in place** rather than leaving a graveyard of near-copies.
  - **Served as real pages** at `/artifacts/<id>/` and shown in a sandboxed iframe. No spawned process. Path traversal is refused by construction (test covers `../` and absolute paths).
  - Tools: `create_artifact`, `list_artifacts`, `open_workspace(topic)`. `open_workspace` returns the artifacts **plus** what was discussed about that topic (it calls `search_memory` internally) — "where we left off" needs both.
  - **Topic membership is derived, not stored twice.** Creating an artifact is a logged tool call, so the reducer extracts `artifact -part_of-> topic` edges into the graph. The artifact store never becomes a second source of truth.
  - **Verified against the real model:** asked it to build a ticker board → it wrote a complete self-contained page, saved under topic `trading`, served at 4.8 KB. Then "right, let's do trading" → it called `open_workspace` unprompted and described what was already there.
  - **Restore is explicit, not model-guessed** — a tool it calls when you say so, plus a clickable topic-filtered list. Deliberate: a model silently deciding to restore a workspace is worse than one that waits to be asked.
  - Open follow-up: credential handling. A page that needs a trading API key currently can't get one safely — client-side keys are the wrong answer. Wants a harness-side proxy that holds the key and the artifact calls back to it.
- [x] **Persona editor + provider chain editor** (Settings page, `src/providers.rs`).
  - **Provider chain replaces the single provider of §3.** Order is the fallback order: default, fallback 1, 2, 3. `ProviderChain::complete` tries each in turn and only reports failure if *all* fail; a silent failover updates the session's recorded model so the log still says who actually answered. This is not theoretical - the build hit a 402 and a bare 500 from the same provider on different days.
  - Config lives in **`data/providers.json`, not `persona/`** - it holds API keys, and `persona/` is committed while `data/` is gitignored.
  - **Keys are masked to the UI** (`••••••••…4e60`) and a masked key coming back on save means *unchanged*, never *wipe*. Without that rule, opening the page and pressing Save would have destroyed every key. Covered by a test.
  - Persona editor writes only the four `PERSONA_FILES`, refusing anything else (test covers `../../.env` and `graph-seed.json`), and keeps a `.bak` on every save because §5c calls SOUL.md the highest-value target in the system.
  - Edits apply on the **next new chat**, not mid-conversation - §5b requires the assembled prompt stay stable within a session.
- [x] **Turns stream over a websocket** (`/ws/chat`, `Agent::turn_with`). Tool calls and results reach the UI as they happen instead of arriving in a lump. Measured: tool call rendered at +2.8s, reply at +6.7s, where previously nothing appeared until 6.7s. `Agent::turn` is now a thin wrapper over `turn_with(.., None)`, so the CLI is unchanged.

**Checkpoint:** dashboard shows real data in all three views, not placeholders.

### Phase 3 — Persona files
- [x] **All four persona files written and loading** (`persona/`), assembled in §5b order (SOUL → AGENTS → TOOLS → USER), built once at session start and never mutated mid-conversation so the prompt prefix stays cache-friendly.
  - **Name: bluee.** One-line edit in SOUL.md. Two syllables with distinct consonants, chosen so it trains well as a wake word if Phase 5 ever happens.
  - USER.md is pre-seeded with **verified facts only** — hardware, the 3.12/3.14 split, why builds go to `D:/tgt/harness`, that Adithya doesn't write Rust, that he wants plain English, that he builds his own guardrails. No invented facts.
  - TOOLS.md carries the operational knowledge that was expensive to learn: check `snarevec__snarevec_status` before concluding search is broken; read the screen before clicking it; the graph is read-only to you and that is deliberate.
  - **Verified working:** asked "who are you and what do you know about my machine?" — it answered in persona, recited the hardware correctly, then added *"that's what's on record from working with you, not a live check — I'd want to verify before telling you what's running right now."* That is SOUL.md's anti-fabrication rule applied unprompted.
- [ ] Pre-seed the graph with a few known facts (still to do)

**Checkpoint:** restart the harness, confirm persona is consistently applied without re-explaining anything.

### Phase 4 — Tool breadth + skills + screen understanding
- [ ] Daily-use OS tools beyond what UACC covers (file search, notes, clipboard, system info)
- [ ] SnareVec registered as MCP source
- [ ] Tier 1 skill-saving (§5c): `list_skills()` / `run_skill(name)`, explicit save-on-request
- [x] **Screen vision, three modes, gated (§6) — BUILT.** `src/vision.rs` + a segmented toggle in the top bar.
  - **The gate is real, not advisory.** In OFF and PASSIVE, `analyze_screen_vlm` is **not in the toolset at all** — the model cannot reach for vision because it does not know vision exists. Stronger than instructing it not to, and it means the cost and latency of a VLM call cannot happen in a session where it was not authorised. Verified by counting the exposed toolset: passive 11 → active 12 → off 11.
  - The toolset is synced at the start of each turn, so flipping the toggle mid-session takes effect on the next message without restarting. Each flip is written to the log as a `System` event.
  - **PASSIVE is the default and costs nothing** — periodic `uacc__get_screen_info` (accessibility text map), no model involved. **Unchanged screens are skipped by hash**, which is the mitigation §8 asks for against passive capture filling memory with noise; the toggle tooltip reports captured vs skipped.
  - Screen reads go to the **event log only, never the prompt**. Injecting a screen dump into every turn would swamp context with exactly that noise. They become searchable via the reducer, so bluee looks when a question needs it.
  - Capture writes into whichever session is live; with no conversation to attach to it is skipped rather than accumulating orphan context.
  - **ACTIVE needs a vision-capable model** (`LLM_VISION_MODEL`) — the chat model is usually text-only and rejects images. Unset, the tool exists and says so plainly rather than failing opaquely; a provider rejection is reported as "if this model is text-only, set LLM_VISION_MODEL".
  - Mode persists to `data/vision.json` across restarts.
  - **Measured, on one 1920x1080 screenshot (~126 KB PNG -> ~2,070 prompt tokens):**

    | model | latency | in/out tokens | read on-screen text exactly? |
    |---|---|---|---|
    | `qwen/qwen3-vl-8b-instruct` | 2.9s | 2076 / 7 | **yes** - got `Conceptual Project ~ clg` including the tilde |
    | `qwen/qwen3-vl-30b-a3b-instruct` | 7.2s | 2076 / 7 | close - returned `-` instead of `~` |
    | `qwen/qwen3.8-27b` (the chat model) | 11.0s | 2120 / 60 | **no** - burned 60 completion tokens and returned empty |

    **The 8B is both the cheapest and the most accurate**, so `LLM_VISION_MODEL`
    is set to it. Bigger is not better here: the task is OCR-ish reading, not
    reasoning.

    **On whether the chat model handles images:** `qwen3.8-27b` *accepts* them
    without erroring, and gives plausible-sounding descriptions - but it failed
    the one objectively checkable question, returning nothing. Accepting an
    image and reading it are different things, which is exactly why vision is a
    separate configured model rather than reusing the chat one.

    **Cost shape to remember:** roughly **2k prompt tokens per screen look**,
    regardless of the question. That is the number that makes ACTIVE expensive
    if left on, and the reason PASSIVE (text, free) is the default.

  - **End to end through the harness in ACTIVE:** ~30s total, of which about
    **20s is UACC capturing the screenshot** - the model call is only ~2.5s. If
    this path ever needs to be faster, the screenshot is what to optimise, not
    the model.

### Phase 5 — Voice module (optional — add only once Phases 0-4 are solid)
- [ ] Install whisper.cpp in the same 3.12 venv, benchmark `base` vs `small`, pick whichever hits real-time
- [ ] Wire STT → the existing text-in path (no change to the harness core — it already takes text, voice just becomes another way to produce text input)
- [ ] Piper TTS on the response side
- [ ] Wake word via OpenWakeWord, with push-to-talk as the documented fallback (§1 kill criterion)

This phase is skippable entirely if time runs out — the harness is fully usable via text without it, by design.

### Phase 6 — Rehearsal + buffer
- [ ] Run through the flow 2–3 times as you'll actually use it day to day
- [ ] Pre-save 2–3 UACC workflows and 1–2 of your own skills for anything you want reliable, not improvised
- [ ] **No new features after this point.**

---

## 8. Real risks (named mechanisms)

- **Running wake-word + STT + LLM calls + TTS + all memory layers + dashboard on one CPU** could cause contention — mitigated by Phase 0 benchmark + push-to-talk fallback (§1).
- **LLM given shell/process-control tools can run destructive commands** — UACC's safe-mode covers GUI control; custom shell tools need the same confirm-before-destructive pattern, per SOUL.md's hard limits (§5a).
- **Passive screen capture writing too much low-value noise into memory** degrades retrieval over time — dedupe/throttle on meaningful screen change, not every interval.
- **Event log growing unbounded** — it's the source of truth so don't delete it, but do add a compaction/summarization step before it becomes a performance problem in a long-running session (Hermes's harness closes and chains sessions on compression rather than rewriting one file forever — worth mirroring if sessions run long).
- **Automatic skill mining (Tier 2) proposing bad or redundant skills** — this is exactly why it's a stretch goal behind explicit Tier 1 saving, not the default.

---

## 9. Research notes — other harnesses studied, and what was actually borrowed

Quick validation pass on each: is this genuinely useful for a solo 2-day build, or is it interesting-but-out-of-scope?

| Harness | What it does well | Borrowed for this build? |
|---|---|---|
| **OpenClaw** — https://github.com/openclaw (docs: clawdocs.org) | SOUL/AGENTS/USER/TOOLS/MEMORY file split for persona, with clear separation of concerns and a documented anti-pattern list | **Yes** — §5 is directly this pattern, minus a static MEMORY.md since this build has real DB memory instead |
| **DeepSeek Harness (dsh)** — plugin runtime on "Cordis," append-only session event log as canonical source of truth, model-visible state *derived* from events rather than mutated directly | **Yes** — this is exactly §4a's design. This is also the "transparency" property: nothing is silently overwritten, every turn is reconstructable from the log |
| **Hermes Agent** (Nous Research) — https://github.com/NousResearch/hermes-agent | Three-layer memory (session/facts/skills), automatic skill generation→recall→optimize closed loop, tiered prompt assembly (stable/context/volatile) for cache-friendliness | **Partially** — Tier 1 skill-saving borrows the shape; Tier 2 automatic mining is noted as the "real" version of this to build toward later, not now |
| **Pi (pi.dev)** — minimal 4-tool coding harness (read/write/edit/bash), everything else opt-in via extensions | **Philosophically** — the "ship a small core, make everything else swappable" ethos is why §2's architecture keeps UACC/SnareVec as separate pluggable MCP sources rather than merging them into one monolith |
| **Prime Agent** (Prime Intellect) — persistent IPython kernel as the model's one tool, Recursive Language Model (RLM) treating context as a variable, sub-agents as async function calls | **Not for this build** — genuinely powerful, closest in spirit to a real "computational substrate" (their words) for long-horizon agency, but building this is itself a multi-week harness-engineering project. **This is the closest existing reference point for the eventual kernel-level AI-OS goal (§10)** — worth a real read when that project starts |
| **Odysseus** (PewDiePie / Felix Kjellberg) — self-hosted workspace: chat, self-evolving agents (web browsing, file editing, research, email), memory, MCP support, works with local or cloud models | **Conceptually** — validates that "self-hosted, MCP-based, cloud-or-local-model, self-evolving agents" is a coherent and already-successful shape at real scale (tens of thousands of GitHub stars). Not cloned directly — it's a full product surface, this build is one focused assistant — but it's a good feature-completeness reference to glance at in §10 planning |

---

## 10. After this build — notes toward the eventual kernel-level AI-OS

Not part of this 2-day scope, but captured here since it's the stated end goal and this build's architecture should be a stepping stone toward it, not a dead end:

- **Prime Agent's RLM/persistent-kernel model** is the most relevant existing reference for "context as addressable state in a persistent execution environment" — read the actual paper (arXiv 2608.23552) before designing your own kernel-level harness.
- **The event log pattern from §4a scales up conceptually** — a kernel-level harness would want the same "append-only truth, derived views" property, just with the event log itself potentially living closer to the OS layer.
- **This build's persona/memory/tool-calling separation should survive the jump** — SOUL/AGENTS/USER/TOOLS as a persona layer, and MCP as the tool-calling protocol, aren't laptop-scale hacks, they're patterns that hold up in genuinely large deployments (Hermes, OpenClaw, Odysseus all use variations of both).
- Treat this v1 as the thing you actually live with for a while — real usage will surface what a kernel-level version actually needs to solve, which is better information than designing it cold.

---

## 11. Definition of done (acceptance test)

**Core (Phases 0-4, this is the actual bar):** you can type a request and it (a) reasons via cloud API with the split persona files applied, (b) calls the right MCP tool (OS action, GUI/browser control, or memory query) correctly, (c) the event log captures the full trace of what happened, and (d) the dashboard shows the resulting update across all three memory views (event log, vector, graph) in real time — for at least 3 distinct rehearsed scenarios covering tool-calling, memory recall, and screen-context awareness (including toggling vision mode live in the dashboard for one of them).

**Stretch (Phase 5, only if reached):** the same scenarios work end to end via voice instead of text, with no changes needed to the harness core — voice should be purely additive on top of the text path, not a parallel implementation.
