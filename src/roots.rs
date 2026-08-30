//! Granted folders (§4f-c, extended).
//!
//! The playground folder was the only place bluee could see. This lets Adithya
//! point it at any directory on the machine - the same shape as granting a
//! coding agent access to a working directory.
//!
//! **Granting is a human act.** There is no tool that adds a root, and there
//! never should be: the whole value of a boundary is that the thing inside it
//! cannot move it. The model can list, read and delete *within* granted roots,
//! and it can see which roots exist. It cannot create one.
//!
//! Containment is checked by canonicalising and comparing prefixes, not by
//! pattern-matching the string - `..` is only one of several ways out, and
//! symlinks are another.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Root {
    /// Stable handle used by the API and the tools.
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    /// The playground folder, which always exists and cannot be removed.
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RootsFile {
    #[serde(default)]
    roots: Vec<Root>,
}

fn file(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("roots.json")
}

fn playground(cfg: &Config) -> Root {
    Root {
        id: "playground".into(),
        label: "playground".into(),
        path: cfg.data_dir.join("artifacts"),
        builtin: true,
    }
}

/// Every granted root, playground first.
pub fn load(cfg: &Config) -> Vec<Root> {
    let mut out = vec![playground(cfg)];
    if let Ok(text) = std::fs::read_to_string(file(cfg)) {
        if let Ok(f) = serde_json::from_str::<RootsFile>(&text) {
            out.extend(f.roots.into_iter().filter(|r| !r.builtin));
        }
    }
    out
}

fn save(cfg: &Config, roots: &[Root]) -> Result<()> {
    let keep: Vec<&Root> = roots.iter().filter(|r| !r.builtin).collect();
    let p = file(cfg);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&serde_json::json!({ "roots": keep }))?,
    )
    .with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

/// Places that should not become a root by accident.
///
/// Not a security boundary - the owner builds his own guards and can edit
/// `roots.json` by hand. It is a guard against a slip: a delete tool pointed at
/// a drive root or at Windows is a bad afternoon, and refusing costs nothing.
fn too_broad(p: &Path) -> Option<&'static str> {
    // Count *named* components, not all of them: on Windows `C:\` is two
    // components (a Prefix and a RootDir) and zero actual directory names,
    // which is the thing that makes it a drive root.
    let named = p
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if named == 0 {
        return Some("that is a drive root - grant a project folder instead");
    }
    let s = p.to_string_lossy().to_lowercase().replace('\\', "/");
    for bad in ["/windows", "/program files", "/program files (x86)", "/$recycle.bin"] {
        if s.contains(bad) {
            return Some("that is a system folder");
        }
    }
    None
}

/// Windows `canonicalize` hands back an extended-length path (`\\?\D:\...`).
/// Correct, and ugly in a UI. Strip the prefix for anything a person reads.
pub fn pretty(p: &Path) -> String {
    let s = p.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

pub fn add(cfg: &Config, path: &str, label: Option<&str>) -> Result<Root> {
    let raw = PathBuf::from(path.trim().trim_matches('"'));
    let full = raw
        .canonicalize()
        .with_context(|| format!("no such folder: {}", raw.display()))?;
    if !full.is_dir() {
        bail!("not a folder: {}", full.display());
    }
    if let Some(why) = too_broad(&full) {
        bail!("{why}: {}", full.display());
    }

    let mut roots = load(cfg);
    if let Some(existing) = roots.iter().find(|r| r.path == full) {
        return Ok(existing.clone());
    }

    let name = full
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".into());
    let id = {
        let base: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let base = base.trim_matches('-').to_string();
        let mut candidate = if base.is_empty() { "folder".into() } else { base };
        let mut n = 2;
        while roots.iter().any(|r| r.id == candidate) {
            candidate = format!("{candidate}-{n}");
            n += 1;
        }
        candidate
    };

    let root = Root {
        id,
        label: label.map(str::to_string).unwrap_or(name),
        path: full,
        builtin: false,
    };
    roots.push(root.clone());
    save(cfg, &roots)?;
    Ok(root)
}

pub fn remove(cfg: &Config, id: &str) -> Result<()> {
    if id == "playground" {
        bail!("the playground folder cannot be removed");
    }
    let mut roots = load(cfg);
    let before = roots.len();
    roots.retain(|r| r.id != id);
    if roots.len() == before {
        bail!("no granted folder with id `{id}`");
    }
    save(cfg, &roots)
}

pub fn get(cfg: &Config, id: &str) -> Result<Root> {
    load(cfg)
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow::anyhow!("`{id}` is not a granted folder. Grant it in the Playground first."))
}

/// Resolve a path inside a granted root, refusing anything that escapes it.
pub fn resolve(cfg: &Config, root_id: &str, rel: &str) -> Result<(Root, PathBuf)> {
    let root = get(cfg, root_id)?;
    let cleaned = rel.replace('\\', "/");
    let cleaned = cleaned.trim_start_matches('/');
    if cleaned.split('/').any(|p| p == "..") {
        bail!("path must stay inside `{}`: {rel}", root.label);
    }
    let full = if cleaned.is_empty() {
        root.path.clone()
    } else {
        root.path.join(cleaned)
    };
    let canon_root = root.path.canonicalize()?;
    match full.canonicalize() {
        Ok(f) if f.starts_with(&canon_root) => Ok((root, full)),
        Ok(_) => bail!("that path resolves outside `{}`: {rel}", root.label),
        Err(_) => bail!("no such file in `{}`: {rel}", root.label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_in(dir: &Path) -> Config {
        let mut c = Config::load().unwrap_or_else(|_| panic!("config"));
        c.data_dir = dir.to_path_buf();
        c
    }

    #[test]
    fn grants_and_bounds_a_folder() {
        let base = std::env::temp_dir().join(format!("roots-{}", uuid::Uuid::new_v4()));
        let work = base.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("sub/a.txt"), "hi").unwrap();
        std::fs::write(base.join("outside.txt"), "no").unwrap();
        let cfg = cfg_in(&base);

        let r = add(&cfg, work.to_str().unwrap(), None).unwrap();
        assert_eq!(r.label, "work");

        assert!(resolve(&cfg, &r.id, "sub/a.txt").is_ok());
        assert!(resolve(&cfg, &r.id, "../outside.txt").is_err());
        assert!(resolve(&cfg, "not-granted", "sub/a.txt").is_err());

        // The playground is always present and cannot be removed.
        assert!(load(&cfg).iter().any(|x| x.id == "playground"));
        assert!(remove(&cfg, "playground").is_err());

        remove(&cfg, &r.id).unwrap();
        assert!(get(&cfg, &r.id).is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn refuses_a_drive_root_or_system_folder() {
        assert!(too_broad(Path::new("C:\\")).is_some());
        assert!(too_broad(Path::new("C:\\Windows\\System32")).is_some());
        assert!(too_broad(Path::new("D:\\projects\\thing")).is_none());
    }
}
