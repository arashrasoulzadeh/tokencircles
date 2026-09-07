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

impl ClaudeProvider {
    pub fn new() -> Self {
        let root = directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".claude").join("projects"));
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
            .filter(|e| e.path().extension().map_or(false, |x| x == "jsonl"))
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
    Some(UsageEvent {
        dedup_key,
        tool: TOOL,
        ts,
        model,
        tokens: Tokens {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_creation: usage.cache_creation_input_tokens,
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
}
