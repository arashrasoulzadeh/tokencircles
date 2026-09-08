//! TokenHUD core: passive local usage metering for code-generation CLIs.

pub mod advisor;
pub mod aggregate;
pub mod config;
pub mod model;
pub mod pricing;
pub mod providers;
pub mod store;
pub mod summary;
pub mod watch;

pub use config::Config;
pub use model::UsageEvent;
pub use providers::ProviderKind;

use std::path::{Path, PathBuf};

/// How much of the provider set to poll on a given pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Local file readers only.
    LocalOnly,
    /// Also hit the opt-in remote providers (network I/O).
    IncludeRemote,
}

/// A file's `(mtime, size)` fingerprint — cheap to compute, changes on any write.
fn fingerprint(path: &Path) -> Option<(i64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len() as i64))
}

/// Ingest events only from files whose `(mtime, size)` changed since last pass.
/// This keeps a "full" rescan cheap enough to run every few seconds, so the
/// displayed token totals never lag far behind what a CLI just wrote.
/// Returns the number of newly-stored rows.
pub fn ingest_changed_files<F>(store: &mut store::Store, files: &[PathBuf], parse: F) -> usize
where
    F: Fn(&Path) -> Vec<UsageEvent>,
{
    let mut new = 0;
    for f in files {
        let Some((mtime, size)) = fingerprint(f) else {
            continue;
        };
        let key = f.to_string_lossy();
        if store.file_unchanged(&key, mtime, size).unwrap_or(false) {
            continue;
        }
        new += store.ingest(&parse(f)).unwrap_or(0);
        let _ = store.mark_file_scanned(&key, mtime, size);
    }
    new
}

/// Rescan every provider in `scope` and ingest new events. Local providers are
/// rescanned incrementally (only changed files), so this is cheap to call often.
/// Returns the number of newly-stored rows.
pub fn refresh(store: &mut store::Store, scope: Scope) -> usize {
    let mut new = 0;
    for p in providers::all() {
        let wanted = match scope {
            Scope::LocalOnly => p.kind() == ProviderKind::Local,
            Scope::IncludeRemote => true,
        };
        if !(wanted && p.available()) {
            continue;
        }
        let files = p.source_files();
        if files.is_empty() {
            new += store.ingest(&p.scan()).unwrap_or(0);
        } else {
            let prov = &*p;
            new += ingest_changed_files(store, &files, |path| prov.parse_file(path));
        }
        let _ = store.ingest_rate_limits(&p.rate_limits());
    }
    new
}

/// One snapshot per provider that currently has data on disk.
pub fn snapshot_all(store: &store::Store, config: &Config) -> Vec<aggregate::ToolSnapshot> {
    providers::all()
        .iter()
        .filter(|p| p.available())
        .filter_map(|p| aggregate::snapshot(store, p.id(), config).ok())
        .collect()
}

/// Whether any opt-in remote provider is configured and reachable-in-principle.
pub fn has_remote_providers() -> bool {
    providers::all()
        .iter()
        .any(|p| p.kind() == ProviderKind::Remote && p.available())
}

/// Watch roots for every local provider.
pub fn watch_roots() -> Vec<PathBuf> {
    providers::all()
        .iter()
        .flat_map(|p| p.watch_roots())
        .filter(|p| p.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tokens;
    use std::cell::RefCell;
    use std::io::Write;

    fn ev(key: &str) -> UsageEvent {
        UsageEvent {
            dedup_key: key.into(),
            tool: "claude",
            ts: chrono::Utc::now(),
            model: "claude-sonnet-5".into(),
            tokens: Tokens {
                input: 1,
                ..Default::default()
            },
        }
    }

    #[test]
    fn changed_files_are_reread_unchanged_ones_skipped() {
        let dir = std::env::temp_dir().join(format!("tokenhud-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        std::fs::write(&a, "a1\n").unwrap();
        std::fs::write(&b, "b1\n").unwrap();

        let mut store = store::Store::open_memory().unwrap();
        let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let parse = |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            seen.borrow_mut().push(name.clone());
            vec![ev(&format!(
                "{name}:{}",
                std::fs::read_to_string(p).unwrap().trim()
            ))]
        };
        let files = vec![a.clone(), b.clone()];

        // First pass: both files read, both events stored.
        assert_eq!(ingest_changed_files(&mut store, &files, parse), 2);
        assert_eq!(*seen.borrow(), vec!["a.jsonl", "b.jsonl"]);
        seen.borrow_mut().clear();

        // Second pass, nothing touched: no file re-read, nothing new.
        assert_eq!(ingest_changed_files(&mut store, &files, parse), 0);
        assert!(seen.borrow().is_empty());

        // Append to b only: just b is re-read, its new line becomes a new event.
        // (Sleep a hair so the mtime definitely advances on coarse filesystems.)
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut f = std::fs::OpenOptions::new().append(true).open(&b).unwrap();
        writeln!(f, "b2").unwrap();
        drop(f);
        assert_eq!(ingest_changed_files(&mut store, &files, parse), 1);
        assert_eq!(*seen.borrow(), vec!["b.jsonl"]);

        assert_eq!(store.row_count().unwrap(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_are_skipped_not_fatal() {
        let mut store = store::Store::open_memory().unwrap();
        let files = vec![PathBuf::from("/no/such/file.jsonl")];
        assert_eq!(
            ingest_changed_files(&mut store, &files, |_| vec![ev("x")]),
            0
        );
    }
}
