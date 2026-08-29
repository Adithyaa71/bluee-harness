//! Append-only session event log (§4a) - the source of truth.
//!
//! Every user message, assistant message, tool call, tool result and error is
//! appended here in order. Nothing is ever overwritten or mutated in place. The
//! vector layer (§4b), the graph layer (§4c) and saved skills (§5c) are all
//! *derived* from this file, so they can be deleted and rebuilt from it at any
//! time - that is the whole point, and it is only true if writes stay additive.
//!
//! Format is JSONL: one self-describing JSON object per line, so the log stays
//! greppable and tailable by the dashboard without a parser of its own.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// What happened. Serialized with an internal `kind` tag so a line is readable
/// on its own without positional knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// Opens a session. Records which persona files (§5a) were loaded and which
    /// model answered, so a session can be reconstructed exactly later.
    SessionStart {
        model: String,
        persona_files: Vec<String>,
    },
    UserMessage {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    /// A tool the model decided to call. `call_id` pairs it with its result.
    ToolCall {
        call_id: String,
        server: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        ok: bool,
        result: serde_json::Value,
    },
    /// Passive or active screen capture (§6), indexed as timestamped context.
    ScreenContext {
        mode: String,
        text: String,
    },
    /// A human-given name for this session. Renaming appends another one
    /// rather than editing the old line - the log stays append-only and the
    /// last title wins, so a rename is still fully reconstructable.
    SessionTitle {
        title: String,
    },
    /// Harness-level events that are neither the user nor the model speaking:
    /// context compaction, mode changes, and similar. Adding a variant is
    /// backward compatible - older lines simply never carry it.
    System {
        note: String,
    },
    Error {
        context: String,
        message: String,
    },
    SessionEnd {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Monotonic within a session, starting at 1.
    pub seq: u64,
    pub session_id: String,
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Append-only writer over one session's JSONL file.
pub struct EventLog {
    path: PathBuf,
    session_id: String,
    seq: u64,
    file: File,
}

impl EventLog {
    /// Open (or resume) the log for `session_id` under `dir`.
    ///
    /// Resuming matters: if the harness restarts against an existing session we
    /// must continue the sequence rather than restart it, or `seq` stops being
    /// a reliable ordering key.
    pub fn open(dir: impl AsRef<Path>, session_id: impl Into<String>) -> Result<Self> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)
            .with_context(|| format!("creating event log dir {}", dir.display()))?;

        let session_id = session_id.into();
        let path = dir.join(format!("{session_id}.jsonl"));

        let seq = if path.exists() {
            Self::read(&path)?.last().map(|e| e.seq).unwrap_or(0)
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening event log {}", path.display()))?;

        Ok(Self {
            path,
            session_id,
            seq,
            file,
        })
    }

    /// Start a fresh session with a generated id.
    pub fn new_session(dir: impl AsRef<Path>) -> Result<Self> {
        let id = format!(
            "{}-{}",
            Utc::now().format("%Y%m%d-%H%M%S"),
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        Self::open(dir, id)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event and flush it. Returns the assigned sequence number.
    ///
    /// Flushing per event is deliberate: the dashboard tails this file live, and
    /// a crash must not lose the tool call that caused it - that is exactly the
    /// event you want when reading the log backward to ask "why did it do that".
    pub fn append(&mut self, kind: EventKind) -> Result<u64> {
        self.seq += 1;
        let event = Event {
            seq: self.seq,
            session_id: self.session_id.clone(),
            ts: Utc::now(),
            kind,
        };

        let line = serde_json::to_string(&event).context("serializing event")?;
        writeln!(self.file, "{line}").context("appending to event log")?;
        self.file.flush().context("flushing event log")?;

        Ok(event.seq)
    }

    /// Read every event in a session file, in order.
    pub fn read(path: impl AsRef<Path>) -> Result<Vec<Event>> {
        let path = path.as_ref();
        let file =
            File::open(path).with_context(|| format!("opening event log {}", path.display()))?;

        let mut events = Vec::new();
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = line.with_context(|| format!("reading {} line {}", path.display(), i + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(&line)
                .with_context(|| format!("parsing {} line {}", path.display(), i + 1))?;
            events.push(event);
        }
        Ok(events)
    }

    /// Every session log under `dir`, oldest first. Session ids are timestamp
    /// prefixed, so lexical order is chronological order.
    pub fn list_sessions(dir: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let dir = dir.as_ref();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .with_context(|| format!("listing {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        paths.sort();
        Ok(paths)
    }
}

/// The name to show for a session: the last title it was given, or failing
/// that the opening line of the conversation.
///
/// Titles are events, so "the current name" is simply the last one written.
/// Nothing is overwritten and the rename history stays in the log.
pub fn session_title(events: &[Event]) -> Option<String> {
    let explicit = events.iter().rev().find_map(|e| match &e.kind {
        EventKind::SessionTitle { title } if !title.trim().is_empty() => {
            Some(title.trim().to_string())
        }
        _ => None,
    });
    explicit.or_else(|| {
        events.iter().find_map(|e| match &e.kind {
            EventKind::UserMessage { text } if !text.trim().is_empty() => {
                let one = text.trim().lines().next().unwrap_or("").trim();
                let mut s: String = one.chars().take(48).collect();
                if one.chars().count() > 48 {
                    s.push('…');
                }
                Some(s)
            }
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_in_order_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("evlog-{}", uuid::Uuid::new_v4()));
        let mut log = EventLog::open(&dir, "test-session").unwrap();

        log.append(EventKind::UserMessage {
            text: "what is running".into(),
        })
        .unwrap();
        log.append(EventKind::ToolCall {
            call_id: "c1".into(),
            server: "uacc".into(),
            tool: "get_screen_info".into(),
            args: serde_json::json!({}),
        })
        .unwrap();

        let events = EventLog::read(log.path()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn title_falls_back_to_the_opening_line_and_renaming_appends() {
        let dir = std::env::temp_dir().join(format!("evlog-{}", uuid::Uuid::new_v4()));
        let mut log = EventLog::open(&dir, "titled").unwrap();

        // No title yet: the first thing said stands in for one.
        log.append(EventKind::UserMessage {
            text: "what broke with the daemon".into(),
        })
        .unwrap();
        let events = EventLog::read(log.path()).unwrap();
        assert_eq!(
            session_title(&events).as_deref(),
            Some("what broke with the daemon")
        );

        // Renaming twice appends twice; the newest wins and neither is lost.
        log.append(EventKind::SessionTitle {
            title: "daemon triage".into(),
        })
        .unwrap();
        log.append(EventKind::SessionTitle {
            title: "snarevec triage".into(),
        })
        .unwrap();
        let events = EventLog::read(log.path()).unwrap();
        assert_eq!(session_title(&events).as_deref(), Some("snarevec triage"));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::SessionTitle { .. }))
                .count(),
            2,
            "a rename must append, not overwrite - the log is append-only"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumes_sequence_after_reopen() {
        let dir = std::env::temp_dir().join(format!("evlog-{}", uuid::Uuid::new_v4()));

        let mut log = EventLog::open(&dir, "resume").unwrap();
        log.append(EventKind::UserMessage { text: "one".into() })
            .unwrap();
        drop(log);

        // Reopening must continue at 2, not restart at 1.
        let mut log = EventLog::open(&dir, "resume").unwrap();
        let seq = log
            .append(EventKind::UserMessage { text: "two".into() })
            .unwrap();
        assert_eq!(seq, 2);

        fs::remove_dir_all(&dir).ok();
    }
}
