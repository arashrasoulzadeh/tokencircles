//! SQLite-backed event log. Dedup is enforced by the primary key, so ingesting
//! the same scan repeatedly is cheap and idempotent.

use crate::model::{RateLimitStatus, Tokens, UsageEvent};
use chrono::{DateTime, TimeZone, Utc};
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

    /// An ephemeral in-memory store (tests).
    pub fn open_memory() -> rusqlite::Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    pub fn open(path: &PathBuf) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::from_conn(conn)
    }

    fn from_conn(conn: Connection) -> rusqlite::Result<Self> {
        // Schema v2 split cache writes into 5m/1h. Events rebuild from source
        // files on the next scan, so an incompatible old schema is just dropped.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 2 {
            conn.execute_batch("DROP TABLE IF EXISTS events; DROP TABLE IF EXISTS rate_limits;")?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                 dedup_key      TEXT PRIMARY KEY,
                 tool           TEXT NOT NULL,
                 ts             INTEGER NOT NULL,  -- unix seconds, UTC
                 model          TEXT NOT NULL,
                 input          INTEGER NOT NULL,
                 output         INTEGER NOT NULL,
                 cache_write_5m INTEGER NOT NULL,
                 cache_write_1h INTEGER NOT NULL,
                 cache_read     INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);

             CREATE TABLE IF NOT EXISTS rate_limits (
                 tool           TEXT NOT NULL,
                 window_minutes INTEGER NOT NULL,
                 window_label   TEXT NOT NULL,
                 used_percent   REAL NOT NULL,
                 resets_at      INTEGER,           -- unix seconds, UTC, nullable
                 observed_at    INTEGER NOT NULL,  -- unix seconds, UTC
                 PRIMARY KEY (tool, window_minutes)
             );

             -- Per-file (mtime, size) fingerprint so a full rescan only re-reads
             -- the log files that actually changed.
             CREATE TABLE IF NOT EXISTS scan_state (
                 path     TEXT PRIMARY KEY,
                 mtime_ns INTEGER NOT NULL,
                 size     INTEGER NOT NULL
             );",
        )?;
        conn.pragma_update(None, "user_version", 2)?;
        Ok(Self { conn })
    }

    /// Upsert rate-limit readings, keeping whichever row was observed most recently.
    pub fn ingest_rate_limits(&mut self, limits: &[RateLimitStatus]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO rate_limits
                     (tool, window_minutes, window_label, used_percent, resets_at, observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(tool, window_minutes) DO UPDATE SET
                     window_label = excluded.window_label,
                     used_percent = excluded.used_percent,
                     resets_at    = excluded.resets_at,
                     observed_at  = excluded.observed_at
                 WHERE excluded.observed_at >= rate_limits.observed_at",
            )?;
            for l in limits {
                stmt.execute(params![
                    l.tool,
                    l.window_minutes,
                    l.window_label,
                    l.used_percent,
                    l.resets_at.map(|t| t.timestamp()),
                    l.observed_at.timestamp(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All stored rate-limit readings for one tool, ascending by window length.
    pub fn rate_limits(&self, tool: &str) -> rusqlite::Result<Vec<RateLimitStatus>> {
        let mut stmt = self.conn.prepare(
            "SELECT window_minutes, window_label, used_percent, resets_at, observed_at
             FROM rate_limits WHERE tool = ?1 ORDER BY window_minutes",
        )?;
        let rows = stmt.query_map(params![tool], |r| {
            let resets: Option<i64> = r.get(3)?;
            Ok(RateLimitStatus {
                tool: tool.to_string(),
                window_minutes: r.get::<_, i64>(0)? as u64,
                window_label: r.get(1)?,
                used_percent: r.get(2)?,
                resets_at: resets.and_then(|s| Utc.timestamp_opt(s, 0).single()),
                observed_at: Utc
                    .timestamp_opt(r.get::<_, i64>(4)?, 0)
                    .single()
                    .unwrap_or_else(Utc::now),
            })
        })?;
        rows.collect()
    }

    /// Insert events, ignoring any whose `dedup_key` is already present.
    /// Returns the number of newly stored rows.
    pub fn ingest(&mut self, events: &[UsageEvent]) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO events
                 (dedup_key, tool, ts, model, input, output, cache_write_5m, cache_write_1h, cache_read)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for e in events {
                inserted += stmt.execute(params![
                    e.dedup_key,
                    e.tool,
                    e.ts.timestamp(),
                    e.model,
                    e.tokens.input,
                    e.tokens.output,
                    e.tokens.cache_write_5m,
                    e.tokens.cache_write_1h,
                    e.tokens.cache_read,
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Has this file already been scanned at exactly this `(mtime_ns, size)`?
    pub fn file_unchanged(&self, path: &str, mtime_ns: i64, size: i64) -> rusqlite::Result<bool> {
        let hit: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT mtime_ns, size FROM scan_state WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        Ok(hit == Some((mtime_ns, size)))
    }

    /// Record that `path` has been scanned at `(mtime_ns, size)`.
    pub fn mark_file_scanned(&self, path: &str, mtime_ns: i64, size: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO scan_state (path, mtime_ns, size) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET mtime_ns = excluded.mtime_ns, size = excluded.size",
            params![path, mtime_ns, size],
        )?;
        Ok(())
    }

    /// Distinct event timestamps for one tool since `from`, ascending.
    pub fn event_times(&self, tool: &str, from: DateTime<Utc>) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts FROM events WHERE tool = ?1 AND ts >= ?2 ORDER BY ts")?;
        let rows = stmt.query_map(params![tool, from.timestamp()], |r| r.get::<_, i64>(0))?;
        rows.collect()
    }

    /// Sum tokens for one tool since `from` (inclusive), grouped by model.
    pub fn totals_since(
        &self,
        tool: &str,
        from: DateTime<Utc>,
    ) -> rusqlite::Result<Vec<(String, Tokens)>> {
        let mut stmt = self.conn.prepare(
            "SELECT model,
                    SUM(input), SUM(output),
                    SUM(cache_write_5m), SUM(cache_write_1h), SUM(cache_read)
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
                    cache_write_5m: r.get::<_, i64>(3)? as u64,
                    cache_write_1h: r.get::<_, i64>(4)? as u64,
                    cache_read: r.get::<_, i64>(5)? as u64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tokens;
    use chrono::Duration;

    fn ev(key: &str, ago_secs: i64, t: Tokens) -> UsageEvent {
        UsageEvent {
            dedup_key: key.into(),
            tool: "claude",
            ts: Utc::now() - Duration::seconds(ago_secs),
            model: "claude-sonnet-5".into(),
            tokens: t,
        }
    }

    fn tok(input: u64, read: u64) -> Tokens {
        Tokens {
            input,
            cache_read: read,
            ..Default::default()
        }
    }

    #[test]
    fn ingest_is_idempotent_on_dedup_key() {
        let mut s = Store::open_memory().unwrap();
        let batch = vec![ev("a", 10, tok(100, 0)), ev("b", 10, tok(200, 0))];
        assert_eq!(s.ingest(&batch).unwrap(), 2);
        assert_eq!(s.ingest(&batch).unwrap(), 0); // same keys → nothing new
        assert_eq!(s.row_count().unwrap(), 2);
    }

    #[test]
    fn totals_since_respects_the_window() {
        let mut s = Store::open_memory().unwrap();
        s.ingest(&[
            ev("recent", 60, tok(10, 5)),
            ev("old", 7 * 24 * 3600, tok(999, 999)),
        ])
        .unwrap();
        let since = Utc::now() - Duration::hours(1);
        let total = s.total_since("claude", since).unwrap();
        assert_eq!(total.input, 10);
        assert_eq!(total.cache_read, 5);
    }

    #[test]
    fn file_fingerprint_tracks_changes() {
        let s = Store::open_memory().unwrap();
        // Unknown file → not "unchanged".
        assert!(!s.file_unchanged("/a.jsonl", 100, 10).unwrap());

        s.mark_file_scanned("/a.jsonl", 100, 10).unwrap();
        assert!(s.file_unchanged("/a.jsonl", 100, 10).unwrap());
        // A new mtime or size means it changed.
        assert!(!s.file_unchanged("/a.jsonl", 101, 10).unwrap());
        assert!(!s.file_unchanged("/a.jsonl", 100, 12).unwrap());

        // Re-marking updates the fingerprint.
        s.mark_file_scanned("/a.jsonl", 101, 20).unwrap();
        assert!(s.file_unchanged("/a.jsonl", 101, 20).unwrap());
        assert!(!s.file_unchanged("/a.jsonl", 100, 10).unwrap());
    }

    #[test]
    fn rate_limit_upsert_keeps_newest_observation() {
        let mut s = Store::open_memory().unwrap();
        let mk = |pct: f64, observed_ago: i64| RateLimitStatus {
            tool: "codex".into(),
            window_label: "weekly".into(),
            window_minutes: 10080,
            used_percent: pct,
            resets_at: None,
            observed_at: Utc::now() - Duration::minutes(observed_ago),
        };
        s.ingest_rate_limits(&[mk(50.0, 60)]).unwrap();
        s.ingest_rate_limits(&[mk(80.0, 5)]).unwrap(); // newer → wins
        s.ingest_rate_limits(&[mk(10.0, 600)]).unwrap(); // older → ignored

        let got = s.rate_limits("codex").unwrap();
        assert_eq!(got.len(), 1);
        assert!((got[0].used_percent - 80.0).abs() < 1e-9);
    }

    #[test]
    fn rate_limits_keyed_per_window_and_round_trip_reset() {
        let mut s = Store::open_memory().unwrap();
        let reset = Utc::now() + Duration::hours(3);
        s.ingest_rate_limits(&[
            RateLimitStatus {
                tool: "claude".into(),
                window_label: "5h".into(),
                window_minutes: 300,
                used_percent: 18.0,
                resets_at: Some(reset),
                observed_at: Utc::now(),
            },
            RateLimitStatus {
                tool: "claude".into(),
                window_label: "weekly".into(),
                window_minutes: 10_080,
                used_percent: 17.0,
                resets_at: None,
                observed_at: Utc::now(),
            },
        ])
        .unwrap();
        let got = s.rate_limits("claude").unwrap();
        assert_eq!(got.len(), 2); // ordered by window_minutes: 5h then weekly
        assert_eq!(got[0].window_label, "5h");
        assert_eq!(got[0].resets_at.unwrap().timestamp(), reset.timestamp());
        assert!(got[1].resets_at.is_none());
        assert!(s.rate_limits("codex").unwrap().is_empty());
    }

    #[test]
    fn totals_since_groups_by_model() {
        let mut s = Store::open_memory().unwrap();
        s.ingest(&[
            ev_model("a", "claude-sonnet-5", 60, 10),
            ev_model("b", "claude-sonnet-5", 60, 20),
            ev_model("c", "claude-opus-5", 60, 5),
        ])
        .unwrap();
        let mut got = s
            .totals_since("claude", Utc::now() - Duration::hours(1))
            .unwrap();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "claude-opus-5");
        assert_eq!(got[0].1.input, 5);
        assert_eq!(got[1].0, "claude-sonnet-5");
        assert_eq!(got[1].1.input, 30);
    }

    #[test]
    fn event_times_are_ascending_and_windowed() {
        let mut s = Store::open_memory().unwrap();
        s.ingest(&[
            ev("new", 100, tok(1, 0)),
            ev("mid", 3600, tok(1, 0)),
            ev("old", 100_000, tok(1, 0)),
        ])
        .unwrap();
        let ts = s
            .event_times("claude", Utc::now() - Duration::hours(2))
            .unwrap();
        assert_eq!(ts.len(), 2, "the 100_000s-old one is excluded");
        assert!(ts[0] <= ts[1]);
    }

    #[test]
    fn opening_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("tokenhud-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.ingest(&[ev("persist", 10, tok(42, 0))]).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.row_count().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ev_model(key: &str, model: &str, ago_secs: i64, input: u64) -> UsageEvent {
        UsageEvent {
            dedup_key: key.into(),
            tool: "claude",
            ts: Utc::now() - Duration::seconds(ago_secs),
            model: model.into(),
            tokens: tok(input, 0),
        }
    }
}
