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
        Self {
            token: Config::load().cloud.cursor_token,
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

        // Shape varies; pull the fields we can find.
        let gpt4 = &json["gpt-4"];
        Some(CursorUsage {
            requests_used: gpt4["numRequests"].as_u64().unwrap_or(0),
            requests_limit: gpt4["maxRequestUsage"].as_u64(),
        })
    }
}

struct CursorUsage {
    requests_used: u64,
    requests_limit: Option<u64>,
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
        let Some(u) = self.fetch() else {
            return Vec::new();
        };
        let Some(limit) = u.requests_limit.filter(|l| *l > 0) else {
            return Vec::new();
        };
        vec![RateLimitStatus {
            tool: TOOL.to_string(),
            window_label: "monthly".into(),
            window_minutes: 43_200,
            used_percent: u.requests_used as f64 / limit as f64 * 100.0,
            resets_at: None,
            observed_at: Utc::now(),
        }]
    }
}
