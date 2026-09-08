//! Optional, opt-in weekly summary via the Anthropic API.
//!
//! This is the one place TokenHUD makes a network call, and only when the user
//! has put an API key in the config. It sends aggregate token counts — never
//! transcript content — and asks for a short plain-language readout.

use crate::aggregate::ToolSnapshot;
use crate::config::SummaryConfig;

pub fn is_enabled(cfg: &SummaryConfig) -> bool {
    !cfg.anthropic_api_key.trim().is_empty()
}

/// The prompt sent to Claude — aggregate counts only, no transcript content.
pub fn build_prompt(snaps: &[ToolSnapshot]) -> String {
    let facts = snaps
        .iter()
        .map(|s| {
            let reported: String = s
                .rate_limits
                .iter()
                .map(|r| format!("; {} reports {:.0}% used", r.window_label, r.used_percent))
                .collect();
            format!(
                "- {}: this week {} tokens (~${:.2}); last 5h {} tokens; this hour {} tokens{}",
                s.tool, s.week.total, s.week.cost_usd, s.five_h.total, s.hour.total, reported
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "You are a usage assistant for AI coding CLIs. Given these aggregate token \
         counts, write 3-4 short sentences: the headline number, which tool and \
         window is under the most pressure, and one concrete suggestion. No preamble.\n\n{facts}"
    )
}

/// Pull the assistant text out of a `/v1/messages` response body.
pub fn parse_response(json: &serde_json::Value) -> Result<String, String> {
    json["content"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("unexpected response shape: {json}"))
}

/// Ask Claude for a 3-4 sentence summary of the week's usage. Blocking.
pub fn weekly(cfg: &SummaryConfig, snaps: &[ToolSnapshot]) -> Result<String, String> {
    if !is_enabled(cfg) {
        return Err("no API key configured (config.toml → [summary] anthropic_api_key)".into());
    }

    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 400,
        "messages": [{ "role": "user", "content": build_prompt(snaps) }],
    });

    let resp = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", cfg.anthropic_api_key.trim())
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(body)
        .map_err(|e| format!("request failed: {e}"))?;

    let json: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    parse_response(&json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::{ToolSnapshot, WindowStat};
    use crate::model::{RateLimitStatus, Tokens};
    use chrono::Utc;

    fn win(total: u64, cost: f64) -> WindowStat {
        WindowStat {
            tokens: Tokens {
                input: total,
                ..Default::default()
            },
            total,
            fresh: total,
            cost_usd: cost,
            cap: None,
            ratio: None,
            remaining: None,
        }
    }

    fn snap() -> ToolSnapshot {
        ToolSnapshot {
            tool: "claude".into(),
            hour: win(1_000, 0.01),
            five_h: win(40_000_000, 12.0),
            week: win(500_000_000, 130.0),
            week_by_model: vec![],
            rate_limits: vec![RateLimitStatus {
                tool: "claude".into(),
                window_label: "weekly".into(),
                window_minutes: 10_080,
                used_percent: 17.0,
                resets_at: None,
                observed_at: Utc::now(),
            }],
            advisories: vec![],
            five_h_reset: None,
            five_h_minutes_left: None,
        }
    }

    #[test]
    fn enabled_only_with_a_key() {
        let mut cfg = SummaryConfig::default();
        assert!(!is_enabled(&cfg));
        cfg.anthropic_api_key = "  ".into();
        assert!(!is_enabled(&cfg));
        cfg.anthropic_api_key = "sk-ant-x".into();
        assert!(is_enabled(&cfg));
    }

    #[test]
    fn weekly_errors_without_a_key() {
        let err = weekly(&SummaryConfig::default(), &[]).unwrap_err();
        assert!(err.contains("no API key"));
    }

    #[test]
    fn prompt_carries_the_aggregate_numbers() {
        let p = build_prompt(&[snap()]);
        assert!(p.contains("claude"));
        assert!(p.contains("500000000 tokens"));
        assert!(p.contains("$130.00"));
        assert!(p.contains("weekly reports 17% used"));
        // Never leak the actual prompt/response content — only counts.
        assert!(!p.contains("message"));
    }

    #[test]
    fn parses_a_messages_response() {
        let ok =
            serde_json::json!({ "content": [{ "type": "text", "text": "  You used a lot.  " }] });
        assert_eq!(parse_response(&ok).unwrap(), "You used a lot.");
    }

    #[test]
    fn rejects_a_malformed_response() {
        assert!(parse_response(&serde_json::json!({ "error": "overloaded" })).is_err());
        assert!(parse_response(&serde_json::json!({ "content": [{ "text": "" }] })).is_err());
    }
}
