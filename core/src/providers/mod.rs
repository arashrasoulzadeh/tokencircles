//! Usage providers: each knows how to discover and parse one CLI tool's local logs.

use crate::model::{RateLimitStatus, UsageEvent};
use std::path::{Path, PathBuf};

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;

/// Where a provider's data comes from — governs how often we poll it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Reads local files; cheap, safe to hit on every filesystem change.
    Local,
    /// Makes a network request; polled on a slow timer, never on the hot path.
    Remote,
}

/// A source of token-usage events. Local providers are passive file readers;
/// remote providers are opt-in and only enabled when the user configures a token.
pub trait UsageProvider: Send + Sync {
    /// Short stable identifier, e.g. `"claude"`.
    fn id(&self) -> &'static str;

    fn kind(&self) -> ProviderKind {
        ProviderKind::Local
    }

    /// Directories to watch for changes. May be empty if nothing exists yet.
    fn watch_roots(&self) -> Vec<PathBuf>;

    /// Scan all available logs and return every usage event found.
    ///
    /// Callers deduplicate by [`UsageEvent::dedup_key`], so returning the same
    /// event twice across calls is harmless.
    fn scan(&self) -> Vec<UsageEvent>;

    /// Individual log files this provider reads. When non-empty, callers rescan
    /// only the files whose mtime/size changed (so a full pass stays cheap and
    /// can run every few seconds). Empty for providers with no discrete files.
    fn source_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Parse one file (from [`source_files`]) into usage events.
    fn parse_file(&self, _path: &Path) -> Vec<UsageEvent> {
        Vec::new()
    }

    /// Authoritative rate-limit readings the tool reports about itself, if any.
    fn rate_limits(&self) -> Vec<RateLimitStatus> {
        Vec::new()
    }

    /// Whether this provider found any parseable data source on disk.
    fn available(&self) -> bool {
        self.watch_roots().iter().any(|p| p.exists())
    }
}

/// Build the set of providers we support today.
pub fn all() -> Vec<Box<dyn UsageProvider>> {
    vec![
        Box::new(claude::ClaudeProvider::new()),
        Box::new(codex::CodexProvider::new()),
        Box::new(gemini::GeminiProvider::new()),
        Box::new(cursor::CursorProvider::new()),
        Box::new(copilot::CopilotProvider::new()),
    ]
}
