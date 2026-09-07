//! Rolling-window definitions and a snapshot the HUD can render.

use crate::model::Tokens;
use crate::store::Store;
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};

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

/// Per-window totals for a single tool.
pub struct ToolSnapshot {
    pub tool: String,
    pub hour: Tokens,
    pub five_h: Tokens,
    pub week: Tokens,
    pub week_by_model: Vec<(String, Tokens)>,
}

pub fn snapshot(store: &Store, tool: &str) -> rusqlite::Result<ToolSnapshot> {
    let w = Windows::at(Local::now());
    Ok(ToolSnapshot {
        tool: tool.to_string(),
        hour: store.total_since(tool, w.hour_start)?,
        five_h: store.total_since(tool, w.five_h_start)?,
        week: store.total_since(tool, w.week_start)?,
        week_by_model: store.totals_since(tool, w.week_start)?,
    })
}

impl ToolSnapshot {
    pub fn print(&self) {
        let now = Local::now();
        println!("[{}]", self.tool);
        let row = |label: &str, t: &Tokens| {
            println!(
                "  {label:<14} {:>13}  (in {}, out {}, cache-w {}, cache-r {})",
                t.total(),
                t.input,
                t.output,
                t.cache_creation,
                t.cache_read
            );
        };
        row(&format!("hour {}:00", now.format("%H")), &self.hour);
        row("trailing 5h", &self.five_h);
        row("this week", &self.week);
        for (m, t) in &self.week_by_model {
            println!("    {m:<20} {:>13}", t.total());
        }
    }
}
