//! Codex CLI provider: parses `~/.codex/sessions/**/rollout-*.jsonl` (and the
//! archived copies). Codex records cumulative token usage per turn and — unlike
//! Claude — reports authoritative rate-limit percentages we can surface directly.

use crate::model::{RateLimitStatus, Tokens, UsageEvent};
use crate::providers::UsageProvider;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use walkdir::WalkDir;

pub const TOOL: &str = "codex";

pub struct CodexProvider {
    roots: Vec<PathBuf>,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexProvider {
    pub fn new() -> Self {
        let base = directories::BaseDirs::new().map(|b| b.home_dir().join(".codex"));
        let roots = base
            .into_iter()
            .flat_map(|c| [c.join("sessions"), c.join("archived_sessions")])
            .collect();
        Self { roots }
    }

    fn session_files(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.roots.iter().flat_map(|root| {
            WalkDir::new(root)
                .into_iter()
                .filter_map(Result::ok)
                .map(|e| e.into_path())
                .filter(|p| {
                    p.extension().is_some_and(|x| x == "jsonl")
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("rollout-"))
                })
        })
    }
}

impl UsageProvider for CodexProvider {
    fn id(&self) -> &'static str {
        TOOL
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        self.roots.clone()
    }

    fn available(&self) -> bool {
        self.session_files().next().is_some()
    }

    fn scan(&self) -> Vec<UsageEvent> {
        let mut out = Vec::new();
        for path in self.session_files() {
            parse_session(&path, &mut out, &mut Vec::new());
        }
        out
    }

    fn rate_limits(&self) -> Vec<RateLimitStatus> {
        let mut limits: Vec<RateLimitStatus> = Vec::new();
        for path in self.session_files() {
            parse_session(&path, &mut Vec::new(), &mut limits);
        }
        // Keep only the newest reading per window.
        limits.sort_by(|a, b| a.observed_at.cmp(&b.observed_at));
        let mut latest: std::collections::BTreeMap<u64, RateLimitStatus> = Default::default();
        for l in limits {
            latest.insert(l.window_minutes, l);
        }
        latest.into_values().collect()
    }
}

fn parse_session(path: &PathBuf, events: &mut Vec<UsageEvent>, limits: &mut Vec<RateLimitStatus>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();

    let mut model = "gpt-codex".to_string();
    let mut prev = CumUsage::default();

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(rec): Result<Record, _> = serde_json::from_str(&line) else {
            continue;
        };
        match rec.kind.as_deref() {
            Some("turn_context") | Some("session_meta") => {
                if let Some(m) = rec.payload.and_then(|p| p.model) {
                    model = m;
                }
            }
            Some("event_msg") => {
                let Some(payload) = rec.payload else { continue };
                if payload.kind.as_deref() != Some("token_count") {
                    continue;
                }
                let ts = rec
                    .timestamp
                    .as_deref()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.to_utc())
                    .unwrap_or_else(Utc::now);

                if let Some(info) = payload.info {
                    let cur = CumUsage::from(&info.total_token_usage);
                    if let Some(delta) = cur.delta_from(&prev) {
                        events.push(UsageEvent {
                            dedup_key: format!("{session_id}:{}", cur.total),
                            tool: TOOL,
                            ts,
                            model: model.clone(),
                            tokens: delta,
                        });
                    }
                    prev = cur;
                }

                if let Some(rl) = payload.rate_limits {
                    for (w, label) in [(rl.primary, None), (rl.secondary, None)] {
                        if let Some(win) = w {
                            limits.push(RateLimitStatus {
                                tool: TOOL.to_string(),
                                window_label: label
                                    .unwrap_or_else(|| window_label(win.window_minutes)),
                                window_minutes: win.window_minutes,
                                used_percent: win.used_percent,
                                resets_at: win
                                    .resets_at
                                    .and_then(|s| Utc.timestamp_opt(s, 0).single()),
                                observed_at: ts,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn window_label(minutes: u64) -> String {
    match minutes {
        0..=60 => "hourly".into(),
        61..=360 => "5h".into(),
        361..=1440 => "daily".into(),
        _ => "weekly".into(),
    }
}

#[derive(Default, Clone, Copy)]
struct CumUsage {
    input: u64,
    cached_input: u64,
    output: u64,
    total: u64,
}

impl CumUsage {
    fn from(t: &TotalUsage) -> Self {
        Self {
            input: t.input_tokens,
            cached_input: t.cached_input_tokens,
            output: t.output_tokens,
            total: t.total_tokens,
        }
    }

    /// Positive per-field difference, or `None` if nothing advanced.
    fn delta_from(&self, prev: &CumUsage) -> Option<Tokens> {
        let uncached = self.input.saturating_sub(self.cached_input);
        let prev_uncached = prev.input.saturating_sub(prev.cached_input);
        let d = Tokens {
            input: uncached.saturating_sub(prev_uncached),
            output: self.output.saturating_sub(prev.output),
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: self.cached_input.saturating_sub(prev.cached_input),
        };
        (d.total() > 0).then_some(d)
    }
}

// ---- Codex rollout JSON shapes (only the fields we need) ----------------------

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    info: Option<Info>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: TotalUsage,
}

#[derive(Deserialize, Default)]
struct TotalUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[derive(Deserialize)]
struct RateLimits {
    primary: Option<Window>,
    secondary: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    #[serde(default)]
    used_percent: f64,
    #[serde(default)]
    window_minutes: u64,
    resets_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_labels_by_length() {
        assert_eq!(window_label(10), "hourly");
        assert_eq!(window_label(300), "5h");
        assert_eq!(window_label(1440), "daily");
        assert_eq!(window_label(10080), "weekly");
    }

    #[test]
    fn cumulative_deltas_exclude_cached_from_input() {
        let prev = CumUsage {
            input: 1000,
            cached_input: 800,
            output: 50,
            total: 1050,
        };
        let cur = CumUsage {
            input: 1500,
            cached_input: 1100,
            output: 90,
            total: 1590,
        };
        let d = cur.delta_from(&prev).expect("advanced");
        assert_eq!(d.input, 200); // (1500-1100) - (1000-800)
        assert_eq!(d.cache_read, 300); // 1100 - 800
        assert_eq!(d.output, 40);
        assert_eq!(d.cache_write_5m, 0);
    }

    #[test]
    fn no_delta_when_unchanged() {
        let same = CumUsage {
            input: 10,
            cached_input: 5,
            output: 2,
            total: 12,
        };
        assert!(same.delta_from(&same).is_none());
    }

    #[test]
    fn parses_a_rollout_session() {
        let dir = std::env::temp_dir().join(format!("tokenhud-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-05-08T17-40-23-abc.jsonl");
        let body = concat!(
            r#"{"type":"turn_context","timestamp":"2026-05-08T14:10:48Z","payload":{"model":"gpt-5.3-codex"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-05-08T14:12:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":600,"output_tokens":40,"total_tokens":1040}},"rate_limits":{"primary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1778853149}}}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-05-08T14:20:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":3000,"cached_input_tokens":2000,"output_tokens":90,"total_tokens":3090}}}}"#,
            "\n",
        );
        std::fs::write(&path, body).unwrap();

        let mut events = Vec::new();
        let mut limits = Vec::new();
        parse_session(&path, &mut events, &mut limits);

        assert_eq!(
            events.len(),
            2,
            "one event per token_count with advancing total"
        );
        assert_eq!(events[0].model, "gpt-5.3-codex");
        assert_eq!(events[0].tokens.input, 400); // (1000-600)
        assert_eq!(events[0].tokens.cache_read, 600);
        assert_eq!(events[1].tokens.input, 600); // (3000-2000)-(1000-600)
        assert_eq!(events[1].tokens.cache_read, 1400);

        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].window_label, "weekly");
        assert!((limits[0].used_percent - 12.5).abs() < 1e-9);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
