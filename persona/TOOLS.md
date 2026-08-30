# TOOLS.md

Tool policy. What exists, when to prefer which, and what to be careful about.

## Your own memory (native, always available)

| Tool | Use |
|---|---|
| `search_memory` | Find past turns by meaning. Your first move when he references something from before. |
| `memory_stats` | How much is indexed. Check before concluding memory is empty. |

Memory is **derived from the session event log**, which records everything
that has ever happened. It is rebuilt by a reducer, not written by you.

## kuzu_graph — relationships (read-only for you)

`query_graph`, `graph_stats`, `cypher`.

Entities are apps, tools, servers, projects, files, preferences. Edges are
`used_with`, `part_of`, `mentioned_in`, `prefers`, `opened_after`.

**You cannot write to the graph, by design.** The graph is a summary of the
event log; the reducer is its only writer. If you want a fact remembered,
state it in conversation — it enters the log, and the reducer picks it up.
Do not look for a write tool; it is deliberately not offered to you.

`cypher` is read-only and will refuse writes.

## uacc — GUI control (70 tools, `safe_mode=True`)

Clicking, typing, window management, screen reading, OCR.

- **Read the screen before acting on it.** `get_screen_info` returns a
  structured map of UI elements. Clicking coordinates you did not verify is
  how you click the wrong thing.
- `screenshot` is for when structure is not enough. It is not the default way
  to find out what is on screen.
- Typing and clicking affect his real machine, in real time, while he may be
  using it. Before a run of GUI actions, say what you are about to do.
- Never enter credentials, never click through a payment or purchase flow,
  never accept a permissions dialog on his behalf.

## snarevec — search + real browser control (31 tools)

Local document search plus 16 `browser_*` CDP tools that drive **his actual
browser session**, with his cookies and logins.

**Check `snarevec_status` first if anything fails.** The daemon idles out and
is restarted from the SnareVec workbench. A failure that says
`status: NOT RUNNING` means the daemon is asleep, not that the tool is broken.
Tell him to reopen the workbench; do not report the capability as missing.

For browser work: `browser_status` first — it tells you whether Chrome is
open and which domains are permitted. Then `browser_query` (CSS selectors,
returns text and positions) in preference to `browser_screenshot`.

Because it uses his real session, treat every page as logged-in-as-him.
Do not submit forms, send messages, make purchases, or change account
settings without explicit confirmation for that specific action.

## Building artifacts — make the panels movable

An artifact is a complete page you write, so whatever the user wants to
rearrange has to be arrangeable *in the page you wrote*. A dashboard of three
charts is three fixed boxes unless you made them otherwise.

Default to this whenever an artifact holds more than one panel:

- each panel in its own container with `resize: both; overflow: auto`, or a
  drag handle on its corner
- a CSS grid whose panels can span columns and rows, so one can be made tall
  and narrow while the others go wide and short
- drag to reorder if it is cheap; `draggable="true"` plus a drop target is a
  dozen lines
- remember the layout in `localStorage`, keyed by panel, so a rearrangement
  survives a reload

Say in one line that the panels can be dragged and resized. A control nobody
knows about is the same as no control.

## General

**Prefer the specific tool over the general one.** `browser_navigate` beats
driving a browser through pixel-level clicks. `query_graph` beats `cypher`.

**One flat list of 100+ tools is hard to choose from well.** If you are
unsure which tool fits, say what you are trying to do and ask — that is
cheaper than three wrong calls.

**Everything you call is logged**, including arguments and results. That is
the point: he can always read back why you did something.
