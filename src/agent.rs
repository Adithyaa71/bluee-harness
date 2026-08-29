//! The agent turn loop, independent of any interface.
//!
//! Extracted from the CLI so the dashboard, the CLI, and later a voice module
//! all drive the *same* loop rather than three parallel implementations. §11
//! makes this explicit for voice ("purely additive on top of the text path"),
//! and it is the same argument for the dashboard.

use anyhow::Result;
use std::sync::Arc;

use crate::config::{self, Config};
use crate::eventlog::{EventKind, EventLog};
use crate::llm::{Message, ToolDef};
use crate::providers::ProviderChain;
use crate::mcp::McpRegistry;
use crate::tools::{NativeTools, WITHHELD_FROM_MODEL};
use crate::vision::{VisionMode, VisionState};

/// Live sink for turn events. The dashboard passes one so tool calls appear
/// the moment they happen instead of arriving in a lump when the turn ends.
pub type EventSink = tokio::sync::mpsc::UnboundedSender<TurnEvent>;

fn emit(out: &mut Vec<TurnEvent>, sink: Option<&EventSink>, ev: TurnEvent) {
    if let Some(tx) = sink {
        let _ = tx.send(ev.clone());
    }
    out.push(ev);
}

/// A runaway tool loop burns money quietly, so it is bounded, not trusted.
const MAX_TOOL_ROUNDS: usize = 8;

/// What `/compact` did, reported back so the effect is visible rather than
/// something that silently happened to your conversation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompactReport {
    pub session: String,
    pub indexed: usize,
    pub replaced: usize,
    pub dropped: usize,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub before_msgs: usize,
    pub after_msgs: usize,
}

/// What happened during a turn, in order. Interfaces render these however
/// suits them - the CLI prints them, the dashboard streams them as JSON.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    ToolCall {
        server: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolResult {
        ok: bool,
        result: serde_json::Value,
    },
    Reply {
        text: String,
    },
    Error {
        message: String,
    },
}

pub struct Agent {
    chain: ProviderChain,
    registry: Arc<McpRegistry>,
    native: NativeTools,
    log: EventLog,
    history: Vec<Message>,
    tool_defs: Vec<ToolDef>,
    pub persona_files: Vec<String>,
    pub model: String,
    pub provider_name: String,
    pub native_count: usize,
    vision: Arc<VisionState>,
    /// Separate client: ACTIVE screen reads need a model that accepts images,
    /// which is rarely the same one doing the chatting.
    vision_client: crate::llm::OpenAiCompatible,
    vision_model: String,
    /// Whether the VLM tool is currently in `tool_defs`, so the gate can be
    /// synced without rebuilding the whole list every turn.
    vlm_offered: bool,
}

/// The one tool that only exists when you have said it may (§6).
fn vlm_tool_def() -> ToolDef {
    ToolDef {
        name: "analyze_screen_vlm".into(),
        description: "Look at the screen with a vision model and answer a question about it.             Expensive and slow compared to reading the accessibility tree - use             uacc__get_screen_info first, and only reach for this when the structure genuinely             is not enough: a chart, a canvas, an image, an unlabelled icon, an ambiguous layout."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "What you need to know about what is on screen."
                }
            },
            "required": ["question"]
        }),
    }
}

impl Agent {
    /// Start a fresh session.
    pub async fn new(
        cfg: &Config,
        registry: Arc<McpRegistry>,
        vision: Arc<VisionState>,
    ) -> Result<Self> {
        Self::start(cfg, registry, vision, None).await
    }

    /// Reopen an existing session and genuinely continue it: the event log is
    /// appended to rather than replaced, and the conversation is rebuilt from
    /// what was actually said.
    pub async fn resume(
        cfg: &Config,
        registry: Arc<McpRegistry>,
        vision: Arc<VisionState>,
        session_id: &str,
    ) -> Result<Self> {
        Self::start(cfg, registry, vision, Some(session_id)).await
    }

    async fn start(
        cfg: &Config,
        registry: Arc<McpRegistry>,
        vision: Arc<VisionState>,
        resume: Option<&str>,
    ) -> Result<Self> {
        cfg.require_credentials()?;

        let (persona, persona_files) = config::load_persona(&cfg.persona_dir)?;
        // A chain, not one provider: if the first endpoint is out of credit or
        // throwing 500s, the next one answers instead of the turn dying.
        let chain = ProviderChain::build(cfg)?;
        let (provider_name, active_model) = chain.primary();

        let allow: Option<Vec<String>> = std::env::var("HARNESS_TOOL_SERVERS")
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect());

        let native = NativeTools::open(&cfg.data_dir, &cfg.skills_dir)?;
        let mut tool_defs = NativeTools::defs();
        let native_count = tool_defs.len();

        tool_defs.extend(
            registry
                .tools()
                .iter()
                .filter(|t| allow.as_ref().is_none_or(|a| a.contains(&t.server)))
                .filter(|t| !WITHHELD_FROM_MODEL.contains(&t.qualified().as_str()))
                .map(|t| ToolDef {
                    name: t.qualified(),
                    description: t.description.clone(),
                    parameters: t.schema.clone(),
                }),
        );

        let mut log = match resume {
            Some(id) => EventLog::open(cfg.events_dir(), id)?,
            None => EventLog::new_session(cfg.events_dir())?,
        };
        log.append(EventKind::SessionStart {
            model: active_model.clone(),
            persona_files: persona_files.clone(),
        })?;

        // Assembled once and never mutated mid-session (§5b), which keeps the
        // prompt prefix stable for provider-side caching.
        let mut history = Vec::new();
        if !persona.is_empty() {
            history.push(Message::system(persona));
        }

        // Replay a resumed conversation into the prompt.
        //
        // Deliberately only user and assistant *text* is replayed, not the
        // tool_call/tool_result pairs. Reconstructing those means re-emitting
        // matching ids in exactly the shape the provider expects, and a single
        // mismatch gets the whole request rejected. The tool detail is not lost
        // - it is all still in the event log, and reachable through
        // search_memory - it just does not go back into the prompt verbatim.
        if resume.is_some() {
            let mut replayed = 0usize;
            for event in EventLog::read(log.path())? {
                match event.kind {
                    EventKind::UserMessage { text } => {
                        history.push(Message::user(text));
                        replayed += 1;
                    }
                    EventKind::AssistantMessage { text } if !text.is_empty() => {
                        history.push(Message::assistant(text));
                        replayed += 1;
                    }
                    _ => {}
                }
            }
            eprintln!("[bluee] resumed session with {replayed} message(s) of history");
        }

        // §6's gate: in OFF and PASSIVE the tool is not merely discouraged, it
        // is absent, so the model cannot call something you did not authorise.
        let vlm_offered = vision.mode().vlm_allowed();
        if vlm_offered {
            tool_defs.push(vlm_tool_def());
        }

        Ok(Self {
            chain,
            registry,
            native,
            log,
            history,
            tool_defs,
            persona_files,
            model: active_model,
            provider_name,
            native_count,
            vision_client: crate::llm::OpenAiCompatible::new(
                &cfg.base_url,
                &cfg.api_key,
                &cfg.vision_model,
                cfg.max_tokens,
            ),
            vision_model: cfg.vision_model.clone(),
            vision,
            vlm_offered,
        })
    }

    /// Add or remove the VLM tool if the toggle moved since the last turn.
    fn sync_vision_tools(&mut self) {
        let allowed = self.vision.mode().vlm_allowed();
        if allowed == self.vlm_offered {
            return;
        }
        if allowed {
            self.tool_defs.push(vlm_tool_def());
        } else {
            self.tool_defs.retain(|t| t.name != "analyze_screen_vlm");
        }
        self.vlm_offered = allowed;
        let _ = self.log.append(EventKind::System {
            note: format!(
                "vision mode {} - analyze_screen_vlm {} the toolset",
                self.vision.mode().as_str(),
                if allowed { "added to" } else { "removed from" }
            ),
        });
    }

    /// Take a screenshot and ask a vision model about it (§6 ACTIVE path).
    ///
    /// Takes `&mut self` rather than `&self` only because `Agent` holds a
    /// rusqlite `Connection` (a `RefCell` inside), which makes `&Agent` not
    /// `Send` and therefore unusable across an await in a spawned task. `&mut`
    /// needs `Send`, not `Sync`, so it compiles and stays sound.
    async fn analyze_screen(&mut self, question: &str) -> Result<serde_json::Value> {
        if !self.vision.mode().vlm_allowed() {
            anyhow::bail!("vision is not in ACTIVE mode");
        }
        if self.vision_model.is_empty() {
            anyhow::bail!(
                "no vision model configured. Set LLM_VISION_MODEL to a model that accepts                  images (the chat model almost certainly does not)."
            );
        }
        let shot = self
            .registry
            .call("uacc", "screenshot", serde_json::json!({}))
            .await?;
        let b64 = crate::vision::extract_image_b64(&shot)
            .ok_or_else(|| anyhow::anyhow!("screenshot returned no image data"))?;

        let answer = self
            .vision_client
            .vision(&self.vision_model, question, &b64)
            .await?;
        Ok(serde_json::json!({
            "question": question,
            "answer": answer,
            "model": self.vision_model,
        }))
    }

    pub fn vision_mode(&self) -> VisionMode {
        self.vision.mode()
    }

    /// Record a passive screen read into the session log.
    ///
    /// It goes into the log and nowhere else - deliberately not into the
    /// prompt. Injecting a screen dump into every turn would swamp the context
    /// with exactly the low-value noise §8 warns about. It becomes searchable
    /// like anything else once the reducer runs, so bluee can go and look when
    /// a question actually needs it.
    pub fn log_screen(&mut self, mode: &str, text: &str) {
        let _ = self.log.append(EventKind::ScreenContext {
            mode: mode.to_string(),
            text: text.chars().take(4000).collect(),
        });
    }

    pub fn session_id(&self) -> &str {
        self.log.session_id()
    }

    pub fn tool_count(&self) -> usize {
        self.tool_defs.len()
    }

    pub fn provider_count(&self) -> usize {
        self.chain.len()
    }

    /// What the context meter measures against - the active provider's
    /// configured window, not a constant baked into the dashboard.
    pub fn context_window(&self) -> u32 {
        self.chain.context_window()
    }

    /// Name this session. Appends rather than edits, so the rename history
    /// stays in the log like everything else.
    pub fn set_title(&mut self, title: &str) -> Result<()> {
        self.log.append(EventKind::SessionTitle {
            title: title.trim().to_string(),
        })?;
        Ok(())
    }

    /// Current display name for this session, from its own log.
    pub fn title(&self) -> Option<String> {
        let events = EventLog::read(self.log.path()).unwrap_or_default();
        crate::eventlog::session_title(&events)
    }

    /// Rough prompt size. Not exact tokenisation - the point is a usable
    /// "how full am I" signal, and ~4 chars per token is close enough to warn
    /// at the right time. Over-reporting is safer than under-reporting here.
    pub fn context_estimate(&self) -> (usize, usize) {
        let chars: usize = self
            .history
            .iter()
            .map(|m| {
                m.content.as_ref().map(|c| c.len()).unwrap_or(0)
                    + m.tool_calls
                        .as_ref()
                        .map(|t| t.iter().map(|c| c.function.arguments.len() + 40).sum())
                        .unwrap_or(0)
            })
            .sum();
        let tool_schema_chars: usize = self
            .tool_defs
            .iter()
            .map(|t| t.name.len() + t.description.len() + t.parameters.to_string().len())
            .sum();
        ((chars + tool_schema_chars) / 4, self.history.len())
    }

    /// Fold this session into memory and shrink the prompt (§4f-d item 6).
    ///
    /// Nothing is summarised and nothing is thrown away: the event log already
    /// holds every character permanently, so "lossless" is true by
    /// construction. What this does is move the conversation *out of the
    /// prompt* and *into searchable memory*, scoped to this session so it does
    /// not blur into general memory and can be recalled preferentially.
    ///
    /// `keep` recent messages stay verbatim so the thread of the current
    /// exchange is not broken mid-task.
    pub fn compact(&mut self, keep: usize) -> Result<CompactReport> {
        let (before_tokens, before_msgs) = self.context_estimate();

        let events = EventLog::read(self.log.path())?;
        let chunks = crate::reduce::chunk_session(&events);

        let session = self.log.session_id().to_string();
        let store = self.native.store();
        let removed = store.clear_session(&session)?;

        let mut indexed = 0usize;
        if !chunks.is_empty() {
            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = self.native.embed(texts)?;
            let store = self.native.store();
            for (chunk, embedding) in chunks.iter().zip(embeddings) {
                store.insert_scoped(chunk, &embedding, "session")?;
                indexed += 1;
            }
        }

        // Trim the prompt: system message, a marker so the model knows why the
        // history looks short, then the tail.
        let system: Vec<Message> = self
            .history
            .iter()
            .filter(|m| m.role == "system")
            .cloned()
            .collect();
        let rest: Vec<Message> = self
            .history
            .iter()
            .filter(|m| m.role != "system")
            .cloned()
            .collect();

        let tail_start = rest.len().saturating_sub(keep);
        // Never start the tail on a tool result - it would have no call to
        // pair with and the provider would reject the request.
        let mut tail_start = tail_start;
        while tail_start < rest.len() && rest[tail_start].role == "tool" {
            tail_start += 1;
        }
        let dropped = tail_start;

        let mut history = system;
        if dropped > 0 {
            history.push(Message::system(format!(
                "[context compacted] {dropped} earlier message(s) of this session were moved out \
                 of the prompt into searchable session memory. Nothing was lost or summarised - \
                 the full record is intact. If you need anything from earlier in this \
                 conversation, call search_memory; results from this session are marked \
                 scope \"session\"."
            )));
        }
        history.extend_from_slice(&rest[tail_start..]);
        self.history = history;

        let (after_tokens, after_msgs) = self.context_estimate();

        self.log.append(EventKind::System {
            note: format!(
                "compacted: {indexed} chunk(s) indexed, {dropped} message(s) moved out of prompt, \
                 ~{before_tokens} -> ~{after_tokens} tokens"
            ),
        })?;

        Ok(CompactReport {
            session,
            indexed,
            replaced: removed,
            dropped,
            before_tokens,
            after_tokens,
            before_msgs,
            after_msgs,
        })
    }

    /// Run one full turn, returning everything that happened.
    pub async fn turn(&mut self, input: &str) -> Vec<TurnEvent> {
        self.turn_with(input, None).await
    }

    /// Same turn, but also pushing each event to `sink` as it occurs.
    pub async fn turn_with(&mut self, input: &str, sink: Option<&EventSink>) -> Vec<TurnEvent> {
        let mut out = Vec::new();

        if let Err(e) = self.log.append(EventKind::UserMessage { text: input.into() }) {
            emit(&mut out, sink, TurnEvent::Error { message: format!("{e:#}") });
            return out;
        }
        self.history.push(Message::user(input));
        self.sync_vision_tools();

        for round in 0..MAX_TOOL_ROUNDS {
            let completion = match self.chain.complete(&self.history, &self.tool_defs).await {
                // The chain reports which provider actually answered, so a
                // silent failover still shows up in the session metadata.
                Ok((c, model)) => {
                    if model != self.model {
                        self.model = model;
                    }
                    c
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    let _ = self.log.append(EventKind::Error {
                        context: "provider.chain".into(),
                        message: message.clone(),
                    });
                    emit(&mut out, sink, TurnEvent::Error { message });
                    return out;
                }
            };

            if completion.tool_calls.is_empty() {
                let text = completion.content.unwrap_or_default();
                let _ = self.log.append(EventKind::AssistantMessage { text: text.clone() });
                self.history.push(Message::assistant(text.clone()));
                emit(&mut out, sink, TurnEvent::Reply { text });
                return out;
            }

            // The model usually says why before it calls something. That text
            // was going into the prompt but never into the log, so the record
            // showed what ran with no trace of the reasoning. Log it - it is a
            // true thing the assistant said, and it is what the Tasks panel
            // shows as the reason for each call.
            if let Some(said) = completion.content.as_deref() {
                if !said.trim().is_empty() {
                    let _ = self.log.append(EventKind::AssistantMessage {
                        text: said.to_string(),
                    });
                }
            }

            // Must go into history verbatim or the tool messages below have
            // nothing to pair against.
            self.history.push(Message {
                role: "assistant".into(),
                content: completion.content.clone(),
                tool_calls: Some(completion.tool_calls.clone()),
                tool_call_id: None,
            });

            for tc in &completion.tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));

                let (server, tool) = if tc.function.name == "analyze_screen_vlm" {
                    ("harness".to_string(), tc.function.name.clone())
                } else if NativeTools::handles(&tc.function.name) {
                    ("harness".to_string(), tc.function.name.clone())
                } else {
                    match self.registry.resolve(&tc.function.name) {
                        Some(pair) => pair,
                        None => {
                            let message = format!("unknown tool `{}`", tc.function.name);
                            let _ = self.log.append(EventKind::Error {
                                context: "tool.resolve".into(),
                                message: message.clone(),
                            });
                            self.history
                                .push(Message::tool_result(&tc.id, message.clone()));
                            emit(&mut out, sink, TurnEvent::Error { message });
                            continue;
                        }
                    }
                };

                emit(
                    &mut out,
                    sink,
                    TurnEvent::ToolCall {
                        server: server.clone(),
                        tool: tool.clone(),
                        args: args.clone(),
                    },
                );
                let _ = self.log.append(EventKind::ToolCall {
                    call_id: tc.id.clone(),
                    server: server.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                });

                let outcome = if tool == "analyze_screen_vlm" {
                    let q = args.get("question").and_then(|v| v.as_str()).unwrap_or("What is on screen?");
                    self.analyze_screen(q).await
                } else if server == "harness" {
                    self.native.call(&tool, &args)
                } else {
                    self.registry.call(&server, &tool, args).await
                };

                match outcome {
                    Ok(value) => {
                        let _ = self.log.append(EventKind::ToolResult {
                            call_id: tc.id.clone(),
                            ok: true,
                            result: value.clone(),
                        });
                        self.history
                            .push(Message::tool_result(&tc.id, value.to_string()));
                        emit(&mut out, sink, TurnEvent::ToolResult { ok: true, result: value });
                    }
                    Err(e) => {
                        // Reported back to the model rather than aborting: it
                        // can often recover by choosing a different tool.
                        let message = format!("{e:#}");
                        let result = serde_json::json!({ "error": message });
                        let _ = self.log.append(EventKind::ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            result: result.clone(),
                        });
                        self.history.push(Message::tool_result(
                            &tc.id,
                            format!("Tool call failed: {message}"),
                        ));
                        emit(&mut out, sink, TurnEvent::ToolResult { ok: false, result });
                    }
                }
            }

            if round == MAX_TOOL_ROUNDS - 1 {
                let message = format!("stopped after {MAX_TOOL_ROUNDS} tool rounds");
                let _ = self.log.append(EventKind::Error {
                    context: "agent.turn".into(),
                    message: message.clone(),
                });
                emit(&mut out, sink, TurnEvent::Error { message });
            }
        }

        out
    }

    pub fn end(&mut self, reason: &str) {
        let _ = self.log.append(EventKind::SessionEnd {
            reason: reason.into(),
        });
    }
}
