//! TokenHUD desktop overlay: owns the store + watcher, streams snapshots to the
//! webview, and adds the OS-level niceties an always-on-top HUD needs — a tray
//! icon, a global toggle shortcut, and per-platform window tweaks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};
use tokenhud_core::{
    aggregate::ToolSnapshot, refresh, snapshot_all, store::Store, watch::Watcher, watch_roots,
};

const HUD: &str = "hud";
const TOGGLE_SHORTCUT: &str = "CmdOrCtrl+Shift+T";

type SharedStore = Arc<Mutex<Store>>;

/// Pull the current snapshots on demand (used by the frontend on load).
#[tauri::command]
fn get_snapshots(store: tauri::State<'_, SharedStore>) -> Result<Vec<ToolSnapshot>, String> {
    let store = store.lock().map_err(|e| e.to_string())?;
    Ok(snapshot_all(&store))
}

/// Show the HUD if hidden, hide it if visible.
fn toggle_hud(app: &AppHandle) {
    let Some(win) = app.get_webview_window(HUD) else {
        return;
    };
    match win.is_visible() {
        Ok(true) => {
            let _ = win.hide();
        }
        _ => {
            let _ = win.show();
            let _ = win.set_always_on_top(true);
        }
    }
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

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Show / hide HUD", true, None::<&str>)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TokenHUD"))?;
    let menu = Menu::with_items(app, &[&toggle, &PredefinedMenuItem::separator(app)?, &quit])?;

    let mut builder = TrayIconBuilder::with_id("tokenhud")
        .tooltip("TokenHUD")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "toggle" {
                toggle_hud(app);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_hud(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// Per-platform window behaviour that can't be expressed in tauri.conf.json.
fn tune_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        // HUD-only: no Dock icon, never becomes the active app.
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }

    #[cfg(target_os = "windows")]
    if let Some(win) = app.get_webview_window(HUD) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        };
        if let Ok(hwnd) = win.hwnd() {
            let hwnd = hwnd.0 as isize;
            unsafe {
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    ex | WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize,
                );
            }
        }
    }

    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!(
            "tokenhud: Wayland session detected — always-on-top and click-through \
             are unreliable here; use the tray icon to summon the HUD."
        );
    }
}

pub fn run() {
    let store: SharedStore = Arc::new(Mutex::new(Store::open_default().expect("open usage store")));
    let worker_store = store.clone();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(tauri_plugin_window_state::StateFlags::POSITION)
                .build(),
        )
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcut(TOGGLE_SHORTCUT)
                .expect("valid shortcut")
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        toggle_hud(app);
                    }
                })
                .build(),
        )
        .manage(store)
        .invoke_handler(tauri::generate_handler![get_snapshots])
        .setup(move |app| {
            let handle = app.handle().clone();
            build_tray(&handle)?;
            tune_window(&handle);
            spawn_worker(handle, worker_store);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running TokenHUD");
}
