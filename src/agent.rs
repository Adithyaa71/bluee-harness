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

/// What Adithya picked in the composer for one message: `/skill` and
/// `@server` / `@tool` / `@browser` chips. Applied to that turn only - the
/// skills are attached, the tools loaded, and the model told plainly that he
/// chose them, which is stronger than any amount of matching on his words.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Picks {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub servers: Vec<String>,
    /// Qualified `server__tool` names.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub browser: Option<String>,
}

impl Picks {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.servers.is_empty() && self.tools.is_empty() && self.browser.is_none()
    }
}

fn emit(out: &mut Vec<TurnEvent>, sink: Option<&EventSink>, ev: TurnEvent) {
    if let Some(tx) = sink {
        let _ = tx.send(ev.clone());
    }
    out.push(ev);
}

/// A runaway tool loop burns money quietly, so it is bounded, not trusted.
///
/// Was 8, and §55's report ended on "stopped after 8 tool rounds" with the
/// answer one call away. With deferred tools a round costs a fraction of what
/// it did, so the bound can afford to be about runaways rather than about
/// ordinary multi-step work. Hitting it no longer ends the turn in an error
/// either - see `finish_without_tools`.
const MAX_TOOL_ROUNDS: usize = 20;

/// What to tell the user when the model "succeeds" with no text at all.
///
/// Session 20260829-101419-58c030b6 got three blank bubbles in a row: the
/// model spent its whole reply budget reasoning, the provider called that a
/// success, and the harness logged an empty message. Nobody - not even bluee,
/// asked afterwards - could say why. The reason is in `finish_reason`.
fn empty_reply_message(finish: Option<&str>, reasoning_chars: usize) -> String {
    match finish {
        Some("length") => format!(
            "No reply: the model spent its whole reply budget thinking and never started the \
             answer (finish_reason: length{}). Nothing is lost - ask again. If it keeps \
             happening, raise the reply cap or lower the thinking budget on Settings -> \
             Providers.",
            if reasoning_chars > 0 {
                format!(", ~{} characters of hidden reasoning", reasoning_chars)
            } else {
                String::new()
            }
        ),
        Some(other) => format!(
            "No reply: the model returned an empty answer (finish_reason: {other}). Ask again."
        ),
        None => "No reply: the model returned an empty answer and gave no reason. Ask again."
            .to_string(),
    }
}

/// How much of one tool result goes back into the PROMPT. The event log keeps
/// every byte; this only bounds what is re-sent on every later round. One
/// `get_screen_info` or crawled page could otherwise be paid for twenty times.
const MAX_RESULT_IN_PROMPT: usize = 20_000;

/// Head and tail of an oversized result, with the cut stated, so the model
/// knows it is looking at a clipped view and that the rest exists.
fn clip_for_prompt(s: String) -> String {
    if s.len() <= MAX_RESULT_IN_PROMPT {
        return s;
    }
    let mut head = MAX_RESULT_IN_PROMPT * 3 / 4;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - MAX_RESULT_IN_PROMPT / 5;
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n\n[... {} characters clipped from the middle of this result to keep the prompt \
         small. The full result is in the event log. Ask a narrower question if you need the \
         missing part.]\n\n{}",
        &s[..head],
        tail - head,
        &s[tail..]
    )
}

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
    /// Sub-agents this agent may start (§ src/subagents.rs). `None` for the
    /// CLI and for loops: a scheduled job that can spawn agents unattended is
    /// a cost hazard, and nothing has asked for it.
    subagents: Option<Arc<crate::subagents::SubAgents>>,
    /// Set when THIS agent is a sub-agent: its line to bluee and to Adithya
    /// (`message_parent`, `ask_user`). Never both this and `subagents`.
    child: Option<crate::subagents::ChildLink>,
    /// Composer picks for the NEXT turn only; taken (and cleared) by it.
    picks: Picks,
    /// Tokens and dollars this agent has spent since it was built, from the
    /// provider's own usage reports. What sub-agent caps are checked against.
    spent: crate::llm::Usage,
    /// Separate client: ACTIVE screen reads need a model that accepts images,
    /// which is rarely the same one doing the chatting.
    vision_client: crate::llm::OpenAiCompatible,
    vision_model: String,
    /// Whether the VLM tool is currently in `tool_defs`, so the gate can be
    /// synced without rebuilding the whole list every turn.
    vlm_offered: bool,
    /// MCP servers the active workspace exposes. `None` means all of them.
    allowed_servers: Option<Vec<String>>,
    /// Deferred tool loading (§ src/toolsearch.rs): whether it is on, which
    /// servers are always loaded, and what `find_tools` has loaded so far.
    defer: bool,
    core_servers: Vec<String>,
    loaded: std::collections::HashSet<String>,
    /// Which of this session's turns are already in memory (by seq_start),
    /// so live indexing embeds only what is new.
    live_indexed: std::collections::HashSet<u64>,
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

/// The composed GUI tools (§ src/gui.rs) are a thin layer over UACC, so they
/// are only offered when UACC is actually connected AND allowed in this
/// workspace. Offering a tool whose backing server is absent is worse than not
/// offering it - the model reaches for it and gets a failure it cannot fix.
fn gui_defs_if_available(
    registry: &McpRegistry,
    allow: &Option<Vec<String>>,
) -> Vec<ToolDef> {
    let uacc_ok = registry.tools().iter().any(|t| t.server == "uacc")
        && allow.as_ref().is_none_or(|a| a.iter().any(|s| s == "uacc"));
    if uacc_ok {
        crate::gui::defs()
    } else {
        Vec::new()
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

        let native = NativeTools::open(cfg)?;
        let native_count = NativeTools::defs().len();

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
        //
        // What a tool RETURNED is folded in as plain text on the assistant's
        // side, though - no ids, so nothing for the provider to reject. Without
        // it a resumed agent sees itself say "teal" with no idea where that
        // came from (found when a sub-agent woke from sleep and denied ever
        // being told the answer its own ask_user call had received).
        if resume.is_some() {
            let mut replayed = 0usize;
            let mut names: std::collections::HashMap<String, String> = Default::default();
            let mut used: Vec<String> = Vec::new();
            let flush = |used: &mut Vec<String>, text: Option<String>| -> Option<String> {
                if used.is_empty() {
                    return text;
                }
                let mut out = format!("[tools I used: {}]", used.join("; "));
                used.clear();
                if let Some(t) = text {
                    out.push_str("\n\n");
                    out.push_str(&t);
                }
                Some(out)
            };
            for event in EventLog::read(log.path())? {
                match event.kind {
                    EventKind::UserMessage { text } => {
                        if let Some(t) = flush(&mut used, None) {
                            history.push(Message::assistant(t));
                        }
                        history.push(Message::user(text));
                        replayed += 1;
                    }
                    EventKind::ToolCall { call_id, tool, .. } => {
                        names.insert(call_id, tool);
                    }
                    EventKind::ToolResult { call_id, ok, result } => {
                        let tool = names.get(&call_id).cloned().unwrap_or_else(|| "tool".into());
                        let body: String = result.to_string().chars().take(600).collect();
                        used.push(format!("{tool} -> {}{body}", if ok { "" } else { "FAILED " }));
                    }
                    EventKind::AssistantMessage { text } if !text.is_empty() => {
                        let t = flush(&mut used, Some(text)).unwrap_or_default();
                        history.push(Message::assistant(t));
                        replayed += 1;
                    }
                    _ => {}
                }
            }
            if let Some(t) = flush(&mut used, None) {
                history.push(Message::assistant(t));
            }
            eprintln!("[bluee] resumed session with {replayed} message(s) of history");
        }

        let mut native = native;
        native.set_session(log.session_id());
        let live_indexed = native
            .store()
            .session_seq_starts(log.session_id())
            .unwrap_or_default();

        let mut agent = Self {
            chain,
            registry,
            native,
            log,
            history,
            tool_defs: Vec::new(),
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
            vlm_offered: false,
            // Starts from the .env allowlist; a workspace can narrow it further
            // at runtime without restarting.
            allowed_servers: allow,
            subagents: None,
            child: None,
            picks: Picks::default(),
            spent: Default::default(),
            defer: crate::toolsearch::enabled(),
            core_servers: crate::toolsearch::core_servers(),
            loaded: Default::default(),
            live_indexed,
        };
        // One place builds the toolset - the same function a workspace switch,
        // a reconnect and a `find_tools` load all go through. §6's gate is
        // applied inside it: in OFF and PASSIVE the VLM tool is absent, not
        // merely discouraged.
        agent.rebuild_tools();
        Ok(agent)
    }

    /// Add or remove the VLM tool if the toggle moved since the last turn.
    /// Narrow the exposed MCP servers to what the active workspace wants.
    ///
    /// This is the cost lever, not a nicety: 106 tool schemas are ~18,300
    /// prompt tokens on every turn. Rebuilding the list rather than filtering
    /// at call time means the model genuinely cannot see the others, which is
    /// also what makes it choose better among the ones it can.
    /// Hand this agent the sub-agent registry. A setter rather than another
    /// constructor argument, because `Agent::new` has four call sites and only
    /// one of them wants this - the same shape `set_allowed_servers` already
    /// uses for the other thing that varies per surface.
    pub fn set_subagents(&mut self, subs: Arc<crate::subagents::SubAgents>) {
        self.subagents = Some(subs);
        self.rebuild_tools();
    }

    /// Make this agent a sub-agent. It gains `message_parent` and `ask_user`.
    pub fn set_child(&mut self, link: crate::subagents::ChildLink) {
        self.child = Some(link);
        self.rebuild_tools();
    }

    /// Put a note into the conversation that stays there: logged as a System
    /// event and kept in the prompt. Used for sub-agent updates and roles.
    pub fn add_note(&mut self, text: &str) {
        let _ = self.log.append(EventKind::System { note: text.to_string() });
        self.history.push(Message::system(text.to_string()));
    }

    pub fn set_allowed_servers(&mut self, allow: Option<Vec<String>>) {
        if allow == self.allowed_servers {
            return;
        }
        self.allowed_servers = allow;
        self.rebuild_tools();
        let note = match &self.allowed_servers {
            Some(list) if list.is_empty() => {
                format!("workspace tools: none - {} tool(s) exposed", self.tool_defs.len())
            }
            Some(list) => format!(
                "workspace tools: {} - {} tool(s) exposed",
                list.join(", "),
                self.tool_defs.len()
            ),
            None => format!("workspace tools: all - {} tool(s) exposed", self.tool_defs.len()),
        };
        let _ = self.log.append(EventKind::System { note });
    }

    /// Re-read the toolset from the registry.
    ///
    /// The tool list is built once at session start, so a live agent goes on
    /// offering the tools it had - including none at all, if the servers failed
    /// to start. Reconnecting has to tell it, or the reconnect appears to do
    /// nothing until the next new chat.
    pub fn refresh_tools(&mut self) {
        self.rebuild_tools();
        let n = self.tool_defs.len();
        let _ = self.log.append(EventKind::System {
            note: format!("tool servers reconnected - {n} tool(s) now available"),
        });
    }

    /// Native tools plus whichever MCP servers are currently allowed.
    fn rebuild_tools(&mut self) {
        let mut defs = NativeTools::defs();
        // Long-term facts are memory, and memory is never optional (§ facts).
        defs.extend(crate::facts::defs());
        if self.subagents.is_some() {
            defs.extend(crate::subagents::SubAgents::defs());
        }
        if let Some(link) = &self.child {
            defs.extend(crate::subagents::ChildLink::defs());
            // An agent that owns a browser gets a fallback for it (§ webtools).
            if let Some(b) = &link.browser {
                defs.extend(crate::webtools::defs(b));
            }
        }
        let allow = self.allowed_servers.clone();
        defs.extend(gui_defs_if_available(&self.registry, &allow));

        let mcp = self.mcp_pool();
        let deferrable = mcp
            .iter()
            .filter(|t| !self.is_core(&t.name) && !self.loaded.contains(&t.name))
            .count();
        if self.defer && deferrable >= crate::toolsearch::MIN_DEFERRED {
            let (now, later): (Vec<&ToolDef>, Vec<&ToolDef>) = mcp
                .iter()
                .partition(|t| self.is_core(&t.name) || self.loaded.contains(&t.name));
            defs.push(crate::toolsearch::def(&later));
            defs.extend(now.into_iter().cloned());
        } else {
            defs.extend(mcp);
        }
        self.tool_defs = defs;
        // The vision gate is applied on top, so switching workspaces cannot
        // smuggle the VLM tool back in. Applied silently here: changes of mode
        // are logged by `sync_vision_tools`, and a rebuild is not one.
        self.vlm_offered = self.vision.mode().vlm_allowed();
        if self.vlm_offered {
            self.tool_defs.push(vlm_tool_def());
        }
    }

    /// Every MCP tool this workspace may use, as the model would see it.
    fn mcp_pool(&self) -> Vec<ToolDef> {
        let allow = &self.allowed_servers;
        // Core servers (the graph - long-term memory) are allowed everywhere:
        // in every workspace, every loop, every sub-agent, whatever the
        // Connectors tick-boxes say. Unticking memory would leave an agent
        // that cannot remember, which is never what a scope choice means.
        self.registry
            .tools()
            .iter()
            .filter(|t| {
                self.core_servers.contains(&t.server)
                    || allow.as_ref().is_none_or(|a| a.contains(&t.server))
            })
            .filter(|t| !WITHHELD_FROM_MODEL.contains(&t.qualified().as_str()))
            .map(|t| ToolDef {
                name: t.qualified(),
                description: t.description.clone(),
                parameters: t.schema.clone(),
            })
            .collect()
    }

    fn is_core(&self, qualified: &str) -> bool {
        qualified
            .split_once("__")
            .is_some_and(|(server, _)| self.core_servers.iter().any(|c| c == server))
    }

    /// `remember` and `recall` (§ src/facts.rs). They live here, not in
    /// NativeTools, because both need the graph server and `remember` needs
    /// the seq of the log line it is being recorded on.
    async fn facts_call(
        &mut self,
        tool: &str,
        args: &serde_json::Value,
        seq: u64,
    ) -> Result<serde_json::Value> {
        let session = self.log.session_id().to_string();
        if tool == crate::facts::REMEMBER {
            let now = chrono::Utc::now().to_rfc3339();
            let fact = crate::facts::parse(args, &session, seq, &now)?;
            return Ok(crate::facts::remember_live(&self.registry, &fact).await);
        }

        let about = args
            .get("about")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("recall needs `about`"))?
            .to_string();
        let history = args.get("include_history").and_then(|v| v.as_bool()).unwrap_or(false);

        // Facts first, matched tolerantly: "Zorblat" finds "Zorblat
        // Testperson". An exact-name graph lookup alone missed that on the
        // first live test and cost a second round.
        let mut names: Vec<String> = vec![about.clone()];
        let facts: Vec<serde_json::Value> = match self
            .registry
            .call("kuzu_graph", "facts", serde_json::json!({"include_history": history}))
            .await
        {
            Ok(v) => v
                .get("facts")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
                .filter(|f| {
                    ["subject", "object"].iter().any(|k| {
                        let n = f.get(*k).and_then(|x| x.as_str()).unwrap_or("");
                        let hit = crate::facts::mentions(&about, n) || crate::facts::mentions(n, &about);
                        if hit && !names.iter().any(|m| m.eq_ignore_ascii_case(n)) {
                            names.push(n.to_string());
                        }
                        hit
                    })
                })
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        };

        // Structural links (tools, artifacts, topics) for the names found.
        let mut links: Vec<serde_json::Value> = Vec::new();
        for name in names.iter().take(4) {
            if let Ok(v) = self
                .registry
                .call(
                    "kuzu_graph",
                    "query_graph",
                    serde_json::json!({"entity": name, "include_history": history, "limit": 20}),
                )
                .await
            {
                for e in v.get("edges").and_then(|x| x.as_array()).into_iter().flatten() {
                    // Facts are already listed above; keep only the other links.
                    if e.get("valid_from").is_none() {
                        let mut e = e.clone();
                        e["from"] = serde_json::json!(name);
                        links.push(e);
                    }
                }
            }
        }

        let convos: Vec<serde_json::Value> = self
            .native
            .search_scoped(&about, "all", 5)
            .unwrap_or_default()
            .iter()
            .map(|h| {
                serde_json::json!({
                    "session": h.chunk.session_id,
                    "this_session": h.chunk.session_id == session,
                    "text": h.chunk.text.chars().take(700).collect::<String>(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "about": about,
            "facts": facts,
            "links": links,
            "conversations": convos,
            "note": if history { "includes facts that are no longer true (they carry valid_to)" }
                    else { "current facts only; pass include_history for past ones" },
        }))
    }

    /// Memory that works without being asked for.
    ///
    /// Models reliably under-use a search tool: they answer from what is in
    /// front of them. So before each message, the harness itself looks up
    /// (a) remembered facts about anything the message names, and (b) past
    /// conversations that match it strongly, and places the few that clear
    /// the bar in front of the model for this turn only.
    ///
    /// The bar matters more than the lookup. §31 measured this index: a real
    /// match tops out around 0.5 cosine and gibberish still scores 0.3, so
    /// anything under 0.42 is left out rather than padding every prompt with
    /// noise. Three past turns at most, clipped - a few hundred tokens, and
    /// usually nothing at all. `HARNESS_AUTO_RECALL=off` disables it.
    /// Composer picks for the next turn (see `Picks`).
    pub fn set_picks(&mut self, picks: Picks) {
        self.picks = picks;
    }

    /// Default work folder for file and command tools (a sub-agent's own).
    pub fn set_home(&mut self, root: &str, sub: &str) {
        self.native.set_home(root, sub);
    }

    /// What this agent has spent so far.
    pub fn spent(&self) -> crate::llm::Usage {
        self.spent
    }

    /// Answer with `model` first, on the same endpoint as the first provider,
    /// with the configured chain behind it as fallback. Logged, because which
    /// model answered is part of the record (§51).
    pub fn prefer_model(&mut self, model: &str) {
        if model.trim().is_empty() || model == self.model {
            return;
        }
        self.chain.prefer_model(model);
        let _ = self.log.append(EventKind::System {
            note: format!("model for this agent: {model} (configured chain behind it as fallback)"),
        });
        self.model = model.to_string();
    }

    /// Apply this turn's picks: allow and load the picked servers and tools,
    /// fetch the picked skills, and say plainly that Adithya chose them.
    ///
    /// A picked server is ADDED to what this agent may reach. It is his
    /// explicit choice, which is exactly what scoping is meant to defer to. On
    /// the main chat the workspace gate is re-applied next turn, so there it
    /// lasts one message; on a sub-agent it stays, since a sub-agent has no
    /// workspace gate and he asked for it in that agent's own window.
    fn apply_picks(&mut self) -> Option<(String, String)> {
        let p = std::mem::take(&mut self.picks);
        if p.is_empty() {
            return None;
        }
        let mut servers = p.servers.clone();
        for t in &p.tools {
            if let Some((s, _)) = t.split_once("__") {
                if !servers.iter().any(|x| x == s) {
                    servers.push(s.to_string());
                }
            }
        }
        if let Some(allow) = &self.allowed_servers {
            let mut a = allow.clone();
            for s in &servers {
                if !a.contains(s) {
                    a.push(s.clone());
                }
            }
            if a.len() != allow.len() {
                let _ = self.log.append(EventKind::System {
                    note: format!("picked: now allowed to use {}", servers.join(", ")),
                });
                self.allowed_servers = Some(a);
            }
        }

        let pool: Vec<String> = self.mcp_pool().into_iter().map(|d| d.name).collect();
        for name in &pool {
            if p.servers.iter().any(|s| name.starts_with(&format!("{s}__")))
                || p.tools.contains(name)
            {
                self.loaded.insert(name.clone());
            }
        }
        let mut skills = Vec::new();
        for want in &p.skills {
            if let Ok(Some(s)) = self.native.skills().get(want) {
                for t in &s.tools {
                    if pool.contains(t) {
                        self.loaded.insert(t.clone());
                    }
                }
                skills.push(s);
            }
        }
        self.rebuild_tools();

        let mut msg = String::from(
            "[picked by Adithya] He chose these in the composer for this message - treat them as \
             part of his instruction.",
        );
        if !p.servers.is_empty() {
            msg.push_str(&format!(
                "\n- Use these tool servers (their tools are loaded): {}",
                p.servers.join(", ")
            ));
        }
        if !p.tools.is_empty() {
            msg.push_str(&format!("\n- Use these tools (loaded): {}", p.tools.join(", ")));
        }
        if let Some(b) = &p.browser {
            msg.push_str(&format!(
                "\n- Do any browsing in the `{b}` browser: pass browser: \"{b}\" to the snarevec \
                 browser_* tools, and do not touch other browsers."
            ));
        }
        for s in &skills {
            let body: String = s.body.chars().take(4000).collect();
            msg.push_str(&format!(
                "\n\n### Skill he picked: {}\n{}\n{}",
                s.name,
                if s.description.is_empty() { String::new() } else { format!("> {}\n", s.description) },
                body
            ));
        }
        let missing: Vec<&String> =
            p.skills.iter().filter(|w| !skills.iter().any(|s| &s.slug == *w || &s.name == *w)).collect();
        if !missing.is_empty() {
            msg.push_str(&format!(
                "\n- (Skill(s) not found, tell him: {})",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        let mut parts = Vec::new();
        if !p.skills.is_empty() { parts.push(format!("skill {}", p.skills.join(", "))); }
        if !p.servers.is_empty() { parts.push(format!("server {}", p.servers.join(", "))); }
        if !p.tools.is_empty() { parts.push(format!("tool {}", p.tools.join(", "))); }
        if let Some(b) = &p.browser { parts.push(format!("browser {b}")); }
        Some((msg, format!("picked: {}", parts.join("; "))))
    }

    async fn auto_recall(&mut self, input: &str) -> Option<(String, String)> {
        if matches!(std::env::var("HARNESS_AUTO_RECALL").as_deref(), Ok("off") | Ok("0")) {
            return None;
        }
        let text = input.trim();
        if text.len() < 12 || text.starts_with('/') {
            return None;
        }
        let lower = text.to_lowercase();
        let session = self.log.session_id().to_string();

        // (a) facts whose subject or object the message names.
        let mut fact_lines: Vec<String> = Vec::new();
        let facts = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.registry.call("kuzu_graph", "facts", serde_json::json!({"limit": 500})),
        )
        .await;
        if let Ok(Ok(v)) = facts {
            for f in v.get("facts").and_then(|x| x.as_array()).into_iter().flatten() {
                let get = |k: &str| f.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let (sub, rel, obj) = (get("subject"), get("relation"), get("object"));
                if crate::facts::mentions(&lower, &sub) || crate::facts::mentions(&lower, &obj) {
                    let since = get("valid_from").chars().take(10).collect::<String>();
                    let note = get("note");
                    fact_lines.push(format!(
                        "- {sub} {rel} {obj} (since {since}){}",
                        if note.is_empty() { String::new() } else { format!(" - {note}") }
                    ));
                }
                if fact_lines.len() >= 8 {
                    break;
                }
            }
        }

        // (b) strongly matching past turns from OTHER conversations - this
        // one is already in context.
        let past: Vec<String> = self
            .native
            .search_scoped(text, "all", 8)
            .unwrap_or_default()
            .into_iter()
            .filter(|h| h.chunk.session_id != session && h.score >= 0.42)
            .take(3)
            .map(|h| {
                let day = h.chunk.session_id.get(..8).unwrap_or("");
                let body: String = h.chunk.text.chars().take(500).collect();
                format!("- [{day}] {}", body.replace('\n', " / "))
            })
            .collect();

        // (c) skills the message refers to - by server ("use uacc"), tool,
        // trigger word ("flipkart cart") or name. Every skill that uses a named
        // server comes up, as asked; three at most, because each is a page of
        // instructions. Their tools are loaded too, so the first round can act
        // instead of spending itself on find_tools.
        let skills: Vec<crate::skills::Skill> =
            self.native.skills().matching(text).into_iter().take(3).collect();
        if !skills.is_empty() {
            let pool: Vec<String> = self.mcp_pool().into_iter().map(|d| d.name).collect();
            let before = self.loaded.len();
            for s in &skills {
                for t in &s.tools {
                    if pool.contains(t) {
                        self.loaded.insert(t.clone());
                    }
                }
            }
            if self.loaded.len() != before {
                self.rebuild_tools();
            }
        }

        if fact_lines.is_empty() && past.is_empty() && skills.is_empty() {
            return None;
        }
        let mut msg = String::from(
            "[memory] Retrieved automatically for this message from long-term memory. Use it if \
             it is relevant, ignore it if not, and never mention that it was retrieved. For more, \
             call recall or search_memory.",
        );
        if !skills.is_empty() {
            msg.push_str(
                "\n\nSkills that apply to this message (saved procedures - follow them; their \
                 tools are already loaded):",
            );
            for s in &skills {
                let body: String = s.body.chars().take(3000).collect();
                msg.push_str(&format!(
                    "\n\n### Skill: {}\n{}\n{}",
                    s.name,
                    if s.description.is_empty() { String::new() } else { format!("> {}\n", s.description) },
                    body
                ));
            }
        }
        if !fact_lines.is_empty() {
            msg.push_str("\n\nKnown facts:\n");
            msg.push_str(&fact_lines.join("\n"));
        }
        if !past.is_empty() {
            msg.push_str("\n\nPast conversations:\n");
            msg.push_str(&past.join("\n"));
        }
        let note = format!(
            "auto-recall: {} fact(s), {} past turn(s){} attached to this message",
            fact_lines.len(),
            past.len(),
            if skills.is_empty() {
                String::new()
            } else {
                format!(
                    ", skill(s): {}",
                    skills.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>().join(", ")
                )
            }
        );
        Some((msg, note))
    }

    /// `find_tools`: load the schemas a query asks for into the live toolset.
    fn find_tools(&mut self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("find_tools requires a `query` string"))?;
        let max = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(6)
            .clamp(1, 20) as usize;

        let pool = self.mcp_pool();
        let candidates: Vec<&ToolDef> = pool
            .iter()
            .filter(|t| !self.is_core(&t.name) && !self.loaded.contains(&t.name))
            .collect();
        let picked = crate::toolsearch::pick(query, max, &candidates);
        // Asking for tools that are ALREADY loaded is not "nothing matched".
        // A skill can load its tools before the model thinks to (auto-recall),
        // and answering "nothing matched" then sent the model round in circles:
        // measured, five find_tools calls in a row for tools it already had.
        if picked.is_empty() {
            let have: Vec<&ToolDef> = pool
                .iter()
                .filter(|t| self.is_core(&t.name) || self.loaded.contains(&t.name))
                .collect();
            let already = crate::toolsearch::pick(query, max, &have);
            if !already.is_empty() {
                return Ok(serde_json::json!({
                    "loaded": [],
                    "already_loaded": already,
                    "note": "These are already loaded - call them directly now. Do not call \
                             find_tools for them again."
                }));
            }
        }
        if picked.is_empty() {
            return Ok(serde_json::json!({
                "loaded": [],
                "note": "Nothing matched. Try other words, or `select:` with names from the \
                         catalogue in this tool's description."
            }));
        }
        let loaded: Vec<serde_json::Value> = pool
            .iter()
            .filter(|t| picked.contains(&t.name))
            .map(|t| {
                let first = t.description.split(['\n', '.']).next().unwrap_or("").trim();
                serde_json::json!({ "name": t.name, "does": first })
            })
            .collect();
        // Skills that drive the servers just loaded - the know-how that goes
        // with the tools. Named, not inlined: run_skill fetches the steps.
        let servers: Vec<String> = picked
            .iter()
            .filter_map(|n| n.split_once("__").map(|(s, _)| s.to_string()))
            .collect();
        let skills: Vec<serde_json::Value> = self
            .native
            .skills()
            .for_servers(&servers)
            .iter()
            .map(|s| serde_json::json!({ "skill": s.slug, "about": s.description }))
            .collect();
        self.loaded.extend(picked);
        self.rebuild_tools();
        Ok(serde_json::json!({
            "loaded": loaded,
            "skills": skills,
            "note": if skills.is_empty() {
                "These are callable now and stay loaded for this conversation."
            } else {
                "These are callable now and stay loaded for this conversation. The skills listed \
                 are saved procedures for these tools - call run_skill for one before improvising."
            }
        }))
    }

    /// Make this turn searchable now, not at the next `reduce`.
    ///
    /// Before this, `search_memory` only knew what the reducer had last
    /// indexed - and §39 found it had not run in three weeks, so nothing from
    /// September was findable at all. Each finished turn is now embedded into
    /// the `session` scope as it lands. The reducer's full rebuild still
    /// happens and still owns `global`; search collapses the two copies of a
    /// turn into one, so nothing is counted twice.
    ///
    /// Best-effort by design: a failure here must never cost the reply that
    /// has already been given. `HARNESS_LIVE_INDEX=off` disables it.
    fn index_live(&mut self) {
        if matches!(std::env::var("HARNESS_LIVE_INDEX").as_deref(), Ok("off") | Ok("0")) {
            return;
        }
        let Ok(events) = EventLog::read(self.log.path()) else { return };
        let fresh: Vec<crate::memory::Chunk> = crate::reduce::chunk_session(&events)
            .into_iter()
            .filter(|c| !self.live_indexed.contains(&c.seq_start))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let texts: Vec<&str> = fresh.iter().map(|c| c.text.as_str()).collect();
        let Ok(vectors) = self.native.embed(texts) else { return };
        for (chunk, v) in fresh.iter().zip(vectors) {
            if self.native.store().insert_scoped(chunk, &v, "session").is_ok() {
                self.live_indexed.insert(chunk.seq_start);
            }
        }
    }

    /// The round budget ran out. Rather than ending the turn on an error with
    /// the work done and nothing said about it, ask once more WITHOUT tools,
    /// so the model has to report what it found and what is left undone.
    async fn finish_without_tools(&mut self, out: &mut Vec<TurnEvent>, sink: Option<&EventSink>) {
        let note = format!(
            "[harness] The tool budget for this turn ({MAX_TOOL_ROUNDS} rounds) is used up. No \
             more tools can be called in this turn. Answer now with what you have: what you \
             found, what you did, and plainly what is still unfinished and what you would do \
             next."
        );
        let _ = self.log.append(EventKind::System {
            note: format!("tool budget of {MAX_TOOL_ROUNDS} rounds reached - asked for a final answer"),
        });
        let mut msgs = self.history.clone();
        msgs.push(Message::system(note));
        match self.chain.complete(&msgs, &[]).await {
            Ok((c, _)) => {
                if let Some(u) = &c.usage {
                    self.spent.add(u);
                }
                let text = c.content.unwrap_or_default();
                if text.trim().is_empty() {
                    let message = empty_reply_message(c.finish_reason.as_deref(), c.reasoning_chars);
                    let _ = self.log.append(EventKind::Error {
                        context: "provider.empty_reply".into(),
                        message: message.clone(),
                    });
                    emit(out, sink, TurnEvent::Error { message });
                    return;
                }
                let _ = self.log.append(EventKind::AssistantMessage { text: text.clone() });
                self.history.push(Message::assistant(text.clone()));
                emit(out, sink, TurnEvent::Reply { text });
            }
            Err(e) => {
                let message = format!("stopped after {MAX_TOOL_ROUNDS} tool rounds ({e:#})");
                let _ = self.log.append(EventKind::Error {
                    context: "agent.turn".into(),
                    message: message.clone(),
                });
                emit(out, sink, TurnEvent::Error { message });
            }
        }
    }

    pub fn allowed_servers(&self) -> Option<Vec<String>> {
        self.allowed_servers.clone()
    }

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
        self.live_indexed.clear();

        let mut indexed = 0usize;
        if !chunks.is_empty() {
            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = self.native.embed(texts)?;
            let store = self.native.store();
            for (chunk, embedding) in chunks.iter().zip(embeddings) {
                store.insert_scoped(chunk, &embedding, "session")?;
                self.live_indexed.insert(chunk.seq_start);
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
    ///
    /// Wraps the turn in automatic recall: whatever memory says about this
    /// message is placed just before it, for THIS turn only, then taken out
    /// again - so it never accumulates in the prompt, and the next message
    /// gets its own fresh recall.
    pub async fn turn_with(&mut self, input: &str, sink: Option<&EventSink>) -> Vec<TurnEvent> {
        let picked = self.apply_picks();
        let recall = self.auto_recall(input).await;
        let recall = match (picked, recall) {
            (None, r) => r,
            (Some(p), None) => Some(p),
            (Some((pt, pn)), Some((rt, rn))) => Some((format!("{pt}\n\n{rt}"), format!("{pn}; {rn}"))),
        };
        let at = recall.as_ref().map(|(text, _)| {
            self.history.push(Message::system(text.clone()));
            self.history.len() - 1
        });
        let out = self.turn_inner(input, sink, recall.map(|(_, note)| note)).await;
        if let Some(i) = at {
            if self.history.get(i).is_some_and(|m| m.role == "system") {
                self.history.remove(i);
            }
        }
        out
    }

    async fn turn_inner(
        &mut self,
        input: &str,
        sink: Option<&EventSink>,
        recall_note: Option<String>,
    ) -> Vec<TurnEvent> {
        let mut out = Vec::new();

        if let Err(e) = self.log.append(EventKind::UserMessage { text: input.into() }) {
            emit(&mut out, sink, TurnEvent::Error { message: format!("{e:#}") });
            return out;
        }
        // Logged after the message it belongs to, so it lands in that turn's
        // memory chunk rather than trailing the previous one.
        if let Some(note) = recall_note {
            let _ = self.log.append(EventKind::System { note });
        }
        self.history.push(Message::user(input));
        self.sync_vision_tools();

        for round in 0..MAX_TOOL_ROUNDS {
            let completion = match self.chain.complete(&self.history, &self.tool_defs).await {
                // The chain reports which provider actually answered.
                Ok((c, model)) => {
                    if let Some(u) = &c.usage {
                        self.spent.add(u);
                    }
                    if model != self.model {
                        /* WRITE IT DOWN. Updating `self.model` alone only
                           changed live memory - `session_start` still named the
                           model the session opened with, so the Log panel went
                           on saying `:free` while a paid fallback answered
                           every turn. Reported, and fair: the log is the source
                           of truth (§4a) and it was not recording the one thing
                           that had changed. An appended event keeps that rule
                           intact - nothing is overwritten, the switch is just
                           part of the trace, and `reduce` picks it up like any
                           other event. */
                        let _ = self.log.append(EventKind::System {
                            note: format!(
                                "provider fell back: {} -> {} (this turn answered by {model})",
                                self.model, model, model = model
                            ),
                        });
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
                // An empty "successful" reply is a failure and must say so.
                if text.trim().is_empty() {
                    let message = empty_reply_message(
                        completion.finish_reason.as_deref(),
                        completion.reasoning_chars,
                    );
                    let _ = self.log.append(EventKind::Error {
                        context: "provider.empty_reply".into(),
                        message: message.clone(),
                    });
                    emit(&mut out, sink, TurnEvent::Error { message });
                    return out;
                }
                let _ = self.log.append(EventKind::AssistantMessage { text: text.clone() });
                self.history.push(Message::assistant(text.clone()));
                emit(&mut out, sink, TurnEvent::Reply { text });
                self.index_live();
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
                // Unparseable arguments used to become `{}` silently, so a call
                // cut off mid-JSON (finish_reason: length) ran with no
                // arguments. Now the model is told, and nothing runs.
                let raw = tc.function.arguments.trim();
                let args: serde_json::Value = if raw.is_empty() {
                    serde_json::json!({})
                } else {
                    match serde_json::from_str(raw) {
                        Ok(v) => v,
                        Err(e) => {
                            let cut = completion.finish_reason.as_deref() == Some("length");
                            let message = format!(
                                "arguments for `{}` were not valid JSON ({e}){} - the call was \
                                 not run. Send it again with complete arguments.",
                                tc.function.name,
                                if cut { ", cut off because the reply budget ran out" } else { "" }
                            );
                            let _ = self.log.append(EventKind::Error {
                                context: "tool.args".into(),
                                message: message.clone(),
                            });
                            self.history.push(Message::tool_result(&tc.id, message.clone()));
                            emit(&mut out, sink, TurnEvent::Error { message });
                            continue;
                        }
                    }
                };

                let (server, tool) = if tc.function.name == "analyze_screen_vlm"
                    || tc.function.name == crate::toolsearch::TOOL_NAME
                    || crate::facts::handles(&tc.function.name)
                    || crate::gui::handles(&tc.function.name)
                    // Sub-agent tools were offered to the model but never
                    // routed: they fell through to the MCP lookup, which
                    // answered "unknown tool `spawn_agent`" - so the panel
                    // could spawn agents and bluee itself never could (§61).
                    || crate::subagents::SubAgents::is_tool(&tc.function.name)
                    || crate::subagents::ChildLink::handles(&tc.function.name)
                    || crate::webtools::handles(&tc.function.name)
                {
                    // Both need the MCP registry, which NativeTools does not
                    // hold - so they are dispatched here rather than there.
                    ("harness".to_string(), tc.function.name.clone())
                } else if NativeTools::handles(&tc.function.name) {
                    ("harness".to_string(), tc.function.name.clone())
                } else {
                    match self.registry.resolve(&tc.function.name) {
                        Some(pair) => {
                            // Called by name without loading it first - the
                            // catalogue told it the name. Only allowed if the
                            // workspace permits it; loaded now so later rounds
                            // carry the real schema.
                            if !self.tool_defs.iter().any(|d| d.name == tc.function.name) {
                                if !self.mcp_pool().iter().any(|d| d.name == tc.function.name) {
                                    let message = format!(
                                        "`{}` is not available in this workspace",
                                        tc.function.name
                                    );
                                    self.history
                                        .push(Message::tool_result(&tc.id, message.clone()));
                                    emit(&mut out, sink, TurnEvent::Error { message });
                                    continue;
                                }
                                self.loaded.insert(tc.function.name.clone());
                                self.rebuild_tools();
                            }
                            pair
                        }
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
                // The seq is a fact's provenance: `remember` points back at it.
                let call_seq = self
                    .log
                    .append(EventKind::ToolCall {
                        call_id: tc.id.clone(),
                        server: server.clone(),
                        tool: tool.clone(),
                        args: args.clone(),
                    })
                    .unwrap_or(0);

                // Adithya's own guards run first, outside the model (§ hooks).
                let session = self.log.session_id().to_string();
                let blocked = crate::hooks::pre_tool(&server, &tool, &args, &session).await.err();

                let outcome = if let Some(reason) = blocked {
                    Err(anyhow::anyhow!(reason))
                } else if tool == crate::toolsearch::TOOL_NAME {
                    self.find_tools(&args)
                } else if crate::facts::handles(&tool) {
                    self.facts_call(&tool, &args, call_seq).await
                } else if tool == "analyze_screen_vlm" {
                    let q = args.get("question").and_then(|v| v.as_str()).unwrap_or("What is on screen?");
                    self.analyze_screen(q).await
                } else if crate::gui::handles(&tool) {
                    crate::gui::call(&*self.registry, &tool, &args).await
                } else if crate::subagents::SubAgents::is_tool(&tool) {
                    // Checked before the `harness` branch: these are native
                    // in the same sense, but they live on the registry rather
                    // than on NativeTools, which has no way to reach one.
                    match &self.subagents {
                        Some(subs) => subs.call(&tool, &args).await,
                        None => Err(anyhow::anyhow!(
                            "sub-agents are not available on this surface"
                        )),
                    }
                } else if crate::webtools::handles(&tool) {
                    match self.child.as_ref().and_then(|l| l.browser.clone().map(|b| (b, l.data_dir.clone()))) {
                        Some((kind, dir)) => crate::webtools::call(&dir, &kind, &tool, &args).await,
                        None => Err(anyhow::anyhow!("only a sub-agent that owns a browser can use `{tool}`")),
                    }
                } else if crate::subagents::ChildLink::handles(&tool) {
                    match &self.child {
                        Some(link) => link.clone().call(&tool, &args).await,
                        None => Err(anyhow::anyhow!("only a sub-agent can use `{tool}`")),
                    }
                } else if server == "harness" {
                    self.native.call(&tool, &args)
                } else {
                    // A sub-agent that owns a browser drives THAT browser and
                    // no other: its snarevec browser_* calls are pinned, and
                    // naming another browser is refused rather than obeyed.
                    let pin = self.child.as_ref().and_then(|l| l.browser.clone());
                    match pin.filter(|_| server == "snarevec" && tool.starts_with("browser_")) {
                        Some(mine) => {
                            let asked = args.get("browser").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                            if !asked.is_empty() && !asked.starts_with(&mine) {
                                Err(anyhow::anyhow!(
                                    "your browser is `{mine}`; `{asked}` belongs to someone else. Use browser: \"{mine}\"."
                                ))
                            } else {
                                let mut a = args.clone();
                                if asked.is_empty() {
                                    a["browser"] = serde_json::json!(mine);
                                }
                                self.registry.call(&server, &tool, a).await
                            }
                        }
                        None => self.registry.call(&server, &tool, args.clone()).await,
                    }
                };

                // Post-hooks see the outcome and may add to it - this is how a
                // `cargo check` after a write gets its errors back to the model.
                let outcome = {
                    let (ok, val) = match &outcome {
                        Ok(v) => (true, v.clone()),
                        Err(e) => (false, serde_json::json!({ "error": format!("{e:#}") })),
                    };
                    match crate::hooks::post_tool(&server, &tool, &args, ok, &val, &session).await {
                        Some(note) => match outcome {
                            Ok(v) => Ok(serde_json::json!({ "result": v, "hook_output": note })),
                            Err(e) => Err(anyhow::anyhow!("{e:#}\n\nhook_output:\n{note}")),
                        },
                        None => outcome,
                    }
                };

                match outcome {
                    Ok(value) => {
                        let _ = self.log.append(EventKind::ToolResult {
                            call_id: tc.id.clone(),
                            ok: true,
                            result: value.clone(),
                        });
                        self.history
                            .push(Message::tool_result(&tc.id, clip_for_prompt(value.to_string())));
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
                self.finish_without_tools(&mut out, sink).await;
                self.index_live();
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
