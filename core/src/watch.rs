//! Debounced filesystem watching over every provider's roots.

use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// Watches the given roots and yields `()` whenever a debounced change lands.
pub struct Watcher {
    _debouncer: notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>,
    rx: Receiver<()>,
}

impl Watcher {
    pub fn new(roots: &[PathBuf], debounce: Duration) -> notify::Result<Self> {
        let (tx, rx) = channel();
        let mut debouncer = new_debouncer(debounce, move |res: DebounceEventResult| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        })?;
        for root in roots {
            if root.exists() {
                debouncer.watcher().watch(root, RecursiveMode::Recursive)?;
            }
        }
        Ok(Self {
            _debouncer: debouncer,
            rx,
        })
    }

    /// Block until the next change (coalescing any that arrive together).
    pub fn next_change(&self) -> bool {
        if self.rx.recv().is_err() {
            return false;
        }
        while self.rx.try_recv().is_ok() {}
        true
    }
}
