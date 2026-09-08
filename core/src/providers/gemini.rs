//! Gemini CLI provider.
//!
//! The Gemini CLI keeps chat logs under `~/.gemini/tmp/<project-hash>/logs.json`
//! but records no token counts there, and its free tier is metered in
//! requests/day rather than tokens. Until a usable local signal exists this
//! provider reports itself unavailable so the HUD simply omits Gemini.

use crate::model::UsageEvent;
use crate::providers::UsageProvider;
use std::path::{Path, PathBuf};

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

    /// Construct against an explicit `~/.gemini/tmp` root (tests).
    pub fn with_root(tmp_root: Option<PathBuf>) -> Self {
        Self { tmp_root }
    }

    /// `<home>/.gemini/tmp`.
    pub fn for_home(home: &Path) -> Self {
        Self {
            tmp_root: Some(home.join(".gemini").join("tmp")),
        }
    }

    /// Any `logs.json` we could parse once support lands.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn log_files(&self) -> Vec<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderKind;

    #[test]
    fn is_a_local_provider_but_never_available() {
        let p = GeminiProvider::with_root(Some(std::env::temp_dir()));
        assert_eq!(p.id(), "gemini");
        assert_eq!(p.kind(), ProviderKind::Local);
        assert!(!p.available(), "no parseable token format yet");
        assert!(p.scan().is_empty());
        assert!(p.rate_limits().is_empty());
    }

    #[test]
    fn for_home_points_at_dot_gemini_tmp() {
        let p = GeminiProvider::for_home(Path::new("/tmp/fakehome"));
        assert_eq!(
            p.watch_roots(),
            vec![PathBuf::from("/tmp/fakehome/.gemini/tmp")]
        );
    }

    #[test]
    fn log_files_finds_logs_json_within_two_levels() {
        let dir = std::env::temp_dir().join(format!("tokenhud-gemini-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("proj-hash");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("logs.json"), "[]").unwrap();
        std::fs::write(proj.join("other.json"), "{}").unwrap();

        let found = GeminiProvider::with_root(Some(dir.clone())).log_files();
        assert_eq!(found, vec![proj.join("logs.json")]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
