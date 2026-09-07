//! Usage providers: each knows how to discover and parse one CLI tool's local logs.

use crate::model::{RateLimitStatus, UsageEvent};
use std::path::PathBuf;

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;

/// A source of token-usage events read from the local filesystem. Passive only.
pub trait UsageProvider: Send + Sync {
    /// Short stable identifier, e.g. `"claude"`.
    fn id(&self) -> &'static str;

    /// Directories to watch for changes. May be empty if nothing exists yet.
    fn watch_roots(&self) -> Vec<PathBuf>;

    /// Scan all available logs and return every usage event found.
    ///
    /// Callers deduplicate by [`UsageEvent::dedup_key`], so returning the same
    /// event twice across calls is harmless.
    fn scan(&self) -> Vec<UsageEvent>;

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
