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
pub use providers::ProviderKind;

use std::path::PathBuf;

/// How much of the provider set to poll on a given pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Local file readers only — cheap, safe on every filesystem change.
    LocalOnly,
    /// Also hit the opt-in remote providers (network I/O).
    IncludeRemote,
}

/// Scan providers in `scope` and ingest new events into `store`.
/// Returns the number of newly stored rows.
pub fn refresh(store: &mut store::Store, scope: Scope) -> usize {
    let mut new = 0;
    for p in providers::all() {
        let wanted = match scope {
            Scope::LocalOnly => p.kind() == ProviderKind::Local,
            Scope::IncludeRemote => true,
        };
        if wanted && p.available() {
            new += store.ingest(&p.scan()).unwrap_or(0);
            let _ = store.ingest_rate_limits(&p.rate_limits());
        }
    }
    new
}

/// Cheap pass: only re-read the self-reported rate limits (Claude's plan-usage
/// file, Codex's rollout tail) — no transcript walk. Safe to run often so the
/// rings stay live while idle.
pub fn refresh_limits(store: &mut store::Store) {
    for p in providers::all() {
        if p.kind() == ProviderKind::Local && p.available() {
            let _ = store.ingest_rate_limits(&p.rate_limits());
        }
    }
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
