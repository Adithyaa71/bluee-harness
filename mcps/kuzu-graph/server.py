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
            origin STRING,
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
            ts STRING,
            session STRING
        )
        """
    )
    # Existing databases predate these columns. Adding them is idempotent and
    # far kinder than asking anyone to delete their graph and rebuild.
    for table, column, decl in (
        ("Entity", "origin", "STRING"),
        ("Rel", "session", "STRING"),
    ):
        try:
            _conn.execute(f"ALTER TABLE {table} ADD IF NOT EXISTS {column} {decl}")
        except Exception:  # noqa: BLE001 - older Kuzu without IF NOT EXISTS
            try:
                _conn.execute(f"ALTER TABLE {table} ADD {column} {decl}")
            except Exception:
                pass  # already there

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
def upsert_entity(name: str, kind: str = "unknown", origin: str = "log") -> dict[str, Any]:
    """Create an entity if absent, leaving an existing one untouched.

    Args:
        name: Unique entity name.
        kind: app | person | project | file | preference | screen_context.
        origin: Where it came from - log | seed | code. Entities from seed and
            code survive a session being deleted; ones from the log do not,
            unless another session still refers to them.
    """
    conn = _connect()
    conn.execute(
        """
        MERGE (e:Entity {name: $name})
        ON CREATE SET e.kind = $kind, e.first_seen = $ts, e.origin = $origin
        """,
        parameters={"name": name, "kind": kind, "ts": _now(), "origin": origin},
    )
    return {"ok": True, "name": name, "kind": kind, "origin": origin}


@server.tool()
def upsert_relation(
    source: str,
    target: str,
    relation: str,
    weight: int = 1,
    session: str = "",
) -> dict[str, Any]:
    """Relate two entities, creating either if needed.

    Repeat observations should raise weight rather than duplicate the edge, so
    the graph reflects how often something actually co-occurs.

    Args:
        source: Source entity name.
        target: Target entity name.
        relation: e.g. used_with, mentioned_in, prefers, part_of, opened_after.
        weight: Increment applied to an existing edge, or initial weight.
        session: Which session produced this edge. Empty means it belongs to no
            single session (seeded facts, the code index). Deleting a session
            deletes the edges it produced and nothing else.
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
            WHERE a.name = $s AND b.name = $t AND r.type = $rel AND r.session = $sess
            RETURN r.weight AS weight
            """,
            parameters={"s": source, "t": target, "rel": relation, "sess": session},
        )
    )

    if existing:
        new_weight = (existing[0]["weight"] or 0) + weight
        conn.execute(
            """
            MATCH (a:Entity)-[r:Rel]->(b:Entity)
            WHERE a.name = $s AND b.name = $t AND r.type = $rel AND r.session = $sess
            SET r.weight = $w, r.ts = $ts
            """,
            parameters={"s": source, "t": target, "rel": relation, "w": new_weight,
                        "ts": ts, "sess": session},
        )
    else:
        new_weight = weight
        conn.execute(
            """
            MATCH (a:Entity), (b:Entity)
            WHERE a.name = $s AND b.name = $t
            CREATE (a)-[:Rel {type: $rel, weight: $w, ts: $ts, session: $sess}]->(b)
            """,
            parameters={"s": source, "t": target, "rel": relation, "w": weight,
                        "ts": ts, "sess": session},
        )

    return {"ok": True, "source": source, "target": target,
            "relation": relation, "weight": new_weight, "session": session}


@server.tool()
def drop_session(session: str) -> dict[str, Any]:
    """Remove everything one session contributed to the graph.

    Edges produced by that session go. Entities go only if nothing else refers
    to them any more and they did not come from the seed file or the code index
    - an entity that several sessions know about is not one session's to delete.

    Args:
        session: The session id whose contribution should be removed.
    """
    if not session:
        return {"ok": False, "error": "a session id is required"}
    conn = _connect()

    before_e = _rows(conn.execute("MATCH (e:Entity) RETURN count(e) AS n"))[0]["n"]
    before_r = _rows(conn.execute("MATCH ()-[r:Rel]->() RETURN count(r) AS n"))[0]["n"]

    conn.execute(
        "MATCH ()-[r:Rel]->() WHERE r.session = $sess DELETE r",
        parameters={"sess": session},
    )
    # Orphans, but only the ones this layer owns. Seeded and code entities are
    # permanent by construction.
    conn.execute(
        """
        MATCH (e:Entity)
        WHERE NOT EXISTS { MATCH (e)-[:Rel]-() }
          AND (e.origin IS NULL OR e.origin = 'log')
        DELETE e
        """
    )

    after_e = _rows(conn.execute("MATCH (e:Entity) RETURN count(e) AS n"))[0]["n"]
    after_r = _rows(conn.execute("MATCH ()-[r:Rel]->() RETURN count(r) AS n"))[0]["n"]
    return {
        "ok": True,
        "session": session,
        "relations_removed": before_r - after_r,
        "entities_removed": before_e - after_e,
        "entities_left": after_e,
    }


@server.tool()
def session_graph(session: str) -> dict[str, Any]:
    """What one session contributed: its edges and the entities they touch.

    Args:
        session: The session id to look at.
    """
    conn = _connect()
    edges = _rows(
        conn.execute(
            """
            MATCH (a:Entity)-[r:Rel]->(b:Entity)
            WHERE r.session = $sess
            RETURN a.name AS source, b.name AS target, r.type AS type, r.weight AS weight
            ORDER BY r.weight DESC
            """,
            parameters={"sess": session},
        )
    )
    names = sorted({e["source"] for e in edges} | {e["target"] for e in edges})
    return {"session": session, "entities": names, "edges": edges,
            "entity_count": len(names), "edge_count": len(edges)}


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
    sessions = _rows(
        conn.execute(
            """
            MATCH ()-[r:Rel]->()
            WHERE r.session <> ''
            RETURN r.session AS session, count(r) AS n ORDER BY n DESC
            """
        )
    )
    return {
        "db_path": str(DB_PATH),
        "entities": entities,
        "edges": edges,
        "relation_types": by_type,
        "entity_kinds": kinds,
        "by_session": sessions,
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
