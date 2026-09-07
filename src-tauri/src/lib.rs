//! TokenHUD desktop overlay: owns the store + watcher, streams snapshots to the webview.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokenhud_core::{aggregate::ToolSnapshot, refresh, snapshot_all, store::Store, watch::Watcher, watch_roots};

type SharedStore = Arc<Mutex<Store>>;

/// Pull the current snapshots on demand (used by the frontend on load).
#[tauri::command]
fn get_snapshots(store: tauri::State<'_, SharedStore>) -> Result<Vec<ToolSnapshot>, String> {
    let store = store.lock().map_err(|e| e.to_string())?;
    Ok(snapshot_all(&store))
}

/// Push the latest snapshots to every window as a `usage` event.
fn emit_snapshots(app: &AppHandle, store: &SharedStore) {
    let payload = {
        let Ok(store) = store.lock() else { return };
        snapshot_all(&store)
    };
    let _ = app.emit("usage", payload);
}

fn spawn_worker(app: AppHandle, store: SharedStore) {
    std::thread::spawn(move || {
        {
            let mut s = store.lock().unwrap();
            refresh(&mut s);
        }
        emit_snapshots(&app, &store);

        let roots = watch_roots();
        let watcher = match Watcher::new(&roots, Duration::from_millis(800)) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("tokenhud: watch failed: {e}");
                return;
            }
        };
        while watcher.next_change() {
            {
                let mut s = store.lock().unwrap();
                refresh(&mut s);
            }
            emit_snapshots(&app, &store);
        }
    });
}

pub fn run() {
    let store: SharedStore = Arc::new(Mutex::new(
        Store::open_default().expect("open usage store"),
    ));

    tauri::Builder::default()
        .manage(store.clone())
        .invoke_handler(tauri::generate_handler![get_snapshots])
        .setup(move |app| {
            spawn_worker(app.handle().clone(), store.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running TokenHUD");
}
