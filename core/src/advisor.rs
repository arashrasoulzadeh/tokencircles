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

    // 1. Authoritative rate limits win — but only call one out once it's
    //    actually pressing (>=80%); below that the HUD's own bar says enough.
    for rl in &snap.rate_limits {
        let adv = from_rate_limit(rl);
        if adv.severity != Severity::Info {
            out.push(adv);
        }
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

    // 3. Any window sitting above 85% of its cap. (No "hour" — Claude and
    //    Codex meter 5h + weekly only; a 1-hour limit doesn't exist.)
    for (label, w) in [("5h", &snap.five_h), ("week", &snap.week)] {
        if let Some(r) = w.ratio {
            if (0.85..1.0).contains(&r) {
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
        text: format!("{} {:.0}% used{resets}", rl.window_label, rl.used_percent),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::{ToolSnapshot, WindowStat};
    use crate::model::Tokens;

    fn win(total: u64, cap: Option<u64>) -> WindowStat {
        let tokens = Tokens {
            input: total,
            ..Default::default()
        };
        WindowStat {
            tokens,
            total,
            fresh: total,
            cost_usd: 0.0,
            cap,
            ratio: cap.map(|c| total as f64 / c as f64),
            remaining: cap.map(|c| c.saturating_sub(total)),
        }
    }

    fn snap(hour: WindowStat, five_h: WindowStat, week: WindowStat) -> ToolSnapshot {
        ToolSnapshot {
            tool: "claude".into(),
            hour,
            five_h,
            week,
            week_by_model: vec![],
            rate_limits: vec![],
            advisories: vec![],
            five_h_reset: None,
            five_h_minutes_left: None,
        }
    }

    #[test]
    fn no_caps_means_no_estimate_advisories() {
        let s = snap(win(10, None), win(50, None), win(100, None));
        assert!(advise(&s).is_empty());
    }

    #[test]
    fn projects_exhaustion_from_5h_pace() {
        // 5h window burned 5M → 1M/h. Week cap 60M, used 40M → 20h left → warn.
        let s = snap(
            win(0, None),
            win(5_000_000, None),
            win(40_000_000, Some(60_000_000)),
        );
        let advs = advise(&s);
        assert!(advs
            .iter()
            .any(|a| a.severity == Severity::Warn && a.text.contains("runs out in ~20h")));
    }

    #[test]
    fn cap_reached_is_critical() {
        let s = snap(
            win(0, None),
            win(0, None),
            win(60_000_000, Some(60_000_000)),
        );
        let advs = advise(&s);
        assert!(advs.iter().any(|a| a.severity == Severity::Critical));
    }

    #[test]
    fn rate_limit_percent_maps_to_severity() {
        let mut s = snap(win(0, None), win(0, None), win(0, None));
        s.rate_limits = vec![RateLimitStatus {
            tool: "codex".into(),
            window_label: "weekly".into(),
            window_minutes: 10080,
            used_percent: 97.0,
            resets_at: None,
            observed_at: Utc::now(),
        }];
        let advs = advise(&s);
        assert_eq!(advs[0].severity, Severity::Critical);
        assert!(advs[0].text.contains("97%"));
    }
}
