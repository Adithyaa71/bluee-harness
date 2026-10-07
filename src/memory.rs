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
    /// "global" (rebuilt by the reducer), "session" (live-indexed as the
    /// conversation happens, and by /compact) or "code" (the repo index).
    pub scope: String,
    /// Row id, which is what joins a dense hit to its keyword rank.
    pub id: i64,
}

pub struct VectorStore {
    conn: Connection,
    /// Whether the FTS5 keyword index exists. False means dense-only search.
    fts: bool,
}

/// Reciprocal-rank-fusion constant. 60 is the value from the original paper;
/// it damps the gap between rank 1 and rank 3 so neither retriever can
/// dominate on one lucky hit.
const RRF_K: f32 = 60.0;
/// How deep each retriever looks before fusion.
const CANDIDATES: usize = 50;

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

        // Keyword index beside the vectors. Dense retrieval is weak on exact
        // tokens - names, error codes, paths, ids - and those are exactly what
        // gets asked about ("the 402", "read_stream"). FTS5 ships inside the
        // bundled SQLite, so this is a virtual table, not a dependency.
        //
        // External-content, kept in step by triggers, so every existing
        // delete path (clear, forget_session, clear_scope...) maintains it
        // without knowing it exists. If FTS5 is somehow unavailable the store
        // still works, dense-only - recall degrades, nothing breaks.
        let fts = conn
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts
                     USING fts5(text, content='chunks', content_rowid='id');
                 CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
                     INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
                 END;
                 CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
                     INSERT INTO chunks_fts(chunks_fts, rowid, text)
                         VALUES ('delete', old.id, old.text);
                 END;",
            )
            .is_ok();
        if fts {
            // Backfill a store that predates the index. `rebuild` re-reads the
            // content table, so it is exact rather than incremental.
            let indexed: i64 = conn
                .query_row("SELECT count(*) FROM chunks_fts_docsize", [], |r| r.get(0))
                .unwrap_or(-1);
            let rows: i64 = conn
                .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
                .unwrap_or(0);
            if indexed != rows {
                let _ = conn.execute("INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild')", []);
            }
        }

        Ok(Self { conn, fts })
    }

    /// Where each of this session's live-indexed chunks starts, so a turn
    /// only embeds what is new rather than the whole conversation again.
    pub fn session_seq_starts(&self, session_id: &str) -> Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq_start FROM chunks WHERE scope = 'session' AND session_id = ?1",
        )?;
        let set = stmt
            .query_map(params![session_id], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|n| n as u64)
            .collect();
        Ok(set)
    }

    /// SQL condition for one memory tier, matching `tools::scope_keep` so the
    /// Memory page's browse and its search agree on what "recent" means.
    fn tier_sql(scope: &str) -> &'static str {
        match scope {
            "code" => "scope = 'code'",
            "session" => "scope <> 'code' AND session_id = ?1",
            "recent" => "scope <> 'code' AND session_id >= ?2",
            _ => "scope <> 'code'",
        }
    }

    /// Browse one tier, newest first (code: by file). A turn indexed twice -
    /// live `session` copy plus the reducer's `global` one - is listed once.
    /// Returns the total for the tier and one page of it.
    pub fn browse(
        &self,
        scope: &str,
        session: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<(Chunk, String)>)> {
        let week_ago = (chrono::Local::now() - chrono::Duration::days(7))
            .format("%Y%m%d")
            .to_string();
        let cond = Self::tier_sql(scope);
        let order = if scope == "code" {
            "session_id ASC, seq_start ASC"
        } else {
            "session_id DESC, seq_start DESC"
        };
        let total: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM chunks WHERE {cond} \
                 AND (?1 IS NOT NULL) AND (?2 IS NOT NULL) GROUP BY session_id, seq_start)"
            ),
            params![session, week_ago],
            |r| r.get(0),
        )?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT session_id, seq_start, max(seq_end), max(text), min(scope) FROM chunks \
             WHERE {cond} AND (?1 IS NOT NULL) AND (?2 IS NOT NULL) \
             GROUP BY session_id, seq_start ORDER BY {order} LIMIT ?3 OFFSET ?4"
        ))?;
        let rows = stmt
            .query_map(params![session, week_ago, limit as i64, offset as i64], |r| {
                Ok((
                    Chunk {
                        session_id: r.get(0)?,
                        seq_start: r.get::<_, i64>(1)? as u64,
                        seq_end: r.get::<_, i64>(2)? as u64,
                        text: r.get(3)?,
                    },
                    r.get::<_, String>(4)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok((total as usize, rows))
    }

    /// How many distinct turns each tier holds, for the Memory page's chips.
    pub fn tier_counts(&self, session: &str) -> Result<serde_json::Value> {
        let mut out = serde_json::Map::new();
        for tier in ["session", "recent", "all", "code"] {
            let (n, _) = self.browse(tier, session, 0, 0)?;
            out.insert(tier.into(), serde_json::json!(n));
        }
        Ok(serde_json::Value::Object(out))
    }

    /// Hybrid recall: dense cosine and BM25 keyword ranks, fused by RRF.
    ///
    /// Why both: MiniLM scores two clearly related short phrases at ~0.22 and
    /// gibberish at ~0.30 (§31), so on exact tokens - an error code, a file
    /// name, a symbol - dense ranking is close to noise. BM25 is the opposite:
    /// exact, and blind to paraphrase. Fusing ranks rather than scores means
    /// neither scale has to be calibrated against the other.
    ///
    /// `score` on each hit stays the cosine, because the UI's relevance bar
    /// and the "nothing matched strongly" banner are calibrated against it.
    /// Order is the fused order. Duplicates of one turn (the live `session`
    /// copy and the reducer's `global` copy) collapse to one.
    pub fn search_hybrid(&self, text: &str, query: &[f32], k: usize) -> Result<Vec<Hit>> {
        self.search_where(text, query, k, |_| true)
    }

    /// Hybrid search restricted to hits `keep` accepts - this conversation
    /// only, the last week, conversations but not code. Filtering happens
    /// BEFORE ranking, so a narrow scope still gets its full k results rather
    /// than whatever survived of a global top-k.
    pub fn search_where(
        &self,
        text: &str,
        query: &[f32],
        k: usize,
        keep: impl Fn(&Hit) -> bool,
    ) -> Result<Vec<Hit>> {
        use std::collections::HashMap;
        let mut dense = self.search(query, usize::MAX)?;
        dense.retain(|h| keep(h));
        let keyword = self.keyword_ids(text, CANDIDATES);

        let by_id: HashMap<i64, usize> =
            dense.iter().enumerate().map(|(i, h)| (h.id, i)).collect();
        // Keyed by turn, not row, so two copies of one turn fuse into one.
        let mut fused: HashMap<(String, u64), (f32, usize)> = HashMap::new();
        let mut add = |i: usize, rank: usize| {
            let h = &dense[i];
            let e = fused
                .entry((h.chunk.session_id.clone(), h.chunk.seq_start))
                .or_insert((0.0, i));
            e.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
        };
        for rank in 0..dense.len().min(CANDIDATES) {
            add(rank, rank);
        }
        for (rank, id) in keyword.iter().enumerate() {
            if let Some(&i) = by_id.get(id) {
                add(i, rank);
            }
        }

        let mut ranked: Vec<(f32, usize)> = fused.into_values().collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(ranked.into_iter().take(k).map(|(_, i)| dense[i].clone()).collect())
    }

    /// Row ids by BM25 rank. Each word is quoted, so punctuation in a query
    /// (a path, `a-b`, `foo()`) is matched as text rather than parsed as FTS
    /// syntax and rejected.
    fn keyword_ids(&self, text: &str, n: usize) -> Vec<i64> {
        if !self.fts {
            return Vec::new();
        }
        let terms: Vec<String> = text
            .split_whitespace()
            .map(|w| w.replace('"', ""))
            .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
            .map(|w| format!("\"{w}\""))
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let q = terms.join(" OR ");
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![q, n as i64], |r| r.get::<_, i64>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
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

    /// Everything one session put here, whatever scope it landed in.
    ///
    /// Used when a session is deleted: its log is the source of truth, so once
    /// that is gone the derived rows are evidence for nothing and should go
    /// with it rather than waiting for the next full rebuild.
    pub fn forget_session(&self, session_id: &str) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM chunks WHERE session_id = ?1", params![session_id])
            .with_context(|| format!("forgetting session {session_id}"))?;
        Ok(n)
    }

    /// Drop everything in one scope. Used to rebuild the code index without
    /// touching what the event log or a live `/compact` put there.
    pub fn clear_scope(&self, scope: &str) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM chunks WHERE scope = ?1", params![scope])
            .with_context(|| format!("clearing {scope} memory"))?;
        Ok(n)
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
            .prepare("SELECT session_id, seq_start, seq_end, text, embedding, scope, id FROM chunks")
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
                    row.get::<_, i64>(6)?,
                ))
            })
            .context("scanning chunks")?
            .filter_map(|r| r.ok())
            .filter(|(_, v, _, _)| v.len() == q.len())
            .map(|(chunk, v, scope, id)| Hit {
                score: dot(&q, &v),
                chunk,
                scope,
                id,
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

        // Hybrid: an exact token the vectors know nothing about still wins.
        let c = Chunk {
            session_id: "s".into(),
            seq_start: 5,
            seq_end: 6,
            text: "provider returned 402 Payment Required".into(),
        };
        store.insert(&c, &[0.0, 0.0, 1.0]).unwrap();
        let hits = store.search_hybrid("the 402 error", &[0.9, 0.1, 0.0], 3).unwrap();
        assert_eq!(hits[0].chunk.text, c.text, "keyword match should fuse to the top");

        // The same turn indexed twice (live session copy + reducer's global
        // copy) must come back once.
        store.insert_scoped(&c, &[0.0, 0.0, 1.0], "session").unwrap();
        let hits = store.search_hybrid("402", &[0.0, 0.0, 1.0], 5).unwrap();
        assert_eq!(hits.iter().filter(|h| h.chunk.seq_start == 5).count(), 1);

        // Browse lists that turn once although it is stored twice.
        let (n, rows) = store.browse("all", "s", 0, 10).unwrap();
        assert_eq!(n, 3, "cats, ships, and the 402 turn once");
        assert_eq!(rows.iter().filter(|(c, _)| c.seq_start == 5).count(), 1);
        let (n, _) = store.browse("session", "s", 0, 10).unwrap();
        assert_eq!(n, 3);
        let (n, _) = store.browse("session", "other", 0, 10).unwrap();
        assert_eq!(n, 0);

        // clear() drops only global. The session copy stays, and so does its
        // keyword entry - the delete trigger removed only the deleted rows.
        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.keyword_ids("402", 5).len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
