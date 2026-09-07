//! TokenHUD milestone 1 spike: parse local Claude Code transcripts and report
//! token usage for the current hour, the trailing 5h window, and the ISO week.
//!
//! Passive only: reads `~/.claude/projects/**/*.jsonl`, never makes network calls.

use chrono::{DateTime, Datelike, Duration, IsoWeek, Local, TimeZone, Timelike, Utc};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use walkdir::WalkDir;

/// One assistant turn's billable token counts.
#[derive(Debug, Default, Clone, Copy)]
struct Tokens {
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
}

impl Tokens {
    fn total(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
    fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_creation += o.cache_creation;
        self.cache_read += o.cache_read;
    }
}

/// A single usage event extracted from a transcript.
struct Event {
    ts: DateTime<Utc>,
    model: String,
    tokens: Tokens,
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

fn claude_projects_dir() -> Option<PathBuf> {
    // ~/.claude/projects — not an XDG path, Claude Code hardcodes it under $HOME.
    directories::BaseDirs::new().map(|b| b.home_dir().join(".claude").join("projects"))
}

fn collect_events(root: &PathBuf) -> Vec<Event> {
    let mut events = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().map_or(false, |x| x == "jsonl"))
    {
        let file = match std::fs::File::open(entry.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let parsed: Line = match serde_json::from_str(&line) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if parsed.kind.as_deref() != Some("assistant") {
                continue;
            }
            let (Some(ts_raw), Some(msg)) = (parsed.timestamp, parsed.message) else {
                continue;
            };
            let Some(usage) = msg.usage else { continue };
            // Streaming writes several assistant lines per message; dedup like ccusage.
            if let (Some(id), Some(req)) = (&msg.id, &parsed.request_id) {
                if !seen.insert(format!("{id}:{req}")) {
                    continue;
                }
            }
            let Ok(ts) = DateTime::parse_from_rfc3339(&ts_raw) else {
                continue;
            };
            let model = msg.model.unwrap_or_else(|| "unknown".into());
            if model == "<synthetic>" {
                continue;
            }
            events.push(Event {
                ts: ts.with_timezone(&Utc),
                model,
                tokens: Tokens {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                    cache_creation: usage.cache_creation_input_tokens,
                    cache_read: usage.cache_read_input_tokens,
                },
            });
        }
    }
    events.sort_by_key(|e| e.ts);
    events
}

/// Windows we report on, all anchored to local time.
struct Windows {
    hour_start: DateTime<Utc>,
    five_h_start: DateTime<Utc>,
    week: IsoWeek,
}

fn windows(now: DateTime<Local>) -> Windows {
    let hour_start = now
        .date_naive()
        .and_hms_opt(now.hour(), 0, 0)
        .and_then(|n| Local.from_local_datetime(&n).single())
        .unwrap_or(now);
    Windows {
        hour_start: hour_start.with_timezone(&Utc),
        five_h_start: (now - Duration::hours(5)).with_timezone(&Utc),
        week: now.iso_week(),
    }
}

fn main() {
    let now = Local::now();
    let w = windows(now);

    let Some(dir) = claude_projects_dir() else {
        eprintln!("could not locate home directory");
        std::process::exit(1);
    };
    if !dir.exists() {
        eprintln!("no transcripts found at {}", dir.display());
        std::process::exit(1);
    }

    let events = collect_events(&dir);

    let mut hour = Tokens::default();
    let mut five_h = Tokens::default();
    let mut week = Tokens::default();
    let mut per_model_week: BTreeMap<String, Tokens> = BTreeMap::new();

    for e in &events {
        let local = e.ts.with_timezone(&Local);
        if e.ts >= w.hour_start {
            hour.add(&e.tokens);
        }
        if e.ts >= w.five_h_start {
            five_h.add(&e.tokens);
        }
        if local.iso_week() == w.week {
            week.add(&e.tokens);
            per_model_week
                .entry(e.model.clone())
                .or_default()
                .add(&e.tokens);
        }
    }

    println!("TokenHUD — Claude Code usage (source: {})", dir.display());
    println!("scanned {} assistant turns\n", events.len());

    let row = |label: &str, t: &Tokens| {
        println!(
            "{label:<16} total {:>12}  (in {}, out {}, cache-w {}, cache-r {})",
            t.total(),
            t.input,
            t.output,
            t.cache_creation,
            t.cache_read
        );
    };
    row(&format!("hour {}:00", now.format("%H")), &hour);
    row("trailing 5h", &five_h);
    row(&format!("ISO week {}", w.week.week()), &week);

    if !per_model_week.is_empty() {
        println!("\nthis week by model:");
        for (m, t) in &per_model_week {
            println!("  {m:<22} {:>12}", t.total());
        }
    }
}
