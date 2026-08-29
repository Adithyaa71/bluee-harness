//! Playground artifacts (§4f-c) - things the assistant *builds*, not says.
//!
//! An artifact is a small self-contained page (or data file) the model wrote:
//! a chart wired to an API, a dashboard, a scratch tool. They are stored as
//! **plain files on disk**, one directory each, so they can be opened, edited,
//! diffed and deleted without going through this program.
//!
//! Artifacts are NOT memory and are not derived from the event log - they are
//! products. What *is* derived is their association with a topic: creating one
//! is a logged tool call, so the reducer sees it and adds the topic edges to
//! the graph. That is what lets "we're doing trading" pull the right surfaces
//! back without artifacts becoming a second, unsynced source of truth.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub name: String,
    /// Work profile this belongs to - "trading", "the git thing". Free text on
    /// purpose: topics are whatever Adithya calls them, not a fixed taxonomy.
    pub topic: String,
    /// html | markdown | json | text
    pub kind: String,
    pub entry: String,
    pub created: String,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub description: String,
}

pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let root = data_dir.join("artifacts");
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create or overwrite an artifact. Re-using a name within a topic updates
    /// it in place, so "make the chart bigger" edits the chart rather than
    /// leaving a graveyard of near-identical copies.
    pub fn put(
        &self,
        name: &str,
        topic: &str,
        kind: &str,
        content: &str,
        description: &str,
    ) -> Result<Artifact> {
        let id = slug(&format!("{topic}-{name}"));
        if id.is_empty() {
            bail!("artifact needs a name");
        }
        let dir = self.root.join(&id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        let entry = match kind {
            "html" => "index.html",
            "markdown" | "md" => "index.md",
            "json" => "data.json",
            _ => "index.txt",
        };
        std::fs::write(dir.join(entry), content)
            .with_context(|| format!("writing {}", dir.join(entry).display()))?;

        let now = chrono::Utc::now().to_rfc3339();
        let created = self
            .get(&id)
            .ok()
            .flatten()
            .map(|a| a.created)
            .unwrap_or_else(|| now.clone());

        let art = Artifact {
            id: id.clone(),
            name: name.to_string(),
            topic: topic.to_string(),
            kind: kind.to_string(),
            entry: entry.to_string(),
            created,
            updated: now,
            description: description.to_string(),
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&art)?)
            .context("writing meta.json")?;
        Ok(art)
    }

    pub fn get(&self, id: &str) -> Result<Option<Artifact>> {
        let meta = self.root.join(sanitise(id)).join("meta.json");
        if !meta.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&meta)?;
        Ok(serde_json::from_str(&text).ok())
    }

    /// Every artifact, newest first, optionally filtered to one topic.
    pub fn list(&self, topic: Option<&str>) -> Result<Vec<Artifact>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&self.root)? {
            let Ok(entry) = entry else { continue };
            if !entry.path().is_dir() {
                continue;
            }
            let meta = entry.path().join("meta.json");
            let Ok(text) = std::fs::read_to_string(&meta) else {
                continue;
            };
            let Ok(a) = serde_json::from_str::<Artifact>(&text) else {
                continue;
            };
            if topic.is_some_and(|t| !a.topic.eq_ignore_ascii_case(t)) {
                continue;
            }
            out.push(a);
        }
        out.sort_by(|a, b| b.updated.cmp(&a.updated));
        Ok(out)
    }

    pub fn topics(&self) -> Result<Vec<(String, usize)>> {
        let mut map: std::collections::BTreeMap<String, usize> = Default::default();
        for a in self.list(None)? {
            *map.entry(a.topic).or_insert(0) += 1;
        }
        Ok(map.into_iter().collect())
    }

    /// Resolve a file inside an artifact, refusing anything that escapes it.
    pub fn file(&self, id: &str, rel: &str) -> Result<PathBuf> {
        let dir = self.root.join(sanitise(id));
        // Reject traversal explicitly rather than relying on canonicalize,
        // which would also need the file to exist.
        if rel.contains("..") || rel.starts_with('/') || rel.starts_with('\\') {
            bail!("bad artifact path");
        }
        let rel = if rel.is_empty() {
            self.get(id)?
                .map(|a| a.entry)
                .unwrap_or_else(|| "index.html".into())
        } else {
            rel.to_string()
        };
        Ok(dir.join(rel))
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.root.join(sanitise(id));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
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

/// Ids come from the model and from URLs, so never let one address anything
/// outside the artifact root.
fn sanitise(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_list_and_update() {
        let dir = std::env::temp_dir().join(format!("art-{}", uuid::Uuid::new_v4()));
        let store = ArtifactStore::open(&dir).unwrap();

        let a = store
            .put("Candle chart", "trading", "html", "<h1>v1</h1>", "live candles")
            .unwrap();
        assert_eq!(a.id, "trading-candle-chart");
        assert_eq!(store.list(None).unwrap().len(), 1);

        // same name+topic updates in place rather than duplicating
        let b = store
            .put("Candle chart", "trading", "html", "<h1>v2</h1>", "live candles")
            .unwrap();
        assert_eq!(b.id, a.id);
        assert_eq!(store.list(None).unwrap().len(), 1);
        assert_eq!(b.created, a.created);

        store.put("Notes", "git", "markdown", "# hi", "").unwrap();
        assert_eq!(store.list(Some("trading")).unwrap().len(), 1);
        assert_eq!(store.topics().unwrap().len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_path_traversal() {
        let dir = std::env::temp_dir().join(format!("art-{}", uuid::Uuid::new_v4()));
        let store = ArtifactStore::open(&dir).unwrap();
        assert!(store.file("x", "../../secret").is_err());
        assert!(store.file("x", "/etc/passwd").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
