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
use crate::providers::{ProviderKind, UsageProvider};
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::Duration;

pub const TOOL: &str = "copilot";

pub struct CopilotProvider {
    token: String,
}

impl Default for CopilotProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotProvider {
    pub fn new() -> Self {
        Self::with_token(Config::load().cloud.github_token)
    }

    pub fn with_token(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

/// Turn the `copilot_internal/user` JSON into per-quota rate-limit readings.
/// `limited_user_quotas` reports the *remaining* fraction (0..1) per quota.
fn parse_copilot_quota(json: &serde_json::Value) -> Vec<RateLimitStatus> {
    let quotas = &json["limited_user_quotas"];
    let resets_at = json["limited_user_reset_date"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.to_utc());
    let observed_at = Utc::now();

    ["chat", "completions"]
        .into_iter()
        .filter_map(|key| {
            let remaining = quotas[key].as_f64()?;
            Some(RateLimitStatus {
                tool: TOOL.to_string(),
                window_label: key.into(),
                window_minutes: 43_200,
                used_percent: (1.0 - remaining).clamp(0.0, 1.0) * 100.0,
                resets_at,
                observed_at,
            })
        })
        .collect()
}

impl UsageProvider for CopilotProvider {
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
        match resp.into_json::<serde_json::Value>() {
            Ok(json) => parse_copilot_quota(&json),
            Err(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_availability() {
        assert_eq!(
            CopilotProvider::with_token("x").kind(),
            ProviderKind::Remote
        );
        assert_eq!(CopilotProvider::with_token("x").id(), "copilot");
        assert!(!CopilotProvider::with_token("  ").available());
        assert!(CopilotProvider::with_token("ghp_1").available());
        assert!(CopilotProvider::with_token("x").scan().is_empty());
    }

    #[test]
    fn parses_chat_and_completion_quotas() {
        let j = serde_json::json!({
            "limited_user_quotas": { "chat": 0.25, "completions": 0.8 },
            "limited_user_reset_date": "2026-10-01T00:00:00Z"
        });
        let out = parse_copilot_quota(&j);
        assert_eq!(out.len(), 2);
        let chat = out.iter().find(|r| r.window_label == "chat").unwrap();
        let comp = out
            .iter()
            .find(|r| r.window_label == "completions")
            .unwrap();
        assert!((chat.used_percent - 75.0).abs() < 1e-9); // 1 - 0.25
        assert!((comp.used_percent - 20.0).abs() < 1e-9); // 1 - 0.8
        assert!(chat.resets_at.is_some());
    }

    #[test]
    fn empty_when_no_quota_block() {
        assert!(parse_copilot_quota(&serde_json::json!({ "plan": "business" })).is_empty());
    }

    #[test]
    fn clamps_out_of_range_fractions() {
        let j = serde_json::json!({ "limited_user_quotas": { "chat": -0.2, "completions": 1.5 } });
        let out = parse_copilot_quota(&j);
        let chat = out.iter().find(|r| r.window_label == "chat").unwrap();
        let comp = out
            .iter()
            .find(|r| r.window_label == "completions")
            .unwrap();
        assert_eq!(chat.used_percent, 100.0);
        assert_eq!(comp.used_percent, 0.0);
    }
}
