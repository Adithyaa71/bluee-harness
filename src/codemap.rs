//! Map the repo into memory, so bluee knows its own source (§4f-d.14).
//!
//! Why this is a *second* projection rather than more work for the reducer:
//! §4a makes the event log the source of truth for what *happened*. The repo is
//! the source of truth for what bluee *is*. Both are derived into the same two
//! stores, and both are fully rebuildable from their source - delete the
//! derived stores, re-run `reduce`, and you get the identical result. The
//! invariant that matters (nothing in the graph without evidence behind it)
//! holds either way, because a file on disk is evidence.
//!
//! Extraction is deterministic and deliberately shallow: paths, module
//! structure, and declared symbols. It does not try to describe what code
//! *does* - a graph full of guessed-at intent is worse than a small true one,
//! which is the same call §4a's reducer makes about entities.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::memory::Chunk;

/// Where the code index lives in the vector store. Kept apart from `global`
/// (the event log) and `session` (`/compact`) so each can be rebuilt without
/// destroying the others.
pub const SCOPE: &str = "code";

/// Never indexed. `.env` and `data/` hold secrets and derived state; the rest
/// is either build output or someone else's source we only read as reference.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "data",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "reference",
    "vendor",
    ".cargo",
    "icons",
];

const SKIP_FILES: &[&str] = &[".env", ".env.local", "Cargo.lock", "providers.json"];

/// Extensions worth reading. Binary and generated files are not.
const TAKE_EXT: &[&str] = &[
    "rs", "py", "js", "html", "css", "toml", "md", "json", "cmd", "vbs", "sh", "mjs",
];

/// A file is skipped if it is larger than this - a huge vendored bundle adds
/// noise and crowds out real recall.
const MAX_BYTES: u64 = 400_000;

#[derive(Debug, Default)]
pub struct CodeFacts {
    /// (name, kind) - kinds are `file`, `module`, `symbol`, `component`.
    pub entities: BTreeSet<(String, String)>,
    /// (source, target, relation)
    pub relations: BTreeSet<(String, String, String)>,
}

#[derive(Debug, Default)]
pub struct CodeIndex {
    pub chunks: Vec<Chunk>,
    pub facts: CodeFacts,
    pub files: usize,
    pub symbols: usize,
}

/// Walk the repo and build the index. Read-only; writes nothing.
pub fn scan(root: &Path) -> Result<CodeIndex> {
    let mut idx = CodeIndex::default();
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();

    for path in files {
        let abs = root.join(&path);
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue; // not UTF-8: skip rather than mangle
        };
        idx.files += 1;

        let component = component_of(&path);
        idx.facts
            .entities
            .insert((component.clone(), "component".into()));
        idx.facts.entities.insert((path.clone(), "file".into()));
        idx.facts
            .relations
            .insert((path.clone(), component, "part_of".into()));

        for (symbol, kind) in symbols(&path, &text) {
            idx.symbols += 1;
            idx.facts.entities.insert((symbol.clone(), kind));
            idx.facts
                .relations
                .insert((symbol, path.clone(), "defined_in".into()));
        }

        idx.chunks.extend(chunk_file(&path, &text));
    }
    Ok(idx)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            collect(root, &p, out)?;
        } else {
            if SKIP_FILES.contains(&name.as_str()) {
                continue;
            }
            let ext = p.extension().map(|e| e.to_string_lossy().to_string());
            if !ext.is_some_and(|e| TAKE_EXT.contains(&e.as_str())) {
                continue;
            }
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

/// Which part of the system a file belongs to. Coarse on purpose: the useful
/// question is "where does this live", not a taxonomy.
fn component_of(path: &str) -> String {
    let first = path.split('/').next().unwrap_or("");
    match first {
        "src" => "harness core".into(),
        "dash" => "dashboard ui".into(),
        "mcps" => "mcp servers".into(),
        "persona" => "persona files".into(),
        "skills" => "skills".into(),
        "tests" | "dev" => "tests and tooling".into(),
        _ => "repo root".into(),
    }
}

/// Declared symbols, by simple prefix matching on the line.
///
/// Not a parser, and not pretending to be one: it reads what a file *declares*,
/// which is exactly the part that stays true as bodies change.
fn symbols(path: &str, text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let rust = path.ends_with(".rs");
    let py = path.ends_with(".py");
    let js = path.ends_with(".js") || path.ends_with(".mjs");

    for raw in text.lines() {
        let line = raw.trim();
        let mut push = |name: &str, kind: &str| {
            let name = name
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string();
            if !name.is_empty() {
                out.push((name, kind.to_string()));
            }
        };
        if rust {
            for (prefix, kind) in [
                ("pub async fn ", "symbol"),
                ("pub fn ", "symbol"),
                ("async fn ", "symbol"),
                ("fn ", "symbol"),
                ("pub struct ", "symbol"),
                ("struct ", "symbol"),
                ("pub enum ", "symbol"),
                ("enum ", "symbol"),
                ("pub trait ", "symbol"),
                ("pub mod ", "module"),
                ("mod ", "module"),
            ] {
                if let Some(rest) = line.strip_prefix(prefix) {
                    let name = rest.split(['(', '<', '{', ' ', ';']).next().unwrap_or("");
                    push(name, kind);
                    break;
                }
            }
        } else if py {
            for (prefix, kind) in [("def ", "symbol"), ("async def ", "symbol"), ("class ", "symbol")] {
                if let Some(rest) = line.strip_prefix(prefix) {
                    let name = rest.split(['(', ':', ' ']).next().unwrap_or("");
                    push(name, kind);
                    break;
                }
            }
        } else if js {
            if let Some(rest) = line.strip_prefix("function ") {
                push(rest.split('(').next().unwrap_or(""), "symbol");
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Roughly this many characters per chunk. §4b's finding stands: short
/// fragments embed badly, so chunks are whole runs of lines with the file path
/// repeated in each one, giving the embedder something to hold on to.
const CHUNK_CHARS: usize = 1400;

fn chunk_file(path: &str, text: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 1u64;
    let mut line_no = 0u64;

    let mut flush = |buf: &mut String, start: u64, end: u64, out: &mut Vec<Chunk>| {
        if buf.trim().is_empty() {
            buf.clear();
            return;
        }
        out.push(Chunk {
            session_id: path.to_string(),
            seq_start: start,
            seq_end: end,
            text: format!("Source file {path} (lines {start}-{end}):\n{buf}"),
        });
        buf.clear();
    };

    for line in text.lines() {
        line_no += 1;
        buf.push_str(line);
        buf.push('\n');
        if buf.len() >= CHUNK_CHARS {
            flush(&mut buf, start, line_no, &mut out);
            start = line_no + 1;
        }
    }
    flush(&mut buf, start, line_no.max(start), &mut out);
    out
}

/// Read one file from the repo for the model.
///
/// The allowlist is by *location*, not by name: anything under a skipped
/// directory, and the secrets in `SKIP_FILES`, are refused, as is any path that
/// escapes the repo. Reading is enabled; writing deliberately is not (§4f-d.14).
pub fn read_source(root: &Path, rel: &str) -> Result<String> {
    let cleaned = rel.replace('\\', "/");
    let candidate = PathBuf::from(&cleaned);
    if candidate.is_absolute() || cleaned.contains("..") {
        anyhow::bail!("path must be relative to the repo and must not contain `..`: {rel}");
    }
    for part in cleaned.split('/') {
        if SKIP_DIRS.contains(&part) || SKIP_FILES.contains(&part) {
            anyhow::bail!("`{part}` is not readable - it holds secrets, build output, or derived state");
        }
    }
    let full = root.join(&cleaned);
    // Belt and braces: even with the checks above, refuse anything that does
    // not actually resolve inside the repo.
    let (canon_root, canon_full) = (root.canonicalize()?, full.canonicalize().ok());
    match canon_full {
        Some(f) if f.starts_with(&canon_root) => {}
        Some(_) => anyhow::bail!("that path resolves outside the repo: {rel}"),
        None => anyhow::bail!("no such file: {rel}"),
    }
    std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_declarations_not_bodies() {
        let src = "pub struct Agent {\n    x: u8,\n}\nimpl Agent {\n    pub fn turn(&self) {}\n    fn helper() {}\n}\nmod inner;\n";
        let got = symbols("src/agent.rs", src);
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"turn"));
        assert!(names.contains(&"helper"));
        assert!(got.iter().any(|(n, k)| n == "inner" && k == "module"));
    }

    #[test]
    fn chunks_carry_their_path_so_they_embed_usefully() {
        let text = (1..=200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let chunks = chunk_file("src/llm.rs", &text);
        assert!(chunks.len() > 1, "a 200-line file should split");
        for c in &chunks {
            assert!(c.text.starts_with("Source file src/llm.rs"));
            assert_eq!(c.session_id, "src/llm.rs");
        }
        assert_eq!(chunks[0].seq_start, 1);
    }

    #[test]
    fn refuses_secrets_and_escapes() {
        let root = std::env::current_dir().unwrap();
        assert!(read_source(&root, ".env").is_err());
        assert!(read_source(&root, "../secrets.txt").is_err());
        assert!(read_source(&root, "data/providers.json").is_err());
        assert!(read_source(&root, "target/release/harness.exe").is_err());
        assert!(read_source(&root, "/etc/passwd").is_err());
    }
}
