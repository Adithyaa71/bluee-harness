//! The reducer (§4a) - reads the event log, writes the derived layers.
//!
//! This is the piece that makes "the event log is the source of truth" true
//! rather than decorative. It is a **full projection, not an incremental
//! update**: every run clears the vector store and the graph and rebuilds both
//! from scratch. That is deliberate - it means deleting the derived stores and
//! re-running reproduces them exactly, which is the property the whole memory
//! design rests on. If the layers were instead written inline as a side effect
//! of the turn loop, they would drift, silently, and nobody would notice until
//! the assistant started recalling things that never happened.
//!
//! Cost of that choice: rebuild time grows with the log. Fine at personal
//! scale; when it stops being fine, add a watermark and reduce incrementally -
//! but keep a full rebuild available to check the incremental path honest.

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::collections::{BTreeMap, BTreeSet};

use crate::config::Config;
use crate::eventlog::{Event, EventKind, EventLog};
use crate::mcp::McpRegistry;
use crate::memory::{Chunk, VectorStore};

#[derive(Debug, Default)]
pub struct ReduceStats {
    pub sessions: usize,
    pub events: usize,
    pub chunks: usize,
    pub entities: usize,
    pub relations: usize,
    pub graph_skipped: bool,
    pub code_files: usize,
    pub code_chunks: usize,
    pub code_symbols: usize,
}

/// Facts we can extract from the log deterministically.
///
/// Note the honest limit: without asking a model, we can only see what the log
/// *structurally* records - which tools ran, on which server, in what order.
/// Richer entities (people, projects, preferences) need either the LLM or the
/// user telling the assistant explicitly, per §4c. We do not guess at them
/// here, because a graph full of hallucinated entities is worse than a small
/// true one.
#[derive(Default)]
struct GraphFacts {
    /// name -> kind
    entities: BTreeMap<String, String>,
    /// (source, target, relation) -> weight
    relations: BTreeMap<(String, String, String), i64>,
}

impl GraphFacts {
    fn entity(&mut self, name: impl Into<String>, kind: &str) {
        let kind = if kind.is_empty() { "unknown" } else { kind };
        self.entities.entry(name.into()).or_insert_with(|| kind.into());
    }

    fn relate(&mut self, source: &str, target: &str, relation: &str) {
        *self
            .relations
            .entry((source.into(), target.into(), relation.into()))
            .or_insert(0) += 1;
    }
}

pub async fn run(cfg: &Config) -> Result<ReduceStats> {
    let mut stats = ReduceStats::default();

    // ---- 1. read the source of truth ------------------------------------
    let sessions = EventLog::list_sessions(cfg.events_dir())?;
    stats.sessions = sessions.len();

    let mut all: Vec<Vec<Event>> = Vec::new();
    for path in &sessions {
        let events = EventLog::read(path)?;
        stats.events += events.len();
        all.push(events);
    }

    // Note: we deliberately do NOT bail out on an empty log. The seeded facts
    // (§5a) still need applying, so a fresh install starts with a graph that
    // already knows the basics rather than nothing at all.

    // ---- 2. chunk ---------------------------------------------------------
    let chunks: Vec<Chunk> = all.iter().flat_map(|events| chunk_session(events)).collect();
    stats.chunks = chunks.len();

    // ---- 3. vector layer (§4b) -------------------------------------------
    let store = VectorStore::open(cfg.data_dir.join("vectors.db"))?;
    store.clear()?;

    // The repo is indexed alongside the log: bluee should know its own source
    // (§4f-d.14). Same rebuild-from-scratch rule applies, so `code` chunks are
    // cleared and rewritten here rather than accumulating.
    let code = crate::codemap::scan(&std::env::current_dir()?).unwrap_or_default();
    stats.code_files = code.files;
    stats.code_chunks = code.chunks.len();
    store.clear_scope(crate::codemap::SCOPE)?;

    if !chunks.is_empty() || !code.chunks.is_empty() {
        let mut model = TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::AllMiniLML6V2))
            .context("loading embedding model (first run downloads it)")?;

        if !chunks.is_empty() {
            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = model.embed(texts, None).context("embedding chunks")?;
            for (chunk, embedding) in chunks.iter().zip(embeddings) {
                store.insert(chunk, &embedding)?;
            }
        }

        if !code.chunks.is_empty() {
            let texts: Vec<&str> = code.chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = model.embed(texts, None).context("embedding source")?;
            for (chunk, embedding) in code.chunks.iter().zip(embeddings) {
                store.insert_scoped(chunk, &embedding, crate::codemap::SCOPE)?;
            }
        }
    }

    // ---- 4. graph layer (§4c) --------------------------------------------
    let mut facts = GraphFacts::default();
    for events in &all {
        extract_graph(events, &mut facts);
    }
    // The repo's structure goes in the same graph: a file on disk is evidence,
    // so this keeps §4c's "nothing without evidence" rule intact.
    for (name, kind) in &code.facts.entities {
        facts.entity(name, kind);
    }
    for (source, target, relation) in &code.facts.relations {
        *facts
            .relations
            .entry((source.clone(), target.clone(), relation.clone()))
            .or_insert(0) += 1;
    }
    stats.code_symbols = code.symbols;

    stats.entities = facts.entities.len();
    stats.relations = facts.relations.len();

    // Rebuild means starting from an empty graph, or repeated runs would keep
    // inflating edge weights and the "same output every time" property dies.
    reset_graph(cfg)?;

    let specs = crate::mcp::load_server_specs(&cfg.mcp_config)?;
    let graph_only: BTreeMap<String, crate::mcp::ServerSpec> = specs
        .into_iter()
        .filter(|(name, _)| name == "kuzu_graph")
        .collect();

    let (registry, failures) = McpRegistry::connect(&graph_only).await;
    if !failures.is_empty() || registry.servers().is_empty() {
        for f in &failures {
            eprintln!("[warn] graph server unavailable - {f}");
        }
        eprintln!("[warn] vector layer rebuilt; graph layer skipped");
        stats.graph_skipped = true;
        return Ok(stats);
    }

    // Seeded facts (§5a) are replayed here rather than written once, because
    // the rebuild above clears the graph - a one-off write would vanish on the
    // next run. Graph = f(event log, seed file), still fully reproducible.
    if let Some(seed) = load_seed(cfg) {
        for e in seed.entities {
            facts.entity(&e.name, &e.kind);
        }
        for r in seed.relations {
            *facts
                .relations
                .entry((r.source, r.target, r.relation))
                .or_insert(0) += r.weight.max(1);
        }
        stats.entities = facts.entities.len();
        stats.relations = facts.relations.len();
    }

    for (name, kind) in &facts.entities {
        registry
            .call(
                "kuzu_graph",
                "upsert_entity",
                serde_json::json!({ "name": name, "kind": kind }),
            )
            .await?;
    }

    for ((source, target, relation), weight) in &facts.relations {
        registry
            .call(
                "kuzu_graph",
                "upsert_relation",
                serde_json::json!({
                    "source": source,
                    "target": target,
                    "relation": relation,
                    "weight": weight,
                }),
            )
            .await?;
    }

    registry.shutdown().await;
    Ok(stats)
}

#[derive(serde::Deserialize)]
struct SeedEntity {
    name: String,
    #[serde(default)]
    kind: String,
}

#[derive(serde::Deserialize)]
struct SeedRelation {
    source: String,
    target: String,
    relation: String,
    #[serde(default)]
    weight: i64,
}

#[derive(serde::Deserialize)]
struct Seed {
    #[serde(default)]
    entities: Vec<SeedEntity>,
    #[serde(default)]
    relations: Vec<SeedRelation>,
}

/// Known facts to seed the graph with. Absent file is fine - it just means
/// the graph is built from the event log alone.
fn load_seed(cfg: &Config) -> Option<Seed> {
    let path = cfg.persona_dir.join("graph-seed.json");
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Seed>(&text) {
        Ok(seed) => Some(seed),
        Err(e) => {
            eprintln!("[warn] ignoring {}: {e}", path.display());
            None
        }
    }
}

/// Delete the graph so the rebuild starts clean.
fn reset_graph(cfg: &Config) -> Result<()> {
    let path = cfg.data_dir.join("graph");
    if path.is_dir() {
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("clearing graph at {}", path.display()))?;
    } else if path.is_file() {
        std::fs::remove_file(&path)
            .with_context(|| format!("clearing graph at {}", path.display()))?;
    }
    Ok(())
}

/// Split a session into turn-sized chunks.
///
/// A turn starts at a user message and runs until the next one, carrying the
/// assistant reply and every tool call in between. Events before the first user
/// message (a bare `harness call`, for instance) form their own leading chunk
/// rather than being dropped.
pub fn chunk_session(events: &[Event]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current: Vec<&Event> = Vec::new();

    let flush = |current: &mut Vec<&Event>, chunks: &mut Vec<Chunk>| {
        if current.is_empty() {
            return;
        }
        let text: Vec<String> = current.iter().filter_map(|e| render(e)).collect();
        if !text.is_empty() {
            chunks.push(Chunk {
                session_id: current[0].session_id.clone(),
                seq_start: current[0].seq,
                seq_end: current[current.len() - 1].seq,
                text: text.join("\n"),
            });
        }
        current.clear();
    };

    for event in events {
        if matches!(event.kind, EventKind::UserMessage { .. }) {
            flush(&mut current, &mut chunks);
        }
        current.push(event);
    }
    flush(&mut current, &mut chunks);

    chunks
}

/// One event as a line of natural-ish text for embedding.
///
/// Session bookkeeping is skipped: it is identical across every session and
/// would only add noise that dilutes retrieval.
fn render(event: &Event) -> Option<String> {
    match &event.kind {
        EventKind::UserMessage { text } => Some(format!("User asked: {text}")),
        EventKind::AssistantMessage { text } => Some(format!("Assistant replied: {text}")),
        EventKind::ToolCall {
            server, tool, args, ..
        } => Some(format!("Called tool {server}.{tool} with {args}")),
        EventKind::ToolResult { ok, result, .. } => Some(format!(
            "Tool {} returned: {}",
            if *ok { "succeeded" } else { "FAILED" },
            truncate(&result.to_string(), 600)
        )),
        EventKind::ScreenContext { mode, text } => {
            Some(format!("Screen context ({mode}): {}", truncate(text, 600)))
        }
        EventKind::Error { context, message } => {
            Some(format!("Error in {context}: {}", truncate(message, 400)))
        }
        EventKind::System { note } => Some(format!("System: {note}")),
        // A title is a label for the session, not something said in it.
        EventKind::SessionTitle { title } => Some(format!("Session titled: {title}")),
        EventKind::SessionStart { .. } | EventKind::SessionEnd { .. } => None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// Turn one session's events into entities and relations.
fn extract_graph(events: &[Event], facts: &mut GraphFacts) {
    let mut previous_tool: Option<String> = None;
    let mut servers_seen: BTreeSet<String> = BTreeSet::new();

    for event in events {
        let EventKind::ToolCall {
            server, tool, args, ..
        } = &event.kind
        else {
            continue;
        };

        // Artifacts are files, but their *topic membership* is derived here
        // from the logged call that created them - so "what belongs to
        // trading" is answerable from the graph, and stays reproducible,
        // without the artifact store becoming a second source of truth.
        if server == "harness" && tool == "create_artifact" {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let topic = args.get("topic").and_then(|v| v.as_str()).unwrap_or("");
            if !name.is_empty() && !topic.is_empty() {
                facts.entity(name, "artifact");
                facts.entity(topic, "topic");
                facts.relate(name, topic, "part_of");
            }
        }

        let qualified = format!("{server}.{tool}");
        facts.entity(&qualified, "tool");
        if servers_seen.insert(server.clone()) {
            facts.entity(server, "server");
        }
        facts.relate(&qualified, server, "part_of");

        // §4c's own example: two tool calls in sequence become a `used_with`
        // edge. Repeats raise the weight, so the graph records how often things
        // actually co-occur rather than merely that they once did.
        if let Some(prev) = &previous_tool {
            if prev != &qualified {
                facts.relate(prev, &qualified, "used_with");
            }
        }
        previous_tool = Some(qualified);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            session_id: "s1".into(),
            ts: Utc::now(),
            kind,
        }
    }

    #[test]
    fn chunks_split_on_user_messages() {
        let events = vec![
            ev(1, EventKind::UserMessage { text: "first".into() }),
            ev(2, EventKind::AssistantMessage { text: "a".into() }),
            ev(3, EventKind::UserMessage { text: "second".into() }),
            ev(4, EventKind::AssistantMessage { text: "b".into() }),
        ];
        let chunks = chunk_session(&events);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].text.contains("first"));
        assert!(chunks[0].text.contains("Assistant replied: a"));
        assert!(chunks[1].text.contains("second"));
    }

    #[test]
    fn extracts_tools_servers_and_sequence_edges() {
        let call = |seq, tool: &str| {
            ev(
                seq,
                EventKind::ToolCall {
                    call_id: format!("c{seq}"),
                    server: "uacc".into(),
                    tool: tool.into(),
                    args: serde_json::json!({}),
                },
            )
        };
        let events = vec![call(1, "screenshot"), call(2, "click"), call(3, "screenshot")];

        let mut facts = GraphFacts::default();
        extract_graph(&events, &mut facts);

        assert_eq!(facts.entities.get("uacc"), Some(&"server".to_string()));
        assert_eq!(
            facts.entities.get("uacc.screenshot"),
            Some(&"tool".to_string())
        );
        // screenshot -> click, then click -> screenshot
        assert_eq!(
            facts
                .relations
                .get(&("uacc.screenshot".into(), "uacc.click".into(), "used_with".into())),
            Some(&1)
        );
        assert_eq!(
            facts
                .relations
                .get(&("uacc.click".into(), "uacc.screenshot".into(), "used_with".into())),
            Some(&1)
        );
    }
}
