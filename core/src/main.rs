//! TokenHUD CLI — passive local usage meter for code-generation CLIs.
//!
//! `tokenhud`          one-shot: scan, ingest, print current windows
//! `tokenhud --watch`  stay running, reprint whenever transcripts change
//! `tokenhud summary`  one-off LLM summary (needs [summary] key in config.toml)

use std::time::Duration;
use tokenhud_core::{
    refresh, snapshot_all, store::Store, summary, watch::Watcher, watch_roots, Config, Scope,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let watch_mode = args.iter().any(|a| a == "--watch" || a == "-w");
    let summary_mode = args.iter().any(|a| a == "summary");

    let config = Config::load();
    let mut store = match Store::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open store: {e}");
            std::process::exit(1);
        }
    };

    let n = refresh(&mut store, Scope::IncludeRemote);
    if n > 0 {
        eprintln!("  +{n} new events");
    }
    let snaps = snapshot_all(&store, &config);

    if summary_mode {
        match summary::weekly(&config.summary, &snaps) {
            Ok(text) => println!("\n{text}"),
            Err(e) => {
                eprintln!("summary unavailable: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let report = |store: &Store| {
        println!(
            "\nTokenHUD — {} events stored — {}",
            store.row_count().unwrap_or(0),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let snaps = snapshot_all(store, &config);
        if snaps.is_empty() {
            eprintln!("no supported CLI logs found on this machine");
        }
        for s in snaps {
            s.print();
        }
    };
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
        let n = refresh(&mut store, Scope::LocalOnly);
        if n > 0 {
            eprintln!("  +{n} new events");
        }
        report(&store);
    }
}
