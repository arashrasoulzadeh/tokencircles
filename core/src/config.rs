//! User configuration: plan caps and opt-in cloud/summary settings.
//!
//! Stored as TOML under the platform config dir. Absent file → all defaults.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Per-tool token caps for each window. A missing entry means "unknown" —
    /// the HUD then shows raw usage and burn rate instead of a percentage.
    #[serde(default)]
    pub caps: BTreeMap<String, Caps>,

    #[serde(default)]
    pub cloud: CloudConfig,

    #[serde(default)]
    pub summary: SummaryConfig,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Caps {
    pub hour: Option<u64>,
    #[serde(rename = "five_h")]
    pub five_h: Option<u64>,
    pub week: Option<u64>,
}

/// Opt-in credentials for tools that only expose usage via a cloud API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CloudConfig {
    /// Cursor session token (from the dashboard). Empty = disabled.
    #[serde(default)]
    pub cursor_token: String,
    /// GitHub token with `read:user` for the Copilot usage endpoint. Empty = disabled.
    #[serde(default)]
    pub github_token: String,
}

/// Optional weekly LLM summary. Off unless an API key is present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SummaryConfig {
    /// Anthropic API key used only for the on-demand `tokenhud summary` call.
    #[serde(default)]
    pub anthropic_api_key: String,
    #[serde(default = "default_summary_model")]
    pub model: String,
}

fn default_summary_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

impl Config {
    pub fn path() -> PathBuf {
        directories::ProjectDirs::from("dev", "tokenhud", "tokenhud")
            .map(|d| d.config_dir().join("config.toml"))
            .unwrap_or_else(|| PathBuf::from("tokenhud-config.toml"))
    }

    /// Load config, falling back to defaults on any missing/broken file.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("tokenhud: config parse error ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }

    pub fn caps_for(&self, tool: &str) -> Caps {
        self.caps.get(tool).copied().unwrap_or_default()
    }
}
