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

/// All connected MCP servers and their tools.
pub struct McpRegistry {
    connections: BTreeMap<String, Connection>,
}

impl McpRegistry {
    /// Launch and handshake every enabled server.
    ///
    /// A server that fails to start is reported and skipped rather than
    /// aborting startup - one broken tool server should not take the assistant
    /// down with it.
    pub async fn connect(specs: &BTreeMap<String, ServerSpec>) -> (Self, Vec<String>) {
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

        (Self { connections }, failures)
    }

    async fn connect_one(name: &str, spec: &ServerSpec) -> Result<Connection> {
        let mut cmd = tokio::process::Command::new(&spec.command);
        cmd.args(&spec.args);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("spawning `{}`", spec.command))?;

        let service = ()
            .serve(transport)
            .await
            .with_context(|| format!("MCP handshake with {name}"))?;

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

    pub fn servers(&self) -> Vec<&str> {
        self.connections.keys().map(|s| s.as_str()).collect()
    }

    pub fn tools(&self) -> Vec<&ToolInfo> {
        self.connections
            .values()
            .flat_map(|c| c.tools.iter())
            .collect()
    }

    /// Resolve a namespaced id (`server__tool`) back to its parts.
    ///
    /// Returns owned strings: the slices would borrow from `qualified`, not
    /// from `self`, and callers generally outlive the id they passed in.
    pub fn resolve(&self, qualified: &str) -> Option<(String, String)> {
        let (server, tool) = qualified.split_once(NS)?;
        if self.connections.contains_key(server) {
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
        let conn = self
            .connections
            .get(server)
            .with_context(|| format!("no connected MCP server named `{server}`"))?;

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

        let result = conn
            .service
            .call_tool(params)
            .await
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
        for (_, conn) in self.connections {
            let _ = conn.service.cancel().await;
        }
    }
}
