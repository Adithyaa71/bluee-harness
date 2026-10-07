//! Sub-agents - other agents this one can start, prompt, and coordinate with.
//!
//! ## What a sub-agent is
//!
//! A real `Agent`, with its own session and its own event log. Not a lighter
//! thing, not a different turn loop (§11) - the same `Agent::turn_with` the
//! chat, the CLI and the loops drive. That is the whole design: a sub-agent is
//! addressable by the model AND by a person, because it is the same object the
//! main conversation is.
//!
//! ## What falls out of §4a for free
//!
//! Each sub-agent owns a session id, so everything it says and every tool it
//! calls is appended to the event log like anything else. That means:
//!
//! - the reducer indexes it, so what a sub-agent worked out is searchable later;
//! - the graph gets its edges tagged with that session (§23), so the Memory
//!   pane can show one sub-agent's contribution on its own;
//! - `spawn_agent` and `ask_agent` are ordinary tool calls, so they appear in
//!   the TASKS panel with their arguments and durations without that panel
//!   knowing sub-agents exist.
//!
//! None of that needed building. It is the third time the append-only log has
//! paid for itself this way, after skills and loops.
//!
//! ## Why a worker task, and not a blocking call
//!
//! The obvious shape - `ask` awaits the sub-agent's turn and returns its reply -
//! does not compile, and should not. A turn can call `ask_agent`, which runs a
//! turn: that is an async recursion rustc can neither size nor prove `Send`
//! through, and the first attempt took axum's handlers down with it. Boxing the
//! future only moved the error.
//!
//! Each sub-agent gets a worker task that OWNS it and reads prompts from a
//! channel. Nothing is recursive any more, and the main conversation never
//! blocks for however long a sub-agent takes - which is the behaviour you want
//! regardless, because a GUI task runs for minutes and the panel is watching.
//!
//! The consequence is that `ask_agent` returns "queued", not an answer. The
//! model collects with `list_agents`. That is a real cost in round trips, and it
//! buys a main thread that never freezes.
//!
//! ## Cost, which decides the shape
//!
//! §12f measured tool schemas at ~22,300 prompt tokens per turn for 129 tools.
//! A sub-agent is the structural fix for that number: give the GUI work its own
//! agent carrying UACC's 70 tools, and the main thread carries none of them.
//!
//! That only works if each sub-agent is scoped, so **`servers` is required, not
//! optional**. An unscoped sub-agent would be the most expensive object in the
//! system - paying the full toolset every turn, in a conversation nobody is
//! watching. `[]` is allowed and means native tools only.

use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::{mpsc, oneshot};

use crate::agent::{Agent, TurnEvent};
use crate::config::Config;
use crate::llm::ToolDef;
use crate::mcp::McpRegistry;
use crate::vision::VisionState;

/// How many sub-agents may exist at once. They cost nothing idle - only turns
/// bill - but each holds a provider chain and a log handle, and an unbounded
/// list is a runaway.
pub const MAX_AGENTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Created, never asked anything.
    Idle,
    /// A turn is in flight.
    Running,
    /// Finished its last turn cleanly.
    Ready,
    /// Its last turn failed. The reason is in `last`.
    Failed,
}

/// What the UI and the model are told about a sub-agent. Deliberately not the
/// `Agent` itself: only its worker may drive it, or two callers could interleave
/// turns on one history.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub purpose: String,
    pub session: String,
    pub servers: Vec<String>,
    pub status: Status,
    /// Last reply, or last error. Trimmed - the full text is in the event log.
    pub last: String,
    pub turns: usize,
    pub tools: usize,
    /// Minutes of idleness after which this agent despawns on its own.
    /// `None` is the default and means never - an agent you forgot about is a
    /// worse surprise when it vanishes than when it lingers, and an idle agent
    /// costs nothing because only turns bill.
    pub despawn_after_mins: Option<u64>,
    /// When it last finished work. Not serialised - `Instant` has no meaning
    /// outside this process, and the panel only needs the countdown.
    #[serde(skip)]
    pub idle_since: Option<std::time::Instant>,
}

/// A prompt, and somewhere to put the answer if the caller is still waiting.
///
/// The oneshot is what lets `ask` return an answer inline instead of making
/// the caller poll. It is optional because a caller that timed out has stopped
/// listening, and the worker must not care.
type Job = (String, Option<oneshot::Sender<Result<String, String>>>);

struct Entry {
    info: Arc<Mutex<AgentInfo>>,
    /// Prompts go here. Dropping it ends the worker, which is how `stop` works.
    tx: mpsc::UnboundedSender<Job>,
}

/// The live set. One per harness process.
pub struct SubAgents {
    cfg: Config,
    registry: Arc<McpRegistry>,
    vision: Arc<VisionState>,
    agents: Mutex<BTreeMap<String, Entry>>,
    next: Mutex<u32>,
}

impl SubAgents {
    pub fn new(cfg: &Config, registry: Arc<McpRegistry>, vision: Arc<VisionState>) -> Self {
        Self {
            cfg: cfg.clone(),
            registry,
            vision,
            agents: Mutex::new(BTreeMap::new()),
            next: Mutex::new(1),
        }
    }

    /// Start one, with a worker task that owns it.
    pub fn spawn(&self, name: &str, purpose: &str, servers: Vec<String>) -> Result<AgentInfo> {
        let mut map = self.agents.lock().unwrap();
        if map.len() >= MAX_AGENTS {
            bail!(
                "already running {} sub-agents, which is the cap. Stop one first - \
                 they are cheap idle, but each one is a full agent.",
                map.len()
            );
        }

        // Reject servers that are not actually connected, rather than handing
        // back an agent whose toolset is quietly smaller than was asked for.
        let live: Vec<String> = self.registry.servers();
        if let Some(bad) = servers.iter().find(|s| !live.contains(s)) {
            bail!(
                "no connected server called `{bad}`. Connected right now: {}",
                if live.is_empty() { "none".to_string() } else { live.join(", ") }
            );
        }

        let id = {
            let mut n = self.next.lock().unwrap();
            let id = format!("a{n}");
            *n += 1;
            id
        };

        // Placeholder until the worker has built it. `Agent::new` is async and
        // this function is not - deliberately: making the whole surface
        // synchronous is what breaks the Send cycle described at the top. The
        // worker fills in the session id and tool count the moment it starts.
        let info = AgentInfo {
            id: id.clone(),
            name: name.to_string(),
            purpose: purpose.to_string(),
            session: String::new(),
            servers: servers.clone(),
            status: Status::Idle,
            last: String::new(),
            turns: 0,
            tools: 0,
            despawn_after_mins: None,
            idle_since: Some(std::time::Instant::now()),
        };

        let shared = Arc::new(Mutex::new(info.clone()));
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let worker_info = shared.clone();
        let (cfg, registry, vision) =
            (self.cfg.clone(), self.registry.clone(), self.vision.clone());
        let title = name.to_string();

        /* The agent is built on the FIRST PROMPT, not here.

           Creating it eagerly opened a session and wrote `session_start` to the
           event log the moment you pressed +, so ten sub-agent tabs meant ten
           empty conversations in your history that had never done anything.
           §4a says the log records what happened; an agent that was opened and
           never used did not happen.

           So nothing touches disk until there is work. `session` and `tools`
           stay empty until then, and the panel says so rather than pretending. */
        tokio::spawn(async move {
            let mut agent: Option<Agent> = None;

            while let Some((prompt, reply_to)) = rx.recv().await {
                if agent.is_none() {
                    match Agent::new(&cfg, registry.clone(), vision.clone()).await {
                        Ok(mut a) => {
                            a.set_allowed_servers(Some(servers.clone()));
                            // Titled so the Sessions page makes it obvious which
                            // conversations were a sub-agent's rather than
                            // something Adithya typed.
                            a.set_title(&format!("sub-agent: {title}")).ok();
                            {
                                let mut i = worker_info.lock().unwrap();
                                i.session = a.session_id().to_string();
                                i.tools = a.tool_count();
                            }
                            agent = Some(a);
                        }
                        Err(e) => {
                            let mut i = worker_info.lock().unwrap();
                            i.status = Status::Failed;
                            i.last = format!("could not start: {e:#}");
                            if let Some(tx) = reply_to {
                                let _ = tx.send(Err(i.last.clone()));
                            }
                            continue;
                        }
                    }
                }
                let a = agent.as_mut().unwrap();

                { worker_info.lock().unwrap().status = Status::Running; }
                let events = a.turn_with(&prompt, None).await;

                let mut reply = String::new();
                let mut failed = None;
                for e in events {
                    match e {
                        TurnEvent::Reply { text } => reply = text,
                        TurnEvent::Error { message } => failed = Some(message),
                        _ => {}
                    }
                }
                {
                    let mut i = worker_info.lock().unwrap();
                    i.turns += 1;
                    i.idle_since = Some(std::time::Instant::now());
                    match &failed {
                        Some(m) => { i.status = Status::Failed; i.last = m.chars().take(400).collect(); }
                        None => { i.status = Status::Ready; i.last = reply.chars().take(400).collect(); }
                    }
                }
                if let Some(tx) = reply_to {
                    let _ = tx.send(match failed { Some(m) => Err(m), None => Ok(reply) });
                }
            }
            // Only close a log we actually opened.
            if let Some(mut a) = agent {
                a.end("sub-agent stopped");
            }
        });

        map.insert(id, Entry { info: shared, tx });
        Ok(info)
    }

    /// Re-attach a stopped sub-agent to the session it left off in.
    ///
    /// A sub-agent is a session, so "resume" is not a special mode - it is
    /// `Agent::resume`, the same call the Sessions page uses to continue any
    /// conversation. Its history comes back, so a follow-up knows what it
    /// already did.
    ///
    /// `servers` has to be given again rather than recovered from the log. The
    /// toolset is a live decision about cost, not a property of the transcript,
    /// and silently restoring a 70-tool agent because it used to have one is
    /// exactly the surprise the scoping rule exists to prevent.
    pub fn resume(&self, session: &str, name: &str, servers: Vec<String>) -> Result<AgentInfo> {
        let mut map = self.agents.lock().unwrap();
        if map.len() >= MAX_AGENTS {
            bail!("already running {} sub-agents, which is the cap.", map.len());
        }
        if map.values().any(|e| e.info.lock().unwrap().session == session) {
            bail!("that session is already open as a sub-agent");
        }
        let live: Vec<String> = self.registry.servers();
        if let Some(bad) = servers.iter().find(|s| !live.contains(s)) {
            bail!("no connected server called `{bad}`");
        }

        let id = {
            let mut n = self.next.lock().unwrap();
            let id = format!("a{n}");
            *n += 1;
            id
        };

        let info = AgentInfo {
            id: id.clone(),
            name: name.to_string(),
            purpose: "resumed".into(),
            session: session.to_string(),
            servers: servers.clone(),
            status: Status::Idle,
            last: String::new(),
            turns: 0,
            tools: 0,
            despawn_after_mins: None,
            idle_since: Some(std::time::Instant::now()),
        };

        let shared = Arc::new(Mutex::new(info.clone()));
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let worker_info = shared.clone();
        let (cfg, registry, vision) =
            (self.cfg.clone(), self.registry.clone(), self.vision.clone());
        let sid = session.to_string();

        tokio::spawn(async move {
            let mut agent = match Agent::resume(&cfg, registry, vision, &sid).await {
                Ok(a) => a,
                Err(e) => {
                    let mut i = worker_info.lock().unwrap();
                    i.status = Status::Failed;
                    i.last = format!("could not resume: {e:#}");
                    return;
                }
            };
            agent.set_allowed_servers(Some(servers));
            {
                let mut i = worker_info.lock().unwrap();
                i.tools = agent.tool_count();
            }
            while let Some((prompt, reply_to)) = rx.recv().await {
                { worker_info.lock().unwrap().status = Status::Running; }
                let events = agent.turn_with(&prompt, None).await;
                let mut reply = String::new();
                let mut failed = None;
                for e in events {
                    match e {
                        TurnEvent::Reply { text } => reply = text,
                        TurnEvent::Error { message } => failed = Some(message),
                        _ => {}
                    }
                }
                {
                    let mut i = worker_info.lock().unwrap();
                    i.turns += 1;
                    i.idle_since = Some(std::time::Instant::now());
                    match &failed {
                        Some(m) => { i.status = Status::Failed; i.last = m.chars().take(400).collect(); }
                        None => { i.status = Status::Ready; i.last = reply.chars().take(400).collect(); }
                    }
                }
                if let Some(tx) = reply_to {
                    let _ = tx.send(match failed { Some(m) => Err(m), None => Ok(reply) });
                }
            }
            agent.end("sub-agent stopped");
        });

        map.insert(id, Entry { info: shared, tx });
        Ok(info)
    }

    /// Give an agent a turn, and wait up to `wait_ms` for the answer.
    ///
    /// `Ok(Some(reply))` means it finished inside the window - one round trip,
    /// no polling. `Ok(None)` means it is still working and the answer will
    /// land on its status and in its log.
    ///
    /// This is the fix for the obvious objection to the queue-and-poll design:
    /// polling costs a whole turn each time, and a turn carries every tool
    /// schema. Most delegated tasks finish in seconds, so waiting briefly turns
    /// the common case into a single exchange and keeps the non-blocking
    /// behaviour only for work that genuinely takes minutes.
    ///
    /// Waiting here does NOT reintroduce the async recursion: this awaits a
    /// channel, not `turn_with`. The turn still runs on the worker's task.
    pub async fn ask(&self, id: &str, prompt: &str, wait_ms: u64) -> Result<Option<String>> {
        let rx = {
            let map = self.agents.lock().unwrap();
            let Some(entry) = map.get(id) else {
                bail!("no sub-agent `{id}`. `list_agents` shows which exist.");
            };
            entry.info.lock().unwrap().status = Status::Running;
            let (tx, rx) = oneshot::channel();
            entry
                .tx
                .send((prompt.to_string(), Some(tx)))
                .map_err(|_| anyhow::anyhow!("sub-agent `{id}` is no longer running"))?;
            rx
        };

        match tokio::time::timeout(std::time::Duration::from_millis(wait_ms), rx).await {
            Ok(Ok(Ok(reply))) => Ok(Some(reply)),
            Ok(Ok(Err(e))) => bail!("sub-agent `{id}` failed: {e}"),
            // Worker gone mid-flight.
            Ok(Err(_)) => bail!("sub-agent `{id}` stopped before it answered"),
            Err(_) => Ok(None),
        }
    }

    /// Set (or clear) the idle despawn timer.
    pub fn set_despawn(&self, id: &str, mins: Option<u64>) -> Result<()> {
        let map = self.agents.lock().unwrap();
        let Some(entry) = map.get(id) else { bail!("no sub-agent `{id}`") };
        entry.info.lock().unwrap().despawn_after_mins = mins;
        Ok(())
    }

    /// Drop agents that have been idle past their own timer.
    ///
    /// Only ones that set a timer - the default is never, so forgetting about
    /// an agent never loses it. Its session is in the event log regardless, so
    /// even a despawned agent can be read back and resumed.
    pub fn sweep(&self) -> Vec<String> {
        let mut map = self.agents.lock().unwrap();
        let mut gone = Vec::new();
        map.retain(|id, e| {
            let i = e.info.lock().unwrap();
            let Some(mins) = i.despawn_after_mins else { return true };
            if i.status == Status::Running {
                return true;
            }
            let idle_ok = i
                .idle_since
                .map(|t| t.elapsed().as_secs() < mins * 60)
                .unwrap_or(true);
            if !idle_ok {
                gone.push(id.clone());
            }
            idle_ok
        });
        gone
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        let map = self.agents.lock().unwrap();
        let mut out = Vec::new();
        for e in map.values() {
            out.push(e.info.lock().unwrap().clone());
        }
        out
    }

    /// End one. Its transcript stays - the conversation happened, and §4a does
    /// not let us pretend otherwise. Only the live agent goes.
    pub fn stop(&self, id: &str) -> Result<()> {
        let mut map = self.agents.lock().unwrap();
        // Dropping the sender ends the worker's recv loop, which closes the
        // agent's log on its way out.
        if map.remove(id).is_none() {
            bail!("no sub-agent `{id}`");
        }
        Ok(())
    }

    /// Tool definitions offered to whichever agent owns this registry.
    ///
    /// Note what a sub-agent does NOT get: these. A sub-agent is built without a
    /// registry of its own, so it is never offered them - a sub-agent that can
    /// spawn sub-agents is a fork bomb with a credit card, and `MAX_AGENTS`
    /// alone would not be a satisfying answer to that.
    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "spawn_agent".into(),
                description:
                    "Start a sub-agent: another assistant with its own conversation, its own \
                     memory of what it did, and its own tools. Use one when a task needs a large \
                     toolset you do not otherwise want to carry - GUI automation, browsing - or \
                     when a job is self-contained enough to hand over whole. You must say which \
                     servers it may use; giving it none still leaves it memory and file tools."
                        .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string",
                            "description": "Short handle, e.g. `gui` or `researcher`." },
                        "purpose": { "type": "string",
                            "description": "One line on what it is for. Shown in the panel." },
                        "servers": {
                            "type": "array", "items": { "type": "string" },
                            "description": "MCP servers it may use, e.g. [\"uacc\"]. Empty means \
                                native tools only. Required - an unscoped sub-agent pays for \
                                every tool schema on every turn."
                        }
                    },
                    "required": ["name", "purpose", "servers"]
                }),
            },
            ToolDef {
                name: "ask_agent".into(),
                description:
                    "Give a sub-agent a task. This does NOT wait for the answer - it returns as \
                     soon as the work is queued, so you can set several agents going and collect \
                     them together. Use list_agents to see status and what each one said. It \
                     keeps its own conversation, so follow-ups remember what it already did."
                        .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "From spawn_agent or list_agents." },
                        "prompt": { "type": "string" }
                    },
                    "required": ["id", "prompt"]
                }),
            },
            ToolDef {
                name: "list_agents".into(),
                description:
                    "Which sub-agents exist, what each is for, what it can reach, whether it is \
                     still working, and what it last said. This is how you collect an answer \
                     after ask_agent."
                        .into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
            ToolDef {
                name: "stop_agent".into(),
                description:
                    "End a sub-agent. Its transcript is kept - you can still search what it did \
                     - but it stops existing as something you can ask."
                        .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
        ]
    }

    pub fn is_tool(name: &str) -> bool {
        matches!(name, "spawn_agent" | "ask_agent" | "list_agents" | "stop_agent")
    }

    /// Run one of the four.
    pub async fn call(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "spawn_agent" => {
                let servers: Vec<String> = args
                    .get("servers")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let info = self.spawn(&s("name"), &s("purpose"), servers)?;
                Ok(serde_json::json!({
                    "id": info.id,
                    "name": info.name,
                    "session": info.session,
                    "tools": info.tools,
                    "note": "Started, and idle. Use ask_agent with this id."
                }))
            }
            "ask_agent" => {
                // Wait a beat rather than returning immediately. Most delegated
                // work lands inside this window, and then the whole exchange is
                // ONE round trip - no polling turn, which would otherwise carry
                // every tool schema again just to ask "done yet?".
                let wait = args.get("wait_seconds").and_then(|v| v.as_u64()).unwrap_or(45);
                match self.ask(&s("id"), &s("prompt"), wait.min(120) * 1000).await? {
                    Some(reply) => Ok(serde_json::json!({ "id": s("id"), "reply": reply })),
                    None => Ok(serde_json::json!({
                        "id": s("id"),
                        "still_working": true,
                        "note": "Not finished inside the wait - it is still going. Its answer                                  will be on list_agents when it lands. Get on with something                                  else rather than asking again."
                    })),
                }
            }
            "list_agents" => Ok(serde_json::json!({ "agents": self.list() })),
            "stop_agent" => {
                self.stop(&s("id"))?;
                Ok(serde_json::json!({ "stopped": s("id") }))
            }
            other => bail!("not a sub-agent tool: {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_tools_are_offered() {
        let defs = SubAgents::defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["spawn_agent", "ask_agent", "list_agents", "stop_agent"]);
        for n in &names {
            assert!(SubAgents::is_tool(n));
        }
        assert!(!SubAgents::is_tool("search_memory"));
    }

    #[test]
    fn servers_is_required_on_spawn() {
        // The cost lever only works if every sub-agent is scoped, so the schema
        // must not let the model omit it and quietly get everything.
        let def = SubAgents::defs().into_iter().find(|d| d.name == "spawn_agent").unwrap();
        let req = def.parameters["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "servers"), "servers must be required");
    }

    #[test]
    fn ask_says_plainly_that_it_does_not_wait() {
        // The whole protocol depends on the model understanding that a reply is
        // collected later. If this description loses that, the model will treat
        // a queued task as a finished one and report work it never saw.
        let def = SubAgents::defs().into_iter().find(|d| d.name == "ask_agent").unwrap();
        assert!(def.description.contains("does NOT wait"));
        assert!(def.description.contains("list_agents"));
    }
}
