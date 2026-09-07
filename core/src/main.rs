//! TokenHUD CLI — passive local usage meter for code-generation CLIs.
//!
//! `tokenhud`          one-shot: scan, ingest, print current windows
//! `tokenhud --watch`  stay running, reprint whenever transcripts change

use std::time::Duration;
use tokenhud_core::{refresh, snapshot_all, store::Store, watch::Watcher, watch_roots};

fn main() {
    let watch_mode = std::env::args().any(|a| a == "--watch" || a == "-w");

    let mut store = match Store::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open store: {e}");
            std::process::exit(1);
        }
    };

    let report = |store: &Store| {
        println!(
            "\nTokenHUD — {} events stored — {}",
            store.row_count().unwrap_or(0),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let snaps = snapshot_all(store);
        if snaps.is_empty() {
            eprintln!("no supported CLI logs found on this machine");
        }
        for s in snaps {
            s.print();
        }
    };

    let n = refresh(&mut store);
    if n > 0 {
        eprintln!("  +{n} new events");
    }
    report(&store);

    if !watch_mode {
        return;
    }

    let roots = watch_roots();
    let watcher = match Watcher::new(&roots, Duration::from_millis(800)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("watch failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("\nwatching {} roots — Ctrl-C to stop", roots.len());
    while watcher.next_change() {
        let n = refresh(&mut store);
        if n > 0 {
            eprintln!("  +{n} new events");
        }
        report(&store);
    }
}
