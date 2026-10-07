//! Deferred tool loading: the model sees a catalogue of names, not every schema.
//!
//! §12f measured the cost: 129 tool schemas are ~22,300 prompt tokens on EVERY
//! round of EVERY turn, before anything is said, and 157 with every server on.
//! Most turns use two or three of them. The standard fix across harnesses in
//! 2026 (Anthropic's tool search tool, OpenAI Agents' `defer_loading`, Claude
//! Code's deferred tools) is the same shape: send the names, let the model load
//! the schemas it actually needs, keep them loaded for the rest of the session.
//!
//! Two properties this keeps from the rest of the harness:
//! - **Scoping still wins.** Only tools the workspace allows are ever listed or
//!   loadable (§25), and withheld tools stay withheld (§4c).
//! - **Nothing is hidden from the log.** Loading is an ordinary tool call, so
//!   the Tasks panel shows what was loaded and why.
//!
//! A side effect worth knowing: providers that compile the whole toolset into
//! a decoding grammar (§53c, ModelRun) never see a deferred schema, so a
//! server with awkward schemas only matters once something actually loads it.

use crate::llm::ToolDef;

pub const TOOL_NAME: &str = "find_tools";

/// Defer whenever there is anything to defer. Asked for explicitly: every
/// server stays connected, nothing but memory is loaded up front, and the
/// agent decides what else it needs.
pub const MIN_DEFERRED: usize = 1;

/// Is deferral on? `HARNESS_TOOL_SEARCH=off` restores the old all-schemas
/// behaviour - an escape hatch for a model that handles the extra step badly.
pub fn enabled() -> bool {
    !matches!(
        std::env::var("HARNESS_TOOL_SEARCH").as_deref().map(str::trim),
        Ok("off") | Ok("0") | Ok("false")
    )
}

/// Servers whose tools are always loaded. The graph is memory - it should
/// never take a search to reach - and its five tools are small.
pub fn core_servers() -> Vec<String> {
    std::env::var("HARNESS_CORE_SERVERS")
        .unwrap_or_else(|_| "kuzu_graph".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The one small tool that stands in for everything deferred. Its description
/// IS the catalogue: every deferred name, grouped by server, so the model
/// knows what exists without paying for how to call it.
pub fn def(deferred: &[&ToolDef]) -> ToolDef {
    let mut by_server: Vec<(String, Vec<String>)> = Vec::new();
    for t in deferred {
        let (server, short) = t.name.split_once("__").unwrap_or(("other", t.name.as_str()));
        match by_server.iter_mut().find(|(s, _)| s == server) {
            Some((_, v)) => v.push(short.to_string()),
            None => by_server.push((server.to_string(), vec![short.to_string()])),
        }
    }
    let catalogue: Vec<String> = by_server
        .iter()
        .map(|(s, names)| format!("{s} ({}): {}", names.len(), names.join(", ")))
        .collect();

    ToolDef {
        name: TOOL_NAME.into(),
        description: format!(
            "Load more tools. To keep every turn cheap, these tools exist but their schemas are \
             not loaded yet - you cannot call them until you load them here. Once loaded they \
             stay available for the rest of the conversation, so load what a task needs once, \
             up front. Full names are `server__tool`.\n\n{}",
            catalogue.join("\n")
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords describing the capability (\"click a button\", \
                        \"browser navigate\"), or exact names as \
                        \"select:uacc__click,uacc__type_text\". Prefix a word with + to \
                        require it in the tool name (\"+browser screenshot\")."
                },
                "max_results": {
                    "type": "integer",
                    "description": "How many to load for a keyword search. Default 6."
                }
            },
            "required": ["query"]
        }),
    }
}

/// Pick which deferred tools a query asks for. Returns full names, best first.
///
/// Deliberately plain keyword scoring: the catalogue is ~150 names, the model
/// writes the query, and a match on the name is worth more than one buried in
/// a description. An embedding model would be slower to load than this is to
/// run, for no accuracy the model cannot supply by asking better.
pub fn pick(query: &str, max: usize, pool: &[&ToolDef]) -> Vec<String> {
    let q = query.trim();
    if let Some(list) = q.strip_prefix("select:") {
        let want: Vec<&str> = list.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        return pool
            .iter()
            .filter(|t| {
                want.iter().any(|w| {
                    *w == t.name || t.name.split_once("__").is_some_and(|(_, short)| short == *w)
                })
            })
            .map(|t| t.name.clone())
            .collect();
    }

    let mut required: Vec<String> = Vec::new();
    let mut terms: Vec<String> = Vec::new();
    for w in q.split_whitespace() {
        let w = w.to_lowercase();
        match w.strip_prefix('+') {
            Some(r) if !r.is_empty() => required.push(r.to_string()),
            _ => terms.push(w),
        }
    }

    let mut scored: Vec<(u32, &ToolDef)> = pool
        .iter()
        .filter_map(|t| {
            let name = t.name.to_lowercase();
            if !required.iter().all(|r| name.contains(r.as_str())) {
                return None;
            }
            let desc = t.description.to_lowercase();
            let mut score = required.len() as u32 * 4;
            for term in &terms {
                if name.contains(term.as_str()) {
                    score += 4;
                } else if desc.contains(term.as_str()) {
                    score += 1;
                }
            }
            (score > 0).then_some((score, *t))
        })
        .collect();
    // Stable on ties, so the catalogue's own order breaks them.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(max.max(1)).map(|(_, t)| t.name.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, desc: &str) -> ToolDef {
        ToolDef { name: name.into(), description: desc.into(), parameters: serde_json::json!({}) }
    }

    #[test]
    fn selects_exact_and_ranks_names_over_descriptions() {
        let a = t("uacc__click", "Click at a screen position");
        let b = t("uacc__type_text", "Type text into the focused control");
        let c = t("snarevec__browser_click", "Click an element in the browser");
        let pool = vec![&a, &b, &c];

        assert_eq!(pick("select:uacc__click,type_text", 5, &pool), vec!["uacc__click", "uacc__type_text"]);
        // "click" in the name beats "click" in a description.
        let got = pick("click", 5, &pool);
        assert_eq!(got.len(), 2);
        assert!(!got.contains(&"uacc__type_text".to_string()));
        // + requires the word in the name.
        assert_eq!(pick("+browser click", 5, &pool), vec!["snarevec__browser_click"]);
        assert!(pick("nothing matches this", 5, &pool).is_empty());
    }

    #[test]
    fn catalogue_groups_by_server() {
        let a = t("uacc__click", "");
        let b = t("uacc__scroll", "");
        let c = t("snarevec__crawl_site", "");
        let d = def(&[&a, &b, &c]);
        assert!(d.description.contains("uacc (2): click, scroll"));
        assert!(d.description.contains("snarevec (1): crawl_site"));
    }
}
