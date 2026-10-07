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
    /// Why the model stopped: `stop`, `tool_calls`, `length`. `length` with no
    /// content means a reasoning model used the whole reply budget thinking.
    pub finish_reason: Option<String>,
    /// Characters of hidden reasoning received. Measured, never shown.
    pub reasoning_chars: usize,
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Completion>;
    async fn list_models(&self) -> Result<Vec<String>>;
    fn model(&self) -> &str;
}

#[derive(Clone)]
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
    /// Is there another provider after this one in the chain? Decides
    /// whether a rate limit is worth waiting out here or is better spent
    /// moving on. Set by `ProviderChain::build`, which is the only thing
    /// that knows the chain's shape.
    has_fallback: bool,
    /// Cap on hidden thinking. Without it, measured on qwen3.8-27b, a long
    /// question used all 3000 reply tokens reasoning and returned an empty
    /// answer as a "success". Capped at 1200, the same prompt answered in
    /// 2106 characters at the same cost (§59).
    reasoning_budget: Option<u32>,
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
            has_fallback: false,
            reasoning_budget: None,
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

    /// Which endpoint this client talks to. The Providers page shows it, so an
    /// "not offered" answer says WHICH catalogue it looked in.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Told by the chain, not guessed: whether anything follows this provider.
    pub fn reasoning(mut self, budget: Option<u32>) -> Self {
        self.reasoning_budget = budget.filter(|n| *n > 0);
        self
    }

    pub fn with_fallback(mut self, yes: bool) -> Self {
        self.has_fallback = yes;
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
    /// OpenRouter's thinking cap, `{"max_tokens": n}`. Omitted unless set, so
    /// a strict OpenAI-compatible server never sees a field it does not know.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<serde_json::Value>,
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
    #[serde(default)]
    finish_reason: Option<String>,
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
                    /* A 429 is not a hiccup, it is a verdict about right now -
                       and when another provider is waiting behind this one,
                       retrying in place is strictly the wrong order. Measured
                       on the real report: six attempts against OpenRouter's
                       rate-limited free pool, ~11s of backoff, before the
                       chain was allowed to try anything else. Worse than
                       wasted - hammering a saturated shared pool is what the
                       limit is there to stop. So: fall through immediately if
                       there is somewhere to fall through TO, and keep the
                       in-place retry only for the last provider in the chain,
                       where it is the only option left. */
                    /* A spent daily allowance is never worth retrying, with or
                       without a fallback - it resets on a calendar, and every
                       attempt counts against the allowance that is already
                       gone. §50's rule covered the momentary kind; this is the
                       other kind, and it burned five attempts before anyone
                       noticed. */
                    let hopeless = is_quota_limit(&e) || (is_rate_limit(&e) && self.has_fallback);
                    if last || hopeless || !is_transient(&e) {
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
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    /// Hidden thinking: `reasoning` on OpenRouter, `reasoning_content` elsewhere.
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
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
#[cfg(test)]
fn absorb(line: &str, content: &mut String, calls: &mut Vec<(String, String, String)>) {
    absorb_full(line, content, calls, &mut None, &mut 0);
}

/// `absorb`, also keeping why the stream ended and how much it reasoned.
fn absorb_full(
    line: &str,
    content: &mut String,
    calls: &mut Vec<(String, String, String)>,
    finish: &mut Option<String>,
    reasoning: &mut usize,
) {
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
    if let Some(f) = choice.finish_reason {
        *finish = Some(f);
    }
    for r in [&choice.delta.reasoning, &choice.delta.reasoning_content].into_iter().flatten() {
        *reasoning += r.len();
    }
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
    let mut finish: Option<String> = None;
    let mut reasoning = 0usize;

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
            absorb_full(&line, &mut content, &mut calls, &mut finish, &mut reasoning);
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
        finish_reason: finish,
        reasoning_chars: reasoning,
    })
}

/// Worth trying again: the request was fine, the far end was not.
/// Turn a provider's error body into one readable line.
///
/// Gateways nest the thing you need to read. OpenRouter's 429 arrives as
/// `{"error":{"message":"Provider returned error","metadata":{"raw":"<the
/// actual sentence>", ...}}}` - and the outer `message` is the useless half.
/// Dumped verbatim that is nine lines of JSON in the chat window with the one
/// sentence that matters buried in the middle of it.
///
/// Falls back to the raw text whenever the shape is not recognised: a message
/// we cannot parse is still better than one we have thrown away.
fn explain(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.chars().take(400).collect();
    };
    let err = v.get("error").unwrap_or(&v);
    // `metadata.raw` first: when both exist it is the specific one.
    let raw = err.pointer("/metadata/raw").and_then(|x| x.as_str());
    let msg = err.get("message").and_then(|x| x.as_str());
    let mut out = match (raw, msg) {
        (Some(r), _) => r.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => body.chars().take(400).collect(),
    };
    // Which upstream, when the gateway says. "rate-limited" means little
    // without knowing whose limit was hit.
    if let Some(p) = err.pointer("/metadata/provider_name").and_then(|x| x.as_str()) {
        if !out.contains(p) {
            out = format!("{out} (upstream: {p})");
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(400).collect()
}

/// A rate limit specifically - "not now", as opposed to "something broke".
fn is_rate_limit(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains(" 429") || s.contains("too many requests") || s.contains("rate-limited")
}

/// A rate limit measured in DAYS, not seconds - a spent allowance rather than a
/// busy moment.
///
/// The difference decides whether waiting is worth anything at all. A saturated
/// upstream pool can clear while you back off; `free-models-per-day` cannot -
/// it resets on a calendar, and no amount of retrying moves it. Observed
/// burning five attempts on exactly that:
///
/// ```text
/// 429 Rate limit exceeded: free-models-per-day. Add 5 credits to unlock
/// 1000 free model requests per day (after 0.0s, attempt 5)
/// ```
///
/// Five requests spent against a wall that resets at midnight, not in seconds.
///
/// (Measured afterwards: a rejected 429 does NOT appear to count against the
/// daily allowance - five failed attempts left `used` at 2, which was the two
/// calls that actually succeeded. So the waste is time, not allowance. Worth
/// stating precisely rather than assuming the worse version.)
fn is_quota_limit(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_lowercase();
    is_rate_limit(e)
        && (s.contains("per-day")
            || s.contains("per day")
            || s.contains("daily")
            || s.contains("quota")
            || s.contains("add credits")
            || s.contains("add 5 credits"))
}

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
            reasoning: self.reasoning_budget.map(|n| serde_json::json!({ "max_tokens": n })),
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
            bail!("provider returned {status}: {}", explain(text.trim()));
        }

        if self.stream {
            return read_stream(res).await;
        }

        let text = res.text().await.context("reading response body")?;
        let parsed: ChatResponse = serde_json::from_str(&text)
            .with_context(|| format!("parsing chat response: {text}"))?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .context("provider returned no choices")?;
        let msg = choice.message;

        Ok(Completion {
            content: msg.content,
            tool_calls: msg.tool_calls.unwrap_or_default(),
            finish_reason: choice.finish_reason,
            reasoning_chars: 0,
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
    fn thinking_until_the_budget_runs_out_is_recognised() {
        // The shape of the blank replies in session 20260829-101419-58c030b6:
        // reasoning frames only, then `length`, and no content at all.
        let (mut content, mut calls) = (String::new(), Vec::new());
        let (mut finish, mut reasoning) = (None, 0usize);
        for line in [
            r#"data: {"choices":[{"delta":{"reasoning":"Let me think about GPUs"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_content":" and VRAM"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"length"}]}"#,
            "data: [DONE]",
        ] {
            absorb_full(line, &mut content, &mut calls, &mut finish, &mut reasoning);
        }
        assert!(content.is_empty());
        assert_eq!(finish.as_deref(), Some("length"));
        assert_eq!(reasoning, "Let me think about GPUs and VRAM".len());
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

    /// A 429 is "not now", which is different from "something broke" - and the
    /// difference decides whether to wait here or move to the next provider.
    #[test]
    fn tells_a_rate_limit_from_a_fault() {
        assert!(is_rate_limit(&anyhow::anyhow!(
            "provider returned 429 Too Many Requests"
        )));
        assert!(is_rate_limit(&anyhow::anyhow!(
            "qwen/qwen3.8-27b:free is temporarily rate-limited upstream"
        )));
        assert!(!is_rate_limit(&anyhow::anyhow!(
            "provider returned 500 Internal Server Error"
        )));
        assert!(!is_rate_limit(&anyhow::anyhow!(
            "provider returned 402 Payment Required"
        )));
    }

    /// A daily allowance and a busy upstream are both 429s and must not be
    /// treated the same: one resets on a calendar, the other in seconds.
    #[test]
    fn tells_a_spent_allowance_from_a_busy_moment() {
        // Verbatim from the report.
        let daily = anyhow::anyhow!(
            "provider returned 429 Too Many Requests: Rate limit exceeded: \
             free-models-per-day. Add 5 credits to unlock 1000 free model requests per day"
        );
        assert!(is_rate_limit(&daily));
        assert!(is_quota_limit(&daily), "a daily cap must never be retried in place");

        // The §50 kind: momentary, and worth falling through for.
        let momentary = anyhow::anyhow!(
            "provider returned 429 Too Many Requests: qwen/qwen3.8-27b:free is \
             temporarily rate-limited upstream. Please retry shortly"
        );
        assert!(is_rate_limit(&momentary));
        assert!(!is_quota_limit(&momentary));

        // Not a rate limit at all.
        assert!(!is_quota_limit(&anyhow::anyhow!(
            "provider returned 500 Internal Server Error"
        )));
    }

    /// The reported error was nine lines of nested JSON with the one useful
    /// sentence buried in it. Verbatim from OpenRouter.
    #[test]
    fn unwraps_the_sentence_that_matters() {
        let body = r#"{"error":{"message":"Provider returned error","code":429,
          "metadata":{"raw":"qwen/qwen3.8-27b:free is temporarily rate-limited upstream. Please retry shortly.",
          "provider_name":"ModelRun","is_byok":false}},"user_id":"user_3Bch"}"#;
        let out = explain(body);
        assert!(out.starts_with("qwen/qwen3.8-27b:free is temporarily rate-limited"));
        assert!(out.contains("upstream: ModelRun"));
        // The outer, useless half must not survive.
        assert!(!out.contains("Provider returned error"));
        assert!(!out.contains("user_id"));

        // Plain `message` when there is no metadata.
        assert_eq!(
            explain(r#"{"error":{"message":"Insufficient Balance"}}"#),
            "Insufficient Balance"
        );
        // Unparseable bodies are passed through, never swallowed.
        assert_eq!(explain("upstream exploded"), "upstream exploded");
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
