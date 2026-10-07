//! Long-term facts: what bluee knows about people, projects, preferences and
//! decisions, with when each became true and when it stopped.
//!
//! RESEARCH.md §1b found the graph's real weakness: nothing in the harness
//! ever put a person into it. §4c was right that a graph of guessed entities
//! is worse than a small true one - so facts are not guessed by a background
//! extractor. The model states them, through `remember`, when it hears them.
//!
//! Why this keeps §4a intact: a `remember` call is an ordinary logged tool
//! call, arguments and all. That log line IS the evidence. The graph is still
//! derived from it - live, the moment it is called, and again identically by
//! `reduce` - so every fact can answer "why do you think that" with a session
//! and a sequence number, and a full rebuild reproduces the same graph.
//!
//! Bi-temporal (Zep/Graphiti's idea, RESEARCH.md §1b): a fact that changes is
//! closed, not overwritten. "Ravi works at Acme" gets a `valid_to` when "Ravi
//! works at Globex" replaces it, so both "where does he work" and "where did
//! he work in March" have answers.

use anyhow::Result;
use serde_json::{json, Value};

use crate::eventlog::{Event, EventKind};
use crate::llm::ToolDef;
use crate::mcp::McpRegistry;

pub const REMEMBER: &str = "remember";
pub const RECALL: &str = "recall";

pub fn handles(name: &str) -> bool {
    name == REMEMBER || name == RECALL
}

pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: REMEMBER.into(),
            description: "Store a fact in long-term memory so it is known in every future \
                conversation. Use it when Adithya tells you something that will still matter \
                later: about a person (who they are, where they work, how he knows them), a \
                project, a preference, a decision, a deadline. One fact per call, as \
                subject - relation - object. Do not store small talk or things only true for \
                this moment. If a fact CHANGES something that can only have one value at a \
                time (a job, a role, where someone lives, a deadline), set replaces_previous - \
                the old value is kept as history, marked no longer true."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "subject": {"type": "string", "description": "Who or what it is about, e.g. \"Ravi\"."},
                    "subject_kind": {"type": "string", "description": "person, project, org, place, preference, tool or thing. Empty if unsure."},
                    "relation": {"type": "string", "description": "Short snake_case verb: works_at, reports_to, prefers, decided, deadline, is, knows."},
                    "object": {"type": "string", "description": "The other end, e.g. \"Acme\"."},
                    "object_kind": {"type": "string", "description": "Same choices as subject_kind. Empty if unsure."},
                    "replaces_previous": {"type": "boolean", "description": "True if this replaces the earlier value of subject+relation."},
                    "no_longer_true": {"type": "boolean", "description": "True to record that this exact fact has ENDED, with nothing replacing it."},
                    "since": {"type": "string", "description": "When it became true, if not now (\"2026-03\", \"last March\"). Empty for now."},
                    "note": {"type": "string", "description": "One line of context: how you know, or why it matters."}
                },
                "required": ["subject", "relation", "object"]
            }),
        },
        ToolDef {
            name: RECALL.into(),
            description: "Everything you know about someone or something: remembered facts \
                (current, and optionally past ones), graph links, and past conversations that \
                mention it. Use this before answering a question about a person, project or \
                earlier decision."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "about": {"type": "string", "description": "The name to recall, e.g. \"Ravi\"."},
                    "include_history": {"type": "boolean", "description": "Also show facts that are no longer true."}
                },
                "required": ["about"]
            }),
        },
    ]
}

/// Does `text` refer to the entity `name`?
///
/// People are rarely called by their full stored name: "Zorblat" for
/// "Zorblat Testperson". Measured on the first live test, whole-name matching
/// missed exactly that and auto-recall attached nothing. So a name also
/// matches on any one of its words, when that word is long enough to be
/// distinctive and appears in the text as a whole word - not inside another.
pub fn mentions(text: &str, name: &str) -> bool {
    let text = text.to_lowercase();
    let name = name.trim().to_lowercase();
    if name.chars().count() < 3 {
        return false;
    }
    let words: std::collections::HashSet<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let parts: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // Whole name, as consecutive words.
    if !parts.is_empty() {
        let joined = parts.join(" ");
        let norm: String = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if format!(" {norm} ").contains(&format!(" {joined} ")) {
            return true;
        }
    }
    // Words that are in plenty of names and say nothing about which one.
    const COMMON: &[&str] = &[
        "test", "team", "work", "home", "group", "company", "project", "with", "from",
        "that", "this", "about", "have", "will", "friend", "main", "new", "labs",
    ];
    parts.len() > 1
        && parts
            .iter()
            .any(|p| p.chars().count() >= 4 && !COMMON.contains(p) && words.contains(p))
}

/// One remembered fact, as the reducer and the live path both see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub subject: String,
    pub subject_kind: String,
    pub relation: String,
    pub object: String,
    pub object_kind: String,
    pub replaces: bool,
    pub ended: bool,
    /// When it became true: `since` if given, else when it was said.
    pub valid_from: String,
    /// Set once something closes it.
    pub valid_to: String,
    /// When it was said - the ordering key for supersession, independent of
    /// whatever `since` claims.
    pub said_at: String,
    pub session: String,
    pub seq: u64,
    pub note: String,
}

fn s(args: &Value, k: &str) -> String {
    args.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

fn b(args: &Value, k: &str) -> bool {
    args.get(k).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// `Works At` / `works-at` / `works at` all become `works_at`, so the same
/// relation said three ways is one relation, and supersession can find it.
pub fn norm_relation(r: &str) -> String {
    let mut out = String::new();
    for c in r.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

/// Relations that hold ONE value at a time, so a new value always replaces
/// the old whether or not the model says so.
///
/// Measured on the first live test: told "Zorblat left and joined Globex", the
/// model stored the new job without `replaces_previous` - leaving two current
/// employers - and then told the user the old one was "kept as history",
/// which was false. The flag is still honoured for anything else; for these,
/// it is implied.
pub const SINGLE_VALUED: &[&str] = &[
    "works_at", "employed_by", "reports_to", "manager", "role", "title", "job",
    "lives_in", "based_in", "located_in", "status", "email", "phone",
    "married_to", "partner",
];

pub fn parse(args: &Value, session: &str, seq: u64, said_at: &str) -> Result<Fact> {
    let subject = s(args, "subject");
    let object = s(args, "object");
    let relation = norm_relation(&s(args, "relation"));
    if subject.is_empty() || object.is_empty() || relation.is_empty() {
        anyhow::bail!("remember needs a subject, a relation and an object");
    }
    let since = s(args, "since");
    let replaces = b(args, "replaces_previous") || SINGLE_VALUED.contains(&relation.as_str());
    Ok(Fact {
        subject,
        subject_kind: s(args, "subject_kind").to_lowercase(),
        relation,
        object,
        object_kind: s(args, "object_kind").to_lowercase(),
        replaces,
        ended: b(args, "no_longer_true"),
        valid_from: if since.is_empty() { said_at.to_string() } else { since },
        valid_to: String::new(),
        said_at: said_at.to_string(),
        session: session.to_string(),
        seq,
        note: s(args, "note"),
    })
}

/// Apply one new fact to the set of facts so far: close what it ends or
/// replaces, and add it unless it repeats something already true.
/// Shared by the rebuild so live and rebuilt graphs cannot disagree.
fn apply(all: &mut Vec<Fact>, f: Fact) {
    let open = |x: &Fact| x.valid_to.is_empty();
    let same_slot = |x: &Fact| {
        x.subject.eq_ignore_ascii_case(&f.subject) && x.relation == f.relation
    };
    if f.ended {
        for x in all.iter_mut().filter(|x| open(x) && same_slot(x)) {
            if x.object.eq_ignore_ascii_case(&f.object) {
                x.valid_to = f.said_at.clone();
            }
        }
        return; // an ending is not itself a fact to store
    }
    if f.replaces {
        for x in all.iter_mut().filter(|x| open(x) && same_slot(x)) {
            if !x.object.eq_ignore_ascii_case(&f.object) {
                x.valid_to = f.said_at.clone();
            }
        }
    }
    let repeat = all
        .iter()
        .any(|x| open(x) && same_slot(x) && x.object.eq_ignore_ascii_case(&f.object));
    if !repeat {
        all.push(f);
    }
}

/// Every fact in the log, in the order it was said, with supersession applied.
/// Only calls that SUCCEEDED count - a `remember` a hook blocked stored nothing.
pub fn derive(sessions: &[Vec<Event>]) -> Vec<Fact> {
    let mut said: Vec<Fact> = Vec::new();
    for events in sessions {
        let ok: std::collections::HashSet<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::ToolResult { call_id, ok: true, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        for e in events {
            if let EventKind::ToolCall { call_id, server, tool, args } = &e.kind {
                if server == "harness" && tool == REMEMBER && ok.contains(call_id.as_str()) {
                    if let Ok(f) = parse(args, &e.session_id, e.seq, &e.ts.to_rfc3339()) {
                        said.push(f);
                    }
                }
            }
        }
    }
    said.sort_by(|a, b| a.said_at.cmp(&b.said_at).then(a.seq.cmp(&b.seq)));
    let mut all = Vec::new();
    for f in said {
        apply(&mut all, f);
    }
    all
}

/// Write one fact into the graph as a `record_fact` edge.
pub async fn write(registry: &McpRegistry, f: &Fact) -> Result<Value> {
    registry
        .call(
            "kuzu_graph",
            "record_fact",
            json!({
                "source": f.subject, "target": f.object, "relation": f.relation,
                "source_kind": f.subject_kind, "target_kind": f.object_kind,
                "valid_from": f.valid_from, "valid_to": f.valid_to,
                "session": f.session, "seq": f.seq, "note": f.note,
            }),
        )
        .await
}

/// The live path: the call has already been logged (that is the evidence);
/// now make the graph reflect it immediately rather than at the next `reduce`.
pub async fn remember_live(registry: &McpRegistry, f: &Fact) -> Value {
    let mut closed: Vec<Value> = Vec::new();
    let mut also_current: Vec<String> = Vec::new();
    let mut graph_note = "stored in long-term memory".to_string();

    let result: Result<()> = async {
        if f.ended || f.replaces {
            let mut args = json!({
                "source": f.subject, "relation": f.relation, "valid_to": f.said_at,
            });
            if f.ended {
                args["only_target"] = json!(f.object);
            } else {
                args["keep_target"] = json!(f.object);
            }
            let r = registry.call("kuzu_graph", "close_facts", args).await?;
            if let Some(list) = r.get("closed").and_then(|v| v.as_array()) {
                closed = list.clone();
            }
        }
        if !f.ended {
            // Skip a repeat of something already true, as `apply` does.
            let now = registry
                .call(
                    "kuzu_graph",
                    "query_graph",
                    json!({"entity": f.subject, "relation": f.relation, "direction": "out"}),
                )
                .await?;
            let repeat = now
                .get("edges")
                .and_then(|v| v.as_array())
                .is_some_and(|edges| {
                    edges.iter().any(|e| {
                        e.get("valid_from").is_some()
                            && e.get("other").and_then(|o| o.as_str())
                                .is_some_and(|o| o.eq_ignore_ascii_case(&f.object))
                    })
                });
            // Say plainly what is STILL true alongside this one. An empty
            // `closed_as_history` was read by the model as "history kept" once.
            if !f.replaces {
                for e in now.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                    if let Some(o) = e.get("other").and_then(|o| o.as_str()) {
                        if e.get("valid_from").is_some() && !o.eq_ignore_ascii_case(&f.object) {
                            also_current.push(o.to_string());
                        }
                    }
                }
            }
            if repeat {
                graph_note = "already known - nothing new stored".into();
            } else {
                write(registry, f).await?;
            }
        }
        Ok(())
    }
    .await;

    if let Err(e) = result {
        // The log line exists, so nothing is lost - `reduce` will put it in
        // the graph once the graph server is back.
        graph_note = format!(
            "recorded in the log, but the graph is unavailable right now ({e:#}); it will \
             appear after the next reduce"
        );
    }
    json!({
        "remembered": if f.ended {
            format!("{} {} {} - no longer true", f.subject, f.relation, f.object)
        } else {
            format!("{} {} {}", f.subject, f.relation, f.object)
        },
        "since": f.valid_from,
        "closed_as_history": closed,
        "also_still_current": also_current,
        "note": if also_current.is_empty() { graph_note } else {
            format!(
                "{graph_note}. The other values listed in also_still_current were NOT closed - \
                 if this replaces them, call remember again with replaces_previous: true"
            )
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn call(seq: u64, secs: i64, args: Value) -> Vec<Event> {
        let ts = Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap();
        vec![
            Event {
                seq, session_id: "s".into(), ts,
                kind: EventKind::ToolCall {
                    call_id: format!("c{seq}"), server: "harness".into(),
                    tool: REMEMBER.into(), args,
                },
            },
            Event {
                seq: seq + 1, session_id: "s".into(), ts,
                kind: EventKind::ToolResult { call_id: format!("c{seq}"), ok: true, result: json!({}) },
            },
        ]
    }

    #[test]
    fn replacing_closes_the_old_value_and_keeps_it() {
        let mut ev = call(1, 0, json!({"subject":"Ravi","relation":"Works At","object":"Acme"}));
        ev.extend(call(3, 10, json!({"subject":"ravi","relation":"works_at","object":"Acme"})));
        // No replaces_previous flag: works_at is single-valued, so it is implied.
        ev.extend(call(5, 20, json!({"subject":"Ravi","relation":"works_at","object":"Globex"})));
        ev.extend(call(7, 30, json!({"subject":"Ravi","relation":"likes","object":"tea"})));
        ev.extend(call(9, 40, json!({"subject":"Ravi","relation":"likes","object":"tea","no_longer_true":true})));
        let facts = derive(&[ev]);

        // The repeat of Acme was not stored twice; the ending was not stored as a fact.
        assert_eq!(facts.len(), 3);
        let acme = facts.iter().find(|f| f.object == "Acme").unwrap();
        let globex = facts.iter().find(|f| f.object == "Globex").unwrap();
        let tea = facts.iter().find(|f| f.object == "tea").unwrap();
        assert_eq!(acme.relation, "works_at");
        assert_eq!(acme.valid_to, globex.said_at, "Acme closed when Globex replaced it");
        assert!(globex.valid_to.is_empty());
        assert!(!tea.valid_to.is_empty(), "tea ended");
    }

    #[test]
    fn names_match_by_distinctive_word() {
        assert!(mentions("what's Zorblat's deadline?", "Zorblat Testperson"));
        assert!(mentions("ask zorblat testperson", "Zorblat Testperson"));
        assert!(mentions("is jev done", "jev"));
        assert!(!mentions("the jevons paradox", "jev"), "not inside another word");
        assert!(!mentions("a new co op", "Acme Test Co"), "short words alone do not count");
        assert!(!mentions("anything", "AI"));
    }

    #[test]
    fn a_blocked_remember_stores_nothing() {
        let mut ev = call(1, 0, json!({"subject":"A","relation":"is","object":"B"}));
        ev[1].kind = EventKind::ToolResult { call_id: "c1".into(), ok: false, result: json!({}) };
        assert!(derive(&[ev]).is_empty());
    }
}
