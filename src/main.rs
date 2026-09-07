//! TokenHUD — passive local usage meter for code-generation CLIs.
//!
//! `tokenhud`          one-shot: scan, ingest, print current windows
//! `tokenhud --watch`  stay running, reprint whenever transcripts change

mod aggregate;
mod model;
mod providers;
mod store;
mod watch;

use std::time::Duration;

fn main() {
    let watch_mode = std::env::args().any(|a| a == "--watch" || a == "-w");

    let mut store = match store::Store::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open store: {e}");
            std::process::exit(1);
        }
    };

    let provs = providers::all();
    let active: Vec<_> = provs.iter().filter(|p| p.available()).collect();
    if active.is_empty() {
        eprintln!("no supported CLI logs found on this machine");
        std::process::exit(1);
    }

    let refresh = |store: &mut store::Store| {
        for p in &provs {
            if !p.available() {
                continue;
            }
            match store.ingest(&p.scan()) {
                Ok(n) if n > 0 => eprintln!("  +{n} new {} events", p.id()),
                Ok(_) => {}
                Err(e) => eprintln!("  ingest error ({}): {e}", p.id()),
            }
        }
    };

    let report = |store: &store::Store| {
        println!(
            "\nTokenHUD — {} events stored — {}",
            store.row_count().unwrap_or(0),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        for p in &provs {
            if !p.available() {
                continue;
            }
            match aggregate::snapshot(store, p.id()) {
                Ok(s) => s.print(),
                Err(e) => eprintln!("snapshot error ({}): {e}", p.id()),
            }
        }
    };

    refresh(&mut store);
    report(&store);

    if !watch_mode {
        return;
    }

    let roots: Vec<_> = provs.iter().flat_map(|p| p.watch_roots()).collect();
    let watcher = match watch::Watcher::new(&roots, Duration::from_millis(800)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("watch failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("\nwatching {} roots — Ctrl-C to stop", roots.len());
    while watcher.next_change() {
        refresh(&mut store);
        report(&store);
    }
}
