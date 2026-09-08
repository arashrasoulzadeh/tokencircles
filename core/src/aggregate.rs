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
    /// Estimated end of the current rolling 5-hour usage block, if one is active.
    #[serde(with = "chrono::serde::ts_seconds_option")]
    pub five_h_reset: Option<DateTime<Utc>>,
    /// Whole minutes until `five_h_reset` (convenience for the frontend).
    pub five_h_minutes_left: Option<i64>,
}

/// Fallback 5-hour reset estimate for when the Claude plan-usage file isn't
/// available (CLI-only): 5h after the first message of the current activity
/// block, where a gap of ≥5h starts a new block.
fn five_h_block_reset(times: &[i64], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    const FIVE_H: i64 = 5 * 3600;
    let mut block_start = None::<i64>;
    let mut prev = None::<i64>;
    for &ts in times {
        match block_start {
            None => block_start = Some(ts),
            Some(bs) => {
                let gap = prev.map_or(0, |p| ts - p);
                if ts - bs >= FIVE_H || gap >= FIVE_H {
                    block_start = Some(ts);
                }
            }
        }
        prev = Some(ts);
    }
    let end = DateTime::from_timestamp(block_start? + FIVE_H, 0)?;
    (end > now).then_some(end)
}

pub fn snapshot(store: &Store, tool: &str, config: &Config) -> rusqlite::Result<ToolSnapshot> {
    let w = Windows::at(Local::now());
    let caps: Caps = config.caps_for(tool);

    let hour = store.totals_since(tool, w.hour_start)?;
    let five_h = store.totals_since(tool, w.five_h_start)?;
    let week = store.totals_since(tool, w.week_start)?;

    let week_by_model = week.iter().map(|(m, t)| (m.clone(), t.total())).collect();

    let now = Utc::now();
    let rate_limits = store.rate_limits(tool)?;

    // Prefer a reset the tool/plan reports (Claude derives it from its own
    // plan-usage history); otherwise estimate the block from our event log.
    let five_h_reset = rate_limits
        .iter()
        .find(|r| r.window_label == "5h")
        .and_then(|r| r.resets_at)
        .filter(|r| *r > now)
        .or_else(|| {
            let times = store
                .event_times(tool, now - Duration::hours(12))
                .unwrap_or_default();
            five_h_block_reset(&times, now)
        });
    let five_h_minutes_left = five_h_reset.map(|r| (r - now).num_minutes());

    let mut snap = ToolSnapshot {
        tool: tool.to_string(),
        hour: window_stat(&hour, caps.hour),
        five_h: window_stat(&five_h, caps.five_h),
        week: window_stat(&week, caps.week),
        week_by_model,
        rate_limits,
        advisories: Vec::new(),
        five_h_reset,
        five_h_minutes_left,
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
        if let Some(m) = self.five_h_minutes_left {
            println!("  5h block ends in ~{}h{:02}m", m / 60, m % 60);
        }
        for (m, total) in &self.week_by_model {
            println!("    {m:<22} {total:>13}");
        }
        for rl in &self.rate_limits {
            println!(
                "  reported {:<8} {:>3.0}% used",
                rl.window_label, rl.used_percent
            );
        }
        for a in &self.advisories {
            println!("  ! {}", a.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_starts_monday_local_midnight() {
        // 2026-09-09 is a Wednesday.
        let now = Local
            .with_ymd_and_hms(2026, 9, 9, 15, 30, 0)
            .single()
            .unwrap();
        let w = Windows::at(now);
        let ws = w.week_start.with_timezone(&Local);
        assert_eq!(ws.weekday(), chrono::Weekday::Mon);
        assert_eq!((ws.hour(), ws.minute(), ws.second()), (0, 0, 0));
        // Monday of that week is the 7th.
        assert_eq!(ws.day(), 7);
    }

    #[test]
    fn five_h_block_reset_from_first_activity() {
        // Start exactly on an hour boundary so the floor is a no-op.
        let start = 1_000_000 - (1_000_000 % 3600); // 999_000
        let now = DateTime::from_timestamp(start + 3600, 0).unwrap(); // 1h into the block
        let end = five_h_block_reset(&[start, start + 600, now.timestamp() - 60], now).unwrap();
        let mins = (end - now).num_minutes();
        assert_eq!(mins, 240, "5h block, 1h elapsed → 4h left");
    }

    #[test]
    fn five_h_block_reset_none_when_expired() {
        let now = DateTime::from_timestamp(1_000_000, 0).unwrap();
        // Only old activity, 6h ago → the block already reset.
        let old = now.timestamp() - 6 * 3600;
        assert!(five_h_block_reset(&[old, old + 300], now).is_none());
    }

    #[test]
    fn five_h_block_reset_starts_new_block_after_gap() {
        let now = DateTime::from_timestamp(2_000_000, 0).unwrap();
        let long_ago = now.timestamp() - 20 * 3600;
        let recent = now.timestamp() - 1800; // 30 min ago, after a >5h gap
        let end = five_h_block_reset(&[long_ago, recent], now).unwrap();
        assert!((end - now).num_minutes() > 240);
    }

    #[test]
    fn hour_and_5h_windows() {
        let now = Local
            .with_ymd_and_hms(2026, 9, 9, 15, 30, 0)
            .single()
            .unwrap();
        let w = Windows::at(now);
        assert_eq!(w.hour_start.with_timezone(&Local).hour(), 15);
        assert_eq!(w.hour_start.with_timezone(&Local).minute(), 0);
        assert_eq!((now.to_utc() - w.five_h_start).num_hours(), 5);
    }

    #[test]
    fn window_stat_computes_ratio_and_remaining() {
        let by_model = vec![(
            "claude-sonnet-5".to_string(),
            Tokens {
                input: 100,
                output: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: 0,
            },
        )];
        let with_cap = window_stat(&by_model, Some(400));
        assert_eq!(with_cap.total, 100);
        assert_eq!(with_cap.ratio, Some(0.25));
        assert_eq!(with_cap.remaining, Some(300));

        let no_cap = window_stat(&by_model, None);
        assert_eq!(no_cap.ratio, None);
        assert_eq!(no_cap.remaining, None);
    }
}
