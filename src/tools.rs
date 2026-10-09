//! Native (in-process) tools the harness offers the model directly.
//!
//! These are not MCP servers. Memory lives inside the harness, so routing
//! `search_memory` out over stdio to a server that would just read the same
//! SQLite file would add a process boundary and buy nothing. MCP is for
//! *external* capability (UACC, SnareVec, the graph); this is for the
//! harness's own.

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::artifacts::ArtifactStore;
use crate::llm::ToolDef;
use crate::memory::VectorStore;
use crate::skills::SkillStore;

/// MCP tools deliberately withheld from the model.
///
/// The graph is *derived* from the event log (§4a) - the reducer is its only
/// writer. If the model could write to it directly, those writes would either
/// be silently erased by the next rebuild or survive as facts with no evidence
/// behind them in the log. Either way "the event log is the source of truth"
/// stops being true. The model gets to read the graph, not edit it.
///
/// Note `kuzu_graph__cypher` is safe to expose: the server itself rejects
/// writes, so it is a read-only escape hatch by construction.
pub const WITHHELD_FROM_MODEL: &[&str] = &[
    "kuzu_graph__upsert_entity",
    "kuzu_graph__upsert_relation",
    // Facts reach the graph only from a logged `remember` call (src/facts.rs),
    // which is what keeps every fact backed by a line in the log.
    "kuzu_graph__record_fact",
    "kuzu_graph__close_facts",
    // Deleting a session's graph is the Sessions page's job, never the model's.
    "kuzu_graph__drop_session",
];

/// Which slice of memory a search looks at. The three tiers the model is
/// told about: this conversation (short-term), the last week, and everything
/// (long-term). Code is separate because 1,600 code chunks would otherwise
/// crowd every conversational question out of the top results.
pub fn scope_keep(scope: &str, session: &str) -> impl Fn(&crate::memory::Hit) -> bool {
    let scope = scope.to_string();
    let session = session.to_string();
    let week_ago = (chrono::Local::now() - chrono::Duration::days(7))
        .format("%Y%m%d")
        .to_string();
    move |h: &crate::memory::Hit| {
        let is_code = h.scope == crate::codemap::SCOPE;
        match scope.as_str() {
            "code" => is_code,
            "everything" => true,
            "session" => !is_code && h.chunk.session_id == session,
            // Session ids start with their date (20260923-212038-...), so
            // recency needs no extra column.
            "recent" => !is_code && h.chunk.session_id.get(..8).is_some_and(|d| d >= week_ago.as_str()),
            _ => !is_code,
        }
    }
}

pub struct NativeTools {
    /// Needed for the granted-folder lookups: which roots exist is config, not
    /// something a tool should carry its own copy of.
    cfg: crate::config::Config,
    store: VectorStore,
    artifacts: ArtifactStore,
    skills: SkillStore,
    /// Loaded on first use. Most turns never search, and loading the model
    /// costs a noticeable pause - no reason to pay it at session start.
    embedder: Option<TextEmbedding>,
    /// The conversation these tools serve, for `scope: "session"`.
    session: String,
    /// A sub-agent's own work folder: (granted root id, sub-folder inside it).
    /// Used when a call names no `root`, and `run_command` runs IN the
    /// sub-folder - so two agents never write over each other's files.
    home: Option<(String, String)>,
}

impl NativeTools {
    pub fn open(cfg: &crate::config::Config) -> Result<Self> {
        Ok(Self {
            store: VectorStore::open(cfg.data_dir.join("vectors.db"))?,
            artifacts: ArtifactStore::open(&cfg.data_dir)?,
            skills: SkillStore::open(&cfg.skills_dir)?,
            cfg: cfg.clone(),
            embedder: None,
            session: String::new(),
            home: None,
        })
    }

    /// Give these tools a default work folder (see `home`).
    pub fn set_home(&mut self, root: &str, sub: &str) {
        self.home = Some((root.to_string(), sub.trim_matches(['/', '\\']).to_string()));
    }

    /// Where artifacts go for this call: the playground, or - with `root` set
    /// to a granted folder - that repo's own `.bluee/artifacts`, so a
    /// project's artifacts travel with it. Returns the store and the URL
    /// prefix its files are served under.
    fn artifact_store(&self, args: &serde_json::Value) -> Result<(ArtifactStore, String)> {
        match args.get("root").and_then(|v| v.as_str()).filter(|r| *r != "playground" && !r.is_empty()) {
            Some(id) => {
                let r = crate::roots::get(&self.cfg, id)?;
                let store = ArtifactStore::at(&r.path.join(".bluee").join("artifacts"))?;
                Ok((store, format!("/files/{}/.bluee/artifacts/", r.id)))
            }
            None => Ok((ArtifactStore::open(&self.cfg.data_dir)?, "/artifacts/".into())),
        }
    }

    /// A path as the call meant it. A sub-agent that names no `root` works in
    /// its own sub-folder, so its relative paths start there - the same place
    /// its commands run.
    fn rel_path(&self, args: &serde_json::Value, path: &str) -> String {
        match (&self.home, args.get("root")) {
            (Some((_, sub)), None) if !sub.is_empty() => format!("{sub}/{}", path.trim_start_matches(['/', '\\'])),
            _ => path.to_string(),
        }
    }

    /// The granted root a call means: its own `root`, else the home folder,
    /// else the playground.
    fn root_arg<'a>(&'a self, args: &'a serde_json::Value) -> &'a str {
        args.get("root")
            .and_then(|v| v.as_str())
            .or(self.home.as_ref().map(|(r, _)| r.as_str()))
            .unwrap_or("playground")
    }

    /// For skill auto-loading, which happens in the agent, not in a tool call.
    pub fn skills(&self) -> &SkillStore {
        &self.skills
    }

    pub fn set_session(&mut self, id: &str) {
        self.session = id.to_string();
    }

    /// Hybrid search over one scope, embedding the query with the shared
    /// model. The one search path: the tool, `recall` and auto-recall all
    /// come through here.
    pub fn search_scoped(
        &mut self,
        query: &str,
        scope: &str,
        limit: usize,
    ) -> Result<Vec<crate::memory::Hit>> {
        if self.store.count()? == 0 {
            return Ok(Vec::new());
        }
        let v = self.embed(vec![query])?;
        let keep = scope_keep(scope, &self.session);
        self.store.search_where(query, &v[0], limit, keep)
    }

    pub fn store(&self) -> &VectorStore {
        &self.store
    }

    /// Embed text using the same model recall uses, loading it on first need.
    pub fn embed(&mut self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>> {
        let embedder = match &mut self.embedder {
            Some(e) => e,
            None => {
                let e = TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::AllMiniLML6V2))
                    .context("loading embedding model")?;
                self.embedder.insert(e)
            }
        };
        embedder.embed(texts, None).context("embedding text")
    }

    pub fn handles(name: &str) -> bool {
        matches!(
            name,
            "search_memory"
                | "memory_stats"
                | "create_artifact"
                | "list_artifacts"
                | "open_workspace"
                | "save_skill"
                | "list_skills"
                | "run_skill"
                | "delete_artifact"
                | "delete_skill"
                | "read_source"
                | "list_files"
                | "delete_file"
                | "write_file"
                | "edit_file"
                | "list_folders"
                | "read_file"
                | "run_command"
                | "open_path"
                | "open_app"
        )
    }

    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "search_memory".into(),
                description: "Search your memory of conversations - by meaning AND by exact \
                    words (names, error codes, file names). Returns past turns: what the user \
                    asked, which tools ran, what came back. Use it whenever the user refers to \
                    something from before, or before re-trying something that may already have \
                    been tried. For facts about a person or project, `recall` is faster."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "What to look for, phrased naturally."
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["all", "session", "recent", "code", "everything"],
                            "description": "all = every past conversation (long-term, the default). \
                                session = only THIS conversation, including parts no longer in \
                                your context. recent = conversations from the last 7 days. \
                                code = bluee's own source code. everything = all of these."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "How many results to return. Default 5."
                        }
                    },
                    "required": ["query"]
                }),
            },
            ToolDef {
                name: "create_artifact".into(),
                description: "Build something the user can actually look at and use - a live                     HTML page, a chart, a small tool, a note. Write the COMPLETE file contents;                     for html that means a full self-contained page including its own CSS and                     JavaScript, since nothing else will be injected. It appears in the user's                     Playground immediately. Re-using the same name and topic UPDATES that                     artifact rather than creating a second copy, so iterate by re-writing it.                     Always pass a topic - it is how this gets found again later."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name":  { "type": "string", "description": "Short human name, e.g. 'Candle chart'." },
                        "topic": { "type": "string", "description": "Work profile it belongs to, e.g. 'trading'. Reuse the user's own word for it." },
                        "root": { "type": "string", "description": "A granted folder id to keep it IN that project (its .bluee/artifacts) - use it when the work is about that repo. Omit for the playground." },
                        "kind":  { "type": "string", "enum": ["html","markdown","json","text"], "description": "Default html." },
                        "content": { "type": "string", "description": "The complete file contents." },
                        "description": { "type": "string", "description": "One line on what it does." }
                    },
                    "required": ["name","topic","content"]
                }),
            },
            ToolDef {
                name: "list_artifacts".into(),
                description: "What has already been built, optionally for one topic. Check this                     before building something new - it may already exist and want updating."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "description": "Optional filter. Omit to see everything and all topics." },
                        "root": { "type": "string", "description": "A granted folder id to list that project's artifacts instead of the playground's." }
                    }
                }),
            },
            ToolDef {
                name: "open_workspace".into(),
                description: "Resume a work profile. Give it a topic like 'trading' and it                     returns the artifacts built for it plus what was discussed last time, so you                     can carry on rather than starting cold. Use this whenever the user says they                     want to work on something you may have worked on before."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "description": "The work profile, e.g. 'trading'." }
                    },
                    "required": ["topic"]
                }),
            },
            ToolDef {
                name: "save_skill".into(),
                description: "Save a reusable procedure so you can follow it again later. Use \
                    this when Adithya says 'remember this as a skill', 'save that for later', or \
                    when you have just worked something out that will obviously come up again. \
                    Write the steps concretely - name the actual tools and the order - as if \
                    handing them to someone who was not here. Same name overwrites, so refine a \
                    skill by re-saving it."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Short name, e.g. 'Restart SnareVec'." },
                        "description": { "type": "string", "description": "One line: what it achieves and when to use it." },
                        "steps": { "type": "string", "description": "The procedure in Markdown. Numbered steps, naming the tools used and what to check at each point." },
                        "tools": { "type": "array", "items": { "type": "string" }, "description": "Tool ids the procedure uses, e.g. ['snarevec__snarevec_status']." },
                        "category": { "type": "string", "enum": ["skills","recorded","toolkit","proposed"], "description": "'skills' when deliberate, 'recorded' when captured from something that just happened. Default 'skills'." }
                    },
                    "required": ["name","steps"]
                }),
            },
            ToolDef {
                name: "list_skills".into(),
                description: "What procedures are already saved. Check here before working \
                    something out from scratch - it may already be solved."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "category": { "type": "string", "description": "Optional filter: skills, recorded, toolkit, proposed." }
                    }
                }),
            },
            ToolDef {
                name: "run_skill".into(),
                description: "Fetch a saved procedure so you can carry it out. This does NOT \
                    execute anything by itself - it returns the steps, and you then follow them \
                    using your normal tools, so every action stays visible in the log. Read the \
                    steps, check the tools it needs are available, then do it."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name or slug." }
                    },
                    "required": ["name"]
                }),
            },
            ToolDef {
                name: "delete_artifact".into(),
                description: "Delete an artifact you built. Use when one is obsolete, was a                     mistake, or the user asks you to clear it. Deletes the files on disk - say                     what you are about to remove and only do it when that is clearly what was                     wanted."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Artifact id from list_artifacts." }
                    },
                    "required": ["id"]
                }),
            },
            ToolDef {
                name: "delete_skill".into(),
                description: "Delete a saved skill by name. Use when a procedure is wrong,                     superseded, or the user asks you to drop it."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name or slug from list_skills." }
                    },
                    "required": ["name"]
                }),
            },
            ToolDef {
                name: "list_folders".into(),
                description: "Which folders you have been granted access to. Adithya grants                     these; you cannot add one yourself. Call this first if you are unsure which                     folder a request is about."
                    .into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
            ToolDef {
                name: "list_files".into(),
                description: "List files and folders inside a granted folder. Defaults to the                     playground - everything you have built, as it sits on disk. Pass `root`                     (from list_folders) to look in another granted folder."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "root": { "type": "string", "description": "Granted folder id. Defaults to \"playground\"." }
                    }
                }),
            },
            ToolDef {
                name: "read_file".into(),
                description: "Read a text file inside a granted folder, by the path list_files                     gave you. For your own source code use read_source instead."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the granted folder." },
                        "root": { "type": "string", "description": "Granted folder id. Defaults to \"playground\"." }
                    },
                    "required": ["path"]
                }),
            },
            ToolDef {
                name: "delete_file".into(),
                description: "Delete a file or folder in the playground folder, by the path                     list_files gave you. Deleting a folder removes what is inside it. Bounded to                     the playground folder - nothing else on the machine is reachable. Say what                     you are removing before you do it."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the granted folder." },
                        "root": { "type": "string", "description": "Granted folder id. Defaults to \"playground\"." }
                    },
                    "required": ["path"]
                }),
            },
            ToolDef {
                name: "write_file".into(),
                description: "Create a file, or replace one, inside a granted folder. Missing parent \
                    folders are made for you. Use it to write code, notes, pages - anything. For a \
                    small change to an existing file, edit_file is cheaper and safer."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the granted folder, e.g. `src/app.js`." },
                        "content": { "type": "string", "description": "The whole new content of the file." },
                        "root": { "type": "string", "description": "Granted folder id. Defaults to \"playground\" (or your own folder, if you are a sub-agent)." },
                        "overwrite": { "type": "boolean", "description": "Replace an existing file (default true). false fails if it exists." }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolDef {
                name: "edit_file".into(),
                description: "Change part of a file in a granted folder: replace an exact piece of text \
                    with new text. `old` must appear exactly once (include enough surrounding lines to \
                    make it unique) unless replace_all is true. Read the file first."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old": { "type": "string", "description": "Exact existing text, whitespace included." },
                        "new": { "type": "string", "description": "What to put in its place." },
                        "replace_all": { "type": "boolean" },
                        "root": { "type": "string", "description": "Granted folder id. Defaults as for write_file." }
                    },
                    "required": ["path", "old", "new"]
                }),
            },
            ToolDef {
                name: "run_command".into(),
                description: "Run a shell command inside a granted folder and get back its exit \
                    code, stdout and stderr. This is how you build, test, run git, install \
                    things, or inspect a project. Working directory is the granted folder. Say \
                    what you are about to run and why before you run it - the command is logged \
                    either way, but Adithya should not have to read the log to know what you did."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The command line to run." },
                        "root": { "type": "string", "description": "Granted folder id to run in. Defaults to \"playground\"." },
                        "timeout_secs": { "type": "integer", "description": "Give up after this long. Default 120." }
                    },
                    "required": ["command"]
                }),
            },
            ToolDef {
                name: "open_path".into(),
                description: "Open a file or folder in whatever application the OS uses for it - \
                    a document in its editor, a folder in Explorer. Use when Adithya wants to \
                    SEE something rather than have you read it."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the granted folder. Empty opens the folder itself." },
                        "root": { "type": "string", "description": "Granted folder id. Defaults to \"playground\"." }
                    }
                }),
            },
            ToolDef {
                name: "open_app".into(),
                description: "Launch an application by name or full path (`notepad`, `code`, \
                    `chrome`), optionally with arguments. Starts it and returns - it does not \
                    wait or capture output. For something whose output you need, use run_command."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "app": { "type": "string", "description": "Executable name on PATH, or a full path." },
                        "args": { "type": "array", "items": { "type": "string" }, "description": "Arguments to pass." }
                    },
                    "required": ["app"]
                }),
            },
            ToolDef {
                name: "read_source".into(),
                description: "Read one file of your own source code, by repo-relative path                     (e.g. `src/llm.rs`, `dash/index.html`, `CLAUDE.md`). Use after search_memory                     points you at a file and you need the exact current text. Secrets, build                     output and derived state are refused. You can read your source; you cannot                     write it."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Repo-relative path, forward slashes." }
                    },
                    "required": ["path"]
                }),
            },
            ToolDef {
                name: "memory_stats".into(),
                description: "How much memory exists: how many past turns are indexed and \
                    searchable. Use to check whether memory is worth searching at all."
                    .into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        ]
    }

    pub fn call(&mut self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        match name {
            "memory_stats" => {
                let count = self.store.count()?;
                let code = self.store.count_scope(crate::codemap::SCOPE)?;
                Ok(serde_json::json!({
                    "conversation_chunks": count - code,
                    "code_chunks": code,
                    "note": "Every finished turn is indexed immediately. Searchable via \
                             search_memory; facts about people and projects via recall."
                }))
            }

            "search_memory" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .context("search_memory requires a `query` string")?;
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5)
                    .clamp(1, 20) as usize;
                let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("all").to_string();

                let hits = self.search_scoped(query, &scope, limit)?;
                if hits.is_empty() {
                    return Ok(serde_json::json!({
                        "scope": scope,
                        "results": [],
                        "note": "Nothing in this scope yet. Try scope \"everything\", or other words."
                    }));
                }

                let results: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "score": (h.score * 1000.0).round() / 1000.0,
                            "session": h.chunk.session_id,
                            "this_session": h.chunk.session_id == self.session,
                            "events": format!("{}-{}", h.chunk.seq_start, h.chunk.seq_end),
                            "text": h.chunk.text,
                        })
                    })
                    .collect();

                Ok(serde_json::json!({ "scope": scope, "count": results.len(), "results": results }))
            }

            "create_artifact" => {
                let name = args.get("name").and_then(|v| v.as_str())
                    .context("create_artifact requires `name`")?;
                let topic = args.get("topic").and_then(|v| v.as_str())
                    .context("create_artifact requires `topic`")?;
                let content = args.get("content").and_then(|v| v.as_str())
                    .context("create_artifact requires `content`")?;
                let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("html");
                let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("");

                let (store, prefix) = self.artifact_store(args)?;
                let a = store.put(name, topic, kind, content, desc)?;
                Ok(serde_json::json!({
                    "ok": true, "id": a.id, "name": a.name, "topic": a.topic,
                    "url": format!("{prefix}{}/{}", a.id, a.entry),
                    "note": "Visible in the Playground now (select that folder to see a repo's \
                             artifacts). Tell the user it is there."
                }))
            }

            "list_artifacts" => {
                let topic = args.get("topic").and_then(|v| v.as_str());
                let (store, _) = self.artifact_store(args)?;
                let list = store.list(topic)?;
                Ok(serde_json::json!({
                    "count": list.len(),
                    "topics": store.topics()?
                        .into_iter().map(|(t,n)| serde_json::json!({"topic":t,"artifacts":n}))
                        .collect::<Vec<_>>(),
                    "artifacts": list.iter().map(|a| serde_json::json!({
                        "id": a.id, "name": a.name, "topic": a.topic, "kind": a.kind,
                        "description": a.description, "updated": a.updated,
                    })).collect::<Vec<_>>(),
                }))
            }

            "open_workspace" => {
                let topic = args.get("topic").and_then(|v| v.as_str())
                    .context("open_workspace requires `topic`")?;
                let arts = self.artifacts.list(Some(topic))?;

                // "Where we left off" is the point of this tool, so it pulls
                // past conversation about the topic as well as the artifacts.
                let recall = self
                    .call("search_memory", &serde_json::json!({ "query": topic, "limit": 5 }))
                    .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));

                Ok(serde_json::json!({
                    "topic": topic,
                    "artifacts": arts.iter().map(|a| serde_json::json!({
                        "id": a.id, "name": a.name, "kind": a.kind,
                        "description": a.description, "updated": a.updated,
                        "url": format!("/artifacts/{}/", a.id),
                    })).collect::<Vec<_>>(),
                    "previously_discussed": recall.get("results").cloned()
                        .unwrap_or(serde_json::json!([])),
                    "note": if arts.is_empty() {
                        "Nothing built for this topic yet."
                    } else {
                        "These are already open in the Playground."
                    }
                }))
            }

            "save_skill" => {
                let name = args.get("name").and_then(|v| v.as_str())
                    .context("save_skill requires `name`")?;
                let steps = args.get("steps").and_then(|v| v.as_str())
                    .context("save_skill requires `steps`")?;
                let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let category = args.get("category").and_then(|v| v.as_str()).unwrap_or("skills");
                let tools: Vec<String> = args
                    .get("tools")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();

                let s = self.skills.put(name, category, desc, &tools, steps)?;
                Ok(serde_json::json!({
                    "ok": true, "name": s.name, "slug": s.slug, "category": s.category,
                    "file": format!("{}/{}/{}.md", self.skills.root().display(), s.category, s.slug),
                    "note": "Saved. Tell Adithya what you saved and what it does."
                }))
            }

            "list_skills" => {
                let category = args.get("category").and_then(|v| v.as_str());
                let list = self.skills.list(category)?;
                Ok(serde_json::json!({
                    "count": list.len(),
                    "skills": list.iter().map(|s| serde_json::json!({
                        "name": s.name, "slug": s.slug, "category": s.category,
                        "description": s.description, "tools": s.tools, "updated": s.updated,
                    })).collect::<Vec<_>>(),
                    "note": if list.is_empty() {
                        "Nothing saved yet. Use save_skill when something is worth keeping."
                    } else {
                        "Use run_skill to fetch the steps for one."
                    }
                }))
            }

            "run_skill" => {
                let name = args.get("name").and_then(|v| v.as_str())
                    .context("run_skill requires `name`")?;
                match self.skills.get(name)? {
                    Some(s) => Ok(serde_json::json!({
                        "found": true,
                        "name": s.name, "category": s.category,
                        "description": s.description, "tools": s.tools,
                        "steps": s.body,
                        // Said explicitly so the model does not report the skill
                        // as "done" merely because it fetched it.
                        "note": "These are instructions, not an action. Nothing has run yet - \
                                 follow the steps yourself using your tools."
                    })),
                    None => {
                        let known: Vec<String> =
                            self.skills.list(None)?.into_iter().map(|s| s.slug).collect();
                        Ok(serde_json::json!({
                            "found": false,
                            "error": format!("no skill named `{name}`"),
                            "available": known,
                        }))
                    }
                }
            }

            "delete_artifact" => {
                let id = args.get("id").and_then(|v| v.as_str())
                    .context("delete_artifact requires `id`")?;
                self.artifacts.delete(id)?;
                Ok(serde_json::json!({
                    "deleted": id,
                    "note": "Files removed from disk. The event log still records that it was                              built and then deleted - that history is not rewritten.",
                }))
            }

            "delete_skill" => {
                let name = args.get("name").and_then(|v| v.as_str())
                    .context("delete_skill requires `name`")?;
                let existed = self.skills.delete(name)?;
                Ok(serde_json::json!({
                    "deleted": existed,
                    "name": name,
                    "note": if existed { "Skill file removed." } else { "No skill by that name." },
                }))
            }

            "list_folders" => {
                let roots: Vec<serde_json::Value> = crate::roots::load(&self.cfg)
                    .into_iter()
                    .map(|r| serde_json::json!({
                        "id": r.id, "label": r.label,
                        "path": crate::roots::pretty(&r.path),
                        "exists": r.path.is_dir(),
                    }))
                    .collect();
                Ok(serde_json::json!({
                    "folders": roots,
                    "note": "Adithya grants these. You cannot add one - ask him to."
                }))
            }

            "list_files" => {
                let id = self.root_arg(args);
                match crate::roots::get(&self.cfg, id) {
                    Ok(r) => Ok(serde_json::json!({
                        "root": r.id, "label": r.label,
                        "files": crate::artifacts::tree(&r.path)?,
                    })),
                    Err(e) => Ok(serde_json::json!({ "root": id, "error": e.to_string() })),
                }
            }

            "read_file" => {
                let path = args.get("path").and_then(|v| v.as_str())
                    .context("read_file requires `path`")?;
                let id = self.root_arg(args);
                let rel = self.rel_path(args, path);
                match crate::roots::resolve(&self.cfg, id, &rel) {
                    Ok((r, full)) => match std::fs::read_to_string(&full) {
                        Ok(text) => Ok(serde_json::json!({
                            "root": r.id, "path": path,
                            "lines": text.lines().count(), "text": text,
                        })),
                        Err(_) => Ok(serde_json::json!({
                            "root": r.id, "path": path,
                            "error": "not a text file",
                        })),
                    },
                    Err(e) => Ok(serde_json::json!({ "path": path, "error": e.to_string() })),
                }
            }

            "delete_file" => {
                let path = args.get("path").and_then(|v| v.as_str())
                    .context("delete_file requires `path`")?;
                let id = self.root_arg(args);
                match crate::roots::get(&self.cfg, id)
                    .and_then(|r| crate::artifacts::delete_path(&r.path, path).map(|x| (r, x)))
                {
                    Ok((r, (folder, n))) => Ok(serde_json::json!({
                        "root": r.id, "deleted": path, "folder": folder, "files_removed": n,
                    })),
                    Err(e) => Ok(serde_json::json!({ "path": path, "error": e.to_string() })),
                }
            }

            "write_file" => {
                let path = args.get("path").and_then(|v| v.as_str())
                    .context("write_file requires `path`")?;
                let content = args.get("content").and_then(|v| v.as_str())
                    .context("write_file requires `content`")?;
                let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(true);
                let rel = self.rel_path(args, path);
                let id = self.root_arg(args);
                match crate::roots::resolve_new(&self.cfg, id, &rel) {
                    Ok((r, full)) => {
                        let existed = full.exists();
                        if existed && !overwrite {
                            return Ok(serde_json::json!({ "path": rel, "error": "already exists (overwrite was false)" }));
                        }
                        if let Some(dir) = full.parent() {
                            std::fs::create_dir_all(dir)?;
                        }
                        std::fs::write(&full, content)?;
                        Ok(serde_json::json!({
                            "root": r.id, "path": rel,
                            "created": !existed, "bytes": content.len(),
                            "lines": content.lines().count(),
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({ "path": rel, "error": e.to_string() })),
                }
            }

            "edit_file" => {
                let path = args.get("path").and_then(|v| v.as_str())
                    .context("edit_file requires `path`")?;
                let old = args.get("old").and_then(|v| v.as_str()).context("edit_file requires `old`")?;
                let new = args.get("new").and_then(|v| v.as_str()).context("edit_file requires `new`")?;
                let all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
                let rel = self.rel_path(args, path);
                let id = self.root_arg(args);
                let (r, full) = match crate::roots::resolve_new(&self.cfg, id, &rel) {
                    Ok(x) => x,
                    Err(e) => return Ok(serde_json::json!({ "path": rel, "error": e.to_string() })),
                };
                let Ok(text) = std::fs::read_to_string(&full) else {
                    return Ok(serde_json::json!({ "path": rel,
                        "error": "no such text file - use write_file to create it" }));
                };
                if old.is_empty() {
                    return Ok(serde_json::json!({ "path": rel, "error": "`old` is empty" }));
                }
                // Windows files often use CRLF while the model writes LF; try
                // the CRLF spelling before calling it a miss.
                let crlf = old.replace("\r\n", "\n").replace('\n', "\r\n");
                let (old_used, new_used) = if text.contains(old) {
                    (old.to_string(), new.to_string())
                } else if text.contains(&crlf) {
                    (crlf, new.replace("\r\n", "\n").replace('\n', "\r\n"))
                } else {
                    return Ok(serde_json::json!({ "path": rel,
                        "error": "`old` was not found - read the file again and copy the text exactly" }));
                };
                let n = text.matches(old_used.as_str()).count();
                if n > 1 && !all {
                    return Ok(serde_json::json!({ "path": rel,
                        "error": format!("`old` appears {n} times - add surrounding lines to make it unique, or set replace_all") }));
                }
                let updated = if all { text.replace(&old_used, &new_used) } else { text.replacen(&old_used, &new_used, 1) };
                std::fs::write(&full, &updated)?;
                Ok(serde_json::json!({
                    "root": r.id, "path": rel, "replaced": if all { n } else { 1 },
                    "lines": updated.lines().count(),
                }))
            }

            "run_command" => {
                let command = args.get("command").and_then(|v| v.as_str())
                    .context("run_command requires `command`")?;
                let id = self.root_arg(args);
                let timeout = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(120);
                // No model-settable override, deliberately.
                //
                // The first version took a `confirm` flag. Tested against the
                // real model - asked to "try running `format C: /q`" - it set
                // `confirm: true` itself and ran it. It survived only because
                // Windows demanded elevation. A confirmation the model can
                // grant itself is not a confirmation, it is decoration. These
                // are refused outright; Adithya has a terminal and can run one
                // himself if he ever actually means to.
                if let Some(pat) = crate::system::looks_catastrophic(command) {
                    return Ok(serde_json::json!({
                        "refused": true,
                        "command": command,
                        "matched": pat,
                        "error": format!(
                            "Refused: this contains `{pat}`, which can destroy the machine. \
                             There is no override on this tool. If that is genuinely what is \
                             wanted, say so plainly and let Adithya run it himself."
                        ),
                    }));
                }

                let root = match crate::roots::get(&self.cfg, id) {
                    Ok(r) => r,
                    Err(e) => return Ok(serde_json::json!({ "root": id, "error": e.to_string() })),
                };
                // A sub-agent with no explicit root runs in its own sub-folder.
                let mut cwd = root.path.clone();
                if args.get("root").is_none() {
                    if let Some((_, sub)) = &self.home {
                        if !sub.is_empty() && !sub.contains("..") {
                            cwd = cwd.join(sub);
                            let _ = std::fs::create_dir_all(&cwd);
                        }
                    }
                }
                match crate::system::run(command, &cwd, timeout) {
                    Ok(out) => Ok(serde_json::to_value(out)?),
                    Err(e) => Ok(serde_json::json!({ "command": command, "error": e.to_string() })),
                }
            }

            "open_path" => {
                let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let id = self.root_arg(args);
                match crate::roots::resolve(&self.cfg, id, rel) {
                    Ok((r, full)) => match crate::system::open_path(&full) {
                        Ok(shown) => Ok(serde_json::json!({
                            "opened": shown, "root": r.id,
                            "note": "Handed to the OS. Whether a window appeared is not something \
                                     this can confirm - ask Adithya if it matters.",
                        })),
                        Err(e) => Ok(serde_json::json!({ "path": rel, "error": e.to_string() })),
                    },
                    Err(e) => Ok(serde_json::json!({ "path": rel, "error": e.to_string() })),
                }
            }

            "open_app" => {
                let app = args.get("app").and_then(|v| v.as_str())
                    .context("open_app requires `app`")?;
                let list: Vec<String> = args
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                match crate::system::open_app(app, &list) {
                    Ok(shown) => Ok(serde_json::json!({ "launched": shown })),
                    Err(e) => Ok(serde_json::json!({ "app": app, "error": e.to_string() })),
                }
            }

            "read_source" => {
                let path = args.get("path").and_then(|v| v.as_str())
                    .context("read_source requires `path`")?;
                let root = std::env::current_dir()?;
                match crate::codemap::read_source(&root, path) {
                    Ok(text) => Ok(serde_json::json!({
                        "path": path,
                        "lines": text.lines().count(),
                        "text": text,
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "path": path,
                        "error": e.to_string(),
                    })),
                }
            }

            other => anyhow::bail!("no native tool named `{other}`"),
        }
    }
}
