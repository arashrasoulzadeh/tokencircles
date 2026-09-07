//! TokenHUD core: passive local usage metering for code-generation CLIs.

pub mod aggregate;
pub mod model;
pub mod providers;
pub mod store;
pub mod watch;

use std::path::PathBuf;

/// Scan every available provider and ingest new events into `store`.
/// Returns the number of newly stored rows across all providers.
pub fn refresh(store: &mut store::Store) -> usize {
    let mut new = 0;
    for p in providers::all() {
        if p.available() {
            new += store.ingest(&p.scan()).unwrap_or(0);
        }
    }
    new
}

/// One snapshot per provider that currently has data on disk.
pub fn snapshot_all(store: &store::Store) -> Vec<aggregate::ToolSnapshot> {
    providers::all()
        .iter()
        .filter(|p| p.available())
        .filter_map(|p| aggregate::snapshot(store, p.id()).ok())
        .collect()
}

/// Watch roots for every available provider.
pub fn watch_roots() -> Vec<PathBuf> {
    providers::all()
        .iter()
        .flat_map(|p| p.watch_roots())
        .filter(|p| p.exists())
        .collect()
}
