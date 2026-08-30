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
pub const WITHHELD_FROM_MODEL: &[&str] = &["kuzu_graph__upsert_entity", "kuzu_graph__upsert_relation"];

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
}

impl NativeTools {
    pub fn open(cfg: &crate::config::Config) -> Result<Self> {
        Ok(Self {
            store: VectorStore::open(cfg.data_dir.join("vectors.db"))?,
            artifacts: ArtifactStore::open(&cfg.data_dir)?,
            skills: SkillStore::open(&cfg.skills_dir)?,
            cfg: cfg.clone(),
            embedder: None,
        })
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
                | "list_folders"
                | "read_file"
        )
    }

    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "search_memory".into(),
                description: "Search your own memory of past sessions by meaning, not keywords. \
                    Returns past turns - what the user asked, which tools ran, and what came back. \
                    Use this when the user refers to something from before, or when you need to \
                    know what has already been tried."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "What to look for, phrased naturally."
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
                        "topic": { "type": "string", "description": "Optional filter. Omit to see everything and all topics." }
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
                Ok(serde_json::json!({
                    "indexed_turns": count,
                    "note": if count == 0 {
                        "Memory is empty. It fills as sessions happen and `harness reduce` runs."
                    } else {
                        "Searchable via search_memory."
                    }
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

                if self.store.count()? == 0 {
                    return Ok(serde_json::json!({
                        "results": [],
                        "note": "Memory is empty - nothing has been indexed yet."
                    }));
                }

                let embedder = match &mut self.embedder {
                    Some(e) => e,
                    None => {
                        let e = TextEmbedding::try_new(TextInitOptions::new(
                            EmbeddingModel::AllMiniLML6V2,
                        ))
                        .context("loading embedding model")?;
                        self.embedder.insert(e)
                    }
                };

                let embedding = embedder
                    .embed(vec![query], None)
                    .context("embedding search query")?;
                let hits = self.store.search(&embedding[0], limit)?;

                let results: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "score": (h.score * 1000.0).round() / 1000.0,
                            "session": h.chunk.session_id,
                            "events": format!("{}-{}", h.chunk.seq_start, h.chunk.seq_end),
                            "text": h.chunk.text,
                        })
                    })
                    .collect();

                Ok(serde_json::json!({ "count": results.len(), "results": results }))
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

                let a = self.artifacts.put(name, topic, kind, content, desc)?;
                Ok(serde_json::json!({
                    "ok": true, "id": a.id, "name": a.name, "topic": a.topic,
                    "url": format!("/artifacts/{}/", a.id),
                    "note": "Visible in the Playground now. Tell the user it is there."
                }))
            }

            "list_artifacts" => {
                let topic = args.get("topic").and_then(|v| v.as_str());
                let list = self.artifacts.list(topic)?;
                Ok(serde_json::json!({
                    "count": list.len(),
                    "topics": self.artifacts.topics()?
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
                let id = args.get("root").and_then(|v| v.as_str()).unwrap_or("playground");
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
                let id = args.get("root").and_then(|v| v.as_str()).unwrap_or("playground");
                match crate::roots::resolve(&self.cfg, id, path) {
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
                let id = args.get("root").and_then(|v| v.as_str()).unwrap_or("playground");
                match crate::roots::get(&self.cfg, id)
                    .and_then(|r| crate::artifacts::delete_path(&r.path, path).map(|x| (r, x)))
                {
                    Ok((r, (folder, n))) => Ok(serde_json::json!({
                        "root": r.id, "deleted": path, "folder": folder, "files_removed": n,
                    })),
                    Err(e) => Ok(serde_json::json!({ "path": path, "error": e.to_string() })),
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
