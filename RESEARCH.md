# Research pass — September 2026

Eight questions, answered against the live repo and current published work.
Measurements are from this machine on 2026-09-16, not from memory.

`CLAUDE.md` is the build log. This is the *decide what to build next* document.

---

## 1. Is the local graph + RAG stack the best available for this machine?

### Vector: yes, and changing it would be wasted work

Measured, today:

```
data/vectors.db     1,192 chunks, 384-dim
  code              1,158   (the repo index)
  global               29   (every conversation, 14 sessions)
  session               5   (written by /compact)
```

Brute-force cosine over 1,192 × 384 is ~458,000 multiply-adds — microseconds.
LanceDB, sqlite-vec and USearch all solve approximate search above roughly a
million vectors. **That is a problem this harness does not have and will not
have soon**: 14 sessions produced 29 chunks.

§4b's reasoning holds, and the live numbers are three orders of magnitude below
where it would stop holding. Revisit past ~1M chunks, not before.

### But there are two real gaps, and neither is the database

**a. No hybrid search.** Pure dense retrieval is weak on exact tokens — names,
error codes, file paths, session ids. The standard fix is BM25 alongside dense,
combined with reciprocal rank fusion. SQLite ships FTS5, so this is roughly 80
lines against a virtual table and **no new dependency**. Highest-value memory
change currently available.

**b. No reranking.** §Phase 0 already recorded the symptom: MiniLM scored two
clearly-related short phrases at 0.22 cosine. §31 found the other end of it — a
gibberish query still returns k rows, topping out at 0.302 against a real
query's 0.526. A cross-encoder reranker over the top ~50 fixes ordering, and
`fastembed` ships rerankers, so again no new dependency.

### Graph: Kùzu is right and should stay

Embedded, single file, real Cypher, no daemon. The alternatives are all worse
*for this shape*: Neo4j needs a server process, SurrealDB is heavier with a less
mature graph layer, `petgraph` has no query language, Oxigraph is RDF/SPARQL.
Nothing on the table beats what is already here.

### The graph's actual problem is extraction, not storage

This is the finding that matters. `src/reduce.rs` emits exactly four entity
kinds from the event log:

```
tool   server   artifact   topic
```

plus `file` and `symbol` from the code index, plus whatever is typed by hand
into `persona/graph-seed.json`.

**There is no code path by which a person ever enters the graph.** The only
person in it is `Adithya`, hand-written into the seed file. So the ask in §1b
below — profiles of people, tracked over time — currently has *zero* substrate,
and no amount of swapping the vector database changes that by one row.

---

## 1b. Methods for maintaining profiles of people

The reference implementation worth stealing from is **Zep / Graphiti**
(arXiv 2501.13956) — a temporal knowledge graph built for exactly this. Two
ideas, neither of which requires adopting their software.

### Bi-temporal edges

Every fact carries two independent times:

- **when it was true** — `valid_from` / `valid_to`
- **when you learned it** — `ingested_at`

When a new fact contradicts an old one, you do not delete the old one. You close
its validity interval and record what superseded it. That is what makes *"who
was Ravi reporting to last March"* answerable at all.

**This is §4a's discipline finally reaching the graph layer.** The event log
never overwrites; the graph currently does. `Rel` already carries `ts` and
`session`, so this is a schema addition and a reducer change, not a rewrite.

### Three subgraphs

Graphiti separates episodic (raw events), semantic (extracted entities and
facts), and community (clusters). This harness has a strong episodic layer — the
event log — and a thin semantic one. The missing stage is the extraction between
them.

### Recommended shape

Add a **gated LLM extraction pass to the reducer**. Not "guess entities from
everything": §4c is right that a graph of invented entities is worse than a small
true one. Instead, run extraction only over turns that actually mention people,
and write:

- `person` entities with `origin: "extracted"`
- typed edges (`works_with`, `mentioned_by`, `prefers`, `decided`)
- **the event `seq` that justified each one**

That last field is what keeps the §4a property intact: the graph can always
answer *"why do you think that"* by pointing back at the line in the log. And
because the reducer is a full projection and the only writer, a bad extraction
pass is fixed by improving the prompt and re-running `reduce` — nothing is ever
stuck with a wrong fact.

---

## 2. Per-session graph and RAG — already built, and built differently

**Yes, this exists.** Verified in code, not assumed. But it is not "a database
per session", and that difference is deliberate and correct.

**Vector** (`src/memory.rs`): the `chunks` table carries `scope`
(`global` / `session` / `code`) and `session_id`, both indexed.
`forget_session()`, `clear_session()`, `clear_scope()` and `count_scope()` all
exist.

**Graph** (`mcps/kuzu-graph/server.py`): `Rel` carries a `session` property;
`Entity` carries `origin` (`log` / `seed` / `code`). `drop_session()` and
`session_graph()` exist.

**Reducer** (`src/reduce.rs`): relations are keyed on
`(source, target, relation, session)` — so two sessions that notice the same
pair stay two edges, and deleting one cannot silently take the other's evidence
with it. A test covers this.

### Why one store beats one-per-session

§23 made this call and it is the right one: **separate databases per session
make the interesting question unanswerable.** "How does this connect to what I
did last month" is inherently cross-session, and N databases cannot answer it.

So the thing being asked for — "mixed as one for overall long-term memory" — is
what already happens, and there is no merge step because there is nothing to
merge. The union *is* the long-term memory. Provenance on each row is what lets
one session be isolated or deleted.

### What is genuinely missing

A **UI to view one session's graph**. `session_graph()` exists on the server and
nothing in the dashboard calls it. That is §12h item 4, still open, and it is a
small addition now that every edge carries its session.

---

## 5. Trending harness features — what is worth taking

Ranked by value *to this harness*, not in general.

| # | Feature | Verdict |
|---|---|---|
| 1 | **Hooks** | **Build it.** Deterministic gates that fire on a tool call, outside the model's control. |
| 2 | **Subagents** | **Build it.** Isolated context for a subtask, results merged back. |
| 3 | Auto-activating skills | Worth doing. Skills exist but must be invoked by name. |
| 4 | Progressive compaction | Low value here. |
| 5 | CodeAct | Not now. Rewrites the turn loop. |

**Hooks** matter here for a reason this repo already paid for. §21: the model
was asked to run `format C: /q`, and it set `confirm: true` *itself*. The fix
was to remove the flag — correct, but specific. A hook is the general form: a
gate the model cannot reach, because it runs outside the model's turn. Given
`run_command` and a shell, this is the single most valuable safety addition
available.

**Subagents** are the cost lever again. §12f measured 129 tools at ~22,300
prompt tokens *per turn*. A subagent can carry the 70 UACC tools for one GUI
task while the main thread carries none, and return only its conclusion. That is
a structural fix for the number §12f calls the biggest lever.

**Compaction** is rated low deliberately: the context window here is 1,000,000
(§4f-d.10) and §4f-d.6 measured that tool schemas, not conversation length,
dominate the prompt. Multi-stage compaction would optimise the smaller half.

### What this harness already has that is ahead of the field

- **Event log as source of truth with full rebuildability.** Most harnesses
  mutate state in place. This one can delete every derived store and reproduce
  it exactly — verified repeatedly.
- **A vision gate that removes the tool from the toolset** rather than
  instructing the model not to use it (§6).
- **Per-workspace tool scoping** with `null` and `[]` kept distinct (§25).

---

## 6. Violoop, and screen-watching generally

### What it actually is

**Hardware.** A palm-sized box: HDMI in from your display, USB out registering
as a standard keyboard and mouse. Kickstarter 15 Sep 2026, $399 intro, ~$799
retail, shipping from mid-October. Trained on 200 apps and 10,000 screens.

Two consequences of that architecture:

- It sidesteps OS permissions entirely and works on machines you cannot install
  software on.
- **The moat is the physical button.** Their own position is that human-in-the-
  loop approval must be hardware to mean anything — which is the same argument
  §21 arrived at independently after the `confirm: true` failure.

### Can the screen-watching part be done locally?

Mostly, and most of it already is. §6 PASSIVE does periodic accessibility-tree
reads with hash dedup, which for most purposes beats pixels: it is text, exact,
and free. What is missing versus Violoop is not perception — it is
**proactivity**, noticing something and speaking first.

### Cost, which is the real question

From §Phase 4's own measurements: **~2,070 prompt tokens per screen look**, and
of a ~30s round trip **about 20s is UACC capturing the screenshot**.

Four levers, in the order they should be pulled:

1. **Fix the 20-second capture first.** It is two-thirds of the latency and
   costs nothing but code. Optimising the model before this is backwards.
2. **Never send pixels when text will do.** The accessibility tree is free.
   Escalate to a VLM only when the tree comes back empty — a canvas, a game, a
   video.
3. **Meaningful-change detection.** Hash dedup already exists; extend it to a
   perceptual hash with a threshold so a blinking cursor is not an event.
4. **Tiered escalation.** A small local VLM as a gate that answers only "is
   anything here worth reporting", with the cloud 8B paid for only on a yes.
   Current best small options: **Moondream 3** (9B MoE, 2B active, purpose-built
   for pointing and grounding) or **Qwen3-VL-2B** quantised.

### Honest answer on running it locally

On this laptop — i5-11300H, no discrete GPU — **continuous** local VLM is out.
§0 said so and it is still true. But *periodic* local VLM on meaningful change
is fine, because it runs in the background where 10 seconds does not matter.

The friend's 8GB GPU box that Phase 5 voice now targets changes this completely,
and is where this should be built.

---

## 7. Better coding in specific languages, without fine-tuning

The literature is unanimous, and the answer is already half-built here.

**1. LSP grounding — it exists and is underused.** §25 measured the argument:
`system::run` resolved to exactly 4 references and correctly **excluded**
`reduce::run`, which a grep matched. Two functions named `run`, one right
answer. The gap is that this is a tool the model *may* call rather than part of
the standard path for a code question. Wiring `find_references` and `hover` into
that path is free accuracy on work already paid for.

**2. Compiler and execution feedback in the loop.** The highest-leverage thing
not yet built: run `cargo check` after an edit, feed errors back, retry. The
type-constrained generation work (arXiv 2504.09246) shows large gains from
exactly this signal.

**3. Per-language convention files.** A `TOOLS.md`-shaped document per language,
loaded only when working in it. Cheap, and it reuses the skills pattern that
already exists.

**4. Type-constrained decoding.** Research-grade, needs provider support. Skip.

**What not to do: fine-tune.** For one person on one codebase it is expensive,
stale the moment the code moves, and it costs general capability. Every source
found says context engineering dominates for this case.

---

## 8. Behind-the-scenes reasoning

This is **loops plus the extraction layer from §1b**. Loops are now built — see
`loops/README.md`. The extraction layer is the remaining dependency, because a
loop that reviews relationships with no `person` entities has nothing to read.

What works today without extraction: a loop's output lands in the event log, so
`reduce` turns it into searchable vector memory automatically. A daily review
that writes down what it noticed about people *is* retrievable next week. That
falls out of §4a rather than being built.

### One honest limit, stated where it belongs

The model reasons from what was typed to it — a partial, one-sided record of
people who are not present to correct it. That does not make it useless; it is
the same evidence a person reasons from, recalled more consistently. But it does
mean the output should be **hypotheses with named disconfirmers, not verdicts**.

That rule is written into `loops/daily-review.md` rather than into this
document, because the prompt is where it actually takes effect.

---

## Recommended order

*Status 2026-09-24 (CLAUDE.md §56): 1, 4 and 5 are built - hybrid search,
hooks, and compiler feedback via post_tool hooks. Subagents (6) were built in
§42. Also built, not on this list: deferred tool loading, live indexing, MCP
call timeouts.*

*Status 2026-09-25 (CLAUDE.md §57): 2 and 3 are built too, in a different
shape from the sketch below. Facts are stated by the model through a logged
`remember` call rather than extracted by a background LLM pass, so the graph
stays derivable from the log with no model in the rebuild. Bi-temporal
`valid_from`/`valid_to` are on every fact edge. Everything on this list is
now done.*

1. **Hybrid search** (FTS5 + RRF). No new dependency, fixes the weakest part of
   recall, unblocks everything that reads memory.
2. **Person extraction with provenance** (§1b). The substrate for §8.
3. **Bi-temporal edges** (§1b). Makes profiles hold up over time.
4. **Hooks** (§5). The general form of the §21 fix.
5. **`cargo check` in the loop** (§7). Cheapest coding-accuracy win available.
6. **Subagents** (§5). Structural fix for the §12f cost number.

Screen proactivity (§6) and the 20-second capture fix sit outside this order —
do the capture fix whenever, it is small and independent.

---

## Sources

- [Zep: A Temporal Knowledge Graph Architecture for Agent Memory](https://arxiv.org/abs/2501.13956)
- [Graphiti — knowledge graph memory](https://www.getzep.com/platform/graphiti/)
- [Type-Constrained Code Generation with Language Models](https://arxiv.org/pdf/2504.09246)
- [A Survey of Context Engineering for Large Language Models](https://arxiv.org/pdf/2507.13334)
- [Violoop — screen-aware AI hardware](https://violoop.ai/)
- [Human-in-the-Loop AI: Why the Approval Must Be Hardware](https://violoop.ai/blog/human-in-the-loop-hardware-approval/)
- [The importance of Agent Harness in 2026](https://www.philschmid.de/agent-harness-2026)
- [awesome-harness-engineering](https://github.com/ai-boost/awesome-harness-engineering)
- [Best Local Vision Language Models in 2026](https://tinyweights.dev/posts/best-local-vision-language-models-2026/)
- [LanceDB vs Chroma vs SQLite-vec](https://kanopylabs.com/blog/lancedb-vs-chroma-vs-sqlite-vec)
