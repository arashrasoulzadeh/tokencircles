//! Rolling-window definitions and the snapshot the HUD renders.

use crate::advisor::{self, Advisory};
use crate::config::{Caps, Config};
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
    /// Non-cache-read tokens.
    pub fresh: u64,
    /// Estimated USD cost across the models seen in this window.
    pub cost_usd: f64,
    /// Configured cap for this window, if the user set one.
    pub cap: Option<u64>,
    /// `total / cap`, if a cap is set.
    pub ratio: Option<f64>,
    /// `cap - total`, if a cap is set (saturating at 0).
    pub remaining: Option<u64>,
}

fn window_stat(by_model: &[(String, Tokens)], cap: Option<u64>) -> WindowStat {
    let mut tokens = Tokens::default();
    let mut cost = 0.0;
    for (model, t) in by_model {
        tokens.add(t);
        cost += t.cost_usd(model);
    }
    let total = tokens.total();
    WindowStat {
        cap,
        ratio: cap.map(|c| total as f64 / c as f64),
        remaining: cap.map(|c| c.saturating_sub(total)),
        total,
        fresh: tokens.fresh(),
        cost_usd: (cost * 100.0).round() / 100.0,
        tokens,
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
    pub advisories: Vec<Advisory>,
}

pub fn snapshot(store: &Store, tool: &str, config: &Config) -> rusqlite::Result<ToolSnapshot> {
    let w = Windows::at(Local::now());
    let caps: Caps = config.caps_for(tool);

    let hour = store.totals_since(tool, w.hour_start)?;
    let five_h = store.totals_since(tool, w.five_h_start)?;
    let week = store.totals_since(tool, w.week_start)?;

    let week_by_model = week
        .iter()
        .map(|(m, t)| (m.clone(), t.total()))
        .collect();

    let mut snap = ToolSnapshot {
        tool: tool.to_string(),
        hour: window_stat(&hour, caps.hour),
        five_h: window_stat(&five_h, caps.five_h),
        week: window_stat(&week, caps.week),
        week_by_model,
        rate_limits: store.rate_limits(tool)?,
        advisories: Vec::new(),
    };
    snap.advisories = advisor::advise(&snap);
    Ok(snap)
}

impl ToolSnapshot {
    pub fn print(&self) {
        println!("[{}]", self.tool);
        let row = |label: &str, w: &WindowStat| {
            let cap = match w.ratio {
                Some(r) => format!("  {:>5.0}% of cap", r * 100.0),
                None => String::new(),
            };
            println!(
                "  {label:<12} {:>13} tok  ${:>8.2}{cap}",
                w.total, w.cost_usd
            );
        };
        row("hour", &self.hour);
        row("trailing 5h", &self.five_h);
        row("this week", &self.week);
        for (m, total) in &self.week_by_model {
            println!("    {m:<22} {total:>13}");
        }
        for a in &self.advisories {
            println!("  ! {}", a.text);
        }
    }
}
