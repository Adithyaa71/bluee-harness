"""Kuzu graph memory as an MCP server (CLAUDE.md §4c).

Why this is a separate Python process rather than part of the Rust harness:
the `kuzu` Rust crate does not link on this machine (113 unresolved
`kuzu_rs$cxxbridge1$*` symbols - a stale-crate/current-toolchain mismatch,
see §4c). Kuzu 0.11.3 works fine from Python, and the harness already speaks
MCP to UACC and SnareVec, so one more MCP server costs nothing architecturally
and keeps Cypher and Kuzu exactly as specified.

Schema note: §4c lists edge types (used_with, mentioned_in, prefers, part_of,
opened_after). Rather than one Kuzu REL TABLE per type, this uses a single
`Rel` table carrying a `type` property. That makes `query_graph(entity,
relation)` a single parameterised query instead of dynamic table-name string
building, and lets the reducer introduce new relation types without a schema
migration. The tradeoff is that Kuzu can't enforce per-type endpoint
constraints - acceptable here, since the reducer is the only writer.
"""

from __future__ import annotations

import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import kuzu
from mcp.server.mcpserver import MCPServer

# Default alongside the other derived stores, so "delete data/ and rebuild"
# stays true for the graph exactly as it is for the vector layer.
DEFAULT_DB = Path(__file__).resolve().parents[2] / "data" / "graph"
DB_PATH = Path(os.environ.get("HARNESS_GRAPH_DB", DEFAULT_DB))

KNOWN_RELATIONS = [
    "used_with",
    "mentioned_in",
    "prefers",
    "part_of",
    "opened_after",
]

server = MCPServer(
    name="kuzu-graph",
    instructions=(
        "Graph memory over entities the assistant has seen: apps, people, "
        "projects, files, preferences and screen-context snapshots. Use "
        "query_graph to look up what an entity relates to. This graph is "
        "derived from the session event log - it is rebuilt, not hand-edited."
    ),
)

_db: kuzu.Database | None = None
_conn: kuzu.Connection | None = None


def _connect() -> kuzu.Connection:
    """Open the database and ensure the schema exists (idempotent)."""
    global _db, _conn
    if _conn is not None:
        return _conn

    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    _db = kuzu.Database(str(DB_PATH))
    _conn = kuzu.Connection(_db)

    _conn.execute(
        """
        CREATE NODE TABLE IF NOT EXISTS Entity(
            name STRING,
            kind STRING,
            first_seen STRING,
            PRIMARY KEY(name)
        )
        """
    )
    _conn.execute(
        """
        CREATE REL TABLE IF NOT EXISTS Rel(
            FROM Entity TO Entity,
            type STRING,
            weight INT64,
            ts STRING
        )
        """
    )
    return _conn


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _rows(result) -> list[dict[str, Any]]:
    """Kuzu QueryResult -> list of dicts, using the declared column names."""
    columns = result.get_column_names()
    out: list[dict[str, Any]] = []
    while result.has_next():
        out.append(dict(zip(columns, result.get_next())))
    return out


@server.tool()
def query_graph(
    entity: str,
    relation: str | None = None,
    direction: str = "both",
    limit: int = 50,
) -> dict[str, Any]:
    """Find what an entity is connected to.

    Args:
        entity: Entity name to start from, e.g. "vscode".
        relation: Optional relation filter, e.g. "used_with". Omit for all.
        direction: "out", "in", or "both" (default).
        limit: Max edges to return.
    """
    conn = _connect()
    params: dict[str, Any] = {"name": entity, "limit": limit}

    rel_filter = ""
    if relation:
        rel_filter = "AND r.type = $relation"
        params["relation"] = relation

    results: list[dict[str, Any]] = []

    if direction in ("out", "both"):
        res = conn.execute(
            f"""
            MATCH (a:Entity)-[r:Rel]->(b:Entity)
            WHERE a.name = $name {rel_filter}
            RETURN b.name AS other, b.kind AS other_kind,
                   r.type AS relation, r.weight AS weight
            ORDER BY r.weight DESC LIMIT $limit
            """,
            parameters=params,
        )
        for row in _rows(res):
            row["direction"] = "out"
            results.append(row)

    if direction in ("in", "both"):
        res = conn.execute(
            f"""
            MATCH (a:Entity)<-[r:Rel]-(b:Entity)
            WHERE a.name = $name {rel_filter}
            RETURN b.name AS other, b.kind AS other_kind,
                   r.type AS relation, r.weight AS weight
            ORDER BY r.weight DESC LIMIT $limit
            """,
            parameters=params,
        )
        for row in _rows(res):
            row["direction"] = "in"
            results.append(row)

    return {"entity": entity, "count": len(results), "edges": results}


@server.tool()
def upsert_entity(name: str, kind: str = "unknown") -> dict[str, Any]:
    """Create an entity if absent, leaving an existing one untouched.

    Args:
        name: Unique entity name.
        kind: app | person | project | file | preference | screen_context.
    """
    conn = _connect()
    conn.execute(
        """
        MERGE (e:Entity {name: $name})
        ON CREATE SET e.kind = $kind, e.first_seen = $ts
        """,
        parameters={"name": name, "kind": kind, "ts": _now()},
    )
    return {"ok": True, "name": name, "kind": kind}


@server.tool()
def upsert_relation(
    source: str,
    target: str,
    relation: str,
    weight: int = 1,
) -> dict[str, Any]:
    """Relate two entities, creating either if needed.

    Repeat observations should raise weight rather than duplicate the edge, so
    the graph reflects how often something actually co-occurs.

    Args:
        source: Source entity name.
        target: Target entity name.
        relation: e.g. used_with, mentioned_in, prefers, part_of, opened_after.
        weight: Increment applied to an existing edge, or initial weight.
    """
    conn = _connect()
    ts = _now()
    for n in (source, target):
        conn.execute(
            "MERGE (e:Entity {name: $name}) ON CREATE SET e.kind = 'unknown', e.first_seen = $ts",
            parameters={"name": n, "ts": ts},
        )

    existing = _rows(
        conn.execute(
            """
            MATCH (a:Entity)-[r:Rel]->(b:Entity)
            WHERE a.name = $s AND b.name = $t AND r.type = $rel
            RETURN r.weight AS weight
            """,
            parameters={"s": source, "t": target, "rel": relation},
        )
    )

    if existing:
        new_weight = (existing[0]["weight"] or 0) + weight
        conn.execute(
            """
            MATCH (a:Entity)-[r:Rel]->(b:Entity)
            WHERE a.name = $s AND b.name = $t AND r.type = $rel
            SET r.weight = $w, r.ts = $ts
            """,
            parameters={"s": source, "t": target, "rel": relation, "w": new_weight, "ts": ts},
        )
    else:
        new_weight = weight
        conn.execute(
            """
            MATCH (a:Entity), (b:Entity)
            WHERE a.name = $s AND b.name = $t
            CREATE (a)-[:Rel {type: $rel, weight: $w, ts: $ts}]->(b)
            """,
            parameters={"s": source, "t": target, "rel": relation, "w": weight, "ts": ts},
        )

    return {"ok": True, "source": source, "target": target,
            "relation": relation, "weight": new_weight}


@server.tool()
def graph_stats() -> dict[str, Any]:
    """Counts of entities and relations, and which relation types are in use."""
    conn = _connect()
    entities = _rows(conn.execute("MATCH (e:Entity) RETURN count(e) AS n"))[0]["n"]
    edges = _rows(conn.execute("MATCH ()-[r:Rel]->() RETURN count(r) AS n"))[0]["n"]
    by_type = _rows(
        conn.execute(
            "MATCH ()-[r:Rel]->() RETURN r.type AS type, count(r) AS n ORDER BY n DESC"
        )
    )
    kinds = _rows(
        conn.execute("MATCH (e:Entity) RETURN e.kind AS kind, count(e) AS n ORDER BY n DESC")
    )
    return {
        "db_path": str(DB_PATH),
        "entities": entities,
        "edges": edges,
        "relation_types": by_type,
        "entity_kinds": kinds,
        "known_relations": KNOWN_RELATIONS,
    }


@server.tool()
def cypher(query: str, limit: int = 100) -> dict[str, Any]:
    """Run a read-only Cypher query. Escape hatch for questions the shaped
    tools don't cover.

    Writes are rejected - the graph is derived from the event log (§4a), so
    anything that mutates it should go through the reducer, not ad-hoc queries.

    Args:
        query: A Cypher MATCH/RETURN query.
        limit: Max rows returned.
    """
    forbidden = ("create ", "merge ", "set ", "delete ", "drop ", "alter ", "copy ")
    lowered = f" {query.lower().strip()} "
    if any(word in lowered for word in forbidden):
        return {
            "ok": False,
            "error": "This tool is read-only. Use upsert_entity / upsert_relation to write.",
        }

    conn = _connect()
    rows = _rows(conn.execute(query))
    return {"ok": True, "count": len(rows), "rows": rows[:limit]}


if __name__ == "__main__":
    server.run()
