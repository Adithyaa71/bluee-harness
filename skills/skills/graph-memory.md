# Graph memory

> How to use long-term memory well: remember durable facts, recall before answering, query the graph for connections.

- category: skills
- tools: kuzu_graph__query_graph, kuzu_graph__facts, kuzu_graph__graph_stats, kuzu_graph__cypher, kuzu_graph__session_graph
- triggers: graph, knowledge graph, what do you know about, long term memory, remember that, who is
- created: 2026-10-07T05:30:00+00:00
- updated: 2026-10-07T05:30:00+00:00

---

## Writing
- `remember` one fact per call as subject - relation - object, with a one-line `note`.
- Jobs, roles, managers, where someone lives replace the old value automatically and keep
  it as history. For other changing facts set `replaces_previous`.
- `no_longer_true` ends a fact without replacing it.
- You cannot write the graph directly (upsert/record tools are withheld on purpose): every
  fact must come from a logged `remember`, so it can always be traced to a conversation.

## Reading
- About a person/project/decision: `recall` first (facts + links + matching conversations,
  tolerant of partial names).
- History: `kuzu_graph__query_graph` with `include_history: true` ("where did he work before").
- Everything remembered: `kuzu_graph__facts`.
- Size and shape: `kuzu_graph__graph_stats`.
- Anything else: `kuzu_graph__cypher` (read-only), e.g.
  `MATCH (a:Entity)-[r:Rel]->(b:Entity) WHERE a.kind = 'person' RETURN a.name, r.type, b.name LIMIT 20`.
- What this conversation added: `kuzu_graph__session_graph`.

## Showing it off
The Graph page draws the whole graph; Memory → Facts lists every fact with history; the
graph under Memory is this conversation's, filling in live.
