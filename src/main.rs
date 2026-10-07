mod agent;
mod artifacts;
mod browser;
mod app;
mod codemap;
mod config;
mod dash;
mod eventlog;
mod facts;
mod gui;
mod hooks;
mod llm;
mod loops;
mod mcp;
mod memory;
mod providers;
mod pty;
mod reduce;
mod roots;
mod skills;
mod subagents;
mod system;
mod tools;
mod toolsearch;
mod vision;
mod voice;
use anyhow::Result;
use config::Config;
use eventlog::{EventKind, EventLog};
use llm::{OpenAiCompatible, Provider};
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, BufReader};

const USAGE: &str = "usage:
  harness chat                     interactive turn loop
  harness models                   list model ids the provider offers
  harness log [id]                 replay a session's event log
  harness tools                    connect MCP servers and list their tools
  harness call <server__tool> [json]  call one MCP tool directly
  harness reduce                   rebuild vector + graph memory from the event log
  harness search <query>           semantic search over memory
  harness loops                   list scheduled loops and when each last ran
  harness loop <name>             run one loop now, ignoring its schedule
  harness dash [port]              open the dashboard in a browser
  harness app                      open the desktop app (Tauri window)";

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::load()?;
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "chat".into());

    match cmd.as_str() {
        "chat" => chat(cfg).await,
        "models" => models(cfg).await,
        "log" => replay(cfg, std::env::args().nth(2)),
        "tools" => tools(cfg).await,
        "reduce" => reduce_cmd(cfg).await,
        "loops" => loops_cmd(cfg),
        "loop" => {
            let Some(name) = std::env::args().nth(2) else {
                eprintln!("loop needs a name - `harness loops` lists them

{USAGE}");
                std::process::exit(2);
            };
            loop_run_cmd(cfg, name).await
        }
        "app" => tokio::task::block_in_place(|| app::run(cfg)),
        "dash" => {
            let port = std::env::args()
                .nth(2)
                .and_then(|p| p.parse().ok())
                .unwrap_or(7777);
            dash::serve(cfg, port).await
        }
        "search" => {
            let query: Vec<String> = std::env::args().skip(2).collect();
            if query.is_empty() {
                eprintln!("search needs a query\n\n{USAGE}");
                std::process::exit(2);
            }
            search_cmd(cfg, query.join(" "))
        }
        "call" => {
            let Some(qualified) = std::env::args().nth(2) else {
                eprintln!("call needs a tool id, e.g. kuzu_graph__graph_stats\n\n{USAGE}");
                std::process::exit(2);
            };
            call(cfg, qualified, std::env::args().nth(3)).await
        }
        other => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Rebuild the derived memory layers from the event log (§4a).
async fn reduce_cmd(cfg: Config) -> Result<()> {
    println!("reducing event log -> vector + graph ...\n");
    let stats = reduce::run(&cfg).await?;

    println!("sessions read : {}", stats.sessions);
    println!("events read   : {}", stats.events);
    println!("chunks embedded: {}", stats.chunks);
    println!(
        "source indexed: {} file(s), {} chunk(s), {} symbol(s)",
        stats.code_files, stats.code_chunks, stats.code_symbols
    );
    if stats.graph_skipped {
        println!("graph         : SKIPPED (server unavailable)");
    } else {
        println!("entities      : {}", stats.entities);
        println!("relations     : {}", stats.relations);
        println!(
            "facts         : {} ({} current, {} history)",
            stats.facts,
            stats.facts - stats.facts_closed,
            stats.facts_closed
        );
    }

    if stats.events == 0 {
        println!("\n(no events yet - the log fills up as you use the harness)");
    }
    Ok(())
}

/// Semantic search over memory. Same path the `search_memory` tool will use
/// once the model is wired up.
fn search_cmd(cfg: Config, query: String) -> Result<()> {
    use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

    let store = memory::VectorStore::open(cfg.data_dir.join("vectors.db"))?;
    let total = store.count()?;
    if total == 0 {
        println!("memory is empty - run `harness reduce` first");
        return Ok(());
    }

    let mut model = TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::AllMiniLML6V2))?;
    let embedding = model.embed(vec![query.as_str()], None)?;
    let hits = store.search_hybrid(&query, &embedding[0], 5)?;

    println!("searching {total} chunk(s) for: {query}\n");
    for hit in hits {
        println!(
            "[{:.3}] {} seq {}-{}",
            hit.score, hit.chunk.session_id, hit.chunk.seq_start, hit.chunk.seq_end
        );
        for line in hit.chunk.text.lines().take(4) {
            println!("        {line}");
        }
        println!();
    }
    Ok(())
}

/// Connect every enabled MCP server and show what they expose. This is the
/// harness's view of its own toolset - the same list the model will be given.
async fn tools(cfg: Config) -> Result<()> {
    let specs = mcp::load_server_specs(&cfg.mcp_config)?;
    let (registry, failures) = mcp::McpRegistry::connect(&specs).await;

    for f in &failures {
        eprintln!("[warn] server failed to start - {f}");
    }

    let all = registry.tools();
    let mut by_server: BTreeMap<&str, Vec<&mcp::ToolInfo>> = BTreeMap::new();
    for t in &all {
        by_server.entry(t.server.as_str()).or_default().push(t);
    }

    println!(
        "{} server(s) connected, {} tool(s) available\n",
        registry.servers().len(),
        all.len()
    );
    for (server, tools) in by_server {
        println!("{server}");
        for t in tools {
            let desc = t.description.lines().next().unwrap_or("");
            println!("  {:<34} {}", t.qualified(), desc);
        }
        println!();
    }

    registry.shutdown().await;
    Ok(())
}

/// Call one MCP tool directly, bypassing the model.
///
/// This exists to prove the tool path independently of the LLM path - Phase 0's
/// "confirm at least one tool round-trips correctly" - and stays useful for
/// debugging a tool server without burning tokens. The call is written to the
/// event log exactly as a model-driven call would be, so the log stays a
/// complete record of what touched the system (§4a).
async fn call(cfg: Config, qualified: String, args_json: Option<String>) -> Result<()> {
    use anyhow::Context;
    let args: serde_json::Value = match args_json {
        Some(raw) => serde_json::from_str(&raw)
            .with_context(|| format!("arguments must be valid JSON, got: {raw}"))?,
        None => serde_json::json!({}),
    };

    let specs = mcp::load_server_specs(&cfg.mcp_config)?;
    let (registry, failures) = mcp::McpRegistry::connect(&specs).await;
    for f in &failures {
        eprintln!("[warn] server failed to start - {f}");
    }

    // The composed GUI tools (§ src/gui.rs) are not MCP tools - they sit on top
    // of UACC - so `harness call` has to know about them too. Without this the
    // only way to exercise them is a real model turn, which costs money and
    // makes a failure harder to attribute.
    if gui::handles(&qualified) {
        let out = gui::call(&registry, &qualified, &args).await;
        match &out {
            Ok(v) => println!("{}", serde_json::to_string_pretty(v)?),
            Err(e) => eprintln!("failed: {e:#}"),
        }
        registry.shutdown().await;
        return out.map(|_| ());
    }

    let Some((server, tool)) = registry.resolve(&qualified) else {
        let known: Vec<String> = registry.tools().iter().map(|t| t.qualified()).collect();
        registry.shutdown().await;
        anyhow::bail!(
            "unknown tool `{qualified}`.\navailable:\n  {}",
            known.join("\n  ")
        );
    };

    let mut log = EventLog::new_session(cfg.events_dir())?;
    let call_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    log.append(EventKind::ToolCall {
        call_id: call_id.clone(),
        server: server.clone(),
        tool: tool.clone(),
        args: args.clone(),
    })?;

    let outcome = registry.call(&server, &tool, args).await;

    match &outcome {
        Ok(value) => {
            log.append(EventKind::ToolResult {
                call_id,
                ok: true,
                result: value.clone(),
            })?;
            println!("{}", serde_json::to_string_pretty(value)?);
        }
        Err(e) => {
            let message = format!("{e:#}");
            log.append(EventKind::ToolResult {
                call_id,
                ok: false,
                result: serde_json::json!({ "error": message }),
            })?;
            eprintln!("[error] {message}");
        }
    }

    println!("\nlogged to {}", log.path().display());
    registry.shutdown().await;
    outcome.map(|_| ())
}

/// Ask the provider what it actually offers. Exists so LLM_MODEL is copied from
/// the provider rather than guessed - model ids are not guessable.
async fn models(cfg: Config) -> Result<()> {
    if cfg.api_key.is_empty() {
        anyhow::bail!(
            "LLM_API_KEY is not set. Copy .env.example to .env and add the key.\n\
             (base url: {})",
            cfg.base_url
        );
    }

    let provider = OpenAiCompatible::new(&cfg.base_url, &cfg.api_key, "", cfg.max_tokens);
    let mut ids = provider.list_models().await?;
    ids.sort();

    println!("{} offers {} model(s):\n", cfg.base_url, ids.len());
    for id in &ids {
        println!("  {id}");
    }
    println!("\nCopy the one you want into LLM_MODEL in .env");
    Ok(())
}

/// Replay a session straight off disk - the §4a transparency window in the
/// terminal, before the dashboard (§2) exists to show the same thing.
fn replay(cfg: Config, which: Option<String>) -> Result<()> {
    let dir = cfg.events_dir();
    let path = match which {
        Some(id) => dir.join(format!("{id}.jsonl")),
        None => EventLog::list_sessions(&dir)?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no sessions in {}", dir.display()))?,
    };

    println!("{}\n", path.display());
    for e in EventLog::read(&path)? {
        let ts = e.ts.format("%H:%M:%S");
        match e.kind {
            EventKind::SessionStart { model, persona_files } => println!(
                "{:>3} [{ts}] session start | model={model} persona={}",
                e.seq,
                if persona_files.is_empty() {
                    "(none)".into()
                } else {
                    persona_files.join(",")
                }
            ),
            EventKind::UserMessage { text } => println!("{:>3} [{ts}] user      | {text}", e.seq),
            EventKind::AssistantMessage { text } => {
                println!("{:>3} [{ts}] assistant | {text}", e.seq)
            }
            EventKind::ToolCall { server, tool, args, .. } => {
                println!("{:>3} [{ts}] tool call | {server}.{tool} {args}", e.seq)
            }
            EventKind::ToolResult { ok, result, .. } => {
                println!("{:>3} [{ts}] tool res  | ok={ok} {result}", e.seq)
            }
            EventKind::ScreenContext { mode, text } => {
                println!("{:>3} [{ts}] screen    | ({mode}) {text}", e.seq)
            }
            EventKind::System { note } => println!("{:>3} [{ts}] system    | {note}", e.seq),
            EventKind::Error { context, message } => {
                println!("{:>3} [{ts}] error     | {context}: {message}", e.seq)
            }
            EventKind::SessionTitle { title } => {
                println!("{:>3} [{ts}] titled    | {title}", e.seq)
            }
            EventKind::SessionEnd { reason } => {
                println!("{:>3} [{ts}] session end ({reason})", e.seq)
            }
        }
    }
    Ok(())
}

/// The turn loop, with tools.
///
/// It drives `Agent` - the same loop the dashboard websocket, loops (§36) and
/// sub-agents (§42) drive - rather than a second copy of it. That was the
/// stated intent when `Agent` was extracted in Phase 2 and the CLI was never
/// actually moved onto it: `chat` went on building an `OpenAiCompatible`
/// straight from `.env` and running its own 180-line tool loop.
///
/// The cost of that was not theoretical. It meant `harness chat` ignored the
/// provider chain entirely - so with OpenRouter configured, enabled and
/// answering 200 to a direct call, the CLI still failed with
/// `402 Insufficient Balance` from the old `.env` endpoint, and the obvious
/// reading of that was "the new provider is broken". It also missed failover
/// logging (§51), workspace tool scoping (§25) and the vision gate (§6),
/// because all three live in `Agent`.
async fn chat(cfg: Config) -> Result<()> {
    // No `require_credentials` here: credentials now come from the provider
    // chain, which may be configured entirely in the Providers page with
    // nothing in `.env` at all. `ProviderChain::build` says so properly when
    // there is genuinely no usable provider.
    let specs = mcp::load_server_specs(&cfg.mcp_config)?;
    let (registry, failures) = mcp::McpRegistry::connect(&specs).await;
    for f in &failures {
        eprintln!("[warn] server failed to start - {f}");
    }
    let registry = std::sync::Arc::new(registry);
    let servers = registry.servers().len();
    let vision = std::sync::Arc::new(vision::VisionState::load(&cfg.data_dir));

    let mut ag = agent::Agent::new(&cfg, registry.clone(), vision).await?;

    // Same scoping mechanism as the workspace tick-boxes (§25) and the vision
    // gate (§6) - the cost lever of §12f, reached from the environment here
    // because the CLI has no UI to tick.
    if let Ok(list) = std::env::var("HARNESS_TOOL_SERVERS") {
        ag.set_allowed_servers(Some(list.split(',').map(|s| s.trim().to_string()).collect()));
    }

    println!("provider: {} ({})", ag.provider_name, ag.model);
    println!("session:  {}", ag.session_id());
    println!(
        "persona:  {}",
        if ag.persona_files.is_empty() {
            "(none)".to_string()
        } else {
            ag.persona_files.join(", ")
        }
    );
    println!(
        "tools:    {} ({} native + {} from {servers} server(s)) | {} provider(s) in the chain",
        ag.tool_count(),
        ag.native_count,
        ag.tool_count() - ag.native_count,
        ag.provider_count()
    );
    println!();
    println!("Type a message, or /quit to exit.");
    println!();

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("> ");
        use std::io::Write;
        std::io::stdout().flush().ok();

        let Some(line) = lines.next_line().await? else {
            break;
        };
        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" || input == "/exit" {
            break;
        }

        // Every one of these is already in the event log by the time it gets
        // here; printing them is the CLI's rendering of the same trace the
        // dashboard streams as JSON.
        for ev in ag.turn(&input).await {
            match ev {
                agent::TurnEvent::ToolCall { server, tool, args } => {
                    println!("  - {server}__{tool} {args}");
                }
                agent::TurnEvent::ToolResult { ok, result } => {
                    if !ok {
                        println!("    failed: {result}");
                    }
                }
                agent::TurnEvent::Reply { text } => {
                    println!();
                    println!("{text}");
                    println!();
                }
                agent::TurnEvent::Error { message } => {
                    eprintln!();
                    eprintln!("[error] {message}");
                    eprintln!();
                }
            }
        }
    }

    ag.end("user exit");
    let id = ag.session_id().to_string();
    drop(ag);
    println!();
    println!("session {id} written to {}", cfg.events_dir().display());
    // `shutdown` consumes the registry, so the agent's handle has to go first.
    if let Ok(reg) = std::sync::Arc::try_unwrap(registry) {
        reg.shutdown().await;
    }
    Ok(())
}


/// List every loop, its schedule, and what happened last time.
///
/// Reads the files and the state, and starts no MCP servers - listing what is
/// configured should not cost a 26-second server boot.
fn loops_cmd(cfg: Config) -> Result<()> {
    let (loops, errs) = loops::load_all(&cfg.loops_dir);
    for e in &errs {
        eprintln!("[warn] {e}");
    }
    if loops.is_empty() {
        println!(
            "no loops in {}

A loop is a markdown file with frontmatter:

             ---
  name: catch-up
  every: 6h
  servers: [kuzu_graph]
---
             What you want it to do, in plain English.",
            cfg.loops_dir.display()
        );
        return Ok(());
    }

    let state = loops::StateFile::load(&cfg.data_dir.join("loops.json"));
    println!("{} loop(s) in {}
", loops.len(), cfg.loops_dir.display());
    for l in &loops {
        let st = state.loops.get(&l.name).cloned().unwrap_or_default();
        let last = st
            .last_run
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "never".into());
        let mark = match st.last_ok {
            Some(true) => "ok",
            Some(false) => "FAILED",
            None => "-",
        };
        let scope = match &l.servers {
            None => "every server".to_string(),
            Some(v) if v.is_empty() => "no tools".to_string(),
            Some(v) => v.join(", "),
        };
        println!(
            "  {:<22} {:<16} {}  last {} ({})",
            l.name,
            l.trigger.describe(),
            if l.enabled { " " } else { "[off]" },
            last,
            mark
        );
        if !l.description.is_empty() {
            println!("      {}", l.description);
        }
        println!("      tools: {scope}");
    }
    Ok(())
}

/// Run one loop now. Deliberately ignores the schedule and the daily cap: this
/// is a person asking for it, and the caps exist to bound what runs unattended.
async fn loop_run_cmd(cfg: Config, name: String) -> Result<()> {
    let (loops, _) = loops::load_all(&cfg.loops_dir);
    let Some(l) = loops.into_iter().find(|l| l.name == name) else {
        eprintln!("no loop called `{name}` - `harness loops` lists them");
        std::process::exit(2);
    };

    let specs = mcp::load_server_specs(&cfg.mcp_config)?;
    let (registry, failures) = mcp::McpRegistry::connect(&specs).await;
    for f in &failures {
        eprintln!("[warn] server failed to start - {f}");
    }
    let vision = std::sync::Arc::new(vision::VisionState::load(&cfg.data_dir));

    println!("running loop `{name}`...
");
    let text = loops::run_once(&cfg, std::sync::Arc::new(registry), vision, &l).await?;
    println!("{text}");
    Ok(())
}
