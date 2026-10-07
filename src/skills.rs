//! Skills (§5c Tier 1) - reusable procedures bluee can save and follow later.
//!
//! A skill is a **recipe, not code**. `run_skill` does not execute anything: it
//! hands the procedure back to the model, which then carries it out with the
//! tools it already has. That matters - a skill that executed on its own would
//! be a second, invisible way for things to happen, and the event log would
//! show a single opaque call instead of the real steps. This way every action
//! a skill causes is a normal, logged tool call.
//!
//! Stored as plain Markdown, one file per skill, so they can be read, edited,
//! version-controlled and shared without this program.
//!
//! Categories (§4f-d item 4):
//!   skills    - procedures written or confirmed deliberately
//!   recorded  - captured from something that actually happened
//!   toolkit   - dropped-in bundles from elsewhere
//!   proposed  - candidates awaiting review; nothing here is offered to the
//!               model until promoted, which is what would make automatic
//!               mining (§5c Tier 2) safe to switch on later

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CATEGORIES: &[&str] = &["skills", "recorded", "toolkit", "proposed"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub slug: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    /// Tools the procedure expects to use, for a quick "can I even run this".
    #[serde(default)]
    pub tools: Vec<String>,
    /// Words that should bring this skill up even when no tool is named:
    /// "amazon, flipkart, cart" for a shopping skill. Optional.
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub updated: String,
    /// The procedure itself, Markdown.
    #[serde(default)]
    pub body: String,
}

pub struct SkillStore {
    root: PathBuf,
}

impl SkillStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        for c in CATEGORIES {
            std::fs::create_dir_all(root.join(c))
                .with_context(|| format!("creating {}", root.join(c).display()))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put(
        &self,
        name: &str,
        category: &str,
        description: &str,
        tools: &[String],
        body: &str,
    ) -> Result<Skill> {
        let category = if CATEGORIES.contains(&category) {
            category
        } else {
            "skills"
        };
        let slug = slug(name);
        if slug.is_empty() {
            bail!("a skill needs a name");
        }

        let now = chrono::Utc::now().to_rfc3339();
        let previous = self.get(&slug).ok().flatten();
        let created = previous
            .as_ref()
            .map(|s| s.created.clone())
            .unwrap_or_else(|| now.clone());
        // Neither the model's save_skill nor the Settings editor knows about
        // triggers, so a re-save must carry them over rather than drop them.
        let triggers = previous.map(|s| s.triggers).unwrap_or_default();

        let skill = Skill {
            name: name.to_string(),
            slug: slug.clone(),
            category: category.to_string(),
            description: description.to_string(),
            tools: tools.to_vec(),
            triggers,
            created,
            updated: now,
            body: body.to_string(),
        };

        // A skill that only exists in one category is easier to reason about
        // than one silently duplicated across two.
        for c in CATEGORIES {
            let p = self.root.join(c).join(format!("{slug}.md"));
            if *c != category && p.exists() {
                let _ = std::fs::remove_file(p);
            }
        }

        std::fs::write(
            self.root.join(category).join(format!("{slug}.md")),
            render(&skill),
        )
        .context("writing skill")?;
        Ok(skill)
    }

    pub fn get(&self, name: &str) -> Result<Option<Skill>> {
        let slug = slug(name);
        for c in CATEGORIES {
            let p = self.root.join(c).join(format!("{slug}.md"));
            if p.exists() {
                let text = std::fs::read_to_string(&p)?;
                return Ok(Some(parse(&text, &slug, c)));
            }
        }
        Ok(None)
    }

    pub fn list(&self, category: Option<&str>) -> Result<Vec<Skill>> {
        let mut out = Vec::new();
        for c in CATEGORIES {
            if category.is_some_and(|f| f != *c) {
                continue;
            }
            let dir = self.root.join(c);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "md") {
                    let slug = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        out.push(parse(&text, &slug, c));
                    }
                }
            }
        }
        out.sort_by(|a, b| b.updated.cmp(&a.updated));
        Ok(out)
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let slug = slug(name);
        let mut gone = false;
        for c in CATEGORIES {
            let p = self.root.join(c).join(format!("{slug}.md"));
            if p.exists() {
                std::fs::remove_file(p)?;
                gone = true;
            }
        }
        Ok(gone)
    }
}

/// Markdown with a small header block. Deliberately not YAML - one dependency
/// less, and the format stays obvious to anyone opening the file.
fn render(s: &Skill) -> String {
    format!(
        "# {}\n\n\
         > {}\n\n\
         - category: {}\n\
         - tools: {}\n{}\
         - created: {}\n\
         - updated: {}\n\n\
         ---\n\n{}\n",
        s.name,
        if s.description.is_empty() {
            "(no description)"
        } else {
            &s.description
        },
        s.category,
        if s.tools.is_empty() {
            "-".to_string()
        } else {
            s.tools.join(", ")
        },
        if s.triggers.is_empty() {
            String::new()
        } else {
            format!("- triggers: {}\n", s.triggers.join(", "))
        },
        s.created,
        s.updated,
        s.body.trim()
    )
}

fn parse(text: &str, slug: &str, category: &str) -> Skill {
    let mut name = slug.replace('-', " ");
    let mut description = String::new();
    let (mut tools, mut created, mut updated) = (Vec::new(), String::new(), String::new());
    let mut triggers: Vec<String> = Vec::new();

    let (head, body) = match text.split_once("\n---\n") {
        Some((h, b)) => (h, b.trim().to_string()),
        None => (text, String::new()),
    };

    for line in head.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("# ") {
            name = rest.trim().to_string();
        } else if let Some(rest) = l.strip_prefix("> ") {
            let d = rest.trim();
            if d != "(no description)" {
                description = d.to_string();
            }
        } else if let Some(rest) = l.strip_prefix("- tools:") {
            let t = rest.trim();
            if t != "-" && !t.is_empty() {
                tools = t.split(',').map(|x| x.trim().to_string()).collect();
            }
        } else if let Some(rest) = l.strip_prefix("- triggers:") {
            triggers = rest
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty() && x != "-")
                .collect();
        } else if let Some(rest) = l.strip_prefix("- created:") {
            created = rest.trim().to_string();
        } else if let Some(rest) = l.strip_prefix("- updated:") {
            updated = rest.trim().to_string();
        }
    }

    Skill {
        name,
        slug: slug.to_string(),
        category: category.to_string(),
        description,
        tools,
        triggers,
        created,
        updated,
        body,
    }
}

/// Lower-case, every run of non-alphanumerics collapsed to one space, padded -
/// so `crawl_site`, "crawl site" and "Crawl-Site" all compare equal, and a
/// match is always whole words (" jev " is not inside " jevons ").
fn norm(s: &str) -> String {
    let mut out = String::from(" ");
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// What people actually call each server, so "use snare vec" or "open it in
/// neovim" finds the skills for `snarevec` and `nvim_lsp`.
fn server_aliases(server: &str) -> Vec<&'static str> {
    match server {
        "kuzu_graph" => vec!["kuzu", "kuzu graph", "graph memory"],
        "snarevec" => vec!["snarevec", "snare vec"],
        "uacc" => vec!["uacc"],
        "nvim_lsp" => vec!["nvim", "neovim", "nvim lsp", "lsp"],
        "nyx_tools" => vec!["nyx", "nyx tools"],
        _ => vec![],
    }
}

impl SkillStore {
    /// Skills the text refers to, most-referenced first.
    ///
    /// A skill is referred to when the text names one of its MCP servers (or
    /// a common alias), one of its tools, one of its triggers, or the skill
    /// itself. So "use uacc to ..." brings up EVERY skill that drives uacc -
    /// which is what was asked for - and "add it to my flipkart cart" brings
    /// up the shopping skill without anyone naming a tool. `proposed/` is
    /// never offered: it is the holding pen for unreviewed skills (§4f-d.4).
    pub fn matching(&self, text: &str) -> Vec<Skill> {
        let t = norm(text);
        let has = |needle: &str| {
            let n = norm(needle);
            n.trim().chars().count() >= 3 && t.contains(&n)
        };
        let mut scored: Vec<(usize, Skill)> = self
            .list(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.category != "proposed")
            .filter_map(|s| {
                let mut hits = 0;
                if has(&s.name) || has(&s.slug) {
                    hits += 3;
                }
                for trig in &s.triggers {
                    if has(trig) {
                        hits += 2;
                    }
                }
                let mut servers: Vec<&str> = Vec::new();
                for tool in &s.tools {
                    let (server, short) = tool.split_once("__").unwrap_or(("", tool.as_str()));
                    if short.chars().count() >= 6 && has(short) {
                        hits += 2;
                    }
                    if !server.is_empty() && !servers.contains(&server) {
                        servers.push(server);
                    }
                }
                for server in servers {
                    if has(server) || server_aliases(server).iter().any(|a| has(a)) {
                        hits += 2;
                    }
                }
                (hits > 0).then_some((hits, s))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, s)| s).collect()
    }

    /// Skills that use any tool on one of these servers - offered when
    /// `find_tools` loads that server's tools.
    pub fn for_servers(&self, servers: &[String]) -> Vec<Skill> {
        self.list(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.category != "proposed")
            .filter(|s| {
                s.tools.iter().any(|t| {
                    t.split_once("__").is_some_and(|(srv, _)| servers.iter().any(|x| x == srv))
                })
            })
            .collect()
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentioning_a_server_tool_or_trigger_brings_the_skill_up() {
        let dir = std::env::temp_dir().join(format!("sk-{}", uuid::Uuid::new_v4()));
        let store = SkillStore::open(&dir).unwrap();
        let tools = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        store.put("Shop add to cart", "skills", "", &tools(&["snarevec__browser_click", "uacc__click"]), "x").unwrap();
        store.put("Crawl a site", "skills", "", &tools(&["snarevec__crawl_site"]), "x").unwrap();
        store.put("Draft idea", "proposed", "", &tools(&["uacc__click"]), "x").unwrap();
        // Triggers are hand-written into the file; a later re-save must keep them.
        let p = dir.join("skills").join("shop-add-to-cart.md");
        let text = std::fs::read_to_string(&p).unwrap().replace("- created:", "- triggers: amazon, flipkart, cart\n- created:");
        std::fs::write(&p, text).unwrap();
        store.put("Shop add to cart", "skills", "edited in Settings", &tools(&["snarevec__browser_click", "uacc__click"]), "x").unwrap();
        assert_eq!(store.get("shop-add-to-cart").unwrap().unwrap().triggers, vec!["amazon", "flipkart", "cart"]);

        let names = |q: &str| store.matching(q).into_iter().map(|s| s.slug).collect::<Vec<_>>();
        // Naming a server loads EVERY skill that uses it - but never a proposed one.
        assert_eq!(names("use snare vec for this").len(), 2);
        assert_eq!(names("do it with uacc"), vec!["shop-add-to-cart"]);
        // A tool name, in any spelling.
        assert_eq!(names("try crawl site on the docs"), vec!["crawl-a-site"]);
        // A trigger, with no tool named at all.
        assert_eq!(names("add the headphones to my Flipkart cart"), vec!["shop-add-to-cart"]);
        assert!(names("what's the weather").is_empty());
        assert!(store.for_servers(&["uacc".into()]).iter().all(|s| s.category != "proposed"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trips_and_moves_category() {
        let dir = std::env::temp_dir().join(format!("sk-{}", uuid::Uuid::new_v4()));
        let store = SkillStore::open(&dir).unwrap();

        let s = store
            .put(
                "Restart SnareVec",
                "recorded",
                "Bring the daemon back when search fails",
                &["snarevec__snarevec_status".to_string()],
                "1. Check status\n2. Tell Adithya to reopen the workbench",
            )
            .unwrap();
        assert_eq!(s.slug, "restart-snarevec");

        let got = store.get("Restart SnareVec").unwrap().unwrap();
        assert_eq!(got.category, "recorded");
        assert!(got.body.contains("Check status"));
        assert_eq!(got.tools.len(), 1);

        // promoting must move the file, not leave a duplicate behind
        store
            .put("Restart SnareVec", "skills", "d", &[], "1. x")
            .unwrap();
        assert_eq!(store.list(None).unwrap().len(), 1);
        assert_eq!(store.get("restart-snarevec").unwrap().unwrap().category, "skills");

        assert!(store.delete("restart-snarevec").unwrap());
        assert!(store.list(None).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
