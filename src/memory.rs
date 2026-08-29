//! Vector memory (§4b) - derived from the event log, never written directly.
//!
//! No vector database. At personal scale one earns nothing: 100k chunks at 384
//! dims is ~153 MB and scoring all of them is ~38M multiply-adds, which is
//! single-digit milliseconds. Vectors live as BLOBs in SQLite and are scored by
//! brute-force cosine. Revisit past ~1M chunks.
//!
//! Vectors are L2-normalised on the way in, so cosine similarity is just a dot
//! product at query time.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

/// A context-carrying slice of the event log.
///
/// Deliberately a whole turn rather than a single line: MiniLM scored two
/// clearly-related short phrases at only ~0.22 cosine during Phase 0 testing.
/// Short abstract fragments embed badly, so chunks need surrounding context.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub session_id: String,
    pub seq_start: u64,
    pub seq_end: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub score: f32,
    pub chunk: Chunk,
    /// "global" (rebuilt by the reducer) or "session" (written by /compact).
    pub scope: String,
}

pub struct VectorStore {
    conn: Connection,
}

impl VectorStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening vector store {}", path.display()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chunks (
                 id         INTEGER PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 seq_start  INTEGER NOT NULL,
                 seq_end    INTEGER NOT NULL,
                 text       TEXT NOT NULL,
                 dim        INTEGER NOT NULL,
                 embedding  BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS chunks_session ON chunks(session_id);",
        )
        .context("creating vector store schema")?;

        // `scope` separates general memory (rebuilt wholesale by the reducer)
        // from session memory written by /compact, which must survive a rebuild
        // and be preferable in recall. Added by migration so existing stores
        // pick it up; the error when it already exists is the expected path.
        let _ = conn.execute(
            "ALTER TABLE chunks ADD COLUMN scope TEXT NOT NULL DEFAULT 'global'",
            [],
        );
        conn.execute_batch("CREATE INDEX IF NOT EXISTS chunks_scope ON chunks(scope);")
            .context("indexing scope")?;

        Ok(Self { conn })
    }

    /// Drop general memory before a rebuild, so the derived state stays a pure
    /// function of the event log rather than an accumulation.
    ///
    /// Session-scoped chunks written by `/compact` are deliberately left alone:
    /// they are what a long-running conversation is leaning on right now, and
    /// wiping them mid-session because an unrelated `reduce` ran would be a
    /// nasty surprise. They remain reproducible from the same log.
    pub fn clear(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM chunks WHERE scope = 'global'", [])
            .context("clearing general memory")?;
        Ok(())
    }

    /// Remove one session's compacted chunks, so re-compacting replaces rather
    /// than duplicates.
    pub fn clear_session(&self, session_id: &str) -> Result<usize> {
        let n = self
            .conn
            .execute(
                "DELETE FROM chunks WHERE scope = 'session' AND session_id = ?1",
                params![session_id],
            )
            .context("clearing session memory")?;
        Ok(n)
    }

    pub fn count_scope(&self, scope: &str) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT count(*) FROM chunks WHERE scope = ?1",
                params![scope],
                |r| r.get(0),
            )
            .context("counting scope")?;
        Ok(n as usize)
    }

    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
            .context("counting chunks")?;
        Ok(n as usize)
    }

    pub fn insert(&self, chunk: &Chunk, embedding: &[f32]) -> Result<()> {
        self.insert_scoped(chunk, embedding, "global")
    }

    pub fn insert_scoped(&self, chunk: &Chunk, embedding: &[f32], scope: &str) -> Result<()> {
        let normalised = normalise(embedding);
        self.conn
            .execute(
                "INSERT INTO chunks (session_id, seq_start, seq_end, text, dim, embedding, scope)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    chunk.session_id,
                    chunk.seq_start as i64,
                    chunk.seq_end as i64,
                    chunk.text,
                    normalised.len() as i64,
                    to_blob(&normalised),
                    scope,
                ],
            )
            .context("inserting chunk")?;
        Ok(())
    }

    /// Brute-force cosine over every stored vector.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<Hit>> {
        let q = normalise(query);

        let mut stmt = self
            .conn
            .prepare("SELECT session_id, seq_start, seq_end, text, embedding, scope FROM chunks")
            .context("preparing search")?;

        let mut hits: Vec<Hit> = stmt
            .query_map([], |row| {
                let blob: Vec<u8> = row.get(4)?;
                Ok((
                    Chunk {
                        session_id: row.get(0)?,
                        seq_start: row.get::<_, i64>(1)? as u64,
                        seq_end: row.get::<_, i64>(2)? as u64,
                        text: row.get(3)?,
                    },
                    from_blob(&blob),
                    row.get::<_, String>(5).unwrap_or_else(|_| "global".into()),
                ))
            })
            .context("scanning chunks")?
            .filter_map(|r| r.ok())
            .filter(|(_, v, _)| v.len() == q.len())
            .map(|(chunk, v, scope)| Hit {
                score: dot(&q, &v),
                chunk,
                scope,
            })
            .collect();

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        Ok(hits)
    }
}

fn normalise(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ranks_by_similarity() {
        let dir = std::env::temp_dir().join(format!("vec-{}", uuid::Uuid::new_v4()));
        let store = VectorStore::open(dir.join("v.db")).unwrap();

        let a = Chunk {
            session_id: "s".into(),
            seq_start: 1,
            seq_end: 2,
            text: "about cats".into(),
        };
        let b = Chunk {
            session_id: "s".into(),
            seq_start: 3,
            seq_end: 4,
            text: "about ships".into(),
        };
        store.insert(&a, &[1.0, 0.0, 0.0]).unwrap();
        store.insert(&b, &[0.0, 1.0, 0.0]).unwrap();
        assert_eq!(store.count().unwrap(), 2);

        // A query pointing along the first axis must rank "cats" first.
        let hits = store.search(&[0.9, 0.1, 0.0], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk.text, "about cats");
        assert!(hits[0].score > hits[1].score);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }
}
