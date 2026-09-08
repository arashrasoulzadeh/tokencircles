//! End-to-end integration tests: fake log files on disk → `refresh_with` →
//! `snapshot_all_with`, asserting the numbers the HUD would show.

use std::fs;
use std::path::{Path, PathBuf};

use tokenhud_core::providers::{claude::ClaudeProvider, codex::CodexProvider, UsageProvider};
use tokenhud_core::store::Store;
use tokenhud_core::{refresh_with, snapshot_all_with, Config, Scope};

/// A throwaway directory that cleans itself up.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "tokenhud-it-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assistant_line(ts: &str, id: &str, req: &str, model: &str, out: u64, cache_read: u64) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","requestId":"{req}","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":10,"output_tokens":{out},"cache_creation_input_tokens":0,"cache_read_input_tokens":{cache_read},"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}}}}}"#
    )
}

fn claude(root: &Path, plan: Option<&Path>) -> Box<dyn UsageProvider> {
    Box::new(ClaudeProvider::with_paths(
        Some(root.to_path_buf()),
        plan.map(Path::to_path_buf),
    ))
}

#[test]
fn claude_transcripts_flow_into_a_snapshot() {
    let dir = TempDir::new("claude");
    let projects = dir.path().join(".claude/projects/proj-a");
    fs::create_dir_all(&projects).unwrap();

    let now = chrono::Utc::now();
    let recent = now - chrono::Duration::minutes(20);
    let session = projects.join("session-1.jsonl");
    fs::write(
        &session,
        format!(
            "{}\n{}\n",
            assistant_line(
                &recent.to_rfc3339(),
                "msg_1",
                "req_1",
                "claude-sonnet-5",
                100,
                1_000_000
            ),
            assistant_line(
                &recent.to_rfc3339(),
                "msg_2",
                "req_2",
                "claude-sonnet-5",
                200,
                2_000_000
            ),
        ),
    )
    .unwrap();

    let mut store = Store::open_memory().unwrap();
    let provs = vec![claude(&dir.path().join(".claude/projects"), None)];

    let n = refresh_with(&mut store, &provs, Scope::LocalOnly);
    assert_eq!(n, 2, "two assistant turns ingested");

    let snaps = snapshot_all_with(&store, &provs, &Config::default());
    assert_eq!(snaps.len(), 1);
    let s = &snaps[0];
    assert_eq!(s.tool, "claude");
    // 10+10 input, 100+200 output, 3M cache read.
    assert_eq!(s.week.tokens.input, 20);
    assert_eq!(s.week.tokens.output, 300);
    assert_eq!(s.week.tokens.cache_read, 3_000_000);
    assert_eq!(s.week.total, 3_000_320);
    // sonnet-5: output $10/MTok, cache-read $0.20/MTok → 0.003 + 0.60 ≈ $0.60.
    assert!(
        (s.week.cost_usd - 0.60).abs() < 0.01,
        "cost {}",
        s.week.cost_usd
    );
    // Same window right now.
    assert_eq!(s.five_h.total, s.week.total);
    assert_eq!(s.hour.total, s.week.total);
}

#[test]
fn rescans_are_incremental_and_dedup() {
    let dir = TempDir::new("incr");
    let root = dir.path().join(".claude/projects/p");
    fs::create_dir_all(&root).unwrap();
    let session = root.join("s.jsonl");
    let now = chrono::Utc::now().to_rfc3339();
    fs::write(
        &session,
        format!(
            "{}\n",
            assistant_line(&now, "m1", "r1", "claude-sonnet-5", 1, 0)
        ),
    )
    .unwrap();

    let mut store = Store::open_memory().unwrap();
    let provs = vec![claude(&dir.path().join(".claude/projects"), None)];

    assert_eq!(refresh_with(&mut store, &provs, Scope::LocalOnly), 1);
    // Nothing changed → no new rows, and the file isn't even re-read.
    assert_eq!(refresh_with(&mut store, &provs, Scope::LocalOnly), 0);

    // Append a second turn.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let mut f = fs::OpenOptions::new().append(true).open(&session).unwrap();
    use std::io::Write;
    writeln!(
        f,
        "{}",
        assistant_line(&now, "m2", "r2", "claude-sonnet-5", 1, 0)
    )
    .unwrap();
    drop(f);

    assert_eq!(refresh_with(&mut store, &provs, Scope::LocalOnly), 1);
    assert_eq!(store.row_count().unwrap(), 2);
}

#[test]
fn claude_plan_usage_becomes_authoritative_rate_limits() {
    let dir = TempDir::new("plan");
    let root = dir.path().join(".claude/projects");
    fs::create_dir_all(&root).unwrap();
    let plan = dir.path().join("plan-usage-history.json");

    let now_ms = chrono::Utc::now().timestamp_millis();
    // fh hit 0 ~90 min ago then climbed to 27%.
    let start = now_ms - 90 * 60_000;
    fs::write(
        &plan,
        format!(
            r#"{{"version":2,"samples":[
                {{"t":{},"org":"x","u":{{"fh":40,"sd":15}}}},
                {{"t":{},"org":"x","u":{{"fh":0,"sd":16}}}},
                {{"t":{},"org":"x","u":{{"fh":27,"sd":20}}}}
            ]}}"#,
            start - 3_600_000,
            start,
            now_ms
        ),
    )
    .unwrap();

    let mut store = Store::open_memory().unwrap();
    let provs = vec![claude(&root, Some(&plan))];
    refresh_with(&mut store, &provs, Scope::LocalOnly);

    let snaps = snapshot_all_with(&store, &provs, &Config::default());
    let s = &snaps[0];
    let five = s
        .rate_limits
        .iter()
        .find(|r| r.window_label == "5h")
        .unwrap();
    let week = s
        .rate_limits
        .iter()
        .find(|r| r.window_label == "weekly")
        .unwrap();
    assert_eq!(five.used_percent, 27.0);
    assert_eq!(week.used_percent, 20.0);

    // Reset ≈ block start + 5h → ~3.5h from now.
    let mins = s.five_h_minutes_left.expect("a countdown");
    assert!((200..=215).contains(&mins), "got {mins} min");
}

#[test]
fn codex_rollouts_produce_deltas_and_reported_limits() {
    let dir = TempDir::new("codex");
    let sessions = dir.path().join("sessions/2026/05/08");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-2026-05-08T17-40-23-abc.jsonl"),
        concat!(
            r#"{"type":"turn_context","timestamp":"2026-05-08T14:10:48Z","payload":{"model":"gpt-5.3-codex"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-05-08T14:12:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":600,"output_tokens":40,"total_tokens":1040}},"rate_limits":{"primary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1778853149}}}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-05-08T14:20:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":3000,"cached_input_tokens":2000,"output_tokens":90,"total_tokens":3090}}}}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut store = Store::open_memory().unwrap();
    let provs: Vec<Box<dyn UsageProvider>> = vec![Box::new(CodexProvider::with_roots(vec![dir
        .path()
        .join("sessions")]))];

    let n = refresh_with(&mut store, &provs, Scope::LocalOnly);
    assert_eq!(n, 2, "two token_count deltas");

    let snaps = snapshot_all_with(&store, &provs, &Config::default());
    let s = &snaps[0];
    assert_eq!(s.tool, "codex");
    // May-2026 events → outside every current window, but still stored.
    assert_eq!(s.week.total, 0);
    let weekly = s
        .rate_limits
        .iter()
        .find(|r| r.window_label == "weekly")
        .unwrap();
    assert!((weekly.used_percent - 12.5).abs() < 1e-9);
}

#[test]
fn unavailable_providers_are_dropped_from_the_snapshot() {
    let dir = TempDir::new("empty");
    let provs = vec![
        claude(&dir.path().join("nope/projects"), None),
        Box::new(CodexProvider::with_roots(vec![dir.path().join("nope")]))
            as Box<dyn UsageProvider>,
    ];
    let store = Store::open_memory().unwrap();
    assert!(snapshot_all_with(&store, &provs, &Config::default()).is_empty());
}
