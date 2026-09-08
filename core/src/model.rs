//! Shared domain types.

use crate::pricing;
use chrono::{DateTime, Utc};
use serde::Serialize;

/// One turn's token counts, split the way both billing and rate limits care about.
///
/// Anthropic reports cache writes separately for the 5-minute and 1-hour TTLs
/// (priced 1.25x and 2x of input); OpenAI/Codex has only "cached input", which
/// maps onto `cache_read`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

impl Tokens {
    /// Every token that moved, cached or not.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }

    /// Fresh (non-cache-read) tokens — a rough proxy for rate-limit pressure
    /// when the real limit is unknown.
    pub fn fresh(&self) -> u64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h
    }

    /// Estimated USD cost at this model's public per-MTok rates.
    pub fn cost_usd(&self, model: &str) -> f64 {
        let r = pricing::rates(model);
        let m = 1_000_000.0;
        (self.input as f64 * r.input
            + self.output as f64 * r.output
            + self.cache_write_5m as f64 * r.cache_write_5m
            + self.cache_write_1h as f64 * r.cache_write_1h
            + self.cache_read as f64 * r.cache_read)
            / m
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn t(input: u64, output: u64, w5: u64, w1h: u64, read: u64) -> Tokens {
        Tokens {
            input,
            output,
            cache_write_5m: w5,
            cache_write_1h: w1h,
            cache_read: read,
        }
    }

    #[test]
    fn totals_and_fresh() {
        let x = t(10, 20, 30, 40, 50);
        assert_eq!(x.total(), 150);
        assert_eq!(x.fresh(), 100); // excludes cache_read
    }

    #[test]
    fn add_accumulates_every_field() {
        let mut a = t(1, 2, 3, 4, 5);
        a.add(&t(10, 20, 30, 40, 50));
        assert_eq!(a, t(11, 22, 33, 44, 55));
    }

    #[test]
    fn cost_matches_sonnet_rates() {
        // 1M output only -> $10 at sonnet rates.
        let x = t(0, 1_000_000, 0, 0, 0);
        assert!((x.cost_usd("claude-sonnet-5") - 10.0).abs() < 1e-6);
        // 1M cache read -> $0.20.
        let y = t(0, 0, 0, 0, 1_000_000);
        assert!((y.cost_usd("claude-sonnet-5") - 0.20).abs() < 1e-6);
    }

    #[test]
    fn cost_sums_every_bucket_at_its_own_rate() {
        // sonnet-5: in 2, out 10, w5m 2.5, w1h 4, read 0.2 ($/MTok)
        let x = t(1_000_000, 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((x.cost_usd("claude-sonnet-5") - (2.0 + 10.0 + 2.5 + 4.0 + 0.2)).abs() < 1e-6);
        assert_eq!(Tokens::default().cost_usd("claude-sonnet-5"), 0.0);
    }

    #[test]
    fn total_fresh_and_weighted_relationships() {
        let x = t(3, 5, 7, 11, 13);
        assert_eq!(x.total(), x.fresh() + x.cache_read);
        assert_eq!(x.fresh(), 3 + 5 + 7 + 11);
    }

    #[test]
    fn rate_limit_status_serializes_timestamps_as_epoch_seconds() {
        let rl = RateLimitStatus {
            tool: "codex".into(),
            window_label: "weekly".into(),
            window_minutes: 10_080,
            used_percent: 42.5,
            resets_at: DateTime::from_timestamp(1_700_000_000, 0),
            observed_at: DateTime::from_timestamp(1_699_999_000, 0).unwrap(),
        };
        let v = serde_json::to_value(&rl).unwrap();
        assert_eq!(v["window_label"], "weekly");
        assert_eq!(v["used_percent"], 42.5);
        assert_eq!(v["resets_at"], 1_700_000_000);
        assert_eq!(v["observed_at"], 1_699_999_000);

        // A `None` reset serializes as JSON null.
        let mut rl2 = rl.clone();
        rl2.resets_at = None;
        assert!(serde_json::to_value(&rl2).unwrap()["resets_at"].is_null());
    }
}
