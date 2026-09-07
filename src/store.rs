//! SQLite-backed event log. Dedup is enforced by the primary key, so ingesting
//! the same scan repeatedly is cheap and idempotent.

use crate::model::{Tokens, UsageEvent};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::PathBuf;

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the database under the user's data dir.
    pub fn open_default() -> rusqlite::Result<Self> {
        let path = default_db_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::open(&path)
    }

    pub fn open(path: &PathBuf) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                 dedup_key      TEXT PRIMARY KEY,
                 tool           TEXT NOT NULL,
                 ts             INTEGER NOT NULL,  -- unix seconds, UTC
                 model          TEXT NOT NULL,
                 input          INTEGER NOT NULL,
                 output         INTEGER NOT NULL,
                 cache_creation INTEGER NOT NULL,
                 cache_read     INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);",
        )?;
        Ok(Self { conn })
    }

    /// Insert events, ignoring any whose `dedup_key` is already present.
    /// Returns the number of newly stored rows.
    pub fn ingest(&mut self, events: &[UsageEvent]) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO events
                 (dedup_key, tool, ts, model, input, output, cache_creation, cache_read)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for e in events {
                inserted += stmt.execute(params![
                    e.dedup_key,
                    e.tool,
                    e.ts.timestamp(),
                    e.model,
                    e.tokens.input,
                    e.tokens.output,
                    e.tokens.cache_creation,
                    e.tokens.cache_read,
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Sum tokens for one tool since `from` (inclusive), grouped by model.
    pub fn totals_since(
        &self,
        tool: &str,
        from: DateTime<Utc>,
    ) -> rusqlite::Result<Vec<(String, Tokens)>> {
        let mut stmt = self.conn.prepare(
            "SELECT model,
                    SUM(input), SUM(output), SUM(cache_creation), SUM(cache_read)
             FROM events
             WHERE tool = ?1 AND ts >= ?2
             GROUP BY model
             ORDER BY model",
        )?;
        let rows = stmt.query_map(params![tool, from.timestamp()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Tokens {
                    input: r.get::<_, i64>(1)? as u64,
                    output: r.get::<_, i64>(2)? as u64,
                    cache_creation: r.get::<_, i64>(3)? as u64,
                    cache_read: r.get::<_, i64>(4)? as u64,
                },
            ))
        })?;
        rows.collect()
    }

    /// Convenience: single combined [`Tokens`] for one tool since `from`.
    pub fn total_since(&self, tool: &str, from: DateTime<Utc>) -> rusqlite::Result<Tokens> {
        let mut sum = Tokens::default();
        for (_, t) in self.totals_since(tool, from)? {
            sum.add(&t);
        }
        Ok(sum)
    }

    pub fn row_count(&self) -> rusqlite::Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
    }
}

pub fn default_db_path() -> PathBuf {
    directories::ProjectDirs::from("dev", "tokenhud", "tokenhud")
        .map(|d| d.data_dir().join("usage.db"))
        .unwrap_or_else(|| PathBuf::from("tokenhud-usage.db"))
}
