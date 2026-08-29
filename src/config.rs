//! Runtime configuration, read from the environment / `.env` (§3).
//!
//! Deliberately provider-agnostic: the harness targets any OpenAI-compatible
//! endpoint, so moving from aicredits.in to OpenRouter (or to a local model
//! later, per §0) is a config change rather than a code change.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub data_dir: PathBuf,
    pub persona_dir: PathBuf,
    pub mcp_config: PathBuf,
    pub skills_dir: PathBuf,
    /// Model used for ACTIVE screen reads. Usually different from the chat
    /// model - most text models reject images outright.
    pub vision_model: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        // Missing .env is fine - the vars may come from the real environment.
        let _ = dotenvy::dotenv();

        let base_url = std::env::var("LLM_BASE_URL")
            .unwrap_or_else(|_| "https://aicredits.in/v1".into())
            .trim_end_matches('/')
            .to_string();

        let api_key = std::env::var("LLM_API_KEY").unwrap_or_default();
        let model = std::env::var("LLM_MODEL").unwrap_or_default();

        // Set explicitly: without it the provider reserves the model's full
        // context as max_tokens, and a low credit balance then refuses the
        // request outright ("you requested 100000 but can only afford 8048").
        let max_tokens = std::env::var("LLM_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096);

        Ok(Self {
            base_url,
            api_key,
            model,
            max_tokens,
            data_dir: PathBuf::from(
                std::env::var("HARNESS_DATA_DIR").unwrap_or_else(|_| "data".into()),
            ),
            persona_dir: PathBuf::from(
                std::env::var("HARNESS_PERSONA_DIR").unwrap_or_else(|_| "persona".into()),
            ),
            mcp_config: PathBuf::from(
                std::env::var("HARNESS_MCP_CONFIG").unwrap_or_else(|_| "mcps/servers.json".into()),
            ),
            // Skills live at the repo root, not under data/: they are meant to
            // be read, edited and version-controlled, unlike derived state.
            skills_dir: PathBuf::from(
                std::env::var("HARNESS_SKILLS_DIR").unwrap_or_else(|_| "skills".into()),
            ),
            vision_model: std::env::var("LLM_VISION_MODEL").unwrap_or_default(),
        })
    }

    pub fn events_dir(&self) -> PathBuf {
        self.data_dir.join("events")
    }

    /// Fail loudly and usefully rather than sending an unauthenticated request
    /// and reporting whatever the provider happens to say about it.
    pub fn require_credentials(&self) -> Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!(
                "LLM_API_KEY is not set.\n\
                 Copy .env.example to .env and fill it in (base url defaults to {}).",
                self.base_url
            );
        }
        if self.model.is_empty() {
            anyhow::bail!(
                "LLM_MODEL is not set.\n\
                 Run `cargo run -- models` to list what {} offers, then copy the exact id.",
                self.base_url
            );
        }
        Ok(())
    }
}

/// Persona files, concatenated in the order §5b specifies: SOUL first so
/// personality dominates early attention, then how-to-work, then tool policy,
/// then user facts. Built once at session start and never mutated mid-session,
/// which also keeps the prefix stable for prompt caching.
pub fn load_persona(dir: &std::path::Path) -> Result<(String, Vec<String>)> {
    const ORDER: [&str; 4] = ["SOUL.md", "AGENTS.md", "TOOLS.md", "USER.md"];

    let mut parts = Vec::new();
    let mut loaded = Vec::new();

    for name in ORDER {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading persona file {}", path.display()))?;
        parts.push(format!("# {name}\n\n{}", body.trim()));
        loaded.push(name.to_string());
    }

    Ok((parts.join("\n\n---\n\n"), loaded))
}
