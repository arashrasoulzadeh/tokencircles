//! Debounced filesystem watching over every provider's roots.

use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
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

    /// Drain any changes that piled up behind the one we just took.
    fn coalesce(&self) {
        while self.rx.try_recv().is_ok() {}
    }

    /// Block until the next change (coalescing any that arrive together).
    /// Returns `false` only when the watcher has been dropped.
    pub fn next_change(&self) -> bool {
        if self.rx.recv().is_err() {
            return false;
        }
        self.coalesce();
        true
    }

    /// Like [`next_change`], but gives up after `timeout`. Returns `true` if a
    /// change arrived, `false` on timeout or disconnect. Used by tests and any
    /// caller that wants a periodic wake even when the filesystem is quiet.
    pub fn next_change_timeout(&self, timeout: Duration) -> bool {
        match self.rx.recv_timeout(timeout) {
            Ok(()) => {
                self.coalesce();
                true
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn timeout_returns_false_when_nothing_changes() {
        let dir = std::env::temp_dir().join(format!("tokenhud-watch-idle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = Watcher::new(std::slice::from_ref(&dir), Duration::from_millis(20)).unwrap();
        assert!(!w.next_change_timeout(Duration::from_millis(120)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_write_wakes_the_watcher() {
        let dir = std::env::temp_dir().join(format!("tokenhud-watch-hit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = Watcher::new(std::slice::from_ref(&dir), Duration::from_millis(20)).unwrap();

        let f = dir.join("a.jsonl");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            let mut h = std::fs::File::create(&f).unwrap();
            writeln!(h, "line").unwrap();
        });

        // Generous ceiling for slow CI filesystems.
        assert!(w.next_change_timeout(Duration::from_secs(5)));

        // Bursts collapse into one wake, then it's quiet again.
        for i in 0..5 {
            std::fs::write(dir.join(format!("b{i}.jsonl")), "x").unwrap();
        }
        assert!(w.next_change_timeout(Duration::from_secs(5)));
        assert!(!w.next_change_timeout(Duration::from_millis(200)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_roots_are_skipped_not_fatal() {
        let w = Watcher::new(
            &[PathBuf::from("/no/such/dir/at/all")],
            Duration::from_millis(20),
        );
        assert!(w.is_ok());
    }
}
