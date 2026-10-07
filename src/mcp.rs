//! MCP client (§2) - the harness connects to tool servers over stdio.
//!
//! This is the seam that keeps the architecture language-agnostic: UACC,
//! SnareVec and the kùzu graph server are all Python, the harness is Rust, and
//! neither side cares. It is also what makes the core swappable later without
//! touching any tool server.

use anyhow::{Context, Result};
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::path::Path;

/// Separator between server name and tool name in the LLM-facing tool id.
///
/// Namespacing is not cosmetic: UACC alone exposes 68 tools, and once several
/// servers are connected, bare tool names will collide. Double underscore keeps
/// the id inside the `^[a-zA-Z0-9_-]+$` charset providers require.
const NS: &str = "__";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Anything else in the entry - notably the `$comment` keys explaining why
    /// a server is set up the way it is. Captured so saving from the UI cannot
    /// silently delete documentation the next person needs.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize)]
struct ServersFile {
    servers: BTreeMap<String, ServerSpec>,
    #[serde(flatten, default)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// Write the server list back, preserving every key we do not model.
pub fn save_server_specs(
    path: impl AsRef<Path>,
    mut servers: BTreeMap<String, ServerSpec>,
) -> Result<()> {
    let path = path.as_ref();
    let mut file: ServersFile = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(ServersFile {
            servers: BTreeMap::new(),
            extra: BTreeMap::new(),
        });

    // Carry forward per-server keys the caller never saw. The UI is only shown
    // the fields it can edit, so it cannot send `$comment` back - if we relied
    // on it round-tripping them, pressing Save would quietly delete the notes
    // explaining why each server is configured the way it is. Merging here
    // means the client never has to know they exist.
    for (name, incoming) in servers.iter_mut() {
        if let Some(previous) = file.servers.get(name) {
            for (k, v) in &previous.extra {
                incoming.extra.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    file.servers = servers;
    std::fs::write(path, serde_json::to_string_pretty(&file)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn load_server_specs(path: impl AsRef<Path>) -> Result<BTreeMap<String, ServerSpec>> {
    let path = path.as_ref();
    // servers.json is local machine config - it holds absolute paths to clones
    // that live wherever you put them - so it is gitignored. On a fresh clone
    // it is absent; seed it from the committed example rather than failing.
    // Copied, not read in place, so the Settings -> MCP page has a file to write.
    if !path.exists() {
        let example = path.with_file_name("servers.example.json");
        if example.exists() {
            std::fs::copy(&example, path).with_context(|| {
                format!("seeding {} from {}", path.display(), example.display())
            })?;
            eprintln!(
                "note: created {} from servers.example.json - edit it to point at your own clones",
                path.display()
            );
        }
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading MCP server config {}", path.display()))?;
    let parsed: ServersFile = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed.servers)
}

/// One tool, as advertised by one server.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

impl ToolInfo {
    /// The namespaced id the model sees and calls back with.
    pub fn qualified(&self) -> String {
        format!("{}{NS}{}", self.server, self.name)
    }
}

struct Connection {
    service: RunningService<RoleClient, ()>,
    tools: Vec<ToolInfo>,
}

/// Everything that changes when servers are (re)connected, swapped as one unit.
struct Inner {
    connections: BTreeMap<String, Connection>,
    /// Why each enabled server is NOT here, kept rather than printed once.
    ///
    /// These used to go to stderr at startup and nowhere else, which is fine
    /// for `harness dash` in a terminal and useless for `harness app`, where
    /// there is no console at all. A desktop window that has silently lost
    /// every tool server looks exactly like one that has an empty graph - and
    /// that is precisely how it was read (§55).
    failures: Vec<String>,
}

/// All connected MCP servers and their tools.
pub struct McpRegistry {
    /// Connections are replaced wholesale by `reconnect`, so every
    /// `Arc<McpRegistry>` already handed out - the live agent, each sub-agent,
    /// the loop scheduler - picks up the new servers without being rebuilt.
    /// An `Arc` snapshot is taken for each call and the guard released
    /// immediately, so no lock is ever held across an await.
    inner: std::sync::RwLock<Arc<Inner>>,
}

impl McpRegistry {
    /// Launch and handshake every enabled server.
    ///
    /// A server that fails to start is reported and skipped rather than
    /// aborting startup - one broken tool server should not take the assistant
    /// down with it.
    pub async fn connect(specs: &BTreeMap<String, ServerSpec>) -> (Self, Vec<String>) {
        let inner = Self::connect_all(specs).await;
        let failures = inner.failures.clone();
        (
            Self {
                inner: std::sync::RwLock::new(Arc::new(inner)),
            },
            failures,
        )
    }

    async fn connect_all(specs: &BTreeMap<String, ServerSpec>) -> Inner {
        let mut connections = BTreeMap::new();
        let mut failures = Vec::new();

        for (name, spec) in specs {
            if !spec.enabled {
                continue;
            }
            match Self::connect_one(name, spec).await {
                Ok(conn) => {
                    connections.insert(name.clone(), conn);
                }
                Err(e) => failures.push(format!("{name}: {e:#}")),
            }
        }

        Inner {
            connections,
            failures,
        }
    }

    /// Start every enabled server again and swap the result in.
    ///
    /// Restarting the whole app was the only cure for a startup where the
    /// servers did not come up, which loses the conversation you were in the
    /// middle of. Returns the failures, so the caller can say what is still
    /// wrong rather than just "done".
    pub async fn reconnect(&self, specs: &BTreeMap<String, ServerSpec>) -> Vec<String> {
        let fresh = Self::connect_all(specs).await;
        let failures = fresh.failures.clone();
        let old = {
            let mut guard = self.inner.write().unwrap();
            std::mem::replace(&mut *guard, Arc::new(fresh))
        };
        // Only shut down the previous set if nothing else is still holding it;
        // a tool call in flight owns its own snapshot and must be allowed to
        // finish. Anything not cancelled here dies with its handle instead.
        if let Ok(old) = Arc::try_unwrap(old) {
            for (_, conn) in old.connections {
                let _ = conn.service.cancel().await;
            }
        }
        failures
    }

    async fn connect_one(name: &str, spec: &ServerSpec) -> Result<Connection> {
        let mut cmd = tokio::process::Command::new(&spec.command);
        cmd.args(&spec.args);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        // The child's own stderr is captured, not inherited. Twice (CLAUDE.md
        // §55, §58) every server died with nothing but "connection closed:
        // initialize response", while each child had printed the real reason -
        // `No Python at '...'` - to a stream nobody could see from the desktop
        // app. Lines are still forwarded to our stderr, so a terminal run looks
        // the same; the last few are also kept for the failure message.
        let (transport, stderr) = TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning `{}`", spec.command))?;
        let tail = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<String>::new()));
        if let Some(err) = stderr {
            let tail = tail.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{line}");
                    let mut t = tail.lock().unwrap();
                    t.push_back(line);
                    if t.len() > 12 {
                        t.pop_front();
                    }
                }
            });
        }

        let service = match ().serve(transport).await {
            Ok(s) => s,
            Err(e) => {
                // Let the dying child's last words arrive before reading them.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let said: Vec<String> = tail
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                let mut why = if said.is_empty() {
                    String::new()
                } else {
                    format!(" - the server said: {}", said.join(" | "))
                };
                if said.iter().any(|l| l.contains("No Python at")) {
                    why.push_str(
                        " (the venv's base Python is not visible to this process - usually \
                         because it was installed from inside a packaged app such as Claude, \
                         which redirects AppData. Point the venv's pyvenv.cfg `home` at a \
                         Python outside AppData; USER_GUIDE.md, 'When something goes wrong'.)",
                    );
                }
                return Err(anyhow::anyhow!(e)).with_context(|| format!("MCP handshake with {name}{why}"));
            }
        };

        let tools = service
            .list_all_tools()
            .await
            .with_context(|| format!("listing tools from {name}"))?
            .into_iter()
            .map(|t| ToolInfo {
                server: name.to_string(),
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                schema: serde_json::Value::Object((*t.input_schema).clone()),
            })
            .collect();

        Ok(Connection { service, tools })
    }

    /// The current set, borrowed for as short a time as possible.
    fn snapshot(&self) -> Arc<Inner> {
        self.inner.read().unwrap().clone()
    }

    pub fn servers(&self) -> Vec<String> {
        self.snapshot().connections.keys().cloned().collect()
    }

    pub fn tools(&self) -> Vec<ToolInfo> {
        self.snapshot()
            .connections
            .values()
            .flat_map(|c| c.tools.iter().cloned())
            .collect()
    }

    /// Why each enabled server that is not connected failed to start.
    pub fn failures(&self) -> Vec<String> {
        self.snapshot().failures.clone()
    }

    /// Resolve a namespaced id (`server__tool`) back to its parts.
    ///
    /// Returns owned strings: the slices would borrow from `qualified`, not
    /// from `self`, and callers generally outlive the id they passed in.
    pub fn resolve(&self, qualified: &str) -> Option<(String, String)> {
        let (server, tool) = qualified.split_once(NS)?;
        if self.snapshot().connections.contains_key(server) {
            Some((server.to_string(), tool.to_string()))
        } else {
            None
        }
    }

    /// Call a tool and return its result as JSON.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Snapshot first: the read guard is dropped here, before any await, so
        // a slow tool cannot block a reconnect and a reconnect cannot cut off
        // a call that is already running.
        let inner = self.snapshot();
        let conn = inner.connections.get(server).with_context(|| {
            let live = inner
                .connections
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if live.is_empty() {
                format!(
                    "no MCP server is connected, so `{server}` is unreachable. \
                     Settings -> MCP has the reason each one failed, and a Reconnect button."
                )
            } else {
                format!("no connected MCP server named `{server}` (connected: {live})")
            }
        })?;

        let arguments = match args {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => anyhow::bail!("tool arguments must be a JSON object, got: {other}"),
        };

        // CallToolRequestParams is #[non_exhaustive] - build it via its builder
        // rather than a struct literal.
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }

        // Bounded. Measured: `uacc__get_screen_info {include_ocr: true}` with
        // pytesseract missing fell through to EasyOCR and never returned - and
        // with no bound, one hung tool held the agent (and every surface that
        // shares it) for good. The provider already had a per-request timeout;
        // tools had none. HARNESS_TOOL_TIMEOUT overrides, in seconds.
        let limit = std::env::var("HARNESS_TOOL_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(limit),
            conn.service.call_tool(params),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "{server}.{tool} did not answer within {limit}s and was abandoned. The server \
                 may still be busy with it - do not repeat the same call; try a different \
                 approach or a lighter variant."
            )
        })?
        .with_context(|| format!("calling {server}.{tool}"))?;

        // Prefer the server's structured output when it provides one; fall back
        // to concatenated text content, which is what most servers return.
        if let Some(structured) = result.structured_content {
            return Ok(structured);
        }

        let text: String = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");

        // Tool results are usually JSON-as-text; hand back real JSON when we can
        // so callers (and the event log) get structure rather than a blob.
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
    }

    /// Shut every server down cleanly, so child processes don't leak.
    pub async fn shutdown(self) {
        let inner = self.inner.into_inner().unwrap();
        if let Ok(inner) = Arc::try_unwrap(inner) {
            for (_, conn) in inner.connections {
                let _ = conn.service.cancel().await;
            }
        }
    }
}
