//! Public per-model token prices, USD per million tokens.
//!
//! Approximate and updated by hand — good enough for a "roughly $X this week"
//! readout, not billing. Sources: Anthropic and OpenAI public pricing pages.

#[derive(Debug, Clone, Copy)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// Anthropic caching multipliers: 5-minute write 1.25x input, 1-hour write 2x,
/// read 0.1x.
const fn anthropic(input: f64, output: f64) -> Rates {
    Rates {
        input,
        output,
        cache_write_5m: input * 1.25,
        cache_write_1h: input * 2.0,
        cache_read: input * 0.1,
    }
}

/// OpenAI has no cache-write surcharge; cached input is 0.1x input.
const fn openai(input: f64, output: f64) -> Rates {
    Rates {
        input,
        output,
        cache_write_5m: input,
        cache_write_1h: input,
        cache_read: input * 0.1,
    }
}

pub fn rates(model: &str) -> Rates {
    let m = model.to_ascii_lowercase();
    match () {
        _ if m.contains("opus") => anthropic(5.0, 25.0),
        _ if m.contains("haiku") => anthropic(1.0, 5.0),
        _ if m.contains("sonnet") || m.contains("claude") => anthropic(2.0, 10.0),
        _ if m.contains("gpt-5") || m.contains("codex") || m.contains("o3") || m.contains("o4") => {
            openai(1.25, 10.0)
        }
        _ if m.contains("gpt-4") => openai(2.5, 10.0),
        _ if m.contains("gemini") => openai(1.25, 10.0),
        // Unknown model: assume mid-tier Anthropic rates.
        _ => anthropic(2.0, 10.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet_rates_and_cache_multipliers() {
        let r = rates("claude-sonnet-5");
        assert_eq!(r.input, 2.0);
        assert_eq!(r.output, 10.0);
        assert_eq!(r.cache_write_5m, 2.5); // 1.25x
        assert_eq!(r.cache_write_1h, 4.0); // 2x
        assert!((r.cache_read - 0.2).abs() < 1e-9); // 0.1x
    }

    #[test]
    fn model_family_matching() {
        // Anthropic tiers.
        assert_eq!(rates("claude-opus-4-8").input, 5.0);
        assert_eq!(rates("Claude-Opus-5").output, 25.0); // case-insensitive
        assert_eq!(rates("claude-haiku-4-5").input, 1.0);
        assert_eq!(rates("claude-sonnet-5").input, 2.0);
        assert_eq!(rates("some-claude-thing").input, 2.0); // bare "claude" → sonnet tier

        // OpenAI / Codex — no cache-write surcharge, cached-in is 0.1x.
        for m in ["gpt-5.3-codex", "o3-mini", "o4", "codex-latest"] {
            let r = rates(m);
            assert_eq!(r.input, 1.25, "{m}");
            assert_eq!(r.cache_write_1h, 1.25, "{m}");
            assert!((r.cache_read - 0.125).abs() < 1e-9, "{m}");
        }
        assert_eq!(rates("gpt-4o").input, 2.5);
        assert_eq!(rates("gemini-2.5-pro").input, 1.25);

        // Unknown → mid-tier Anthropic.
        assert_eq!(rates("totally-unknown").input, 2.0);
        assert_eq!(rates("").input, 2.0);
    }

    #[test]
    fn opus_before_sonnet_when_both_words_present() {
        // "opus" is checked first, so a hypothetical combined name still prices as opus.
        assert_eq!(rates("claude-opus-sonnet-hybrid").input, 5.0);
    }
}
