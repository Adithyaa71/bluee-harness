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
    /// Left unset by default rather than defaulted to a number: an omitted
    /// field lets the provider use the model's own tuning, which is usually
    /// better than a guess baked into the harness.
    temperature: Option<f32>,
    top_p: Option<f32>,
    /// Ask for SSE. Measured against aicredits.in: a non-streamed request is
    /// cut off at ~30s wall clock and comes back `500 Internal Server Error`,
    /// whether the 30s went on a large prompt or a long answer. The same work
    /// streamed ran 362s and completed. So this is not a UI nicety - it is what
    /// makes long turns possible at all.
    stream: bool,
    retries: u32,
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
            temperature: None,
            top_p: None,
            stream: true,
            retries: 2,
        }
    }

    /// Per-provider sampling and request timeout, from the Providers page.
    pub fn tuned(mut self, temperature: Option<f32>, top_p: Option<f32>, timeout_secs: u64) -> Self {
        self.temperature = temperature;
        self.top_p = top_p;
        if timeout_secs > 0 {
            if let Ok(c) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
            {
                self.client = c;
            }
        }
        self
    }

    /// Escape hatch: turn streaming off, or change how many times a transient
    /// failure is retried, per provider.
    pub fn transport(mut self, stream: bool, retries: u32) -> Self {
        self.stream = stream;
        self.retries = retries;
        self
    }

    /// Model list with the metadata the Providers page needs to fill in a
    /// context length for you instead of making you look it up.
    pub async fn list_models_detailed(&self) -> Result<Vec<ModelEntry>> {
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
        Ok(parsed.data)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
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

#[derive(Deserialize, Serialize, Clone)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Present on providers that publish it (aicredits.in does). This is what
    /// the context meter should measure against - guessing it is how you end
    /// up compacting a conversation that had plenty of room left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u32>,
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatible {
    /// Retries transient failures against the *same* provider before the chain
    /// gives up on it. A gateway timeout or a bare 500 is a hiccup, not a
    /// verdict - measured here, the identical request failed three times in a
    /// row and then succeeded, and without a retry that turn was simply dead.
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Completion> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let started = std::time::Instant::now();
            match self.complete_once(messages, tools).await {
                Ok(c) => return Ok(c),
                Err(e) => {
                    let secs = started.elapsed().as_secs_f32();
                    let last = attempt > self.retries;
                    if last || !is_transient(&e) {
                        bail!("{e:#} (after {secs:.1}s, attempt {attempt})");
                    }
                    eprintln!(
                        "[warn] {} attempt {attempt} failed after {secs:.1}s, retrying: {e:#}",
                        self.model
                    );
                    // Short, fixed-ish backoff. The failure mode here is an
                    // upstream that was briefly slow, not one that needs
                    // minutes to recover.
                    tokio::time::sleep(std::time::Duration::from_millis(
                        750 * u64::from(attempt),
                    ))
                    .await;
                }
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        Ok(self
            .list_models_detailed()
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect())
    }

    fn model(&self) -> &str {
        &self.model
    }
}

// --- SSE ---------------------------------------------------------------------

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Deserialize)]
struct DeltaToolCall {
    /// Which call this fragment belongs to. The id and name arrive once, on
    /// the first fragment; the arguments arrive in pieces after it.
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaFn>,
}

#[derive(Deserialize)]
struct DeltaFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Fold one SSE line into the completion being assembled.
///
/// Split out from the reader so the reassembly can be tested against real
/// captured frames rather than only against a live endpoint. Ignoring frames
/// it cannot parse is deliberate: providers sprinkle keepalives and vendor
/// extensions through these streams, and one unknown line must not lose a turn.
fn absorb(line: &str, content: &mut String, calls: &mut Vec<(String, String, String)>) {
    let Some(data) = line.trim().strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) else {
        return;
    };
    let Some(choice) = parsed.choices.into_iter().next() else {
        return;
    };
    if let Some(text) = choice.delta.content {
        content.push_str(&text);
    }
    for frag in choice.delta.tool_calls.unwrap_or_default() {
        // Indexed by the provider's own `index`, not by arrival order: with
        // several calls in one turn the fragments interleave.
        if calls.len() <= frag.index {
            calls.resize(frag.index + 1, Default::default());
        }
        let slot = &mut calls[frag.index];
        if let Some(id) = frag.id {
            slot.0 = id;
        }
        if let Some(f) = frag.function {
            if let Some(name) = f.name {
                slot.1 = name;
            }
            if let Some(args) = f.arguments {
                slot.2.push_str(&args);
            }
        }
    }
}

/// Reassemble one completion from a `text/event-stream` body.
///
/// Nothing is surfaced incrementally: the caller wants a whole `Completion`,
/// and the reason for streaming here is the timeout wall, not live typing.
/// (This provider buffers anyway - measured, the first token and the last
/// arrived 0.1s apart at the end of a 362s request.)
async fn read_stream(res: reqwest::Response) -> Result<Completion> {
    use futures_util::StreamExt;

    let mut content = String::new();
    // Indexed by the `index` the provider assigns, not by arrival order.
    let mut calls: Vec<(String, String, String)> = Vec::new();

    let mut body = res.bytes_stream();
    let mut buf = String::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.context("reading stream body")?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // Frames are newline delimited; a chunk can split one in half, so the
        // tail stays in the buffer until its newline shows up.
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim().to_string();
            buf.drain(..=nl);
            absorb(&line, &mut content, &mut calls);
        }
    }

    let tool_calls = calls
        .into_iter()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(id, name, arguments)| ToolCall {
            id,
            kind: function_kind(),
            function: FunctionCall { name, arguments },
        })
        .collect();

    Ok(Completion {
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls,
    })
}

/// Worth trying again: the request was fine, the far end was not.
fn is_transient(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("timed out")
        || s.contains("timeout")
        || s.contains("connection")
        || s.contains(" 429")
        || s.contains(" 500")
        || s.contains(" 502")
        || s.contains(" 503")
        || s.contains(" 504")
}

impl OpenAiCompatible {
    async fn complete_once(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Completion> {
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
            temperature: self.temperature,
            top_p: self.top_p,
            stream: if self.stream { Some(true) } else { None },
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
        if !status.is_success() {
            // Surface the provider's own error text. When pointing at a new or
            // custom endpoint this is the difference between a usable message
            // and an opaque status code.
            let text = res.text().await.unwrap_or_default();
            bail!("provider returned {status}: {}", text.trim());
        }

        if self.stream {
            return read_stream(res).await;
        }

        let text = res.text().await.context("reading response body")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames captured verbatim from aicredits.in, including the split
    /// arguments and the `"content": null` this provider sends.
    #[test]
    fn reassembles_a_streamed_tool_call() {
        let frames = [
            r#"data: {"choices":[{"index":0,"delta":{"content":null,"role":"assistant","tool_calls":[{"index":0,"id":"chatcmpl-tool-a407","type":"function","function":{"name":"kuzu_graph__graph_stats","arguments":""}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"content":null,"role":"assistant","tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"content":null,"role":"assistant","tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]},"finish_reason":null}]}"#,
            r#": keepalive"#,
            r#"data: {"choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":"tool_calls"}]}"#,
            r#"data: [DONE]"#,
        ];
        let mut content = String::new();
        let mut calls = Vec::new();
        for f in frames {
            absorb(f, &mut content, &mut calls);
        }
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "chatcmpl-tool-a407");
        assert_eq!(calls[0].1, "kuzu_graph__graph_stats");
        // The arguments arrive in pieces and must be concatenated, not replaced.
        assert_eq!(calls[0].2, r#"{"q":"hi"}"#);
    }

    /// Several calls in one turn interleave, so fragments are placed by the
    /// provider's `index` rather than by the order they show up.
    #[test]
    fn keeps_parallel_tool_calls_apart() {
        let frames = [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"first","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"second","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"b\":2}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":1}"}}]}}]}"#,
        ];
        let mut content = String::new();
        let mut calls = Vec::new();
        for f in frames {
            absorb(f, &mut content, &mut calls);
        }
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].1.as_str(), calls[0].2.as_str()), ("first", r#"{"a":1}"#));
        assert_eq!((calls[1].1.as_str(), calls[1].2.as_str()), ("second", r#"{"b":2}"#));
    }

    #[test]
    fn plain_text_frames_accumulate() {
        let mut content = String::new();
        let mut calls = Vec::new();
        for f in [
            r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":", world"}}]}"#,
            r#"data: not json at all"#,
        ] {
            absorb(f, &mut content, &mut calls);
        }
        assert_eq!(content, "Hello, world");
        assert!(calls.is_empty());
    }

    /// The 30s gateway cut-off must be retried; a bad key must not be.
    #[test]
    fn retries_only_what_is_worth_retrying() {
        assert!(is_transient(&anyhow::anyhow!(
            "provider returned 500 Internal Server Error: Internal Server Error"
        )));
        assert!(is_transient(&anyhow::anyhow!("provider returned 429 Too Many Requests")));
        assert!(is_transient(&anyhow::anyhow!("operation timed out")));
        assert!(!is_transient(&anyhow::anyhow!("provider returned 401 Unauthorized")));
        assert!(!is_transient(&anyhow::anyhow!("provider returned 402 Payment Required")));
    }
}
