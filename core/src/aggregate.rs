//! Rolling-window definitions and a snapshot the HUD can render.

use crate::model::{RateLimitStatus, Tokens};
use crate::store::Store;
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use serde::Serialize;

/// Start instants for the windows we report on, anchored to local wall-clock time.
pub struct Windows {
    pub hour_start: DateTime<Utc>,
    pub five_h_start: DateTime<Utc>,
    pub week_start: DateTime<Utc>,
}

impl Windows {
    pub fn at(now: DateTime<Local>) -> Self {
        let hour_start = now
            .date_naive()
            .and_hms_opt(now.hour(), 0, 0)
            .and_then(|n| Local.from_local_datetime(&n).single())
            .unwrap_or(now);

        // Monday 00:00 local of the current ISO week.
        let days_from_monday = now.weekday().num_days_from_monday() as i64;
        let week_start = (now - Duration::days(days_from_monday))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|n| Local.from_local_datetime(&n).single())
            .unwrap_or(now);

        Self {
            hour_start: hour_start.to_utc(),
            five_h_start: (now - Duration::hours(5)).to_utc(),
            week_start: week_start.to_utc(),
        }
    }
}

/// Totals for one rolling window, in the shape the HUD renders.
#[derive(Debug, Clone, Serialize)]
pub struct WindowStat {
    pub tokens: Tokens,
    /// `tokens.total()`, precomputed for the frontend.
    pub total: u64,
    /// Rate-limit-weighted total (see [`Tokens::weighted`]).
    pub weighted: u64,
}

impl From<Tokens> for WindowStat {
    fn from(t: Tokens) -> Self {
        Self {
            total: t.total(),
            weighted: t.weighted(),
            tokens: t,
        }
    }
}

/// Per-window totals for a single tool.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSnapshot {
    pub tool: String,
    pub hour: WindowStat,
    pub five_h: WindowStat,
    pub week: WindowStat,
    pub week_by_model: Vec<(String, u64)>,
    /// Authoritative percentages the tool reports about itself (Codex only today).
    pub rate_limits: Vec<RateLimitStatus>,
}

pub fn snapshot(store: &Store, tool: &str) -> rusqlite::Result<ToolSnapshot> {
    let w = Windows::at(Local::now());
    Ok(ToolSnapshot {
        tool: tool.to_string(),
        hour: store.total_since(tool, w.hour_start)?.into(),
        five_h: store.total_since(tool, w.five_h_start)?.into(),
        week: store.total_since(tool, w.week_start)?.into(),
        week_by_model: store
            .totals_since(tool, w.week_start)?
            .into_iter()
            .map(|(m, t)| (m, t.total()))
            .collect(),
        rate_limits: store.rate_limits(tool)?,
    })
}

impl ToolSnapshot {
    pub fn print(&self) {
        let now = Local::now();
        println!("[{}]", self.tool);
        let row = |label: &str, s: &WindowStat| {
            let t = &s.tokens;
            println!(
                "  {label:<14} {:>13}  weighted {:>12}  (in {}, out {}, cache-w {}, cache-r {})",
                s.total, s.weighted, t.input, t.output, t.cache_creation, t.cache_read
            );
        };
        row(&format!("hour {}:00", now.format("%H")), &self.hour);
        row("trailing 5h", &self.five_h);
        row("this week", &self.week);
        for (m, total) in &self.week_by_model {
            println!("    {m:<20} {total:>13}");
        }
        for rl in &self.rate_limits {
            let resets = rl
                .resets_at
                .map(|t| format!(", resets {}", t.with_timezone(&Local).format("%a %H:%M")))
                .unwrap_or_default();
            println!(
                "  reported {:<10} {:>5.1}% used{}",
                rl.window_label, rl.used_percent, resets
            );
        }
    }
}
