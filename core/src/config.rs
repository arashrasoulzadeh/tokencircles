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

    #[serde(default)]
    pub ui: UiConfig,
}

/// HUD presentation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default)]
    pub mode: HudMode,
    /// Which screen edge circle mode pins to.
    #[serde(default)]
    pub circle_side: ScreenSide,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HudMode {
    /// The full card with labelled rows.
    #[default]
    Card,
    /// A slim strip of progress rings pinned to the screen edge.
    Circle,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScreenSide {
    Left,
    #[default]
    Right,
}

impl ScreenSide {
    pub fn flipped(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
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
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("tokenhud: config parse error ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_defaults() {
        let cfg = Config::load_from(std::path::Path::new("/no/such/tokenhud/config.toml"));
        assert!(cfg.caps.is_empty());
        assert_eq!(cfg.caps_for("claude"), Caps::default());
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = std::env::temp_dir().join(format!("tokenhud-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.caps.insert(
            "claude".into(),
            Caps {
                hour: Some(1_000),
                five_h: None,
                week: Some(9_999),
            },
        );
        cfg.summary.anthropic_api_key = "sk-test".into();
        cfg.save_to(&path).unwrap();

        let back = Config::load_from(&path);
        let c = back.caps_for("claude");
        assert_eq!(c.hour, Some(1_000));
        assert_eq!(c.five_h, None);
        assert_eq!(c.week, Some(9_999));
        assert_eq!(back.summary.anthropic_api_key, "sk-test");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_toml_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("tokenhud-badcfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "this is not [valid toml").unwrap();
        assert!(Config::load_from(&path).caps.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
