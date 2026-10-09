//! Dashboard backend (§4e/§4f) - the surface Adithya actually works in.
//!
//! Not just a memory viewer: this serves the chat, the shared terminal, and
//! the memory panels from the same binary. Built browser-first because Tauri's
//! frontend *is* a web app (§4g), so none of it is thrown away by the wrap.

use anyhow::Result;
use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::Agent;
use crate::artifacts::ArtifactStore;
use crate::config::Config;
use crate::eventlog::EventLog;
use crate::mcp::McpRegistry;
use crate::memory::VectorStore;
use crate::providers;
use crate::pty::PtyManager;
use crate::vision::{VisionMode, VisionState};

struct AppState {
    cfg: Config,
    registry: Arc<McpRegistry>,
    embedder: Mutex<Option<TextEmbedding>>,
    /// Created on first message - the dashboard should open even with no
    /// credentials configured, so you can still read your own history.
    agent: Mutex<Option<Agent>>,
    pty: PtyManager,
    vision: Arc<VisionState>,
    /// Which granted folder is currently open, so the agent can be scoped to
    /// the servers that workspace asked for.
    workspace: Mutex<String>,
    /// Where we are actually listening, so a pop-out window can be pointed
    /// back at this same server.
    port: u16,
    /// A browser the harness starts and drives itself (§ src/browser.rs).
    /// Replaces the SnareVec MCP path, which depended on a daemon that idles
    /// out and on a config flag only Adithya may set - so in practice the
    /// panel never worked.
    browser: crate::browser::Browser,
    /// Local speech in and out (§ src/voice.rs). Off until switched on, so the
    /// models cost nothing - no process, no VRAM - until someone wants them.
    voice: crate::voice::Voice,
    /// Sub-agents the main conversation can start (§ src/subagents.rs).
    /// Only the dashboard's agent gets these: the CLI and the loops do not,
    /// because a scheduled job that can spawn agents unattended is a cost
    /// hazard nobody asked for.
    subagents: Arc<crate::subagents::SubAgents>,
}

type Shared = Arc<AppState>;

fn fail(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

/// Bind a fresh port and serve (CLI path).
pub async fn serve(cfg: Config, port: u16) -> Result<()> {
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}"))?;
    listener.set_nonblocking(true)?;
    println!("dashboard: http://127.0.0.1:{port}");
    println!("(ctrl-c to stop)");
    serve_on(cfg, listener).await
}

/// Serve on a listener the caller already bound. The desktop shell binds port
/// 0 first so it knows the real port before creating the window.
pub async fn serve_on(cfg: Config, std_listener: std::net::TcpListener) -> Result<()> {
    // Every server, not just the graph: the dashboard has a chat box now, and
    // the model there should reach the same 106 tools the CLI does.
    let specs = crate::mcp::load_server_specs(&cfg.mcp_config)?;
    let (registry, failures) = McpRegistry::connect(&specs).await;
    for f in &failures {
        eprintln!("[warn] server failed to start - {f}");
    }
    println!(
        "connected {} server(s), {} tool(s)",
        registry.servers().len(),
        registry.tools().len()
    );

    let port = std_listener.local_addr()?.port();
    let cfg_vision_dir = cfg.data_dir.clone();
    // Cloned before the struct literal moves the originals in.
    let cfg_for_subs = cfg.clone();
    let registry_for_subs = Arc::new(registry);
    let vision_for_subs = Arc::new(VisionState::load(&cfg_vision_dir));
    let state: Shared = Arc::new(AppState {
        cfg,
        registry: registry_for_subs.clone(),
        embedder: Mutex::new(None),
        agent: Mutex::new(None),
        pty: PtyManager::default(),
        vision: vision_for_subs.clone(),
        workspace: Mutex::new("playground".into()),
        port,
        browser: crate::browser::Browser::new(&cfg_vision_dir),
        voice: crate::voice::Voice::new(&cfg_vision_dir),
        subagents: Arc::new(crate::subagents::SubAgents::new(
            &cfg_for_subs, registry_for_subs, vision_for_subs)),
    });

    // The loop scheduler (§ src/loops.rs) rides along with the server rather
    // than being its own process: it needs the same MCP registry and the same
    // config, and a second process would mean a second set of connections to
    // the same tool servers. It spends money unattended, so everything that
    // bounds it lives in the loop files themselves.
    {
        let cfg = state.cfg.clone();
        let registry = state.registry.clone();
        let vision = state.vision.clone();
        tokio::spawn(async move {
            crate::loops::run_scheduler(cfg, registry, vision).await;
        });
    }

    /* Idle sweeper: sleep after `sleep_after_mins`, end after `end_after_mins`
       (§ src/subagents.rs). Busy agents are never touched. 30s is fine
       granularity for timeouts measured in minutes. */
    {
        let subs = state.subagents.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let (slept, ended) = subs.sweep();
                for id in slept {
                    println!("sub-agent {id}: idle, sleeping");
                }
                for id in ended {
                    println!("sub-agent {id}: idle past its end timer, ended");
                }
            }
        });
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/vendor/xterm.js", get(|| asset("application/javascript", include_str!("../dash/vendor/xterm.js"))))
        .route("/vendor/xterm.css", get(|| asset("text/css", include_str!("../dash/vendor/xterm.css"))))
        .route("/vendor/addon-fit.js", get(|| asset("application/javascript", include_str!("../dash/vendor/addon-fit.js"))))
        .route("/api/stats", get(stats))
        .route("/api/sessions", get(sessions))
        .route("/api/events", get(events))
        .route("/api/search", get(search))
        .route("/api/memory", get(memory_browse))
        .route("/api/facts", get(facts_list))
        .route("/api/graph", get(graph))
        .route("/api/agents", get(agents_list).post(agents_spawn))
        .route("/api/agents/ask", post(agents_ask))
        .route("/api/agents/stop", post(agents_stop))
        .route("/api/agents/despawn", post(agents_despawn))
        .route("/api/agents/resume", post(agents_resume))
        .route("/api/agents/say", post(agents_say))
        .route("/api/agents/answer", post(agents_answer))
        .route("/api/agents/timers", post(agents_timers))
        .route("/api/agents/sleep", post(agents_sleep))
        .route("/api/agents/inbox", get(agents_inbox))
        .route("/api/agents/caps", post(agents_caps))
        .route("/api/agents/templates", get(agents_templates))
        .route("/api/browsers", get(browsers_list))
        .route("/ws/agent", get(agent_ws))
        .route("/ws/agents", get(agents_feed_ws))
        .route("/api/tools", get(tools))
        .route("/api/session", get(current_session))
        .route("/api/tasks", get(tasks))
        .route("/api/session/open", post(open_session))
        .route("/api/session/delete", post(delete_session))
        .route("/api/session/title", post(set_session_title))
        .route("/api/models", post(models))
        .route("/api/artifacts", get(list_artifacts))
        .route("/api/files", get(list_files))
        .route("/api/file", get(read_file))
        .route("/api/file/delete", post(delete_file))
        .route("/api/window", post(open_window))
        .route("/api/roots", get(list_roots).post(add_root))
        .route("/api/roots/remove", post(remove_root))
        .route("/api/roots/rename", post(rename_root))
        .route("/api/roots/servers", post(set_root_servers))
        .route("/api/workspace", post(set_workspace))
        .route("/api/browser", get(browser_state).post(browser_act))
        .route("/artifacts/{id}/", get(artifact_root))
        .route("/artifacts/{id}/{*path}", get(artifact_file))
        .route("/api/chat", post(chat))
        .route("/ws/chat", get(chat_ws))
        .route("/api/persona", get(get_persona).post(save_persona))
        .route("/api/providers", get(get_providers).post(save_providers))
        .route("/api/mcp", get(get_mcp).post(save_mcp))
        .route("/api/mcp/reconnect", post(reconnect_mcp))
        .route("/api/vision", get(get_vision).post(set_vision))
        .route("/api/voice", get(get_voice).post(set_voice))
        .route("/api/voice/health", get(voice_health))
        .route("/api/voice/voices", get(voice_voices))
        .route("/api/voice/unload", post(voice_unload))
        .route("/api/stt", post(stt))
        .route("/api/tts", post(tts))
        .route("/api/upload", post(upload))
        .route("/api/skills", get(get_skills).post(save_skill))
        .route("/api/skills/delete", post(delete_skill))
        .route("/ws/terminal", get(terminal_ws))
        .with_state(state.clone());

    // Passive screen capture (§6). Runs only while the mode allows it, and
    // writes into whichever session is live - screen context with no
    // conversation to attach to is not worth keeping.
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let wait = st.vision.interval().max(10);
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                if !st.vision.mode().captures() {
                    continue;
                }
                let Ok(v) = st.registry.call("uacc", "get_screen_info", json!({})).await else {
                    continue;
                };
                let Some(text) = crate::vision::extract_screen_text(&v) else {
                    continue;
                };
                // Unchanged screens are skipped: §8 flags passive capture
                // filling memory with noise as a real risk to retrieval.
                if !st.vision.is_new(&text) {
                    continue;
                }
                if let Some(agent) = st.agent.lock().await.as_mut() {
                    agent.log_screen(st.vision.mode().as_str(), &text);
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::from_std(std_listener)?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn asset(mime: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, mime)], body).into_response()
}

async fn index() -> impl IntoResponse {
    Html(include_str!("../dash/index.html"))
}

// ---------------------------------------------------------------- chat

#[derive(Deserialize)]
struct ChatBody {
    message: String,
}

async fn chat(
    State(s): State<Shared>,
    Json(body): Json<ChatBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut guard = s.agent.lock().await;
    if guard.is_none() {
        *guard = Some(
            Agent::new(&s.cfg, s.registry.clone(), s.vision.clone())
                .await
                .map_err(fail)?,
        );
    }
    let agent = guard.as_mut().unwrap();
    let events = agent.turn(&body.message).await;

    Ok(Json(json!({
        "session": agent.session_id(),
        "model": agent.model,
        "events": events,
    })))
}

/// Stream a turn: each tool call and result reaches the UI the moment it
/// happens, instead of the whole turn landing at once when it finishes.
async fn chat_ws(ws: WebSocketUpgrade, State(s): State<Shared>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| chat_stream(socket, s))
}

async fn chat_stream(mut socket: WebSocket, s: Shared) {
    use futures_util::{SinkExt, StreamExt};

    while let Some(Ok(msg)) = socket.next().await {
        let WsMessage::Text(raw) = msg else {
            if matches!(msg, WsMessage::Close(_)) {
                break;
            }
            continue;
        };
        let Ok(frame) = serde_json::from_str::<Value>(&raw) else { continue };
        /* `wake`: the UI starting a turn by itself because a sub-agent result
           bluee was waiting on has arrived. The message says plainly that
           Adithya did not type it - the log must not put words in his mouth. */
        let wake = frame.get("wake").and_then(|v| v.as_bool()).unwrap_or(false);
        let picks: crate::agent::Picks = frame
            .get("picks")
            .and_then(|p| serde_json::from_value(p.clone()).ok())
            .unwrap_or_default();
        let text = if wake {
            if s.subagents.hub().pending() == 0 {
                let done = json!({ "type": "done" });
                let _ = socket.send(WsMessage::Text(done.to_string().into())).await;
                continue;
            }
            "(Automatic notice - not typed by Adithya.) Results from sub-agents you were waiting \
             on have arrived; they are in the note above. Carry on with what you were doing and \
             tell Adithya what came back."
                .to_string()
        } else {
            match frame.get("message").and_then(|m| m.as_str()) {
                Some(t) => t.to_string(),
                None => continue,
            }
        };

        // Slash commands are handled by the harness, not sent to the model:
        // they are about the session itself, and paying for a round trip to be
        // told what /help says would be silly.
        if text.trim_start().starts_with('/') {
            let reply = run_command(&s, text.trim()).await;
            let ev = json!({ "type": "reply", "text": reply });
            let _ = socket.send(WsMessage::Text(ev.to_string().into())).await;
            let session = s.agent.lock().await.as_ref().map(|a| a.session_id().to_string());
            let done = json!({ "type": "done", "session": session });
            if socket.send(WsMessage::Text(done.to_string().into())).await.is_err() {
                break;
            }
            continue;
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // The agent lock is held for the whole turn; the loop below forwards
        // events out meanwhile, which is the entire point of streaming.
        let state = s.clone();
        let turn = tokio::spawn(async move {
            let mut guard = state.agent.lock().await;
            if guard.is_none() {
                match Agent::new(&state.cfg, state.registry.clone(), state.vision.clone()).await {
                    Ok(mut a) => {
                        // Only this agent gets sub-agent tools.
                        a.set_subagents(state.subagents.clone());
                        *guard = Some(a);
                    }
                    Err(e) => {
                        let _ = tx.send(crate::agent::TurnEvent::Error {
                            message: format!("{e:#}"),
                        });
                        return None;
                    }
                }
            }
            let agent = guard.as_mut().unwrap();
            // Re-apply the workspace gate each turn, the same way the vision
            // gate is synced: whichever folder is open decides the toolset.
            let ws = state.workspace.lock().await.clone();
            if let Ok(root) = crate::roots::get(&state.cfg, &ws) {
                agent.set_allowed_servers(root.servers.clone());
            }
            // Whatever sub-agents reported since the last turn goes in first,
            // and stays in the conversation (§ src/subagents.rs, inbox).
            if let Some(note) = state.subagents.hub().drain() {
                agent.add_note(&note);
            }
            agent.set_picks(picks);
            agent.turn_with(&text, Some(&tx)).await;
            Some((
                agent.session_id().to_string(),
                agent.model.clone(),
                agent.title().unwrap_or_default(),
            ))
        });

        while let Some(ev) = rx.recv().await {
            let frame = serde_json::to_string(&ev).unwrap_or_default();
            if socket.send(WsMessage::Text(frame.into())).await.is_err() {
                break;
            }
        }

        let done = turn.await.ok().flatten();
        let end = match done {
            Some((session, model, title)) => {
                json!({ "type": "done", "session": session, "model": model, "title": title })
            }
            None => json!({ "type": "done" }),
        };
        if socket.send(WsMessage::Text(end.to_string().into())).await.is_err() {
            break;
        }
    }
}

// --------------------------------------------------------- slash commands

const HELP: &str = "**Commands**

- `/compact [keep]` — fold this session into searchable memory and shrink the \
prompt. Nothing is summarised or lost; `keep` is how many recent messages stay \
verbatim (default 6).
- `/context` — how full the prompt is right now.
- `/sessions` — list past conversations.
- `/mcp` — connected tool servers.
- `/reduce` — rebuild general memory from the event log.
- `/skills` — saved skills (not built yet).
- `/help` — this.

Anything not starting with `/` goes to the model as usual.";

async fn run_command(s: &Shared, line: &str) -> String {
    let mut parts = line.trim_start_matches('/').split_whitespace();
    let cmd = parts.next().unwrap_or("").to_lowercase();
    let arg = parts.next().unwrap_or("");

    match cmd.as_str() {
        "help" | "?" => HELP.to_string(),

        "context" => match s.agent.lock().await.as_ref() {
            Some(a) => {
                let (tokens, msgs) = a.context_estimate();
                format!(
                    "~**{tokens}** tokens across {msgs} message(s), including tool schemas.\n\n\
                     This is an estimate (~4 chars per token), not exact tokenisation. \
                     Run `/compact` when it starts feeling heavy."
                )
            }
            None => "No active session yet — send a message first.".into(),
        },

        "compact" => {
            let keep = arg.parse::<usize>().unwrap_or(6);
            let mut guard = s.agent.lock().await;
            let Some(agent) = guard.as_mut() else {
                return "No active session to compact.".into();
            };
            match agent.compact(keep) {
                Ok(r) => format!(
                    "**Compacted.**\n\n\
                     - {} chunk(s) of this session indexed into session memory{}\n\
                     - {} message(s) moved out of the prompt, {} kept verbatim\n\
                     - prompt ~{} → ~{} tokens\n\n\
                     Nothing was summarised and nothing was lost — the full record is still in \
                     the event log, and this session's chunks are now searchable and tagged \
                     `session` so they're distinguishable from general memory.",
                    r.indexed,
                    if r.replaced > 0 {
                        format!(" (replacing {} from an earlier compact)", r.replaced)
                    } else {
                        String::new()
                    },
                    r.dropped,
                    r.after_msgs.saturating_sub(1),
                    r.before_tokens,
                    r.after_tokens,
                ),
                Err(e) => format!("Compact failed: {e:#}"),
            }
        }

        "sessions" => {
            let paths = EventLog::list_sessions(s.cfg.events_dir()).unwrap_or_default();
            if paths.is_empty() {
                return "No sessions yet.".into();
            }
            let mut out = format!("**{} session(s)**, newest first:\n\n", paths.len());
            for p in paths.iter().rev().take(12) {
                let id = p.file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                let events = EventLog::read(p).unwrap_or_default();
                let first = events
                    .iter()
                    .find_map(|e| match &e.kind {
                        crate::eventlog::EventKind::UserMessage { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "(no messages)".into());
                out.push_str(&format!(
                    "- `{id}` — {}\n",
                    first.chars().take(70).collect::<String>()
                ));
            }
            out.push_str("\nOpen or delete them from the Sessions page in the left rail.");
            out
        }

        "mcp" => {
            let mut by: std::collections::BTreeMap<String, usize> = Default::default();
            for t in s.registry.tools() {
                *by.entry(t.server.clone()).or_insert(0) += 1;
            }
            if by.is_empty() {
                return "No MCP servers connected.".into();
            }
            let mut out = format!("**{} server(s) connected**\n\n", by.len());
            for (srv, n) in by {
                out.push_str(&format!("- `{srv}` — {n} tools\n"));
            }
            out.push_str("\nDeclared in `mcps/servers.json`. A UI to add them is not built yet.");
            out
        }

        "reduce" => match crate::reduce::run(&s.cfg).await {
            Ok(st) => format!(
                "**Memory rebuilt** from {} session(s), {} event(s):\n\n\
                 - {} chunk(s) embedded\n- {} entities, {} relations\n\n\
                 Session memory from `/compact` was left untouched.",
                st.sessions, st.events, st.chunks, st.entities, st.relations
            ),
            Err(e) => format!("Reduce failed: {e:#}"),
        },

        "skills" => match crate::skills::SkillStore::open(&s.cfg.skills_dir)
            .and_then(|st| st.list(if arg.is_empty() { None } else { Some(arg) }))
        {
            Ok(list) if list.is_empty() => "No skills saved yet.\n\nAsk bluee to \
                *\"remember this as a skill\"* after it works something out, and it writes the \
                procedure to `skills/` as Markdown you can read and edit."
                .to_string(),
            Ok(list) => {
                let mut out = format!("**{} skill(s)**\n\n", list.len());
                for k in list {
                    out.push_str(&format!(
                        "- **{}** `[{}]` — {}\n",
                        k.name,
                        k.category,
                        if k.description.is_empty() {
                            "(no description)".to_string()
                        } else {
                            k.description
                        }
                    ));
                }
                out.push_str(
                    "\nSay *\"run the <name> skill\"* and bluee fetches the steps and follows \
                     them with its normal tools.",
                );
                out
            }
            Err(e) => format!("Could not read skills: {e:#}"),
        },

        "clear" => "Use **new chat** in the top bar — that starts a fresh session and \
                    leaves this one intact in Sessions."
            .into(),

        other => format!("Unknown command `/{other}`.\n\n{HELP}"),
    }
}

// ------------------------------------------------------------- tasks panel

/// What ran during the CURRENT session, with how long each took and why.
///
/// Duration needs no schema change: `tool_call` and `tool_result` are both
/// timestamped and paired by `call_id`, so it is just a subtraction.
///
/// "Reason" is not something the OpenAI tool-call format carries. The honest
/// source is whatever the assistant said immediately before calling, which the
/// agent now logs. When it said nothing, the reason is reported as absent
/// rather than invented.
async fn tasks(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let session = match s.agent.lock().await.as_ref() {
        Some(a) => a.session_id().to_string(),
        None => {
            return Ok(Json(json!({
                "session": Value::Null, "tasks": [],
                "note": "No active session yet - send a message."
            })))
        }
    };

    let path = s.cfg.events_dir().join(format!("{session}.jsonl"));
    let events = EventLog::read(&path).unwrap_or_default();

    use crate::eventlog::EventKind as K;
    let mut tasks: Vec<Value> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = Default::default();
    let mut pending_reason: Option<String> = None;

    for e in &events {
        match &e.kind {
            K::AssistantMessage { text } if !text.trim().is_empty() => {
                // Becomes the reason for every call in the batch that follows,
                // not just the first: the model explains once and then calls
                // several tools together, so consuming the reason on the first
                // call left the rest looking unexplained. It stays until the
                // assistant speaks again.
                pending_reason = Some(first_line(text));
            }
            K::ToolCall {
                call_id,
                server,
                tool,
                args,
            } => {
                index.insert(call_id.clone(), tasks.len());
                tasks.push(json!({
                    "call_id": call_id,
                    "seq": e.seq,
                    "at": e.ts.to_rfc3339(),
                    "started_ms": e.ts.timestamp_millis(),
                    "server": server,
                    "tool": tool,
                    "args": args,
                    "reason": pending_reason.clone(),
                    "state": "running",
                }));
            }
            K::ToolResult {
                call_id,
                ok,
                result,
            } => {
                if let Some(i) = index.get(call_id) {
                    let started = tasks[*i]["started_ms"].as_i64().unwrap_or(0);
                    let ms = (e.ts.timestamp_millis() - started).max(0);
                    tasks[*i]["ms"] = json!(ms);
                    tasks[*i]["ok"] = json!(ok);
                    tasks[*i]["state"] = json!(if *ok { "done" } else { "failed" });
                    tasks[*i]["result"] = result.clone();
                }
            }
            _ => {}
        }
    }

    let total_ms: i64 = tasks.iter().filter_map(|t| t["ms"].as_i64()).sum();
    let failed = tasks.iter().filter(|t| t["state"] == "failed").count();
    // Anything still without a result was in flight when the log was read -
    // that is exactly the "background task" case, not an error.
    let running = tasks.iter().filter(|t| t["state"] == "running").count();

    tasks.reverse(); // newest first
    Ok(Json(json!({
        "session": session,
        "count": tasks.len(),
        "total_ms": total_ms,
        "failed": failed,
        "running": running,
        "tasks": tasks,
    })))
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    line.chars().take(160).collect()
}

// ---------------------------------------------------------- session access

#[derive(Deserialize)]
struct SessionRef {
    session: String,
}

/// Reopen a past conversation and continue it in place.
async fn open_session(
    State(s): State<Shared>,
    Json(b): Json<SessionRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let path = s.cfg.events_dir().join(format!("{}.jsonl", safe_id(&b.session)));
    if !path.exists() {
        return Err(fail(format!("no such session: {}", b.session)));
    }

    let agent = Agent::resume(&s.cfg, s.registry.clone(), s.vision.clone(), &safe_id(&b.session))
        .await
        .map_err(fail)?;
    let (session, model, tools, title) = (
        agent.session_id().to_string(),
        agent.model.clone(),
        agent.tool_count(),
        agent.title(),
    );
    *s.agent.lock().await = Some(agent);

    Ok(Json(json!({
        "ok": true, "session": session, "model": model, "tools": tools, "title": title,
        "note": "Resumed. The conversation continues in the same log."
    })))
}

/// Delete one session. This removes source-of-truth data (§4a), so it is
/// explicit and one-at-a-time by design - there is deliberately no "delete all".
/// The derived stores still hold its chunks until the next `reduce`.
async fn delete_session(
    State(s): State<Shared>,
    Json(b): Json<SessionRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let id = safe_id(&b.session);
    let path = s.cfg.events_dir().join(format!("{id}.jsonl"));
    if !path.exists() {
        return Err(fail(format!("no such session: {id}")));
    }
    std::fs::remove_file(&path).map_err(fail)?;

    // If the live agent was writing to it, drop it so the UI does not keep
    // showing a session whose file no longer exists.
    let mut guard = s.agent.lock().await;
    if guard.as_ref().is_some_and(|a| a.session_id() == id) {
        *guard = None;
    }
    drop(guard);

    // The derived layers go with it, now rather than at the next full rebuild.
    // Once the log is gone those rows are evidence for nothing, and leaving
    // them would mean deleting a conversation still left it findable - which is
    // not what anyone means by delete.
    let chunks = VectorStore::open(s.cfg.data_dir.join("vectors.db"))
        .and_then(|v| v.forget_session(&id))
        .unwrap_or(0);

    let graph = s
        .registry
        .call("kuzu_graph", "drop_session", json!({ "session": id }))
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }));

    Ok(Json(json!({
        "ok": true, "deleted": id,
        "chunks_removed": chunks,
        "graph": graph,
        "note": "Its memory chunks and its graph edges went with it. Entities other                  sessions still refer to were kept - they are not this one's to delete."
    })))
}

/// Session ids are ours (timestamp + uuid), so anything else is refused rather
/// than allowed to address a path.
fn safe_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(80)
        .collect()
}

// ------------------------------------------------------ persona + providers

async fn get_persona(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut files = Vec::new();
    for name in providers::PERSONA_FILES {
        files.push(json!({
            "name": name,
            "body": providers::read_persona(&s.cfg.persona_dir, name).map_err(fail)?,
        }));
    }
    Ok(Json(json!({ "dir": s.cfg.persona_dir.display().to_string(), "files": files })))
}

#[derive(Deserialize)]
struct PersonaSave {
    name: String,
    body: String,
}

async fn save_persona(
    State(s): State<Shared>,
    Json(b): Json<PersonaSave>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    providers::write_persona(&s.cfg.persona_dir, &b.name, &b.body).map_err(fail)?;
    Ok(Json(json!({
        "ok": true,
        "note": "Saved, with a .bak of the previous version. Applies to the next new chat: the persona is assembled once per session and never mutated mid-conversation."
    })))
}

async fn get_providers(State(s): State<Shared>) -> impl IntoResponse {
    let file = providers::load(&s.cfg);
    Json(json!({
        "providers": file.providers.iter().enumerate().map(|(i, p)| json!({
            "position": i,
            "role": if i == 0 { "default".to_string() } else { format!("fallback {i}") },
            "name": p.name, "base_url": p.base_url, "model": p.model,
            "max_tokens": p.max_tokens, "enabled": p.enabled,
            "context_window": p.context_window,
            "temperature": p.temperature,
            "top_p": p.top_p,
            "timeout_secs": p.timeout_secs,
            "stream": p.stream,
            "retries": p.retries,
            "api_key": providers::mask(&p.api_key),
            "has_key": !p.api_key.is_empty(),
        })).collect::<Vec<_>>(),
        "note": "Order is the fallback order. The first entry is the default.",
        "fields": {
            "max_tokens": "Ceiling on the reply. NOT the context window - setting this to the model's full window makes the provider reserve that budget up front and refuse the request on a small balance.",
            "context_window": "How much the model holds at once. Drives the context meter only.",
            "temperature": "Blank means the model's own default.",
            "timeout_secs": "Give up and fall through to the next provider.",
            "stream": "Ask for the answer as a stream. aicredits.in cuts any non-streamed request off at ~30s wall clock and returns a bare 500 - streamed, the same work runs for minutes. Leave this on unless a provider streams badly.",
            "retries": "Extra attempts against this same provider before falling through to the next."
        }
    }))
}

async fn save_providers(
    State(s): State<Shared>,
    Json(mut incoming): Json<providers::ProvidersFile>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let existing = providers::load(&s.cfg);

    // The UI only ever sees masked keys, so a key coming back masked or blank
    // means "unchanged", never "wipe it". Without this, opening the page and
    // pressing Save would destroy every key you had.
    for p in &mut incoming.providers {
        let masked = p.api_key.is_empty() || p.api_key.contains('\u{2022}');
        if masked {
            p.api_key = existing
                .providers
                .iter()
                .find(|e| e.name == p.name)
                .map(|e| e.api_key.clone())
                .unwrap_or_default();
        }
    }

    providers::save(&s.cfg, &incoming).map_err(fail)?;
    Ok(Json(json!({
        "ok": true,
        "providers": incoming.providers.len(),
        "note": "Saved. Start a new chat to pick it up."
    })))
}

// ------------------------------------------------------------ vision (§6)

async fn get_vision(State(s): State<Shared>) -> impl IntoResponse {
    let (kept, skipped) = s.vision.stats();
    let mode = s.vision.mode();
    Json(json!({
        "mode": mode.as_str(),
        "interval_secs": s.vision.interval(),
        "vlm_tool_offered": mode.vlm_allowed(),
        "vision_model": s.cfg.vision_model,
        "captured": kept,
        "skipped_unchanged": skipped,
        "modes": {
            "off":     "No capture at all.",
            "passive": "Periodic accessibility read, text only. No model, no cost. Default.",
            "active":  "Unlocks the vision model - both when you ask, and as a tool bluee can choose."
        },
        "note": "In off and passive the analyze_screen_vlm tool is not in the toolset at all,                  so bluee cannot use vision even if it wanted to."
    }))
}

#[derive(Deserialize)]
struct VisionSet {
    mode: String,
    #[serde(default)]
    interval_secs: Option<u64>,
}

async fn set_vision(
    State(s): State<Shared>,
    Json(b): Json<VisionSet>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mode = VisionMode::parse(&b.mode)
        .ok_or_else(|| fail("mode must be off, passive or active"))?;
    s.vision.set(mode, b.interval_secs).map_err(fail)?;
    Ok(Json(json!({
        "ok": true,
        "mode": mode.as_str(),
        "vlm_tool_offered": mode.vlm_allowed(),
        "note": "Takes effect on your next message - the toolset is synced at the start of each turn."
    })))
}

// -------------------------------------------------------------- mcp page

/// Configured servers, each annotated with whether it is actually connected
/// *in this running process* and how many tools it contributed.
///
/// Configured and connected are different things - a server can be enabled in
/// the file and still have failed to start - and the page is much less useful
/// if it cannot tell you which.
async fn get_mcp(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let specs = crate::mcp::load_server_specs(&s.cfg.mcp_config).map_err(fail)?;
    let live: std::collections::BTreeSet<String> = s.registry.servers().into_iter().collect();

    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for t in s.registry.tools() {
        *counts.entry(t.server.clone()).or_insert(0) += 1;
    }

    /* Why each one is not connected. These used to be printed to stderr once
       at startup and kept nowhere, which means `harness app` - a window with no
       console - threw the reason away entirely. A run where every server failed
       then looked exactly like an empty graph (§55). */
    let failures = s.registry.failures();
    let reason = |name: &str| -> Option<String> {
        failures
            .iter()
            .find(|f| f.starts_with(&format!("{name}: ")))
            .map(|f| f[name.len() + 2..].to_string())
    };

    let servers: Vec<Value> = specs
        .iter()
        .map(|(name, spec)| {
            json!({
                "name": name,
                "command": spec.command,
                "args": spec.args,
                "env": spec.env,
                "enabled": spec.enabled,
                "connected": live.contains(name),
                "tools": counts.get(name).copied().unwrap_or(0),
                "error": reason(name),
                "note": spec.extra.get("$comment"),
            })
        })
        .collect();

    Ok(Json(json!({
        "file": s.cfg.mcp_config.display().to_string(),
        "servers": servers,
        "connected": live.len(),
        "failures": failures,
        "note": "Changes are written to the file immediately but only take effect on restart - \
                 servers are launched once at startup."
    })))
}

#[derive(Deserialize)]
struct McpSave {
    servers: std::collections::BTreeMap<String, crate::mcp::ServerSpec>,
}

async fn save_mcp(
    State(s): State<Shared>,
    Json(b): Json<McpSave>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let n = b.servers.len();
    crate::mcp::save_server_specs(&s.cfg.mcp_config, b.servers).map_err(fail)?;
    Ok(Json(json!({
        "ok": true, "servers": n,
        "note": "Saved. Restart bluee for it to connect (or stop connecting to) these."
    })))
}

/// Start the tool servers again, without restarting bluee.
///
/// Worth having because the alternative was restarting the whole app, which
/// ends the conversation you are in the middle of - and the one time this
/// mattered, every server had failed at startup and the only visible symptom
/// was an empty graph. The registry swaps its connections in place, so the
/// live agent, every sub-agent and the loop scheduler pick the new servers up
/// without being rebuilt; a turn already running keeps the set it started with.
async fn reconnect_mcp(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let specs = crate::mcp::load_server_specs(&s.cfg.mcp_config).map_err(fail)?;
    let failures = s.registry.reconnect(&specs).await;
    let servers = s.registry.servers();
    let tools = s.registry.tools().len();

    // The live agent caches its tool list, so it has to be told. This waits if
    // a turn is in flight - the agent lock is held for a whole turn - which is
    // the right way round: the running turn finishes with the toolset it began
    // with, and the next one gets the new servers.
    if let Some(agent) = s.agent.lock().await.as_mut() {
        agent.refresh_tools();
    }

    Ok(Json(json!({
        "ok": failures.is_empty(),
        "connected": servers.len(),
        "servers": servers,
        "tools": tools,
        "failures": failures,
    })))
}

// ----------------------------------------------------------- skills page

async fn get_skills(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = crate::skills::SkillStore::open(&s.cfg.skills_dir).map_err(fail)?;
    let all = store.list(None).map_err(fail)?;
    Ok(Json(json!({
        "dir": store.root().display().to_string(),
        "categories": crate::skills::CATEGORIES,
        "skills": all,
    })))
}

#[derive(Deserialize)]
struct SkillSave {
    name: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    body: String,
}

async fn save_skill(
    State(s): State<Shared>,
    Json(b): Json<SkillSave>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = crate::skills::SkillStore::open(&s.cfg.skills_dir).map_err(fail)?;
    let sk = store
        .put(&b.name, &b.category, &b.description, &b.tools, &b.body)
        .map_err(fail)?;
    Ok(Json(json!({ "ok": true, "skill": sk })))
}

#[derive(Deserialize)]
struct SkillRef {
    name: String,
}

async fn delete_skill(
    State(s): State<Shared>,
    Json(b): Json<SkillRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = crate::skills::SkillStore::open(&s.cfg.skills_dir).map_err(fail)?;
    let gone = store.delete(&b.name).map_err(fail)?;
    Ok(Json(json!({ "ok": gone, "deleted": b.name })))
}

// ----------------------------------------------------------- playground

async fn list_artifacts(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = ArtifactStore::open(&s.cfg.data_dir).map_err(fail)?;
    let all = store.list(None).map_err(fail)?;
    let topics = store.topics().map_err(fail)?;
    Ok(Json(json!({
        "topics": topics.into_iter()
            .map(|(t, n)| json!({ "topic": t, "count": n })).collect::<Vec<_>>(),
        "artifacts": all,
    })))
}

async fn artifact_root(
    State(s): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    serve_artifact(&s, &id, "").await
}

async fn artifact_file(
    State(s): State<Shared>,
    axum::extract::Path((id, path)): axum::extract::Path<(String, String)>,
) -> Response {
    serve_artifact(&s, &id, &path).await
}

/// Serve a file from inside one artifact directory.
///
/// The store refuses traversal, so a model-authored id or a hand-typed URL
/// cannot reach outside `data/artifacts`.
async fn serve_artifact(s: &Shared, id: &str, rel: &str) -> Response {
    let store = match ArtifactStore::open(&s.cfg.data_dir) {
        Ok(st) => st,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let path = match store.file(id, rel) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return (StatusCode::NOT_FOUND, format!("no such artifact file: {rel}")).into_response();
    };
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "md" | "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, mime)], bytes).into_response()
}

// ------------------------------------------------------------ terminal

#[derive(Deserialize)]
struct TermQuery {
    #[serde(default = "default_term_id")]
    id: String,
    #[serde(default = "default_shell")]
    shell: String,
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default = "default_cols")]
    cols: u16,
}

fn default_term_id() -> String {
    "main".into()
}
fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".into()
    } else {
        "bash".into()
    }
}
fn default_rows() -> u16 {
    24
}
fn default_cols() -> u16 {
    100
}

async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(s): State<Shared>,
    Query(q): Query<TermQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| terminal_loop(socket, s, q))
}

async fn terminal_loop(mut socket: WebSocket, s: Shared, q: TermQuery) {
    let session = match s.pty.attach(&q.id, &q.shell, q.rows, q.cols) {
        Ok(sess) => sess,
        Err(e) => {
            let _ = socket
                .send(WsMessage::Text(format!("\r\n[harness] {e:#}\r\n").into()))
                .await;
            return;
        }
    };

    // Report what we actually attached to. The manager returns an EXISTING
    // session when the id matches, ignoring the requested shell - so without
    // this the UI can silently show "bash" while you are typing into
    // powershell, which is exactly the bug this fixes.
    let hello = serde_json::json!({
        "type": "attached", "id": session.id, "shell": session.shell,
        "requested_shell": q.shell,
        "reused": session.shell != q.shell,
    });
    if socket
        .send(WsMessage::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Subscribe BEFORE replaying scrollback, or output produced between the
    // two would be lost.
    let mut rx = session.subscribe();
    let backlog = session.scrollback();
    if !backlog.is_empty() && socket.send(WsMessage::Binary(backlog.into())).await.is_err() {
        return;
    }

    let (mut sink, mut stream) = {
        use futures_util::StreamExt;
        socket.split()
    };

    // PTY -> browser
    let to_browser = tokio::spawn(async move {
        use futures_util::SinkExt;
        while let Ok(chunk) = rx.recv().await {
            if sink.send(WsMessage::Binary(chunk.into())).await.is_err() {
                break;
            }
        }
    });

    // browser -> PTY
    {
        use futures_util::StreamExt;
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                WsMessage::Text(t) => {
                    // Control frames are JSON; anything else is keystrokes.
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        match v.get("type").and_then(|x| x.as_str()) {
                            Some("input") => {
                                if let Some(d) = v.get("data").and_then(|x| x.as_str()) {
                                    let _ = session.write(d.as_bytes());
                                }
                                continue;
                            }
                            Some("resize") => {
                                let rows = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(24) as u16;
                                let cols = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(100) as u16;
                                let _ = session.resize(rows, cols);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    let _ = session.write(t.as_bytes());
                }
                WsMessage::Binary(b) => {
                    let _ = session.write(&b);
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }
    }

    to_browser.abort();
    // Deliberately NOT closing the pty session: the shell outlives the tab.
}

/// Which session the live agent is writing to, so the Log panel can show the
/// CURRENT conversation instead of the whole history. Null before the first
/// message, because the agent is created lazily.
async fn current_session(State(s): State<Shared>) -> impl IntoResponse {
    let guard = s.agent.lock().await;
    let ctx = guard.as_ref().map(|a| a.context_estimate());
    Json(json!({
        "session": guard.as_ref().map(|a| a.session_id().to_string()),
        "title": guard.as_ref().and_then(|a| a.title()),
        "model": guard.as_ref().map(|a| a.model.clone()),
        "tools": guard.as_ref().map(|a| a.tool_count()),
        "providers": guard.as_ref().map(|a| a.provider_count()),
        "context_tokens": ctx.map(|c| c.0),
        "messages": ctx.map(|c| c.1),
        // Advisory only. Comes from the active provider's configured window
        // (Providers page), and the token count is a ~4-chars-per-token
        // estimate, so it warns rather than enforces.
        "context_budget": guard.as_ref().map(|a| a.context_window()),
    }))
}

// ------------------------------------------- playground folder (§4f-c)

#[derive(Deserialize)]
struct FileRef {
    #[serde(default)]
    path: String,
    /// Which granted folder the path is relative to. Defaults to the
    /// playground so every existing caller keeps working.
    #[serde(default = "default_root")]
    root: String,
}

fn default_root() -> String {
    "playground".into()
}

#[derive(Deserialize)]
struct WindowReq {
    /// Which single view the new window should show.
    view: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    height: Option<f64>,
}

/// Open one view of the dashboard in a real OS window.
///
/// The window loads the same page with `?only=<view>`, so every pop-out reuses
/// the code it popped out of rather than a second implementation that drifts.
/// When not running under Tauri (plain `harness dash` in a browser) this says
/// so and the page falls back to `window.open`.
async fn open_window(
    State(s): State<Shared>,
    Json(b): Json<WindowReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut url = format!("http://127.0.0.1:{}/?only={}", s.port, b.view);
    if let Some(id) = b.id.as_deref() {
        url.push_str(&format!("&id={}", urlencode(id)));
    }
    let label = format!(
        "pop-{}-{}",
        b.view,
        b.id.as_deref().unwrap_or("main").replace(|c: char| !c.is_alphanumeric(), "-")
    );
    let title = b.title.clone().unwrap_or_else(|| format!("bluee - {}", b.view));

    if crate::app::handle().is_none() {
        return Ok(Json(json!({
            "ok": false, "tauri": false, "url": url,
            "note": "Not running as the desktop app - open it from the browser instead."
        })));
    }
    crate::app::open_view(
        url.clone(),
        label.clone(),
        title,
        b.width.unwrap_or(900.0),
        b.height.unwrap_or(620.0),
    )
    .map_err(fail)?;
    Ok(Json(json!({ "ok": true, "tauri": true, "label": label, "url": url })))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Deserialize)]
struct RootAdd {
    path: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
struct RootRef {
    id: String,
}

#[derive(Deserialize)]
struct RootServers {
    id: String,
    /// `null` means every server; `[]` means none, which is a real choice.
    #[serde(default)]
    servers: Option<Vec<String>>,
}

async fn set_root_servers(
    State(s): State<Shared>,
    Json(b): Json<RootServers>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    crate::roots::set_servers(&s.cfg, &b.id, b.servers.clone()).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "id": b.id, "servers": b.servers })))
}

#[derive(Deserialize)]
struct WorkspaceRef {
    root: String,
}

/// Say which workspace is active, so the live agent exposes that workspace's
/// tools and nothing else.
///
/// Applied to the running agent immediately rather than at the next session:
/// switching workspace mid-conversation is normal, and the point of scoping is
/// the prompt gets smaller *now*.
async fn set_workspace(
    State(s): State<Shared>,
    Json(b): Json<WorkspaceRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let root = crate::roots::get(&s.cfg, &b.root).map_err(fail)?;
    *s.workspace.lock().await = root.id.clone();

    let mut guard = s.agent.lock().await;
    let exposed = match guard.as_mut() {
        Some(a) => {
            a.set_allowed_servers(root.servers.clone());
            a.tool_count()
        }
        None => 0,
    };
    Ok(Json(json!({
        "ok": true, "workspace": root.id, "label": root.label,
        "servers": root.servers, "tools_exposed": exposed,
        "note": "null servers means every server is exposed."
    })))
}

#[derive(Deserialize)]
struct RootRename {
    id: String,
    label: String,
}

/// Rename a workspace. The label is what you call it; the id and the path do
/// not move, so nothing that referenced it breaks.
async fn rename_root(
    State(s): State<Shared>,
    Json(b): Json<RootRename>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let label = b.label.trim();
    if label.is_empty() {
        return Err(fail(anyhow::anyhow!("a workspace name cannot be empty")));
    }
    crate::roots::rename(&s.cfg, &b.id, label).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "id": b.id, "label": label })))
}

// ------------------------------------------------- the real browser (§4f-c)

/// What the browser panel needs to know before it draws anything.
///
/// This used to proxy SnareVec's `browser_*` MCP tools. That drove the browser
/// Adithya was already using - genuinely nicer, because it carried his session
/// and cookies - but it needed the SnareVec daemon up AND `"browser":
/// {"enabled": true}` set by hand in `~/.snarevec/config.json`, a gate §12e
/// records as deliberately human-only. Three ways to be broken, and the panel
/// was broken all of them in practice.
///
/// The harness now owns the whole path: it finds Chrome or Edge, starts it,
/// and speaks CDP. Nothing to enable, nothing to keep running.
///
/// The cost, stated plainly: this is a SEPARATE profile, so it is logged out of
/// everything. Its cookies persist in `data/browser` between runs, so signing
/// in once sticks, but it is not the browser you have open right now.
async fn browser_state(State(s): State<Shared>) -> impl IntoResponse {
    Json(json!({
        "status": s.browser.status().await,
        "available": s.browser.available().map(|p| p.display().to_string()),
        "running": s.browser.running(),
        "note": "bluee drives this browser itself. It has its own profile, so it is                  signed out of everything until you sign it in - those logins are                  kept in data/browser."
    }))
}

#[derive(Deserialize)]
struct BrowserAct {
    action: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    dy: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

async fn browser_act(
    State(s): State<Shared>,
    Json(b): Json<BrowserAct>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let b_ = &s.browser;
    let out = match b.action.as_str() {
        "start" => {
            // `shot` starts the browser as a side effect of needing a page.
            b_.shot().await.map(|_| json!({ "ok": true }))
        }
        "stop" => {
            b_.stop();
            Ok(json!({ "ok": true }))
        }
        "screenshot" => b_.shot().await,
        "navigate" => b_.navigate(b.url.as_deref().unwrap_or("about:blank")).await,
        "click" => b_.click(b.x.unwrap_or(0.0), b.y.unwrap_or(0.0)).await,
        "scroll" => {
            b_.scroll(b.x.unwrap_or(0.0), b.y.unwrap_or(0.0), b.dy.unwrap_or(0.0))
                .await
        }
        "type" => b_.type_text(b.text.as_deref().unwrap_or("")).await,
        "key" => b_.key(b.key.as_deref().unwrap_or("Enter")).await,
        "back" => b_.history(-1).await,
        "forward" => b_.history(1).await,
        "reload" => b_.reload().await,
        "text" => b_.text().await,
        other => return Err(fail(anyhow::anyhow!("unknown browser action: {other}"))),
    };
    Ok(Json(out.map_err(fail)?))
}

/* ---------- voice: local STT and TTS (§ src/voice.rs) ----------
   There is no API key here and no endpoint to point at, because none of this
   leaves the machine. What the page needs to know instead is what is installed
   and which device it will land on, and only the worker can answer that - so
   `/api/voice/health` asks it rather than guessing from this side. */

async fn get_voice(State(s): State<Shared>) -> impl IntoResponse {
    let cfg = s.voice.config();
    Json(json!({
        "config": cfg,
        "piper_voices": s.voice.local_piper_voices(),
        "model_dir": s.cfg.data_dir.join("voice-models").display().to_string(),
        "note": "All local. Nothing here is sent anywhere - there is no key to set                  because there is no service to call."
    }))
}

#[derive(Deserialize)]
struct VoiceSave {
    config: crate::voice::VoiceConfig,
}

async fn set_voice(
    State(s): State<Shared>,
    Json(b): Json<VoiceSave>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    b.config.save(&s.cfg.data_dir).map_err(fail)?;
    // The worker re-reads the file on mtime change, so most edits need nothing
    // more. Switching voice off is the exception: stop the process so the
    // models actually leave memory rather than idling with the weights loaded.
    if !b.config.enabled {
        s.voice.stop();
    }
    Ok(Json(json!({ "ok": true, "running": s.voice.running() })))
}

async fn voice_health(State(s): State<Shared>) -> impl IntoResponse {
    Json(s.voice.health().await)
}

async fn voice_voices(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Ok(Json(s.voice.voices().await.map_err(fail)?))
}

async fn voice_unload(State(s): State<Shared>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    s.voice.unload().await.map_err(fail)?;
    Ok(Json(json!({ "ok": true })))
}

/// Microphone audio in, text out. The body is the raw recording exactly as the
/// browser produced it; `ext` says which container so the decoder is not left
/// sniffing.
async fn stt(
    State(s): State<Shared>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let ext = headers
        .get("x-audio-ext")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("webm")
        .to_string();
    let out = s
        .voice
        .transcribe(body.to_vec(), &ext)
        .await
        .map_err(fail)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct SpeakReq {
    text: String,
    #[serde(flatten)]
    over: Value,
}

/// Text in, WAV out. Returned as real audio rather than base64 in JSON: the
/// page hands it straight to an <audio> element, and a minute of speech as a
/// data: URI is megabytes of string for no reason.
async fn tts(
    State(s): State<Shared>,
    Json(b): Json<SpeakReq>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let wav = s.voice.speak(&b.text, b.over).await.map_err(fail)?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "audio/wav"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        wav,
    )
        .into_response())
}

#[derive(Deserialize)]
struct UploadQuery {
    name: Option<String>,
}

/// Composer attachments that aren't text (images, PDFs, anything binary) land
/// here instead of being inlined into the prompt as a string. Text files never
/// hit this route - the page reads those client-side and sends their content
/// directly, same as before this existed.
///
/// Only the basename of `name` ever reaches disk, so a crafted query value
/// can't escape `data/attachments/<id>/` - there is no directory component to
/// escape with, since one is never taken from the input.
async fn upload(
    State(s): State<Shared>,
    Query(q): Query<UploadQuery>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    const MAX: usize = 50 * 1024 * 1024;
    if body.len() > MAX {
        return Err(fail("file too large (50MB limit)"));
    }
    let raw = q.name.unwrap_or_else(|| "file".to_string());
    let name = std::path::Path::new(&raw)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let dir = s.cfg.data_dir.join("attachments").join(&id);
    tokio::fs::create_dir_all(&dir).await.map_err(fail)?;
    let path = dir.join(&name);
    tokio::fs::write(&path, &body).await.map_err(fail)?;
    Ok(Json(json!({
        "name": name,
        "path": path.display().to_string(),
        "bytes": body.len(),
    })))
}

async fn list_roots(State(s): State<Shared>) -> impl IntoResponse {
    let roots: Vec<Value> = crate::roots::load(&s.cfg)
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id, "label": r.label, "builtin": r.builtin,
                "servers": r.servers,
                "path": crate::roots::pretty(&r.path),
                // Say whether it is still there: a folder can be moved or
                // deleted after it was granted, and a tree that silently comes
                // back empty is worse than one that says why.
                "exists": r.path.is_dir(),
            })
        })
        .collect();
    Json(json!({
        "roots": roots,
        "note": "Granting a folder is something you do, not something bluee can do.                  It reads, writes and deletes only inside these."
    }))
}

async fn add_root(
    State(s): State<Shared>,
    Json(b): Json<RootAdd>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let r = crate::roots::add(&s.cfg, &b.path, b.label.as_deref()).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "id": r.id, "label": r.label,
                    "path": crate::roots::pretty(&r.path) })))
}

async fn remove_root(
    State(s): State<Shared>,
    Json(b): Json<RootRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    crate::roots::remove(&s.cfg, &b.id).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "removed": b.id })))
}

async fn list_files(
    State(s): State<Shared>,
    Query(q): Query<FileRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let root = crate::roots::get(&s.cfg, &q.root).map_err(fail)?;
    let nodes = crate::artifacts::tree(&root.path).map_err(fail)?;
    Ok(Json(json!({
        "root": crate::roots::pretty(&root.path),
        "root_id": root.id, "label": root.label, "files": nodes,
    })))
}

/// Read one file for the preview pane. Binary files are reported as such
/// rather than dumped as mojibake.
async fn read_file(
    State(s): State<Shared>,
    Query(q): Query<FileRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (_, full) = crate::roots::resolve(&s.cfg, &q.root, &q.path).map_err(fail)?;
    let bytes = std::fs::read(&full).map_err(fail)?;
    let size = bytes.len();
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Json(json!({
            "path": q.path, "size": size, "text": text, "binary": false,
        }))),
        Err(_) => Ok(Json(json!({
            "path": q.path, "size": size, "binary": true,
            "note": "Not text - preview it in the browser instead.",
        }))),
    }
}

async fn delete_file(
    State(s): State<Shared>,
    Json(b): Json<FileRef>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let root = crate::roots::get(&s.cfg, &b.root).map_err(fail)?;
    let (was_dir, n) = crate::artifacts::delete_path(&root.path, &b.path).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "path": b.path, "root": root.id,
                    "folder": was_dir, "files_removed": n })))
}

#[derive(Deserialize)]
struct TitleSet {
    session: String,
    title: String,
}

/// Rename a session. Written as an event, not a stored field: the log is the
/// source of truth and a rename is just the newest title in it.
async fn set_session_title(
    State(s): State<Shared>,
    Json(b): Json<TitleSet>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let title = b.title.trim().chars().take(120).collect::<String>();
    if title.is_empty() {
        return Err(fail(anyhow::anyhow!("a title cannot be empty")));
    }

    // The live agent already holds this log open for appending. Writing
    // through it keeps the sequence numbers monotonic; a second writer on the
    // same file would not.
    let mut guard = s.agent.lock().await;
    let handled = match guard.as_mut() {
        Some(a) if a.session_id() == b.session => {
            a.set_title(&title).map_err(fail)?;
            true
        }
        _ => false,
    };
    drop(guard);

    if !handled {
        let mut log = EventLog::open(s.cfg.events_dir(), b.session.clone()).map_err(fail)?;
        log.append(crate::eventlog::EventKind::SessionTitle {
            title: title.clone(),
        })
        .map_err(fail)?;
    }
    Ok(Json(json!({ "ok": true, "session": b.session, "title": title })))
}

/// What the endpoint says it offers, including each model's real context
/// length where it publishes one - so the Providers page can fill that in
/// rather than making you look it up.
/// Which endpoint `detect` should ask. Sent by the Providers page from the row
/// the button was pressed on.
#[derive(Deserialize, Default)]
struct ModelsReq {
    #[serde(default)]
    base_url: String,
    /// May arrive masked, exactly like on save - masked means "the key already
    /// stored under this name", never "no key".
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    name: String,
}

/// List the models an endpoint offers.
///
/// **This must ask the provider the button was pressed on, not the saved
/// default.** It used to use `chain.primary_client()`, so pressing `detect` on
/// a newly added OpenRouter row queried aicredits.in instead and reported that
/// OpenRouter "does not offer" a model OpenRouter plainly does. The count in
/// the error message was the giveaway: 412 is aicredits.in's catalogue,
/// OpenRouter's is 447. A row that has not been saved yet has no entry in the
/// chain at all, which is exactly the case you are in while adding one.
async fn models(
    State(s): State<Shared>,
    body: Option<Json<ModelsReq>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();

    let client = if req.base_url.is_empty() {
        // No row supplied (the GET form): fall back to the saved default.
        let chain = crate::providers::ProviderChain::build(&s.cfg).map_err(fail)?;
        chain.primary_client().clone()
    } else {
        let key = if req.api_key.is_empty() || req.api_key.contains('\u{2022}') {
            crate::providers::load(&s.cfg)
                .providers
                .iter()
                .find(|e| e.name == req.name)
                .map(|e| e.api_key.clone())
                .unwrap_or_default()
        } else {
            req.api_key.clone()
        };
        crate::llm::OpenAiCompatible::new(req.base_url.trim_end_matches('/'), key, "", 1)
    };

    let list = client.list_models_detailed().await.map_err(fail)?;
    Ok(Json(json!({
        "count": list.len(),
        "models": list,
        "endpoint": client.base_url(),
    })))
}

// ------------------------------------------------------------- reads

async fn tools(State(s): State<Shared>) -> impl IntoResponse {
    let list: Vec<Value> = s
        .registry
        .tools()
        .iter()
        .map(|t| {
            json!({
                "id": t.qualified(),
                "server": t.server,
                "name": t.name,
                "description": t.description.lines().next().unwrap_or(""),
            })
        })
        .collect();
    Json(json!({ "count": list.len(), "tools": list }))
}

async fn stats(State(s): State<Shared>) -> impl IntoResponse {
    let sessions = EventLog::list_sessions(s.cfg.events_dir()).unwrap_or_default();
    let mut events = 0usize;
    for p in &sessions {
        events += EventLog::read(p).map(|e| e.len()).unwrap_or(0);
    }
    let chunks = VectorStore::open(s.cfg.data_dir.join("vectors.db"))
        .and_then(|v| v.count())
        .unwrap_or(0);
    let graph = s
        .registry
        .call("kuzu_graph", "graph_stats", json!({}))
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }));

    Json(json!({
        "sessions": sessions.len(),
        "events": events,
        "chunks": chunks,
        "tools": s.registry.tools().len(),
        "servers": s.registry.servers().len(),
        "tool_search": crate::toolsearch::enabled(),
        "core_servers": crate::toolsearch::core_servers(),
        "graph": graph,
    }))
}

async fn sessions(State(s): State<Shared>) -> impl IntoResponse {
    let paths = EventLog::list_sessions(s.cfg.events_dir()).unwrap_or_default();
    let mut out = Vec::new();
    for p in paths.iter().rev() {
        let id = p.file_stem().map(|s| s.to_string_lossy().to_string());
        let events = EventLog::read(p).unwrap_or_default();

        // The first thing you said is what makes a session recognisable in a
        // list - a timestamp and an event count are not.
        let mut preview = String::new();
        let (mut messages, mut tool_calls) = (0usize, 0usize);
        for e in &events {
            match &e.kind {
                crate::eventlog::EventKind::UserMessage { text } => {
                    messages += 1;
                    if preview.is_empty() {
                        preview = text.chars().take(120).collect();
                    }
                }
                crate::eventlog::EventKind::AssistantMessage { .. } => messages += 1,
                crate::eventlog::EventKind::ToolCall { .. } => tool_calls += 1,
                _ => {}
            }
        }

        out.push(json!({
            "id": id,
            "title": crate::eventlog::session_title(&events),
            "events": events.len(),
            "messages": messages,
            "tool_calls": tool_calls,
            "preview": preview,
            "started": events.first().map(|e| e.ts.to_rfc3339()),
            "last": events.last().map(|e| e.ts.to_rfc3339()),
        }));
    }
    Json(json!({ "sessions": out }))
}

#[derive(Deserialize)]
struct EventsQuery {
    session: Option<String>,
}

async fn events(
    State(s): State<Shared>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let dir = s.cfg.events_dir();
    let path = match q.session {
        Some(id) => dir.join(format!("{id}.jsonl")),
        None => EventLog::list_sessions(&dir)
            .map_err(fail)?
            .pop()
            .ok_or_else(|| fail("no sessions yet"))?,
    };
    let events = EventLog::read(&path).map_err(fail)?;
    Ok(Json(
        json!({ "session": path.file_stem().map(|s| s.to_string_lossy()), "events": events }),
    ))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
    /// Memory tier: session | recent | all | code | everything (default).
    #[serde(default)]
    scope: Option<String>,
    /// The conversation "session" means. The page knows it; the server's
    /// live agent may not be the one the page is looking at.
    #[serde(default)]
    session: Option<String>,
}

#[derive(Deserialize)]
struct BrowseQuery {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Browse memory without having to guess a search first.
///
/// The Memory page was search-only: to see anything you had to already know
/// what to ask for, and an empty box showed three example chips. "Whole RAG
/// must be visible" - so this lists a tier outright, newest first, a page at a
/// time, with the count of every tier for the chips.
async fn memory_browse(
    State(s): State<Shared>,
    Query(q): Query<BrowseQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = VectorStore::open(s.cfg.data_dir.join("vectors.db")).map_err(fail)?;
    let scope = q.scope.unwrap_or_else(|| "all".into());
    let session = q.session.unwrap_or_default();
    let (total, rows) = store
        .browse(&scope, &session, q.offset.unwrap_or(0), q.limit.unwrap_or(30).min(200))
        .map_err(fail)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|(c, sc)| {
            json!({
                "session": c.session_id, "seq_start": c.seq_start, "seq_end": c.seq_end,
                "text": c.text, "scope": sc,
            })
        })
        .collect();
    Ok(Json(json!({
        "scope": scope,
        "total": total,
        "counts": store.tier_counts(&session).map_err(fail)?,
        "items": items,
    })))
}

#[derive(Deserialize)]
struct FactsQuery {
    #[serde(default)]
    history: Option<bool>,
}

/// Remembered facts (src/facts.rs), current and optionally past.
async fn facts_list(
    State(s): State<Shared>,
    Query(q): Query<FactsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let v = s
        .registry
        .call(
            "kuzu_graph",
            "facts",
            json!({ "include_history": q.history.unwrap_or(false), "limit": 1000 }),
        )
        .await
        .map_err(fail)?;
    Ok(Json(v))
}

async fn search(
    State(s): State<Shared>,
    Query(sq): Query<SearchQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = VectorStore::open(s.cfg.data_dir.join("vectors.db")).map_err(fail)?;
    if store.count().map_err(fail)? == 0 {
        return Ok(Json(
            json!({ "hits": [], "note": "memory empty - run `harness reduce`" }),
        ));
    }

    let mut guard = s.embedder.lock().await;
    if guard.is_none() {
        *guard = Some(
            TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::AllMiniLML6V2))
                .map_err(fail)?,
        );
    }
    let embedding = guard
        .as_mut()
        .unwrap()
        .embed(vec![sq.q.as_str()], None)
        .map_err(fail)?;

    let scope = sq.scope.clone().unwrap_or_else(|| "everything".into());
    let keep = crate::tools::scope_keep(&scope, sq.session.as_deref().unwrap_or(""));
    let hits: Vec<Value> = store
        .search_where(&sq.q, &embedding[0], sq.limit.unwrap_or(10), keep)
        .map_err(fail)?
        .iter()
        .map(|h| {
            json!({
                "score": h.score,
                "session": h.chunk.session_id,
                "seq_start": h.chunk.seq_start,
                "seq_end": h.chunk.seq_end,
                "text": h.chunk.text,
            })
        })
        .collect();

    Ok(Json(json!({ "hits": hits })))
}

/// Which slice of the graph to draw.
///
/// `?session=<id>` narrows to what ONE conversation contributed. Every edge
/// already carries the session that produced it (§23), so this is a filter on
/// data that is already there rather than a second store - which is exactly why
/// per-session graphs were built as provenance instead of a database per
/// session. The Graph page asks for everything; the pane under Memory asks for
/// the session you are in.
#[derive(serde::Deserialize)]
struct GraphQuery {
    #[serde(default)]
    session: Option<String>,
}

async fn graph(
    State(s): State<Shared>,
    Query(q): Query<GraphQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if let Some(sid) = q.session.as_deref().map(safe_id).filter(|v| !v.is_empty()) {
        return graph_for_session(&s, &sid).await;
    }
    let nodes = s
        .registry
        .call(
            "kuzu_graph",
            "cypher",
            // The graph server's `cypher` defaults to limit 100. Without an
            // explicit one the page silently showed the first 100 of 1,013
            // entities - and truncated the edges separately, so many pointed at
            // nodes that were not in the set and were dropped. Ask for the lot.
            json!({
                "query": "MATCH (e:Entity) RETURN e.name AS name, e.kind AS kind",
                "limit": GRAPH_LIMIT,
            }),
        )
        .await
        .map_err(fail)?;
    let edges = s
        .registry
        .call(
            "kuzu_graph",
            "cypher",
            json!({
                "query": "MATCH (a:Entity)-[r:Rel]->(b:Entity) RETURN a.name AS source, b.name AS target, r.type AS type, r.weight AS weight",
                "limit": GRAPH_LIMIT,
            }),
        )
        .await
        .map_err(fail)?;

    let n = nodes.get("rows").cloned().unwrap_or(json!([]));
    let e = edges.get("rows").cloned().unwrap_or(json!([]));
    let (nc, ec) = (n.as_array().map_or(0, |a| a.len()), e.as_array().map_or(0, |a| a.len()));
    Ok(Json(json!({
        "nodes": n,
        "edges": e,
        // Say so rather than quietly showing part of the graph as if it were
        // all of it - that is the bug this replaced.
        "truncated": nc >= GRAPH_LIMIT || ec >= GRAPH_LIMIT,
        "limit": GRAPH_LIMIT,
    })))
}

/// Ceiling on what the Graph page draws at once. High enough for the whole
/// graph today (1,013 entities, 1,179 relations) and low enough that a runaway
/// one cannot hang the canvas.
const GRAPH_LIMIT: usize = 20_000;

/// One session's contribution: its edges, and only the entities they touch.
///
/// Both endpoints come back on the same row so the node set is derived from the
/// edges rather than fetched separately. Asking for them in two queries is how
/// the whole-graph path ended up drawing edges that pointed at nodes it had not
/// loaded (§16a); scoping makes that mismatch far likelier, so it is avoided by
/// construction here.
async fn graph_for_session(
    s: &Shared,
    session: &str,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows = s
        .registry
        .call(
            "kuzu_graph",
            "cypher",
            json!({
                "query": format!(
                    "MATCH (a:Entity)-[r:Rel]->(b:Entity) WHERE r.session = '{session}'                      RETURN a.name AS source, a.kind AS skind, b.name AS target,                             b.kind AS tkind, r.type AS type, r.weight AS weight"
                ),
                "limit": GRAPH_LIMIT,
            }),
        )
        .await
        .map_err(fail)?;

    let rows = rows.get("rows").and_then(|r| r.as_array()).cloned().unwrap_or_default();
    let mut nodes: std::collections::BTreeMap<String, Value> = Default::default();
    let mut edges = Vec::new();
    for r in &rows {
        let get = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let (src, tgt) = (get("source"), get("target"));
        if src.is_empty() || tgt.is_empty() {
            continue;
        }
        nodes.entry(src.clone())
            .or_insert_with(|| json!({ "name": src, "kind": get("skind") }));
        nodes.entry(tgt.clone())
            .or_insert_with(|| json!({ "name": tgt, "kind": get("tkind") }));
        edges.push(json!({
            "source": src, "target": tgt,
            "type": get("type"),
            "weight": r.get("weight").cloned().unwrap_or(json!(1)),
        }));
    }

    // Plus whatever the session's log implies that `reduce` has not stored
    // yet - so the graph of the conversation you are IN is not empty until the
    // next rebuild. Same extraction the reducer uses, derived and discarded.
    let mut live_added = 0usize;
    if let Ok(events) =
        crate::eventlog::EventLog::read(s.cfg.events_dir().join(format!("{session}.jsonl")))
    {
        let (lnodes, ledges) = crate::reduce::live_session_graph(&events);
        for (src, tgt, ty, w) in ledges {
            let known = edges.iter().any(|e| {
                e["source"] == json!(src) && e["target"] == json!(tgt) && e["type"] == json!(ty)
            });
            if known {
                continue;
            }
            for n in [&src, &tgt] {
                let kind = lnodes.iter().find(|(x, _)| x == n).map(|(_, k)| k.clone()).unwrap_or_default();
                nodes.entry(n.clone()).or_insert_with(|| json!({ "name": n, "kind": kind }));
            }
            edges.push(json!({ "source": src, "target": tgt, "type": ty, "weight": w, "live": true }));
            live_added += 1;
        }
    }

    let n: Vec<Value> = nodes.into_values().collect();
    Ok(Json(json!({
        "nodes": n,
        "edges": edges,
        "scope": "session",
        "session": session,
        "live_edges": live_added,
        "truncated": rows.len() >= GRAPH_LIMIT,
        "limit": GRAPH_LIMIT,
    })))
}

// ---------------------------------------------------------------- sub-agents
//
// Thin over `SubAgents` on purpose. A sub-agent's TRANSCRIPT is not served here
// because it does not need to be: it is a session like any other, so the panel
// reads it through /api/events with the agent's session id, and the Sessions
// page lists it without knowing what a sub-agent is.

#[derive(Deserialize)]
struct SpawnBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    purpose: String,
    /// Required unless `template` is given, and may be empty. See
    /// src/subagents.rs on why there is no "every server" option.
    #[serde(default)]
    servers: Option<Vec<String>>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    browser: Option<String>,
}

#[derive(Deserialize)]
struct AskBody {
    id: String,
    prompt: String,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

#[derive(Deserialize)]
struct StopBody {
    id: String,
}

async fn agents_list(State(s): State<Shared>) -> Json<Value> {
    Json(json!({ "agents": s.subagents.list(), "max": crate::subagents::MAX_AGENTS }))
}

async fn agents_spawn(
    State(s): State<Shared>,
    Json(b): Json<SpawnBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut spec = match b.template.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => {
            let t = crate::templates::get(&s.cfg.agents_dir, t).map_err(fail)?;
            let mut spec = crate::subagents::Spec::from_template(&t, Some(&b.name), Some(&b.purpose));
            if let Some(sv) = b.servers {
                spec.servers = sv;
            }
            spec
        }
        None => {
            let Some(servers) = b.servers else {
                return Err(fail(anyhow::anyhow!("servers is required unless a template is given")));
            };
            let name = if b.name.trim().is_empty() { "agent" } else { b.name.trim() };
            crate::subagents::Spec::new(name, &b.purpose, servers)
        }
    };
    if let Some(m) = b.model.filter(|m| !m.trim().is_empty()) {
        spec.model = Some(m);
    }
    if let Some(br) = b.browser.filter(|x| !x.trim().is_empty()) {
        spec.browser = Some(br.trim().to_lowercase());
    }
    let info = s.subagents.start(spec, String::new()).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "agent": info })))
}

#[derive(Deserialize)]
struct CapsBody {
    id: String,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    max_cost: Option<f64>,
}

async fn agents_caps(
    State(s): State<Shared>,
    Json(b): Json<CapsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let info = s.subagents.set_caps(&b.id, b.max_turns, b.max_cost).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "agent": info })))
}

/// The SnareVec daemon's own list of connected browsers, read with the same
/// address and token its MCP server uses (~/.snarevec/config.json). Empty when
/// the daemon is down - which is normal, it idles out.
async fn snarevec_browsers() -> Vec<Value> {
    /* Remembered briefly. When the daemon is down - its normal idle state -
       Windows takes ~1.5s to refuse a connection to a closed local port, and
       the spawn dialog waited on that every time it opened (measured: 1.5s
       per call, found by the panel's own UI check timing out). */
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<Value>)>> = std::sync::Mutex::new(None);
    if let Some((at, v)) = CACHE.lock().unwrap().as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(15) {
            return v.clone();
        }
    }
    let v = snarevec_browsers_fresh().await;
    *CACHE.lock().unwrap() = Some((std::time::Instant::now(), v.clone()));
    v
}

async fn snarevec_browsers_fresh() -> Vec<Value> {
    let path = std::env::var("SNAREVEC_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                .join(".snarevec")
                .join("config.json")
        });
    let Ok(raw) = std::fs::read_to_string(&path) else { return vec![] };
    let raw = raw.trim_start_matches('\u{feff}');
    let Ok(cfg) = serde_json::from_str::<Value>(raw) else { return vec![] };
    let port = cfg.pointer("/settings/port").and_then(|v| v.as_u64()).unwrap_or(8756);
    let token = cfg.pointer("/settings/token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(300))
        .timeout(std::time::Duration::from_millis(1500))
        .build()
    else {
        return vec![];
    };
    let Ok(res) = client
        .get(format!("http://127.0.0.1:{port}/browser/status"))
        .header("X-Snarevec-Token", token)
        .send()
        .await
    else {
        return vec![];
    };
    let v: Value = res.json().await.unwrap_or(Value::Null);
    v.get("browsers").and_then(|b| b.as_array()).cloned().unwrap_or_default()
}

/// Browsers an agent could own: Adithya's real ones connected through
/// SnareVec, plus the kinds installed for bluee's own fallback (§ webtools).
async fn browsers_list(State(s): State<Shared>) -> Json<Value> {
    let owners: std::collections::HashMap<String, String> = s
        .subagents
        .list()
        .into_iter()
        .filter_map(|a| a.browser.clone().map(|b| (b, a.name)))
        .collect();
    let mut out: Vec<Value> = Vec::new();
    let real = snarevec_browsers().await;
    for b in &real {
        let label = b.get("label").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let tabs = b.get("tabs").and_then(|v| v.as_u64()).unwrap_or(0);
        let on = b.get("active_url").and_then(|v| v.as_str()).unwrap_or("");
        out.push(json!({
            "id": label, "label": label, "source": "snarevec",
            "owner": owners.get(&label),
            "desc": format!("your real {label} · {tabs} tab(s){}{}",
                if on.is_empty() { String::new() } else { format!(" · {}", on.chars().take(40).collect::<String>()) },
                owners.get(&label).map(|o| format!(" · owned by {o}")).unwrap_or_default()),
        }));
    }
    for (kind, path) in crate::webtools::installed() {
        if real.iter().any(|b| b.get("kind").and_then(|k| k.as_str()) == Some(kind.as_str())) {
            continue;
        }
        out.push(json!({
            "id": kind, "label": kind, "source": "native",
            "owner": owners.get(&kind),
            "desc": format!("bluee's own {kind} (signed out){}",
                owners.get(&kind).map(|o| format!(" · owned by {o}")).unwrap_or_default()),
            "path": path.display().to_string(),
        }));
    }
    Json(json!({ "browsers": out, "snarevec_connected": !real.is_empty() }))
}

async fn agents_templates(State(s): State<Shared>) -> Json<Value> {
    Json(json!({
        "dir": s.cfg.agents_dir.display().to_string(),
        "templates": crate::templates::load_all(&s.cfg.agents_dir),
    }))
}

async fn agents_ask(
    State(s): State<Shared>,
    Json(b): Json<AskBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // The panel waits a beat too: a quick answer should appear without the
    // UI having to poll for it.
    let wait = b.wait_seconds.unwrap_or(45).min(120) * 1000;
    match s
        .subagents
        .ask(&b.id, &b.prompt, crate::subagents::From::User, wait)
        .await
        .map_err(fail)?
    {
        Some(reply) => Ok(Json(json!({ "ok": true, "reply": reply }))) ,
        None => Ok(Json(json!({ "ok": true, "still_working": true }))),
    }
}

async fn agents_stop(
    State(s): State<Shared>,
    Json(b): Json<StopBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    s.subagents.stop(&b.id).map_err(fail)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DespawnBody {
    id: String,
    /// Minutes of idleness before it ends itself. Absent or null means never,
    /// which is the default - an agent vanishing on you is a worse surprise
    /// than one that lingers, and idle agents cost nothing.
    #[serde(default)]
    mins: Option<u64>,
}

async fn agents_despawn(
    State(s): State<Shared>,
    Json(b): Json<DespawnBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Kept for the old panel clock: it set the END timer.
    let sleep = s.subagents.get(&b.id).and_then(|i| i.sleep_after_mins);
    s.subagents.set_timers(&b.id, sleep, b.mins).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "id": b.id, "mins": b.mins })))
}

#[derive(Deserialize)]
struct SayBody {
    id: String,
    text: String,
    #[serde(default)]
    picks: crate::agent::Picks,
}

/// Adithya talking to an agent from its window. Queued, never waited on: the
/// reply streams back over /ws/agent, and bluee hears about it via the inbox.
async fn agents_say(
    State(s): State<Shared>,
    Json(b): Json<SayBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if b.text.trim().is_empty() {
        return Err(fail(anyhow::anyhow!("nothing to say")));
    }
    s.subagents
        .ask_with(&b.id, &b.text, crate::subagents::From::User, 0, b.picks)
        .await
        .map_err(fail)?;
    Ok(Json(json!({ "ok": true, "queued": true })))
}

/// Adithya answering an agent's `ask_user` question.
async fn agents_answer(
    State(s): State<Shared>,
    Json(b): Json<SayBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    s.subagents.answer(&b.id, &b.text).map_err(fail)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct TimersBody {
    id: String,
    /// Minutes idle before sleeping / ending. null = never.
    #[serde(default)]
    sleep: Option<u64>,
    #[serde(default)]
    end: Option<u64>,
}

async fn agents_timers(
    State(s): State<Shared>,
    Json(b): Json<TimersBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let info = s.subagents.set_timers(&b.id, b.sleep, b.end).map_err(fail)?;
    Ok(Json(json!({ "ok": true, "agent": info })))
}

async fn agents_sleep(
    State(s): State<Shared>,
    Json(b): Json<StopBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    s.subagents.sleep(&b.id).map_err(fail)?;
    Ok(Json(json!({ "ok": true })))
}

async fn agents_inbox(State(s): State<Shared>) -> Json<Value> {
    Json(json!({ "pending": s.subagents.hub().pending() }))
}

#[derive(Deserialize)]
struct AgentWsQuery {
    id: String,
}

/// One agent's live feed: its status, what it is told, tool calls as they
/// happen, its replies, questions for Adithya, and `ended`. History before
/// connecting comes from /api/events with its session id.
async fn agent_ws(
    ws: WebSocketUpgrade,
    Query(q): Query<AgentWsQuery>,
    State(s): State<Shared>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        let mut socket = socket;
        let first = match s.subagents.get(&q.id) {
            Some(info) => json!({ "type": "status", "agent": info }),
            None => json!({ "type": "ended" }),
        };
        if socket.send(WsMessage::Text(first.to_string().into())).await.is_err() {
            return;
        }
        let Some(rx) = s.subagents.subscribe(&q.id) else { return };
        forward_feed(socket, rx).await;
    })
}

/// Everything about every agent, for the main window: spawns (open a window),
/// status changes, inbox notes, questions, ends.
async fn agents_feed_ws(ws: WebSocketUpgrade, State(s): State<Shared>) -> impl IntoResponse {
    let rx = s.subagents.hub().events.subscribe();
    ws.on_upgrade(move |socket| forward_feed(socket, rx))
}

async fn forward_feed(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<Value>) {
    use futures_util::StreamExt;
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(v) => {
                    let ended = v.get("type").and_then(|t| t.as_str()) == Some("ended");
                    if socket.send(WsMessage::Text(v.to_string().into())).await.is_err() || ended {
                        break;
                    }
                }
                // Fell behind: say so rather than silently missing events.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    let v = json!({ "type": "lagged", "missed": n });
                    if socket.send(WsMessage::Text(v.to_string().into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            incoming = socket.next() => match incoming {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}

#[derive(Deserialize)]
struct ResumeBody {
    session: String,
    #[serde(default)]
    name: String,
    /// Given again rather than recovered from the transcript - the toolset is a
    /// live decision about cost, not a property of what was said.
    #[serde(default)]
    servers: Vec<String>,
}

async fn agents_resume(
    State(s): State<Shared>,
    Json(b): Json<ResumeBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let name = if b.name.trim().is_empty() { "resumed" } else { b.name.trim() };
    let info = s
        .subagents
        .resume(&safe_id(&b.session), name, b.servers)
        .map_err(fail)?;
    Ok(Json(json!({ "ok": true, "agent": info })))
}
