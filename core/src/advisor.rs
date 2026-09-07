//! Turns a snapshot into short, plain-language advice: burn rate, when a cap
//! runs out at the current pace, and whether a window is close to its limit.

use crate::aggregate::ToolSnapshot;
use crate::model::RateLimitStatus;
use chrono::{Local, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct Advisory {
    pub severity: Severity,
    pub text: String,
}

/// Build advisories for one tool's snapshot.
pub fn advise(snap: &ToolSnapshot) -> Vec<Advisory> {
    let mut out = Vec::new();

    // 1. Authoritative rate limits (Codex) win — no guessing needed.
    for rl in &snap.rate_limits {
        out.push(from_rate_limit(rl));
    }

    // 2. Estimated-cap advisories for windows that have a configured cap.
    if let Some(cap) = snap.week.cap {
        let used = snap.week.tokens.total();
        let burn_per_hour = snap.five_h.tokens.total() as f64 / 5.0;
        if used >= cap {
            out.push(Advisory {
                severity: Severity::Critical,
                text: "Weekly cap reached.".into(),
            });
        } else if burn_per_hour > 0.0 {
            let hours_left = (cap - used) as f64 / burn_per_hour;
            let sev = if hours_left < 6.0 {
                Severity::Critical
            } else if hours_left < 24.0 {
                Severity::Warn
            } else {
                Severity::Info
            };
            if hours_left < 72.0 {
                out.push(Advisory {
                    severity: sev,
                    text: format!(
                        "At the last 5h pace the weekly cap runs out in ~{}.",
                        humanize_hours(hours_left)
                    ),
                });
            }
        }
    }

    // 3. Any window sitting above 85% of its cap.
    for (label, w) in [
        ("hour", &snap.hour),
        ("5h", &snap.five_h),
        ("week", &snap.week),
    ] {
        if let Some(r) = w.ratio {
            if r >= 0.85 && r < 1.0 {
                out.push(Advisory {
                    severity: Severity::Warn,
                    text: format!("{label} window at {:.0}% of cap.", r * 100.0),
                });
            }
        }
    }

    out
}

fn from_rate_limit(rl: &RateLimitStatus) -> Advisory {
    let severity = if rl.used_percent >= 95.0 {
        Severity::Critical
    } else if rl.used_percent >= 80.0 {
        Severity::Warn
    } else {
        Severity::Info
    };
    let resets = rl
        .resets_at
        .filter(|t| *t > Utc::now())
        .map(|t| format!(", resets {}", t.with_timezone(&Local).format("%a %H:%M")))
        .unwrap_or_default();
    Advisory {
        severity,
        text: format!(
            "{} {:.0}% used{resets}",
            rl.window_label, rl.used_percent
        ),
    }
}

fn humanize_hours(h: f64) -> String {
    if h < 1.0 {
        format!("{}m", (h * 60.0).round() as i64)
    } else if h < 48.0 {
        format!("{}h", h.round() as i64)
    } else {
        format!("{}d", (h / 24.0).round() as i64)
    }
}
