//! Provider chain (§3, extended) - a default plus ordered fallbacks.
//!
//! §3 originally assumed one provider. It shouldn't: a daily driver that stops
//! working because one endpoint 500s or runs out of credit is not a daily
//! driver, and this has already happened twice during the build (a 402 for
//! in-flight budget, then a bare 500).
//!
//! Config lives in `data/providers.json` - **not** `persona/`, because it holds
//! API keys and `persona/` is committed while `data/` is gitignored.
//! If that file is absent, a single provider is synthesised from `.env` so the
//! existing setup keeps working untouched.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::llm::{Completion, Message, OpenAiCompatible, Provider, ToolDef};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_max_tokens() -> u32 {
    4096
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersFile {
    /// Order IS the fallback order: index 0 is the default, then 1, 2, 3…
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

pub fn path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("providers.json")
}

/// Load the configured chain, or synthesise one from `.env`.
pub fn load(cfg: &Config) -> ProvidersFile {
    let p = path(cfg);
    if let Ok(text) = std::fs::read_to_string(&p) {
        match serde_json::from_str::<ProvidersFile>(&text) {
            Ok(f) if !f.providers.is_empty() => return f,
            Ok(_) => {}
            Err(e) => eprintln!("[warn] ignoring {}: {e}", p.display()),
        }
    }
    ProvidersFile {
        providers: vec![ProviderConfig {
            name: "default (.env)".into(),
            base_url: cfg.base_url.clone(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            max_tokens: cfg.max_tokens,
            enabled: true,
        }],
    }
}

pub fn save(cfg: &Config, file: &ProvidersFile) -> Result<()> {
    let p = path(cfg);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(file)?)
        .with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

/// Show a key without leaking it: enough to recognise, not enough to use.
pub fn mask(key: &str) -> String {
    let n = key.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 8 {
        return "•".repeat(n);
    }
    let tail: String = key.chars().skip(n - 4).collect();
    format!("{}…{tail}", "•".repeat(8))
}

/// Tries each enabled provider in order until one answers.
pub struct ProviderChain {
    entries: Vec<(ProviderConfig, OpenAiCompatible)>,
}

impl ProviderChain {
    pub fn build(cfg: &Config) -> Result<Self> {
        let file = load(cfg);
        let entries: Vec<_> = file
            .providers
            .into_iter()
            .filter(|p| p.enabled && !p.api_key.is_empty() && !p.model.is_empty())
            .map(|p| {
                let client =
                    OpenAiCompatible::new(&p.base_url, &p.api_key, &p.model, p.max_tokens);
                (p, client)
            })
            .collect();

        if entries.is_empty() {
            bail!(
                "no usable provider. Set LLM_API_KEY and LLM_MODEL in .env, \
                 or configure one in the Providers page."
            );
        }
        Ok(Self { entries })
    }

    /// Name+model of whichever provider is first in line.
    pub fn primary(&self) -> (String, String) {
        let (c, _) = &self.entries[0];
        (c.name.clone(), c.model.clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Ask each provider in turn. Errors from earlier ones are collected and
    /// only surfaced if *every* provider fails - otherwise a working fallback
    /// would still look like a failure to the caller.
    pub async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<(Completion, String)> {
        let mut problems = Vec::new();
        for (conf, client) in &self.entries {
            match client.complete(messages, tools).await {
                Ok(c) => return Ok((c, conf.model.clone())),
                Err(e) => {
                    eprintln!("[warn] provider `{}` failed: {e:#}", conf.name);
                    problems.push(format!("{} ({}): {e:#}", conf.name, conf.model));
                }
            }
        }
        bail!("all {} provider(s) failed:\n  {}", self.entries.len(), problems.join("\n  "))
    }
}

/// Persona files the editor is allowed to touch. A fixed list, so a crafted
/// filename cannot make the editor write somewhere else.
pub const PERSONA_FILES: &[&str] = &["SOUL.md", "AGENTS.md", "TOOLS.md", "USER.md"];

pub fn read_persona(dir: &Path, name: &str) -> Result<String> {
    if !PERSONA_FILES.contains(&name) {
        bail!("not an editable persona file: {name}");
    }
    Ok(std::fs::read_to_string(dir.join(name)).unwrap_or_default())
}

pub fn write_persona(dir: &Path, name: &str, body: &str) -> Result<()> {
    if !PERSONA_FILES.contains(&name) {
        bail!("not an editable persona file: {name}");
    }
    std::fs::create_dir_all(dir)?;
    // A backup on every save: SOUL.md is the file §5c calls the highest-value
    // target, and a UI that can silently destroy it is worse than no UI.
    let target = dir.join(name);
    if target.exists() {
        let _ = std::fs::copy(&target, dir.join(format!("{name}.bak")));
    }
    std::fs::write(&target, body).with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_without_leaking() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("abc"), "•••");
        assert!(mask("sk-live-1234567890abcdef").ends_with("cdef"));
        assert!(!mask("sk-live-1234567890abcdef").contains("1234567890"));
    }

    #[test]
    fn persona_editor_refuses_other_files() {
        let dir = std::env::temp_dir().join(format!("pers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(write_persona(&dir, "../../.env", "x").is_err());
        assert!(write_persona(&dir, "graph-seed.json", "x").is_err());
        assert!(write_persona(&dir, "SOUL.md", "# hi").is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
