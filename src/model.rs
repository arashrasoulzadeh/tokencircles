//! Shared domain types.

use chrono::{DateTime, Utc};

/// One assistant turn's billable token counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_creation += o.cache_creation;
        self.cache_read += o.cache_read;
    }
}

/// A single usage event produced by a provider, ready to persist.
#[derive(Debug, Clone)]
pub struct UsageEvent {
    /// Stable identity for deduplication across rescans (e.g. `msgid:reqid`).
    pub dedup_key: String,
    /// Which CLI tool the usage belongs to.
    pub tool: &'static str,
    pub ts: DateTime<Utc>,
    pub model: String,
    pub tokens: Tokens,
}
