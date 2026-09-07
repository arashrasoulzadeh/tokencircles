//! Claude Code provider: parses `~/.claude/projects/**/*.jsonl` transcripts.

use crate::model::{Tokens, UsageEvent};
use crate::providers::UsageProvider;
use chrono::DateTime;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use walkdir::WalkDir;

pub const TOOL: &str = "claude";

pub struct ClaudeProvider {
    root: Option<PathBuf>,
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeProvider {
    pub fn new() -> Self {
        let root =
            directories::BaseDirs::new().map(|b| b.home_dir().join(".claude").join("projects"));
        Self { root }
    }
}

impl UsageProvider for ClaudeProvider {
    fn id(&self) -> &'static str {
        TOOL
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        self.root.iter().cloned().collect()
    }

    fn scan(&self) -> Vec<UsageEvent> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        {
            let Ok(file) = std::fs::File::open(entry.path()) else {
                continue;
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Some(ev) = parse_line(&line) {
                    out.push(ev);
                }
            }
        }
        out
    }
}

fn parse_line(line: &str) -> Option<UsageEvent> {
    let parsed: Line = serde_json::from_str(line).ok()?;
    if parsed.kind.as_deref() != Some("assistant") {
        return None;
    }
    let msg = parsed.message?;
    let usage = msg.usage?;
    let model = msg.model.unwrap_or_else(|| "unknown".into());
    if model == "<synthetic>" {
        return None;
    }
    let ts = DateTime::parse_from_rfc3339(&parsed.timestamp?)
        .ok()?
        .to_utc();
    // Streaming writes several assistant lines per message; this key collapses them.
    let dedup_key = match (&msg.id, &parsed.request_id) {
        (Some(id), Some(req)) => format!("{id}:{req}"),
        _ => format!("{}:{}", ts.timestamp_millis(), model),
    };
    // Prefer the 5m/1h breakdown; fall back to treating all writes as 5m.
    let (w5, w1h) = match usage.cache_creation {
        Some(c) => (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens),
        None => (usage.cache_creation_input_tokens, 0),
    };
    Some(UsageEvent {
        dedup_key,
        tool: TOOL,
        ts,
        model,
        tokens: Tokens {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_write_5m: w5,
            cache_write_1h: w1h,
            cache_read: usage.cache_read_input_tokens,
        },
    })
}

// ---- Transcript JSON shapes (only the fields we need) --------------------------

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSISTANT: &str = r#"{"type":"assistant","timestamp":"2026-08-23T11:40:55.196Z","requestId":"req_1","message":{"id":"msg_1","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":70,"cache_creation_input_tokens":14894,"cache_read_input_tokens":35220,"cache_creation":{"ephemeral_1h_input_tokens":14894,"ephemeral_5m_input_tokens":0}}}}"#;

    #[test]
    fn parses_usage_and_splits_cache_writes() {
        let ev = parse_line(ASSISTANT).expect("assistant line parses");
        assert_eq!(ev.tool, "claude");
        assert_eq!(ev.model, "claude-sonnet-5");
        assert_eq!(ev.dedup_key, "msg_1:req_1");
        assert_eq!(ev.tokens.input, 2);
        assert_eq!(ev.tokens.output, 70);
        assert_eq!(ev.tokens.cache_write_1h, 14894);
        assert_eq!(ev.tokens.cache_write_5m, 0);
        assert_eq!(ev.tokens.cache_read, 35220);
    }

    #[test]
    fn falls_back_to_5m_when_no_breakdown() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-23T11:40:55Z","requestId":"r","message":{"id":"m","model":"claude-sonnet-5","usage":{"cache_creation_input_tokens":500}}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev.tokens.cache_write_5m, 500);
        assert_eq!(ev.tokens.cache_write_1h, 0);
    }

    #[test]
    fn skips_non_assistant_and_synthetic() {
        assert!(
            parse_line(r#"{"type":"user","message":{"role":"user","content":"hi"}}"#).is_none()
        );
        assert!(parse_line(r#"{"type":"queue-operation","operation":"enqueue"}"#).is_none());
        let synth = r#"{"type":"assistant","timestamp":"2026-08-23T11:40:55Z","message":{"model":"<synthetic>","usage":{"output_tokens":1}}}"#;
        assert!(parse_line(synth).is_none());
        assert!(parse_line("not json at all").is_none());
    }
}
