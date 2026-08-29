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
        let created = self
            .get(&slug)
            .ok()
            .flatten()
            .map(|s| s.created)
            .unwrap_or_else(|| now.clone());

        let skill = Skill {
            name: name.to_string(),
            slug: slug.clone(),
            category: category.to_string(),
            description: description.to_string(),
            tools: tools.to_vec(),
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
         - tools: {}\n\
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
        s.created,
        s.updated,
        s.body.trim()
    )
}

fn parse(text: &str, slug: &str, category: &str) -> Skill {
    let mut name = slug.replace('-', " ");
    let mut description = String::new();
    let (mut tools, mut created, mut updated) = (Vec::new(), String::new(), String::new());

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
        created,
        updated,
        body,
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
