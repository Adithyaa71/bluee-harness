//! LLM provider layer (§3) - cloud API, provider-agnostic.
//!
//! Only one implementation exists because only one is needed: aicredits.in and
//! OpenRouter both speak the OpenAI chat-completions dialect, so moving between
//! them is a `.env` edit. The trait exists so a genuinely different provider
//! (or a local model, per §0) can be dropped in without touching the turn loop.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON, but delivered as a *string* by the OpenAI dialect - callers must
    /// parse it, and must tolerate models emitting malformed JSON here.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_kind() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Set on `role: "tool"` messages to pair a result with its call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text("system", text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::text("user", text)
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text("assistant", text)
    }

    fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// A tool result being fed back to the model.
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// A tool as advertised to the model. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Completion>;
    async fn list_models(&self) -> Result<Vec<String>>;
    fn model(&self) -> &str;
}

pub struct OpenAiCompatible {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl OpenAiCompatible {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        max_tokens: u32,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens,
        }
    }
}

// --- wire types -------------------------------------------------------------

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    max_tokens: u32,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: &'a ToolDef,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatible {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Completion> {
        let url = format!("{}/chat/completions", self.base_url);

        let body = ChatRequest {
            model: &self.model,
            messages,
            tools: if tools.is_empty() {
                None
            } else {
                Some(
                    tools
                        .iter()
                        .map(|t| WireTool {
                            kind: "function",
                            function: t,
                        })
                        .collect(),
                )
            },
            tool_choice: if tools.is_empty() { None } else { Some("auto") },
            max_tokens: self.max_tokens,
        };

        let res = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        let status = res.status();
        let text = res.text().await.context("reading response body")?;

        // Surface the provider's own error text. When pointing at a new or
        // custom endpoint this is the difference between a usable message and
        // an opaque status code.
        if !status.is_success() {
            bail!("provider returned {status}: {text}");
        }

        let parsed: ChatResponse = serde_json::from_str(&text)
            .with_context(|| format!("parsing chat response: {text}"))?;

        let msg = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .context("provider returned no choices")?;

        Ok(Completion {
            content: msg.content,
            tool_calls: msg.tool_calls.unwrap_or_default(),
        })
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.base_url);
        let res = self
            .client
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;

        let status = res.status();
        let text = res.text().await.context("reading response body")?;
        if !status.is_success() {
            bail!("provider returned {status}: {text}");
        }

        let parsed: ModelList =
            serde_json::from_str(&text).with_context(|| format!("parsing model list: {text}"))?;
        Ok(parsed.data.into_iter().map(|m| m.id).collect())
    }

    fn model(&self) -> &str {
        &self.model
    }
}

/// One-off vision request: a question plus a base64 image (§6 ACTIVE path).
///
/// Kept separate from `complete` on purpose. OpenAI-style multimodal messages
/// carry content as an array of parts rather than a string, and reshaping the
/// whole `Message` type for a path used only in ACTIVE mode would complicate
/// every ordinary turn for no benefit.
impl OpenAiCompatible {
    pub async fn vision(&self, model: &str, question: &str, image_b64: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "max_tokens": self.max_tokens,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": question },
                    { "type": "image_url",
                      "image_url": { "url": format!("data:image/png;base64,{image_b64}") } }
                ]
            }]
        });

        let res = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        let status = res.status();
        let text = res.text().await.context("reading response body")?;
        if !status.is_success() {
            // Most likely cause by far is a text-only model being handed an
            // image, so say that rather than leaving a bare status code.
            bail!(
                "vision request failed ({status}). If this model is text-only, set \
                 LLM_VISION_MODEL to one that accepts images. Provider said: {text}"
            );
        }

        let parsed: ChatResponse =
            serde_json::from_str(&text).with_context(|| format!("parsing vision response: {text}"))?;
        Ok(parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default())
    }
}
