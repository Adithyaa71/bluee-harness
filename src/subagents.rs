//! Sub-agents - other agents this one can start, prompt, and coordinate with.
//!
//! ## What a sub-agent is
//!
//! A real `Agent`, with its own session and its own event log. Not a lighter
//! thing, not a different turn loop (§11) - the same `Agent::turn_with` the
//! chat, the CLI and the loops drive. So it is addressable by the model AND by
//! a person: bluee asks it things, and Adithya talks to it in its own window.
//!
//! Because each one owns a session, its transcript is in the event log, the
//! reducer indexes it, the graph tags its edges with it (§23), and the Sessions
//! page lists it - none of which needed building.
//!
//! ## One worker per agent
//!
//! `ask` awaiting `turn_with` is an async recursion rustc can neither size nor
//! prove `Send` through (a turn can ask an agent, which runs a turn). So each
//! sub-agent has a worker task that OWNS it and reads jobs from a channel.
//! New, resumed and woken-from-sleep agents all run the same worker: the agent
//! is built lazily on the first job, from the session id if it has one.
//!
//! ## Talking both ways
//!
//! - **bluee -> agent**: `ask_agent`. It waits briefly; if the answer lands in
//!   time it is returned inline (one round trip). If not, or if bluee asked in
//!   the background, the answer goes to bluee's INBOX when it lands - no
//!   polling turn, which would carry every tool schema just to ask "done yet?".
//! - **agent -> bluee**: `message_parent`, also into the inbox.
//! - **Adithya -> agent**: its window. What was said and answered there goes
//!   into bluee's inbox too, so the main conversation knows.
//! - **agent -> Adithya**: `ask_user`, which pauses the agent until he answers
//!   in its window.
//!
//! The inbox is delivered at the start of bluee's next turn, and the UI can
//! start that turn itself when bluee was waiting on a result (`wake`).
//!
//! ## Lifecycle
//!
//! Busy - a turn running, a job queued, or waiting on `ask_user` - never sleeps
//! or ends, window open or not. Idle past `sleep_after_mins` (45) the agent is
//! dropped from memory but keeps its session; the next message resumes it.
//! Idle past `end_after_mins` (120) the worker ends and its window closes; the
//! session stays in history and can be resumed. Both are per agent.
//!
//! ## Cost
//!
//! `servers` is required on spawn: an unscoped sub-agent would pay for every
//! tool schema on every turn in a conversation nobody is watching. Sub-agents
//! cannot spawn sub-agents - they get `message_parent`/`ask_user` instead.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::agent::{Agent, TurnEvent};
use crate::config::Config;
use crate::llm::ToolDef;
use crate::mcp::McpRegistry;
use crate::vision::VisionState;

/// How many sub-agents may exist at once. Idle ones cost nothing - only turns
/// bill - but an unbounded list is a runaway.
pub const MAX_AGENTS: usize = 8;
pub const DEFAULT_SLEEP_MINS: u64 = 45;
pub const DEFAULT_END_MINS: u64 = 120;
/// Marks a job from bluee in the sub-agent's prompt and log. The window strips
/// it to label the message instead.
pub const PARENT_PREFIX: &str = "(from bluee, the main assistant) ";
/// How long `ask_user` waits for Adithya before giving up.
const ASK_USER_TIMEOUT_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Created, never asked anything.
    Idle,
    /// A turn is in flight.
    Running,
    /// Paused on `ask_user`, waiting for Adithya.
    Waiting,
    /// Finished its last turn cleanly.
    Ready,
    /// Its last turn failed. The reason is in `last`.
    Failed,
    /// Unloaded after idling; the next message resumes it.
    Sleeping,
}

/// Who gave a job. Decides where its answer is reported.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum From {
    /// Adithya, from the agent's window or the panel.
    User,
    /// bluee, through `ask_agent`.
    Parent,
}

/// What the UI and the model are told about a sub-agent. Deliberately not the
/// `Agent` itself: only its worker may drive it.
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
    /// Jobs waiting behind the current one.
    pub queued: usize,
    /// The open `ask_user` question, if it is waiting on one.
    pub question: Option<String>,
    /// Minutes idle before it sleeps / ends. `None` = never.
    pub sleep_after_mins: Option<u64>,
    pub end_after_mins: Option<u64>,
    /// Seconds since it last finished work. Filled in by `list`.
    pub idle_secs: u64,
    pub browser: Option<String>,
    pub model: Option<String>,
    pub template: Option<String>,
    /// `Instant` means nothing outside this process; `idle_secs` is the
    /// serialised form.
    #[serde(skip)]
    pub idle_since: Option<Instant>,
}

impl AgentInfo {
    fn busy(&self) -> bool {
        matches!(self.status, Status::Running | Status::Waiting) || self.queued > 0
    }
}

/// Everything needed to (re)build an agent. Kept by the worker so a sleeping
/// agent wakes up as the same thing it was.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub name: String,
    pub purpose: String,
    pub servers: Vec<String>,
    /// Extra instructions on top of the persona (from a template).
    pub instructions: Option<String>,
    pub browser: Option<String>,
    pub model: Option<String>,
    pub template: Option<String>,
    pub sleep_after_mins: Option<u64>,
    pub end_after_mins: Option<u64>,
}

impl Spec {
    pub fn new(name: &str, purpose: &str, servers: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            purpose: purpose.to_string(),
            servers,
            sleep_after_mins: Some(DEFAULT_SLEEP_MINS),
            end_after_mins: Some(DEFAULT_END_MINS),
            ..Default::default()
        }
    }
}

/// Something an agent wants bluee to know. Delivered at bluee's next turn.
#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub agent: String,
    pub name: String,
    /// `done`, `failed`, `message` or `talk` (Adithya spoke to it directly).
    pub kind: String,
    pub text: String,
    /// bluee was waiting on this - the UI may start a turn to deliver it.
    pub wake: bool,
}

impl Note {
    fn render(&self) -> String {
        match self.kind.as_str() {
            "done" => format!("Sub-agent {} ({}) finished the task you gave it:\n{}", self.name, self.agent, self.text),
            "failed" => format!("Sub-agent {} ({}) FAILED the task you gave it: {}", self.name, self.agent, self.text),
            "message" => format!("Sub-agent {} ({}) sent you a message:\n{}", self.name, self.agent, self.text),
            _ => format!("Adithya talked to sub-agent {} ({}) directly:\n{}", self.name, self.agent, self.text),
        }
    }
}

/// Shared between every sub-agent and the main conversation.
pub struct Hub {
    inbox: Mutex<Vec<Note>>,
    /// Global feed for the main window: spawns, status changes, inbox, questions.
    pub events: broadcast::Sender<Value>,
    answers: Mutex<HashMap<String, oneshot::Sender<String>>>,
}

impl Hub {
    fn new() -> Self {
        Self {
            inbox: Mutex::new(Vec::new()),
            events: broadcast::channel(256).0,
            answers: Mutex::new(HashMap::new()),
        }
    }

    fn push(&self, note: Note) {
        let _ = self.events.send(json!({ "type": "inbox", "note": &note }));
        self.inbox.lock().unwrap().push(note);
    }

    /// Take everything waiting for bluee, as one block of text for its prompt.
    pub fn drain(&self) -> Option<String> {
        let notes: Vec<Note> = std::mem::take(&mut *self.inbox.lock().unwrap());
        if notes.is_empty() {
            return None;
        }
        let body: Vec<String> = notes.iter().map(Note::render).collect();
        Some(format!("[Sub-agent updates since your last turn]\n\n{}", body.join("\n\n")))
    }

    pub fn pending(&self) -> usize {
        self.inbox.lock().unwrap().len()
    }

    fn publish(&self, info: &AgentInfo) {
        let _ = self.events.send(json!({ "type": "agent", "agent": info }));
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push_str(" ...");
        out
    }
}

// ------------------------------------------------------------------ child side

/// What a sub-agent holds to reach the outside: bluee's inbox and Adithya.
#[derive(Clone)]
pub struct ChildLink {
    pub id: String,
    pub name: String,
    pub browser: Option<String>,
    info: Arc<Mutex<AgentInfo>>,
    bus: broadcast::Sender<Value>,
    hub: Arc<Hub>,
}

impl ChildLink {
    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "message_parent".into(),
                description: "Send a message to bluee, the main assistant that started you. Use it to \
                    report progress on a long job, hand over a finding, or ask bluee for something \
                    you cannot do. It arrives at bluee's next turn; set wake to true only when \
                    bluee is blocked waiting on you."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "wake": { "type": "boolean", "description": "Start bluee's turn now rather than waiting for its next one." }
                    },
                    "required": ["text"]
                }),
            },
            ToolDef {
                name: "ask_user".into(),
                description: "Ask Adithya a question and wait for his answer, shown in your window. \
                    Use it when you need a decision or a detail only he has - not for things you can \
                    look up. You are paused until he answers (up to 30 minutes)."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "question": { "type": "string" } },
                    "required": ["question"]
                }),
            },
        ]
    }

    pub fn handles(name: &str) -> bool {
        matches!(name, "message_parent" | "ask_user")
    }

    pub async fn call(&self, tool: &str, args: &Value) -> Result<Value> {
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        match tool {
            "message_parent" => {
                let text = s("text");
                if text.is_empty() {
                    bail!("text is empty");
                }
                let wake = args.get("wake").and_then(|v| v.as_bool()).unwrap_or(false);
                self.hub.push(Note {
                    agent: self.id.clone(),
                    name: self.name.clone(),
                    kind: "message".into(),
                    text,
                    wake,
                });
                Ok(json!({ "delivered": true, "note": "bluee will see this at its next turn." }))
            }
            "ask_user" => {
                let question = s("question");
                if question.is_empty() {
                    bail!("question is empty");
                }
                let (tx, rx) = oneshot::channel();
                self.hub.answers.lock().unwrap().insert(self.id.clone(), tx);
                {
                    let mut i = self.info.lock().unwrap();
                    i.status = Status::Waiting;
                    i.question = Some(question.clone());
                    self.hub.publish(&i);
                }
                // The tool_call event for this very call travels through the
                // turn's forwarding task; give it a moment so the window draws
                // the call before the question rather than after it.
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                let _ = self.bus.send(json!({ "type": "question", "text": question }));
                let _ = self.hub.events.send(json!({
                    "type": "agent_question", "id": self.id, "name": self.name, "text": question
                }));
                let waited = tokio::time::timeout(
                    std::time::Duration::from_secs(ASK_USER_TIMEOUT_SECS),
                    rx,
                )
                .await;
                self.hub.answers.lock().unwrap().remove(&self.id);
                {
                    let mut i = self.info.lock().unwrap();
                    i.status = Status::Running;
                    i.question = None;
                    self.hub.publish(&i);
                }
                match waited {
                    Ok(Ok(answer)) => {
                        let _ = self.bus.send(json!({ "type": "answer", "text": answer }));
                        Ok(json!({ "answer": answer }))
                    }
                    _ => bail!("Adithya did not answer within 30 minutes - carry on without it or stop and say so"),
                }
            }
            other => bail!("not a sub-agent child tool: {other}"),
        }
    }
}

// ---------------------------------------------------------------- parent side

struct Job {
    prompt: String,
    from: From,
    /// Somewhere to put the answer if the caller is still listening. If the
    /// caller gave up (timed out), a Parent job's answer goes to the inbox.
    reply_to: Option<oneshot::Sender<Result<String, String>>>,
}

enum Msg {
    Job(Job),
    Sleep,
}

struct Entry {
    info: Arc<Mutex<AgentInfo>>,
    /// Dropping it ends the worker, which is how `stop` and `end` work.
    tx: mpsc::UnboundedSender<Msg>,
    /// Live feed for this agent's window.
    bus: broadcast::Sender<Value>,
}

/// The live set. One per harness process.
pub struct SubAgents {
    cfg: Config,
    registry: Arc<McpRegistry>,
    vision: Arc<VisionState>,
    hub: Arc<Hub>,
    agents: Mutex<BTreeMap<String, Entry>>,
    next: Mutex<u32>,
}

/// Everything a worker owns.
struct Worker {
    cfg: Config,
    registry: Arc<McpRegistry>,
    vision: Arc<VisionState>,
    hub: Arc<Hub>,
    id: String,
    spec: Spec,
    info: Arc<Mutex<AgentInfo>>,
    bus: broadcast::Sender<Value>,
}

impl Worker {
    async fn build(&self) -> Result<Agent> {
        let session = self.info.lock().unwrap().session.clone();
        let mut a = if session.is_empty() {
            let mut a = Agent::new(&self.cfg, self.registry.clone(), self.vision.clone()).await?;
            // Titled so the Sessions page makes it obvious which conversations
            // were a sub-agent's rather than something Adithya typed.
            a.set_title(&format!("sub-agent: {}", self.spec.name)).ok();
            a
        } else {
            Agent::resume(&self.cfg, self.registry.clone(), self.vision.clone(), &session).await?
        };
        a.set_allowed_servers(Some(self.spec.servers.clone()));
        a.set_child(ChildLink {
            id: self.id.clone(),
            name: self.spec.name.clone(),
            browser: self.spec.browser.clone(),
            info: self.info.clone(),
            bus: self.bus.clone(),
            hub: self.hub.clone(),
        });
        if let Some(extra) = &self.spec.instructions {
            a.add_note(&format!("Your role as sub-agent `{}`:\n{extra}", self.spec.name));
        }
        {
            let mut i = self.info.lock().unwrap();
            i.session = a.session_id().to_string();
            i.tools = a.tool_count();
        }
        Ok(a)
    }

    fn set(&self, f: impl FnOnce(&mut AgentInfo)) {
        let mut i = self.info.lock().unwrap();
        f(&mut i);
        self.hub.publish(&i);
        let _ = self.bus.send(json!({ "type": "status", "agent": &*i }));
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        let mut agent: Option<Agent> = None;
        while let Some(msg) = rx.recv().await {
            let job = match msg {
                Msg::Sleep => {
                    // Dropping the Agent closes nothing permanent: the session
                    // log is on disk and `build` resumes it on the next job.
                    agent = None;
                    self.set(|i| i.status = Status::Sleeping);
                    continue;
                }
                Msg::Job(j) => j,
            };
            self.set(|i| {
                i.queued = i.queued.saturating_sub(1);
                i.status = Status::Running;
            });
            let _ = self.bus.send(json!({ "type": "user", "from": job.from, "text": job.prompt }));

            if agent.is_none() {
                match self.build().await {
                    Ok(a) => agent = Some(a),
                    Err(e) => {
                        let msg = format!("could not start: {e:#}");
                        self.finish(&job, Err(msg.clone()));
                        self.deliver(job, Err(msg));
                        continue;
                    }
                }
            }
            let a = agent.as_mut().unwrap();

            // Forward the turn's events to the window as they happen.
            let (etx, mut erx) = mpsc::unbounded_channel::<TurnEvent>();
            let bus = self.bus.clone();
            let fwd = tokio::spawn(async move {
                while let Some(ev) = erx.recv().await {
                    if let Ok(v) = serde_json::to_value(&ev) {
                        let _ = bus.send(v);
                    }
                }
            });
            // The agent should know who is talking: Adithya is its user, bluee
            // is a colleague handing it work. Marked in the prompt itself so the
            // log - and a replay of it - says so too.
            let prompt = match job.from {
                From::Parent => format!("{PARENT_PREFIX}{}", job.prompt),
                From::User => job.prompt.clone(),
            };
            let events = a.turn_with(&prompt, Some(&etx)).await;
            drop(etx);
            let _ = fwd.await;

            let mut reply = String::new();
            let mut failed = None;
            for e in events {
                match e {
                    TurnEvent::Reply { text } => reply = text,
                    TurnEvent::Error { message } => failed = Some(message),
                    _ => {}
                }
            }
            let result = match failed {
                Some(m) if reply.is_empty() => Err(m),
                _ => Ok(reply),
            };
            self.finish(&job, result.clone());
            self.deliver(job, result);
        }
        if let Some(mut a) = agent {
            a.end("sub-agent ended");
        }
        self.set(|i| i.status = Status::Sleeping);
        let _ = self.bus.send(json!({ "type": "ended" }));
        let _ = self.hub.events.send(json!({ "type": "agent_ended", "id": self.id }));
    }

    fn finish(&self, _job: &Job, result: Result<String, String>) {
        self.set(|i| {
            i.turns += 1;
            i.idle_since = Some(Instant::now());
            i.question = None;
            match &result {
                Ok(r) => {
                    i.status = if i.queued > 0 { Status::Running } else { Status::Ready };
                    i.last = clip(r, 400);
                }
                Err(m) => {
                    i.status = Status::Failed;
                    i.last = clip(m, 400);
                }
            }
        });
        let _ = self.bus.send(json!({
            "type": "done",
            "ok": result.is_ok(),
            "text": match &result { Ok(r) => r.clone(), Err(m) => m.clone() },
        }));
    }

    /// Hand the answer to whoever is waiting, or to bluee's inbox.
    fn deliver(&self, job: Job, result: Result<String, String>) {
        let undelivered = match job.reply_to {
            Some(tx) => tx.send(result.clone()).err().is_some(),
            None => true,
        };
        if !undelivered {
            return;
        }
        let (kind, text, wake) = match (job.from, &result) {
            (From::Parent, Ok(r)) => ("done", clip(r, 4000), true),
            (From::Parent, Err(m)) => ("failed", clip(m, 1000), true),
            // Adithya spoke to it directly. bluee should know, but nothing
            // is waiting on it, so no wake.
            (From::User, r) => (
                "talk",
                format!(
                    "He said: {}\nIt answered: {}",
                    clip(&job.prompt, 1000),
                    clip(r.as_ref().map(|s| s.as_str()).unwrap_or_else(|e| e.as_str()), 1500)
                ),
                false,
            ),
        };
        self.hub.push(Note {
            agent: self.id.clone(),
            name: self.spec.name.clone(),
            kind: kind.into(),
            text,
            wake,
        });
    }
}

impl SubAgents {
    pub fn new(cfg: &Config, registry: Arc<McpRegistry>, vision: Arc<VisionState>) -> Self {
        Self {
            cfg: cfg.clone(),
            registry,
            vision,
            hub: Arc::new(Hub::new()),
            agents: Mutex::new(BTreeMap::new()),
            next: Mutex::new(1),
        }
    }

    pub fn hub(&self) -> Arc<Hub> {
        self.hub.clone()
    }

    /// Start one. Nothing touches disk until its first job: an agent that was
    /// opened and never used did not happen (§4a).
    pub fn spawn(&self, name: &str, purpose: &str, servers: Vec<String>) -> Result<AgentInfo> {
        self.start(Spec::new(name, purpose, servers), String::new())
    }

    /// Re-attach a stopped sub-agent to the session it left off in. `servers`
    /// is given again rather than recovered: what an agent may reach is a
    /// live decision about cost, not a property of its transcript.
    pub fn resume(&self, session: &str, name: &str, servers: Vec<String>) -> Result<AgentInfo> {
        if self.agents.lock().unwrap().values().any(|e| e.info.lock().unwrap().session == session) {
            bail!("that session is already open as a sub-agent");
        }
        let mut spec = Spec::new(name, "resumed", servers);
        spec.purpose = "resumed".into();
        self.start(spec, session.to_string())
    }

    pub fn start(&self, spec: Spec, session: String) -> Result<AgentInfo> {
        let mut map = self.agents.lock().unwrap();
        if map.len() >= MAX_AGENTS {
            bail!(
                "already running {} sub-agents, which is the cap. Stop one first.",
                map.len()
            );
        }
        // Reject servers that are not connected, rather than handing back an
        // agent whose toolset is quietly smaller than was asked for.
        let live: Vec<String> = self.registry.servers();
        if let Some(bad) = spec.servers.iter().find(|s| !live.contains(s)) {
            bail!(
                "no connected server called `{bad}`. Connected right now: {}",
                if live.is_empty() { "none".to_string() } else { live.join(", ") }
            );
        }
        if spec.name.trim().is_empty() {
            bail!("a sub-agent needs a name");
        }

        let id = {
            let mut n = self.next.lock().unwrap();
            let id = format!("a{n}");
            *n += 1;
            id
        };
        let info = AgentInfo {
            id: id.clone(),
            name: spec.name.clone(),
            purpose: spec.purpose.clone(),
            session,
            servers: spec.servers.clone(),
            status: Status::Idle,
            last: String::new(),
            turns: 0,
            tools: 0,
            queued: 0,
            question: None,
            sleep_after_mins: spec.sleep_after_mins,
            end_after_mins: spec.end_after_mins,
            idle_secs: 0,
            browser: spec.browser.clone(),
            model: spec.model.clone(),
            template: spec.template.clone(),
            idle_since: Some(Instant::now()),
        };
        let shared = Arc::new(Mutex::new(info.clone()));
        let (tx, rx) = mpsc::unbounded_channel::<Msg>();
        let bus = broadcast::channel(512).0;
        let worker = Worker {
            cfg: self.cfg.clone(),
            registry: self.registry.clone(),
            vision: self.vision.clone(),
            hub: self.hub.clone(),
            id: id.clone(),
            spec,
            info: shared.clone(),
            bus: bus.clone(),
        };
        tokio::spawn(worker.run(rx));
        map.insert(id, Entry { info: shared, tx, bus });
        // `spawned` tells the main window to open this agent's window.
        let _ = self.hub.events.send(json!({ "type": "spawned", "agent": &info }));
        Ok(info)
    }

    fn send(&self, id: &str, prompt: &str, from: From) -> Result<oneshot::Receiver<Result<String, String>>> {
        let map = self.agents.lock().unwrap();
        let Some(entry) = map.get(id) else {
            bail!("no sub-agent `{id}`. `list_agents` shows which exist.");
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut i = entry.info.lock().unwrap();
            i.queued += 1;
            self.hub.publish(&i);
        }
        entry
            .tx
            .send(Msg::Job(Job { prompt: prompt.to_string(), from, reply_to: Some(tx) }))
            .map_err(|_| anyhow::anyhow!("sub-agent `{id}` is no longer running"))?;
        Ok(rx)
    }

    /// Give an agent a turn and wait up to `wait_ms` for the answer.
    ///
    /// `Ok(Some(reply))` - it finished in time, one round trip. `Ok(None)` - it
    /// is still working; the answer will be delivered when it lands (to bluee's
    /// inbox if bluee asked). `wait_ms == 0` is a pure background ask.
    pub async fn ask(&self, id: &str, prompt: &str, from: From, wait_ms: u64) -> Result<Option<String>> {
        let rx = self.send(id, prompt, from)?;
        if wait_ms == 0 {
            // Dropping the receiver is what routes the answer to the inbox.
            drop(rx);
            return Ok(None);
        }
        match tokio::time::timeout(std::time::Duration::from_millis(wait_ms), rx).await {
            Ok(Ok(Ok(reply))) => Ok(Some(reply)),
            Ok(Ok(Err(e))) => bail!("sub-agent `{id}` failed: {e}"),
            Ok(Err(_)) => bail!("sub-agent `{id}` stopped before it answered"),
            Err(_) => Ok(None),
        }
    }

    /// Adithya answering an `ask_user` question.
    pub fn answer(&self, id: &str, text: &str) -> Result<()> {
        match self.hub.answers.lock().unwrap().remove(id) {
            Some(tx) => {
                let _ = tx.send(text.to_string());
                Ok(())
            }
            None => bail!("sub-agent `{id}` is not waiting on a question"),
        }
    }

    /// Set the idle timers. `None` = never.
    pub fn set_timers(&self, id: &str, sleep: Option<u64>, end: Option<u64>) -> Result<AgentInfo> {
        let map = self.agents.lock().unwrap();
        let Some(entry) = map.get(id) else { bail!("no sub-agent `{id}`") };
        let mut i = entry.info.lock().unwrap();
        i.sleep_after_mins = sleep;
        i.end_after_mins = end;
        self.hub.publish(&i);
        Ok(i.clone())
    }

    /// Put an idle agent to sleep now.
    pub fn sleep(&self, id: &str) -> Result<()> {
        let map = self.agents.lock().unwrap();
        let Some(entry) = map.get(id) else { bail!("no sub-agent `{id}`") };
        if entry.info.lock().unwrap().busy() {
            bail!("`{id}` is working - it can sleep once it is idle");
        }
        let _ = entry.tx.send(Msg::Sleep);
        Ok(())
    }

    /// Apply the idle timers. Returns (slept, ended) ids. Busy agents are
    /// never touched, whether or not their window is open.
    pub fn sweep(&self) -> (Vec<String>, Vec<String>) {
        let mut map = self.agents.lock().unwrap();
        let (mut slept, mut ended) = (Vec::new(), Vec::new());
        map.retain(|id, e| {
            let i = e.info.lock().unwrap();
            if i.busy() {
                return true;
            }
            let idle = i.idle_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            if i.end_after_mins.is_some_and(|m| idle >= m * 60) {
                ended.push(id.clone());
                return false;
            }
            if i.status != Status::Sleeping && i.sleep_after_mins.is_some_and(|m| idle >= m * 60) {
                let _ = e.tx.send(Msg::Sleep);
                slept.push(id.clone());
            }
            true
        });
        (slept, ended)
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        let map = self.agents.lock().unwrap();
        map.values()
            .map(|e| {
                let mut i = e.info.lock().unwrap().clone();
                i.idle_secs = if i.busy() {
                    0
                } else {
                    i.idle_since.map(|t| t.elapsed().as_secs()).unwrap_or(0)
                };
                i
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<AgentInfo> {
        self.list().into_iter().find(|i| i.id == id)
    }

    /// Live feed for one agent's window.
    pub fn subscribe(&self, id: &str) -> Option<broadcast::Receiver<Value>> {
        self.agents.lock().unwrap().get(id).map(|e| e.bus.subscribe())
    }

    /// End one. Its transcript stays - the conversation happened (§4a).
    pub fn stop(&self, id: &str) -> Result<()> {
        if self.agents.lock().unwrap().remove(id).is_none() {
            bail!("no sub-agent `{id}`");
        }
        // A question it was waiting on can never be answered now.
        self.hub.answers.lock().unwrap().remove(id);
        Ok(())
    }

    /// Accept an id or a name, since the model often uses the name it chose.
    fn resolve(&self, key: &str) -> String {
        let map = self.agents.lock().unwrap();
        if map.contains_key(key) {
            return key.to_string();
        }
        map.iter()
            .find(|(_, e)| e.info.lock().unwrap().name == key)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| key.to_string())
    }

    /// Tools offered to the main agent. Sub-agents never get these - a
    /// sub-agent that can spawn sub-agents is a fork bomb with a credit card.
    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "spawn_agent".into(),
                description:
                    "Start a sub-agent: another assistant with its own conversation, its own window \
                     Adithya can talk to it in, and its own tools. Use one for a self-contained job, \
                     for work that needs a big toolset you do not want to carry (GUI, browsing), or \
                     to run several jobs in parallel - spawn several, each with a task. If you give \
                     a `task` it starts working at once, in the background, and its result is \
                     delivered to you automatically when it finishes."
                        .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Short handle, e.g. `gui` or `researcher`." },
                        "purpose": { "type": "string", "description": "One line on what it is for. Shown in its window." },
                        "servers": {
                            "type": "array", "items": { "type": "string" },
                            "description": "MCP servers it may use, e.g. [\"uacc\"]. Empty means native tools \
                                and memory only. Required - an unscoped sub-agent pays for every tool schema on \
                                every turn."
                        },
                        "task": { "type": "string", "description": "Optional first job. Runs in the background; the answer comes to you when it lands." }
                    },
                    "required": ["name", "purpose", "servers"]
                }),
            },
            ToolDef {
                name: "ask_agent".into(),
                description:
                    "Give a sub-agent a job (by id or name). By default waits up to 45 seconds and returns \
                     the answer inline. If it is not done by then - or you set background: true - you get \
                     `still_working` and the answer is delivered to you automatically when it lands, so \
                     do NOT poll list_agents for it: get on with something else or finish your reply. \
                     It keeps its own conversation, so follow-ups remember what it already did."
                        .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "From spawn_agent or list_agents; the name works too." },
                        "prompt": { "type": "string" },
                        "background": { "type": "boolean", "description": "Return at once; the answer is delivered later." },
                        "wait_seconds": { "type": "integer", "description": "How long to wait inline (max 120)." }
                    },
                    "required": ["id", "prompt"]
                }),
            },
            ToolDef {
                name: "list_agents".into(),
                description:
                    "Which sub-agents exist, what each is for, what it can reach, whether it is working, \
                     sleeping or waiting on Adithya, and what it last said."
                        .into(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            ToolDef {
                name: "stop_agent".into(),
                description:
                    "End a sub-agent and close its window. Its transcript is kept and can be resumed from \
                     Sessions."
                        .into(),
                parameters: json!({
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

    /// Run one of the four, on behalf of the main agent.
    pub async fn call(&self, name: &str, args: &Value) -> Result<Value> {
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "spawn_agent" => {
                let servers: Vec<String> = args
                    .get("servers")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let info = self.spawn(&s("name"), &s("purpose"), servers)?;
                let task = s("task");
                if !task.trim().is_empty() {
                    self.ask(&info.id, &task, From::Parent, 0).await?;
                }
                Ok(json!({
                    "id": info.id,
                    "name": info.name,
                    "window": "opened for Adithya",
                    "note": if task.trim().is_empty() {
                        "Started and idle. Give it work with ask_agent."
                    } else {
                        "Started and working in the background. Its answer will be delivered to you when it lands - do not poll."
                    }
                }))
            }
            "ask_agent" => {
                let id = self.resolve(&s("id"));
                let background = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
                let wait = if background {
                    0
                } else {
                    args.get("wait_seconds").and_then(|v| v.as_u64()).unwrap_or(45).min(120) * 1000
                };
                match self.ask(&id, &s("prompt"), From::Parent, wait).await? {
                    Some(reply) => Ok(json!({ "id": id, "reply": reply })),
                    None => Ok(json!({
                        "id": id,
                        "still_working": true,
                        "note": "Still going. Its answer will be delivered to you automatically when it \
                                 lands - do not poll list_agents. Carry on or finish your reply."
                    })),
                }
            }
            "list_agents" => Ok(json!({ "agents": self.list() })),
            "stop_agent" => {
                let id = self.resolve(&s("id"));
                self.stop(&id)?;
                Ok(json!({ "stopped": id }))
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
            assert!(!ChildLink::handles(n), "a child must never get parent tools");
        }
        assert!(!SubAgents::is_tool("search_memory"));
        for d in ChildLink::defs() {
            assert!(ChildLink::handles(&d.name));
            assert!(!SubAgents::is_tool(&d.name));
        }
    }

    #[test]
    fn servers_is_required_on_spawn() {
        let def = SubAgents::defs().into_iter().find(|d| d.name == "spawn_agent").unwrap();
        let req = def.parameters["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "servers"), "servers must be required");
    }

    #[test]
    fn ask_tells_the_model_not_to_poll() {
        // If this is lost the model treats a queued job as finished, or burns
        // a whole turn of tool schemas asking "done yet?".
        let def = SubAgents::defs().into_iter().find(|d| d.name == "ask_agent").unwrap();
        assert!(def.description.contains("delivered to you automatically"));
        assert!(def.description.contains("do NOT poll"));
    }

    #[test]
    fn busy_means_running_waiting_or_queued() {
        let mut i = AgentInfo {
            id: "a1".into(), name: "x".into(), purpose: String::new(), session: String::new(),
            servers: vec![], status: Status::Ready, last: String::new(), turns: 0, tools: 0,
            queued: 0, question: None, sleep_after_mins: None, end_after_mins: None,
            idle_secs: 0, browser: None, model: None, template: None, idle_since: None,
        };
        assert!(!i.busy());
        i.queued = 1;
        assert!(i.busy(), "a queued job is work given");
        i.queued = 0;
        i.status = Status::Waiting;
        assert!(i.busy(), "waiting on Adithya is not idle");
        i.status = Status::Sleeping;
        assert!(!i.busy());
    }

    #[test]
    fn inbox_drains_once_and_says_who_spoke() {
        let hub = Hub::new();
        assert!(hub.drain().is_none());
        hub.push(Note { agent: "a1".into(), name: "grapher".into(), kind: "done".into(), text: "42".into(), wake: true });
        hub.push(Note { agent: "a2".into(), name: "web".into(), kind: "talk".into(), text: "He said: hi".into(), wake: false });
        assert_eq!(hub.pending(), 2);
        let block = hub.drain().unwrap();
        assert!(block.contains("grapher (a1) finished"));
        assert!(block.contains("Adithya talked to sub-agent web"));
        assert!(hub.drain().is_none(), "delivered once");
    }
}
