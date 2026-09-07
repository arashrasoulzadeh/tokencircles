//! GitHub Copilot provider (opt-in, cloud).
//!
//! Reads the authenticated user's Copilot billing/quota via the GitHub API using
//! a token from `config.toml`:
//!
//! ```toml
//! [cloud]
//! github_token = "ghp_..."   # needs the "Copilot" read scope
//! ```
//!
//! Copilot is a flat subscription with completion/chat quotas rather than tokens,
//! so this surfaces the quota percentage as a rate limit and records no token
//! events.

use crate::config::Config;
use crate::model::{RateLimitStatus, UsageEvent};
use crate::providers::UsageProvider;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::Duration;

pub const TOOL: &str = "copilot";

pub struct CopilotProvider {
    token: String,
}

impl CopilotProvider {
    pub fn new() -> Self {
        Self {
            token: Config::load().cloud.github_token,
        }
    }
}

impl UsageProvider for CopilotProvider {
    fn id(&self) -> &'static str {
        TOOL
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn available(&self) -> bool {
        !self.token.trim().is_empty()
    }

    fn scan(&self) -> Vec<UsageEvent> {
        Vec::new()
    }

    fn rate_limits(&self) -> Vec<RateLimitStatus> {
        let resp = match ureq::get("https://api.github.com/copilot_internal/user")
            .set("Authorization", &format!("token {}", self.token))
            .set("User-Agent", "tokenhud")
            .timeout(Duration::from_secs(8))
            .call()
        {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let Ok(json): Result<serde_json::Value, _> = resp.into_json() else {
            return Vec::new();
        };

        // `limited_user_quotas` / `limited_user_reset_date` appear for quota'd plans.
        let quotas = &json["limited_user_quotas"];
        let mut out = Vec::new();
        for key in ["chat", "completions"] {
            if let Some(remaining) = quotas[key].as_f64() {
                // The API reports remaining fraction (0..1) for some plans.
                let used = (1.0 - remaining).clamp(0.0, 1.0) * 100.0;
                out.push(RateLimitStatus {
                    tool: TOOL.to_string(),
                    window_label: key.into(),
                    window_minutes: 43_200,
                    used_percent: used,
                    resets_at: json["limited_user_reset_date"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.to_utc()),
                    observed_at: Utc::now(),
                });
            }
        }
        out
    }
}
