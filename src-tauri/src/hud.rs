//! Pure HUD geometry + notification logic, split out so it can be unit-tested
//! without a running Tauri app or a real display.

use std::collections::HashSet;
use tokenhud_core::aggregate::ToolSnapshot;
use tokenhud_core::config::{HudMode, ScreenSide};

/// Logical size of the card window (width, height).
pub const CARD_SIZE: (f64, f64) = (268.0, 180.0);
/// Logical size of the circle strip — tall enough for three rings.
pub const CIRCLE_SIZE: (f64, f64) = (76.0, 244.0);
/// Gap between the circle strip and the screen edge.
pub const EDGE_MARGIN: f64 = 8.0;

pub fn mode_str(m: HudMode) -> &'static str {
    match m {
        HudMode::Card => "card",
        HudMode::Circle => "circle",
    }
}

/// A logical-pixel rectangle: `(x, y)` origin + `(w, h)` size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    fn right(&self) -> f64 {
        self.x + self.w
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Where to pin the circle strip: hard against `side`, vertically centred.
pub fn circle_edge_position(monitor: Rect, win: (f64, f64), side: ScreenSide) -> (f64, f64) {
    let (w, h) = win;
    let x = match side {
        ScreenSide::Left => monitor.x + EDGE_MARGIN,
        ScreenSide::Right => monitor.x + monitor.w - w - EDGE_MARGIN,
    };
    let y = monitor.y + (monitor.h - h) / 2.0;
    (x, y)
}

/// Whether at least a graspable sliver of the window's title strip is on-screen.
pub fn window_is_reachable(win: Rect, monitors: &[Rect]) -> bool {
    monitors.iter().any(|m| {
        win.x + 48.0 < m.right()
            && win.right() - 48.0 > m.x
            && win.y + 8.0 < m.bottom()
            && win.y + 8.0 > m.y - 8.0
    })
}

/// A safe fallback position on `primary` for a window that has drifted off every
/// monitor — near the top-right, never past the left edge.
pub fn rescue_position(primary: Rect, win: (f64, f64)) -> (f64, f64) {
    let (w, _h) = win;
    let x = (primary.x + primary.w - w - 24.0).max(primary.x);
    (x, primary.y + 40.0)
}

// ---- notification thresholds ------------------------------------------------

/// Fire a notification the first time a tracked window crosses one of these
/// percentages; re-arm when it falls back under [`REARM_BELOW`].
pub const THRESHOLDS: [u32; 2] = [95, 80];
pub const REARM_BELOW: f64 = 0.5;

/// Every `(name, ratio)` pair the HUD watches for alerting: configured-cap
/// ratios and tool-reported rate-limit percentages.
fn tracked_ratios(snaps: &[ToolSnapshot]) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for s in snaps {
        for (label, w) in [("hour", &s.hour), ("5h", &s.five_h), ("week", &s.week)] {
            if let Some(r) = w.ratio {
                out.push((format!("{} {label}", s.tool), r));
            }
        }
        for rl in &s.rate_limits {
            out.push((
                format!("{} {}", s.tool, rl.window_label),
                rl.used_percent / 100.0,
            ));
        }
    }
    out
}

/// Decide which alert bodies to raise right now, updating `fired` so each
/// threshold notifies once until its window drops back below the re-arm level.
pub fn pending_alerts(snaps: &[ToolSnapshot], fired: &mut HashSet<String>) -> Vec<String> {
    let mut bodies = Vec::new();
    for (name, ratio) in tracked_ratios(snaps) {
        for pct in THRESHOLDS {
            let key = format!("{name}:{pct}");
            if ratio >= pct as f64 / 100.0 {
                if fired.insert(key) {
                    bodies.push(format!("{name} usage at {:.0}%", ratio * 100.0));
                    break;
                }
            } else if ratio < REARM_BELOW {
                fired.remove(&key);
            }
        }
    }
    bodies
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenhud_core::aggregate::{ToolSnapshot, WindowStat};
    use tokenhud_core::model::{RateLimitStatus, Tokens};

    fn mon(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn circle_pins_flush_to_either_edge_and_centres_vertically() {
        let m = mon(0.0, 0.0, 1440.0, 900.0);
        let (lx, ly) = circle_edge_position(m, CIRCLE_SIZE, ScreenSide::Left);
        assert_eq!(lx, EDGE_MARGIN);
        assert_eq!(ly, (900.0 - CIRCLE_SIZE.1) / 2.0);

        let (rx, _ry) = circle_edge_position(m, CIRCLE_SIZE, ScreenSide::Right);
        assert_eq!(rx, 1440.0 - CIRCLE_SIZE.0 - EDGE_MARGIN);
        assert!(rx + CIRCLE_SIZE.0 <= 1440.0, "never past the right edge");
    }

    #[test]
    fn circle_position_respects_a_non_zero_monitor_origin() {
        let m = mon(-1920.0, 0.0, 1920.0, 1080.0); // monitor to the left of primary
        let (x, _) = circle_edge_position(m, CIRCLE_SIZE, ScreenSide::Left);
        assert_eq!(x, -1920.0 + EDGE_MARGIN);
    }

    #[test]
    fn reachability_detects_a_window_dragged_off_screen() {
        let screen = [mon(0.0, 0.0, 1440.0, 900.0)];
        assert!(window_is_reachable(
            Rect::new(1200.0, 100.0, 76.0, 244.0),
            &screen
        ));
        // Entirely to the right of the monitor.
        assert!(!window_is_reachable(
            Rect::new(1440.0, 100.0, 76.0, 244.0),
            &screen
        ));
        // Title bar above the top edge.
        assert!(!window_is_reachable(
            Rect::new(100.0, -300.0, 268.0, 180.0),
            &screen
        ));
    }

    #[test]
    fn rescue_clamps_to_the_primary_top_right() {
        let (x, y) = rescue_position(mon(0.0, 0.0, 1440.0, 900.0), CARD_SIZE);
        assert_eq!(x, 1440.0 - CARD_SIZE.0 - 24.0);
        assert_eq!(y, 40.0);
        // A window wider than the monitor still can't go past the left edge.
        let (x2, _) = rescue_position(mon(0.0, 0.0, 100.0, 900.0), CARD_SIZE);
        assert_eq!(x2, 0.0);
    }

    #[test]
    fn mode_str_round_trips() {
        assert_eq!(mode_str(HudMode::Card), "card");
        assert_eq!(mode_str(HudMode::Circle), "circle");
    }

    fn wstat(ratio: Option<f64>) -> WindowStat {
        WindowStat {
            tokens: Tokens::default(),
            total: 0,
            fresh: 0,
            cost_usd: 0.0,
            cap: ratio.map(|_| 100),
            ratio,
            remaining: None,
        }
    }

    fn snap(week_ratio: Option<f64>, reported: &[(&str, f64)]) -> ToolSnapshot {
        ToolSnapshot {
            tool: "claude".into(),
            hour: wstat(None),
            five_h: wstat(None),
            week: wstat(week_ratio),
            week_by_model: vec![],
            rate_limits: reported
                .iter()
                .map(|(label, pct)| RateLimitStatus {
                    tool: "claude".into(),
                    window_label: (*label).into(),
                    window_minutes: 300,
                    used_percent: *pct,
                    resets_at: None,
                    observed_at: chrono::Utc::now(),
                })
                .collect(),
            advisories: vec![],
            five_h_reset: None,
            five_h_minutes_left: None,
        }
    }

    #[test]
    fn alert_fires_once_per_threshold_then_stays_quiet() {
        let mut fired = HashSet::new();
        // 82% → the 80 threshold fires.
        let a = pending_alerts(&[snap(None, &[("5h", 82.0)])], &mut fired);
        assert_eq!(a, vec!["claude 5h usage at 82%"]);
        // Still 82% next tick → nothing new.
        assert!(pending_alerts(&[snap(None, &[("5h", 82.0)])], &mut fired).is_empty());
        // Climbs to 96% → the 95 threshold fires (once).
        let b = pending_alerts(&[snap(None, &[("5h", 96.0)])], &mut fired);
        assert_eq!(b, vec!["claude 5h usage at 96%"]);
        assert!(pending_alerts(&[snap(None, &[("5h", 96.0)])], &mut fired).is_empty());
    }

    #[test]
    fn alert_rearms_after_the_window_drops_below_half() {
        let mut fired = HashSet::new();
        pending_alerts(&[snap(None, &[("weekly", 90.0)])], &mut fired);
        assert!(!fired.is_empty());
        // Reset happened → 12% → re-arm.
        assert!(pending_alerts(&[snap(None, &[("weekly", 12.0)])], &mut fired).is_empty());
        assert!(fired.is_empty(), "re-armed");
        // Next spike fires again.
        let a = pending_alerts(&[snap(None, &[("weekly", 88.0)])], &mut fired);
        assert_eq!(a, vec!["claude weekly usage at 88%"]);
    }

    #[test]
    fn configured_cap_ratio_also_alerts() {
        let mut fired = HashSet::new();
        let a = pending_alerts(&[snap(Some(0.9), &[])], &mut fired);
        assert_eq!(a, vec!["claude week usage at 90%"]);
    }

    #[test]
    fn nothing_fires_below_eighty() {
        let mut fired = HashSet::new();
        assert!(pending_alerts(&[snap(Some(0.5), &[("5h", 70.0)])], &mut fired).is_empty());
        assert!(fired.is_empty());
    }
}
