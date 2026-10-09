//! Sub-agent templates - `agents/*.md`.
//!
//! A template is a saved kind of sub-agent: "researcher", "shopper", "coder".
//! Same shape as a skill or a loop - frontmatter for the settings, prose for
//! the instructions - because the easiest thing to customise by hand is a text
//! file. `spawn_agent` with `template: "researcher"` gets all of it at once.
//!
//! ```text
//! ---
//! name: researcher
//! description: Reads the web and reports back with sources
//! servers: [snarevec]
//! skills: [snarevec-crawl-and-search]
//! model: z-ai/glm-5.3-flash
//! browser: chrome
//! folder: playground/agents/researcher
//! sleep: 45
//! end: 120
//! max_turns: 30
//! max_cost: 0.50
//! ---
//! You research one question at a time ...
//! ```
//!
//! Every key but `name` is optional. `sleep`/`end` take minutes or `never`.
//! `max_turns` and `max_cost` (US dollars, from the provider's own usage
//! reports) stop a runaway: once reached, the agent refuses new work until
//! Adithya raises the cap.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub servers: Vec<String>,
    pub skills: Vec<String>,
    pub model: Option<String>,
    pub browser: Option<String>,
    /// `<granted root id>[/<sub folder>]`.
    pub folder: Option<String>,
    /// Minutes. `Some(None)` = explicitly never; `None` = default.
    pub sleep: Option<Option<u64>>,
    pub end: Option<Option<u64>>,
    pub max_turns: Option<u32>,
    pub max_cost: Option<f64>,
    pub instructions: String,
    /// File it came from, for the UI.
    pub file: String,
}

fn list_value(v: &str) -> Vec<String> {
    v.trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn mins(v: &str) -> Option<Option<u64>> {
    if matches!(v.to_ascii_lowercase().as_str(), "never" | "off" | "none") {
        return Some(None);
    }
    let v = v.trim_end_matches(['m', 'M']);
    if let Some(h) = v.strip_suffix(['h', 'H']) {
        return h.trim().parse::<u64>().ok().map(|n| Some(n * 60));
    }
    v.trim().parse::<u64>().ok().map(Some)
}

pub fn parse(path: &Path, text: &str) -> Result<Template> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let text = text.trim_start_matches('\u{feff}');
    let (head, body) = match text.strip_prefix("---") {
        Some(rest) => {
            let rest = rest.trim_start_matches(['\r', '\n']);
            match rest.find("\n---") {
                Some(i) => (rest[..i].to_string(), rest[i + 4..].trim_start_matches(['\r', '\n']).to_string()),
                None => (String::new(), rest.to_string()),
            }
        }
        None => (String::new(), text.to_string()),
    };
    let mut t = Template {
        name: stem,
        instructions: body.trim().to_string(),
        file: path.display().to_string(),
        ..Default::default()
    };
    for line in head.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_matches('"').trim_matches('\'').trim();
        match k.as_str() {
            "name" if !v.is_empty() => t.name = v.to_string(),
            "description" | "desc" => t.description = v.to_string(),
            "servers" => t.servers = list_value(v),
            "skills" => t.skills = list_value(v),
            "model" if !v.is_empty() => t.model = Some(v.to_string()),
            "browser" if !v.is_empty() => t.browser = Some(v.to_ascii_lowercase()),
            "folder" if !v.is_empty() => t.folder = Some(v.replace('\\', "/")),
            "sleep" => t.sleep = mins(v),
            "end" => t.end = mins(v),
            "max_turns" => t.max_turns = v.parse().ok(),
            "max_cost" => t.max_cost = v.trim_start_matches('$').parse().ok(),
            _ => {}
        }
    }
    Ok(t)
}

/// Every template in the folder. A file that fails to parse is skipped, not
/// fatal - one bad template must not hide the rest.
pub fn load_all(dir: &Path) -> Vec<Template> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let is_md = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md"));
        let readme = p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case("readme"));
        if !is_md || readme {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(t) = parse(&p, &text) {
                out.push(t);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn get(dir: &Path, name: &str) -> Result<Template> {
    let want = name.trim().to_ascii_lowercase();
    load_all(dir)
        .into_iter()
        .find(|t| t.name.to_ascii_lowercase() == want)
        .with_context(|| {
            let have: Vec<String> = load_all(dir).into_iter().map(|t| t.name).collect();
            format!(
                "no agent template called `{name}`. Templates: {}",
                if have.is_empty() { "none yet (add one to agents/)".into() } else { have.join(", ") }
            )
        })
}

/// Split `root/sub/dir` into the granted root id and the sub folder.
pub fn split_folder(folder: &str) -> (String, String) {
    let f = folder.trim_matches('/');
    match f.split_once('/') {
        Some((root, sub)) => (root.to_string(), sub.to_string()),
        None => (f.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_key() {
        let t = parse(
            Path::new("agents/researcher.md"),
            "---\nname: researcher\ndescription: reads the web\nservers: [snarevec, uacc]\n\
             skills: [snarevec-crawl-and-search]\nmodel: z-ai/glm-5.3-flash\nbrowser: Brave\n\
             folder: playground/agents/r\nsleep: 30\nend: 3h\nmax_turns: 12\nmax_cost: $0.25\n---\n\
             You research.\n",
        )
        .unwrap();
        assert_eq!(t.name, "researcher");
        assert_eq!(t.servers, ["snarevec", "uacc"]);
        assert_eq!(t.skills, ["snarevec-crawl-and-search"]);
        assert_eq!(t.browser.as_deref(), Some("brave"));
        assert_eq!(t.sleep, Some(Some(30)));
        assert_eq!(t.end, Some(Some(180)));
        assert_eq!(t.max_turns, Some(12));
        assert_eq!(t.max_cost, Some(0.25));
        assert_eq!(t.instructions, "You research.");
        assert_eq!(split_folder(t.folder.as_deref().unwrap()), ("playground".into(), "agents/r".into()));
    }

    #[test]
    fn never_is_distinct_from_unset() {
        let t = parse(Path::new("x.md"), "---\nsleep: never\n---\nhi").unwrap();
        assert_eq!(t.sleep, Some(None), "never");
        assert_eq!(t.end, None, "unset keeps the default");
        assert_eq!(t.name, "x", "name defaults to the file name");
    }
}
