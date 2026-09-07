//! Gemini CLI provider.
//!
//! The Gemini CLI keeps chat logs under `~/.gemini/tmp/<project-hash>/logs.json`
//! but records no token counts there, and its free tier is metered in
//! requests/day rather than tokens. Until a usable local signal exists this
//! provider reports itself unavailable so the HUD simply omits Gemini.

use crate::model::UsageEvent;
use crate::providers::UsageProvider;
use std::path::PathBuf;

pub const TOOL: &str = "gemini";

pub struct GeminiProvider {
    tmp_root: Option<PathBuf>,
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiProvider {
    pub fn new() -> Self {
        let tmp_root =
            directories::BaseDirs::new().map(|b| b.home_dir().join(".gemini").join("tmp"));
        Self { tmp_root }
    }

    /// Any `logs.json` we could parse once support lands.
    #[allow(dead_code)]
    fn log_files(&self) -> Vec<PathBuf> {
        let Some(root) = &self.tmp_root else {
            return Vec::new();
        };
        walkdir::WalkDir::new(root)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.into_path())
            .filter(|p| p.file_name().is_some_and(|n| n == "logs.json"))
            .collect()
    }
}

impl UsageProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        TOOL
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        self.tmp_root.iter().cloned().collect()
    }

    fn available(&self) -> bool {
        // No token-bearing log format is parseable yet.
        false
    }

    fn scan(&self) -> Vec<UsageEvent> {
        Vec::new()
    }
}
