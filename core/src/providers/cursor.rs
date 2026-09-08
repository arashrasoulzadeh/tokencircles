//! Cursor provider (opt-in, cloud).
//!
//! Cursor keeps no usable local usage log, so this reads the dashboard's usage
//! endpoint using a session token the user pastes into `config.toml`:
//!
//! ```toml
//! [cloud]
//! cursor_token = "..."   # the `WorkosCursorSessionToken` cookie value
//! ```
//!
//! The endpoint is unofficial and may change; failures degrade to "unavailable".

use crate::config::Config;
use crate::model::{RateLimitStatus, Tokens, UsageEvent};
use crate::providers::{ProviderKind, UsageProvider};
use chrono::{Datelike, Local, TimeZone, Utc};
use std::path::PathBuf;
use std::time::Duration;

pub const TOOL: &str = "cursor";

pub struct CursorProvider {
    token: String,
}

impl Default for CursorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorProvider {
    pub fn new() -> Self {
        Self::with_token(Config::load().cloud.cursor_token)
    }

    pub fn with_token(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    fn fetch(&self) -> Option<CursorUsage> {
        let month_start = Local
            .with_ymd_and_hms(Local::now().year(), Local::now().month(), 1, 0, 0, 0)
            .single()?
            .timestamp_millis();
        let url = format!("https://cursor.com/api/usage?startDate={month_start}");

        let resp = ureq::get(&url)
            .set(
                "Cookie",
                &format!("WorkosCursorSessionToken={}", self.token),
            )
            .timeout(Duration::from_secs(8))
            .call()
            .ok()?;
        let json: serde_json::Value = resp.into_json().ok()?;
        Some(parse_cursor_usage(&json))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorUsage {
    requests_used: u64,
    requests_limit: Option<u64>,
}

/// Extract month-to-date request usage from the dashboard's `/api/usage` JSON.
/// The shape varies by plan; we pull the `gpt-4` bucket that's always present.
fn parse_cursor_usage(json: &serde_json::Value) -> CursorUsage {
    let gpt4 = &json["gpt-4"];
    CursorUsage {
        requests_used: gpt4["numRequests"].as_u64().unwrap_or(0),
        requests_limit: gpt4["maxRequestUsage"].as_u64(),
    }
}

/// Month-to-date request usage as a monthly rate-limit percentage, if a cap exists.
fn cursor_rate_limit(u: &CursorUsage) -> Option<RateLimitStatus> {
    let limit = u.requests_limit.filter(|l| *l > 0)?;
    Some(RateLimitStatus {
        tool: TOOL.to_string(),
        window_label: "monthly".into(),
        window_minutes: 43_200,
        used_percent: u.requests_used as f64 / limit as f64 * 100.0,
        resets_at: None,
        observed_at: Utc::now(),
    })
}

impl UsageProvider for CursorProvider {
    fn id(&self) -> &'static str {
        TOOL
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Remote
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn available(&self) -> bool {
        !self.token.trim().is_empty()
    }

    fn scan(&self) -> Vec<UsageEvent> {
        // Cursor bills in requests, not tokens; we record a single synthetic
        // event so the month-to-date request count is visible, tagged with a
        // stable per-day key so re-scans replace rather than accumulate.
        let Some(u) = self.fetch() else {
            return Vec::new();
        };
        let now = Utc::now();
        vec![UsageEvent {
            dedup_key: format!("cursor:{}", now.format("%Y-%m-%d")),
            tool: TOOL,
            ts: now,
            model: "cursor-requests".into(),
            tokens: Tokens {
                input: u.requests_used,
                ..Default::default()
            },
        }]
    }

    fn rate_limits(&self) -> Vec<RateLimitStatus> {
        self.fetch()
            .as_ref()
            .and_then(cursor_rate_limit)
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_remote_and_no_watch_roots() {
        let p = CursorProvider::with_token("");
        assert_eq!(p.kind(), ProviderKind::Remote);
        assert!(p.watch_roots().is_empty());
        assert_eq!(p.id(), "cursor");
    }

    #[test]
    fn available_only_with_a_non_blank_token() {
        assert!(!CursorProvider::with_token("").available());
        assert!(!CursorProvider::with_token("   ").available());
        assert!(CursorProvider::with_token("tok_abc").available());
    }

    #[test]
    fn parses_usage_from_dashboard_json() {
        let j = serde_json::json!({
            "gpt-4": { "numRequests": 412, "maxRequestUsage": 500 },
            "gpt-3.5-turbo": { "numRequests": 9 }
        });
        let u = parse_cursor_usage(&j);
        assert_eq!(u.requests_used, 412);
        assert_eq!(u.requests_limit, Some(500));
    }

    #[test]
    fn tolerates_missing_fields() {
        let u = parse_cursor_usage(&serde_json::json!({}));
        assert_eq!(u.requests_used, 0);
        assert_eq!(u.requests_limit, None);
    }

    #[test]
    fn rate_limit_percentage() {
        let rl = cursor_rate_limit(&CursorUsage {
            requests_used: 250,
            requests_limit: Some(500),
        })
        .unwrap();
        assert_eq!(rl.window_label, "monthly");
        assert!((rl.used_percent - 50.0).abs() < 1e-9);
    }

    #[test]
    fn no_rate_limit_without_a_cap() {
        assert!(cursor_rate_limit(&CursorUsage {
            requests_used: 10,
            requests_limit: None,
        })
        .is_none());
        assert!(cursor_rate_limit(&CursorUsage {
            requests_used: 10,
            requests_limit: Some(0),
        })
        .is_none());
    }
}
