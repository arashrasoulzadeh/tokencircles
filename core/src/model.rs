//! Shared domain types.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// One assistant turn's billable token counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
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

    /// Tokens that count toward rate limits, weighting cache reads at 10%
    /// (matches Anthropic's cache-read pricing ratio; not an official limit rule).
    pub fn weighted(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read / 10
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

/// An authoritative rate-limit reading reported by the tool itself (e.g. Codex
/// emits `used_percent` for its rolling windows). Preferred over our estimates.
#[derive(Debug, Clone, Serialize)]
pub struct RateLimitStatus {
    pub tool: String,
    /// Human label for the window, e.g. `"weekly"` or `"5h"`.
    pub window_label: String,
    pub window_minutes: u64,
    pub used_percent: f64,
    #[serde(with = "chrono::serde::ts_seconds_option")]
    pub resets_at: Option<DateTime<Utc>>,
    /// When the tool reported this figure.
    #[serde(with = "chrono::serde::ts_seconds")]
    pub observed_at: DateTime<Utc>,
}
