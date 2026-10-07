# Loops

Work bluee does on its own, on a schedule. One loop is one Markdown file in
this folder.

```markdown
---
name: catch-up
description: What changed since I was last here.
every: 6h
servers: [kuzu_graph]
max_runs: 4
---

Everything below the fence is the instruction, in plain English.
```

That is the whole format. Edit a file, save it, and the change is live within
30 seconds — no restart.

## The keys

| key | meaning |
|---|---|
| `name` | Defaults to the filename. What `harness loop <name>` takes. |
| `description` | One line, shown in listings. |
| `every` | `30m`, `6h`, `1d`. Interval since the last run. |
| `at` | `22:00`. Once a day, local time. |
| `on` | `startup`. Once per harness start. |
| `enabled` | `false` switches it off without deleting it. |
| `servers` | Which MCP servers it may use. Omit for all, `[]` for none. |
| `max_runs` | Hard cap per day. Default 24. Applies to `on: startup` loops too. |
| `min_gap` | Shortest time between runs, e.g. `min_gap: 2h`. For `on: startup` it defaults to `4h`, so restarting the app doesn't re-run (and re-bill) a catch-up you had ten minutes ago. |

Use **one** of `every` / `at` / `on`. The last one in the file wins.

## Three things worth knowing before you write one

**No schedule means it never runs.** A file with no `every`, `at` or `on` is
manual-only. This is deliberate: dropping a half-finished file in here must not
be able to start spending money. Run those with `harness loop <name>`.

**`servers:` is the cost lever, not a preference.** Tool schemas are paid on
every turn before the model says anything:

```
all servers   ~22,300 prompt tokens/turn
[kuzu_graph]     ~900
[]                 0  (native tools only — search_memory, read_source, artifacts)
```

An unattended loop with no `servers:` line is the most expensive thing in this
folder. Most loops want `[]` or one server.

**A loop is a real session.** It runs through the same turn loop as chat, and
everything it says lands in the event log — so `reduce` indexes it and what a
loop noticed last week is searchable this week. You do not have to do anything
to make that happen; it falls out of the log being the source of truth.

Loop sessions are titled `loop: <name>`, so they are obvious in the Sessions
page and never look like something you said.

## Commands

```bash
harness loops          # list them, with when each last ran and whether it worked
harness loop catch-up  # run one now, ignoring both schedule and daily cap
```

## When a loop misbehaves

`data/loops.json` holds the last run time and today's count for each. Delete an
entry to reset that loop's schedule. The file is state, not config — editing a
loop never touches it.

A loop that crashes still counts against `max_runs`. That is on purpose: a
reliably-failing loop should stop, not retry all night.
