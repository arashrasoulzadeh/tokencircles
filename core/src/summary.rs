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

/// Ask Claude for a 3-4 sentence summary of the week's usage. Blocking.
pub fn weekly(cfg: &SummaryConfig, snaps: &[ToolSnapshot]) -> Result<String, String> {
    if !is_enabled(cfg) {
        return Err("no API key configured (config.toml → [summary] anthropic_api_key)".into());
    }

    let facts = snaps
        .iter()
        .map(|s| {
            format!(
                "- {}: this week {} tokens (~${:.2}); last 5h {} tokens; this hour {} tokens{}",
                s.tool,
                s.week.total,
                s.week.cost_usd,
                s.five_h.total,
                s.hour.total,
                s.rate_limits
                    .iter()
                    .map(|r| format!("; {} reports {:.0}% used", s.tool, r.used_percent))
                    .collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are a usage assistant for AI coding CLIs. Given these aggregate token \
         counts, write 3-4 short sentences: the headline number, which tool and \
         window is under the most pressure, and one concrete suggestion. No preamble.\n\n{facts}"
    );

    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 400,
        "messages": [{ "role": "user", "content": prompt }],
    });

    let resp = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", cfg.anthropic_api_key.trim())
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(body)
        .map_err(|e| format!("request failed: {e}"))?;

    let json: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    json["content"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("unexpected response shape: {json}"))
}
