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
