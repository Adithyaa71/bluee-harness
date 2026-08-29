mod agent;
mod artifacts;
mod app;
mod codemap;
mod config;
mod dash;
mod eventlog;
mod llm;
mod mcp;
mod memory;
mod providers;
mod pty;
mod reduce;
mod skills;
mod tools;
mod vision;
// Aliased because this file already has a `tools` subcommand function.
use tools as native_tools;

use anyhow::Result;
use config::Config;
use eventlog::{EventKind, EventLog};
use llm::{Message, OpenAiCompatible, Provider};
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
    let hits = store.search(&embedding[0], 5)?;

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

/// The turn loop. Text in, text out, no tools yet - Phase 0's smallest proof
/// that the plumbing works. Every turn is written to the event log as it
/// happens, so the log is complete from the very first run rather than
/// retrofitted later.
/// The turn loop, with tools.
///
/// The model is handed every connected MCP tool and decides which to call; the
/// harness executes them and feeds results back until the model answers in
/// plain text. Every step - user message, each tool call, each result, the
/// final reply - is appended to the event log as it happens (§4a), so the log
/// is a complete trace rather than a summary written afterwards.
async fn chat(cfg: Config) -> Result<()> {
    cfg.require_credentials()?;

    let (persona, persona_files) = config::load_persona(&cfg.persona_dir)?;
    let provider = OpenAiCompatible::new(&cfg.base_url, &cfg.api_key, &cfg.model, cfg.max_tokens);

    // Connect tool servers up front so the toolset is fixed for the session.
    let specs = mcp::load_server_specs(&cfg.mcp_config)?;
    let (registry, failures) = mcp::McpRegistry::connect(&specs).await;
    for f in &failures {
        eprintln!("[warn] server failed to start - {f}");
    }

    // 106 tools is past what most models choose well from, and their schemas
    // are a large chunk of every prompt. HARNESS_TOOL_SERVERS narrows it, which
    // is the same mechanism workspaces (§4f) and the vision gate (§6) will use.
    let allow: Option<Vec<String>> = std::env::var("HARNESS_TOOL_SERVERS")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect());

    // The harness's own memory tools come first: recall should be as reachable
    // as action, and putting them at the head of the list keeps them visible
    // when the tool list is long.
    let mut native = native_tools::NativeTools::open(&cfg.data_dir, &cfg.skills_dir)?;
    let mut tool_defs: Vec<llm::ToolDef> = native_tools::NativeTools::defs();
    let native_count = tool_defs.len();

    tool_defs.extend(
        registry
            .tools()
            .iter()
            .filter(|t| allow.as_ref().is_none_or(|a| a.contains(&t.server)))
            // Withhold the graph's write tools - the reducer is its only
            // writer, see native_tools::WITHHELD_FROM_MODEL.
            .filter(|t| !native_tools::WITHHELD_FROM_MODEL.contains(&t.qualified().as_str()))
            .map(|t| llm::ToolDef {
                name: t.qualified(),
                description: t.description.clone(),
                parameters: t.schema.clone(),
            }),
    );

    let mut log = EventLog::new_session(cfg.events_dir())?;
    log.append(EventKind::SessionStart {
        model: cfg.model.clone(),
        persona_files: persona_files.clone(),
    })?;

    println!("model:   {}", cfg.model);
    println!("session: {}", log.session_id());
    println!(
        "persona: {}",
        if persona_files.is_empty() {
            "(none yet - Phase 3)".to_string()
        } else {
            persona_files.join(", ")
        }
    );
    println!(
        "tools:   {} ({} memory + {} from {} server(s)){}",
        tool_defs.len(),
        native_count,
        tool_defs.len() - native_count,
        registry.servers().len(),
        match &allow {
            Some(a) => format!(" [limited to {}]", a.join(",")),
            None => String::new(),
        }
    );
    println!("\nType a message, or /quit to exit.\n");

    let mut history: Vec<Message> = Vec::new();
    if !persona.is_empty() {
        history.push(Message::system(persona));
    }

    // A runaway tool loop burns money silently, so it is capped rather than
    // trusted. Hitting the cap is reported, not swallowed.
    const MAX_TOOL_ROUNDS: usize = 8;

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("> ");
        use std::io::Write;
        std::io::stdout().flush().ok();

        let Some(line) = lines.next_line().await? else {
            break;
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" || input == "/exit" {
            break;
        }

        log.append(EventKind::UserMessage { text: input.into() })?;
        history.push(Message::user(input));

        for round in 0..MAX_TOOL_ROUNDS {
            let completion = match provider.complete(&history, &tool_defs).await {
                Ok(c) => c,
                Err(e) => {
                    let message = format!("{e:#}");
                    eprintln!("\n[error] {message}\n");
                    log.append(EventKind::Error {
                        context: "provider.complete".into(),
                        message,
                    })?;
                    break;
                }
            };

            // No tools requested: the model is answering, so the turn is done.
            if completion.tool_calls.is_empty() {
                let reply = completion.content.unwrap_or_default();
                println!("\n{reply}\n");
                log.append(EventKind::AssistantMessage {
                    text: reply.clone(),
                })?;
                history.push(Message::assistant(reply));
                break;
            }

            // The assistant turn carrying the tool calls must go into history
            // verbatim, or the follow-up tool messages have nothing to pair to.
            history.push(Message {
                role: "assistant".into(),
                content: completion.content.clone(),
                tool_calls: Some(completion.tool_calls.clone()),
                tool_call_id: None,
            });

            for tc in &completion.tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));

                println!("  · {} {}", tc.function.name, args);

                // Native memory tools run in-process. They are logged with
                // server "harness" so the event log records them exactly like
                // any other tool call - the trace stays complete (§4a).
                if native_tools::NativeTools::handles(&tc.function.name) {
                    log.append(EventKind::ToolCall {
                        call_id: tc.id.clone(),
                        server: "harness".into(),
                        tool: tc.function.name.clone(),
                        args: args.clone(),
                    })?;

                    match native.call(&tc.function.name, &args) {
                        Ok(value) => {
                            log.append(EventKind::ToolResult {
                                call_id: tc.id.clone(),
                                ok: true,
                                result: value.clone(),
                            })?;
                            history.push(Message::tool_result(&tc.id, value.to_string()));
                        }
                        Err(e) => {
                            let message = format!("{e:#}");
                            log.append(EventKind::ToolResult {
                                call_id: tc.id.clone(),
                                ok: false,
                                result: serde_json::json!({ "error": message }),
                            })?;
                            history.push(Message::tool_result(
                                &tc.id,
                                format!("Tool call failed: {message}"),
                            ));
                        }
                    }
                    continue;
                }

                let Some((server, tool)) = registry.resolve(&tc.function.name) else {
                    let message = format!("unknown tool `{}`", tc.function.name);
                    log.append(EventKind::Error {
                        context: "tool.resolve".into(),
                        message: message.clone(),
                    })?;
                    history.push(Message::tool_result(&tc.id, message));
                    continue;
                };

                log.append(EventKind::ToolCall {
                    call_id: tc.id.clone(),
                    server: server.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                })?;

                match registry.call(&server, &tool, args).await {
                    Ok(value) => {
                        log.append(EventKind::ToolResult {
                            call_id: tc.id.clone(),
                            ok: true,
                            result: value.clone(),
                        })?;
                        history.push(Message::tool_result(&tc.id, value.to_string()));
                    }
                    Err(e) => {
                        // A failed tool is reported back to the model rather
                        // than aborting: it can often recover by trying another.
                        let message = format!("{e:#}");
                        log.append(EventKind::ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            result: serde_json::json!({ "error": message }),
                        })?;
                        history.push(Message::tool_result(
                            &tc.id,
                            format!("Tool call failed: {message}"),
                        ));
                    }
                }
            }

            if round == MAX_TOOL_ROUNDS - 1 {
                let message = format!("stopped after {MAX_TOOL_ROUNDS} tool rounds");
                eprintln!("\n[warn] {message}\n");
                log.append(EventKind::Error {
                    context: "chat.tool_loop".into(),
                    message,
                })?;
            }
        }
    }

    log.append(EventKind::SessionEnd {
        reason: "user exit".into(),
    })?;
    println!(
        "\nsession {} written to {}",
        log.session_id(),
        log.path().display()
    );
    registry.shutdown().await;
    Ok(())
}
