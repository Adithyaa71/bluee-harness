# kuzu-graph — MCP server

Graph memory (CLAUDE.md §4c) exposed over MCP.

## Why it's a separate process

The `kuzu` Rust crate does not link on this machine — the C++ builds fine
(1,060 objects, `kuzu.lib` produced) but the final link fails with 113
unresolved `kuzu_rs$cxxbridge1$*` symbols, a stale-crate/current-toolchain
mismatch (crate published 2025-10-10, pins `cxx 1.0.138` vs current 1.0.199,
against rustc 1.97.1).

Kùzu 0.11.3 works fine from Python, and the harness already speaks MCP to UACC
and SnareVec — so this costs nothing architecturally and keeps Cypher and Kùzu
exactly as §4c specifies. This is the payoff of the "everything is an MCP
server" shape in §2.

## Run

```bash
.venv/Scripts/python.exe mcps/kuzu-graph/server.py
```

Speaks MCP over stdio. The harness launches it as a child process; you
generally don't run it by hand except to debug.

`HARNESS_GRAPH_DB` overrides the database location (default `data/graph`,
alongside the other derived stores).

## Tools

| Tool | Purpose |
|---|---|
| `query_graph(entity, relation?, direction?, limit?)` | §4c's LLM-facing lookup — what is this entity connected to |
| `upsert_entity(name, kind)` | Create if absent; existing entities untouched |
| `upsert_relation(source, target, relation, weight)` | Relate two entities; repeat observations **raise weight** rather than duplicating the edge |
| `graph_stats()` | Entity/edge counts, relation types, entity kinds |
| `cypher(query, limit)` | Read-only escape hatch; writes are rejected |

## Schema

One `Entity` node table (`name` PK, `kind`, `first_seen`) and one `Rel` edge
table carrying a `type` property, rather than a separate REL TABLE per
relation type. That makes `query_graph` a single parameterised query instead
of dynamic table-name building, and lets the reducer introduce new relation
types without a schema migration.

Tradeoff: Kùzu can't enforce per-type endpoint constraints. Acceptable, since
the reducer is the only writer.

## This store is derived, not authoritative

Per §4a the event log is the source of truth. This database is rebuilt from
it. Deleting `data/graph` should be a non-event — if it ever isn't, something
is writing here that should be writing to the event log instead. That is why
`cypher()` rejects writes.
