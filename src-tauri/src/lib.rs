//! TokenHUD desktop overlay: owns the store + watcher, streams snapshots to the
//! webview, raises threshold notifications, and adds the OS-level niceties an
//! always-on-top HUD needs — a tray icon, a global toggle shortcut, a settings
//! window, and per-platform window tweaks.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_notification::NotificationExt;
use tokenhud_core::{
    aggregate::ToolSnapshot, config::Config, has_remote_providers, refresh, snapshot_all,
    store::Store, summary, watch::Watcher, watch_roots, Scope,
};

/// How often opt-in remote providers (Cursor, Copilot) are polled.
const REMOTE_POLL: Duration = Duration::from_secs(300);

const HUD: &str = "hud";
const SETTINGS: &str = "settings";
const TOGGLE_SHORTCUT: &str = "CmdOrCtrl+Shift+T";

type Shared<T> = Arc<Mutex<T>>;

struct AppState {
    store: Shared<Store>,
    config: Shared<Config>,
}

// ---- commands ---------------------------------------------------------------

#[tauri::command]
fn get_snapshots(state: tauri::State<'_, AppState>) -> Result<Vec<ToolSnapshot>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(snapshot_all(&store, &config))
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> Result<Config, String> {
    state
        .config
        .lock()
        .map(|c| c.clone())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_caps(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    tool: String,
    hour: Option<u64>,
    five_h: Option<u64>,
    week: Option<u64>,
) -> Result<(), String> {
    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        let entry = config.caps.entry(tool).or_default();
        entry.hour = hour;
        entry.five_h = five_h;
        entry.week = week;
        config.save().map_err(|e| e.to_string())?;
    }
    push_snapshots(&app, &state);
    Ok(())
}

#[tauri::command]
fn run_summary(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let snaps = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let config = state.config.lock().map_err(|e| e.to_string())?;
        snapshot_all(&store, &config)
    };
    let config = state.config.lock().map_err(|e| e.to_string())?;
    summary::weekly(&config.summary, &snaps)
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    show_settings(&app);
}

// ---- snapshot plumbing -----------------------------------------------------

fn push_snapshots(app: &AppHandle, state: &AppState) {
    let payload = {
        let (Ok(store), Ok(config)) = (state.store.lock(), state.config.lock()) else {
            return;
        };
        snapshot_all(&store, &config)
    };
    let _ = app.emit("usage", &payload);
}

/// Fire a desktop notification the first time a window crosses 80% / 95%, and
/// re-arm once it falls back below 50%.
fn check_thresholds(app: &AppHandle, snaps: &[ToolSnapshot], fired: &mut HashSet<String>) {
    let mut ratios: Vec<(String, f64)> = Vec::new();
    for s in snaps {
        for (label, w) in [("hour", &s.hour), ("5h", &s.five_h), ("week", &s.week)] {
            if let Some(r) = w.ratio {
                ratios.push((format!("{} {label}", s.tool), r));
            }
        }
        for rl in &s.rate_limits {
            ratios.push((
                format!("{} {}", s.tool, rl.window_label),
                rl.used_percent / 100.0,
            ));
        }
    }

    for (name, ratio) in ratios {
        for pct in [95u32, 80] {
            let key = format!("{name}:{pct}");
            let crossed = ratio >= pct as f64 / 100.0;
            if crossed && !fired.contains(&key) {
                fired.insert(key);
                let _ = app
                    .notification()
                    .builder()
                    .title("TokenHUD")
                    .body(format!("{name} usage at {:.0}%", ratio * 100.0))
                    .show();
                break;
            }
            if ratio < 0.5 {
                fired.remove(&key);
            }
        }
    }
}

fn refresh_and_emit(
    app: &AppHandle,
    store: &Shared<Store>,
    config: &Shared<Config>,
    scope: Scope,
    fired: &Shared<HashSet<String>>,
) {
    {
        let Ok(mut s) = store.lock() else { return };
        refresh(&mut s, scope);
    }
    let snaps = {
        let (Ok(s), Ok(c)) = (store.lock(), config.lock()) else {
            return;
        };
        snapshot_all(&s, &c)
    };
    if let Ok(mut f) = fired.lock() {
        check_thresholds(app, &snaps, &mut f);
    }
    let _ = app.emit("usage", &snaps);
}

fn spawn_worker(app: AppHandle, store: Shared<Store>, config: Shared<Config>) {
    let fired: Shared<HashSet<String>> = Arc::new(Mutex::new(HashSet::new()));

    // Fast path: re-scan local logs on every debounced filesystem change.
    {
        let (app, store, config, fired) =
            (app.clone(), store.clone(), config.clone(), fired.clone());
        std::thread::spawn(move || {
            refresh_and_emit(&app, &store, &config, Scope::IncludeRemote, &fired);
            let roots = watch_roots();
            let watcher = match Watcher::new(&roots, Duration::from_millis(800)) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("tokenhud: watch failed: {e}");
                    return;
                }
            };
            while watcher.next_change() {
                refresh_and_emit(&app, &store, &config, Scope::LocalOnly, &fired);
            }
        });
    }

    // Slow path: poll opt-in remote providers on a timer, off the hot path.
    std::thread::spawn(move || loop {
        std::thread::sleep(REMOTE_POLL);
        if has_remote_providers() {
            refresh_and_emit(&app, &store, &config, Scope::IncludeRemote, &fired);
        }
    });
}

// ---- windows / tray ------------------------------------------------------------

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

/// Run the opt-in weekly LLM summary on a worker thread and show it as a
/// notification (it needs an API key in config.toml).
fn show_summary(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let result = {
            let snaps = {
                let (Ok(store), Ok(config)) = (state.store.lock(), state.config.lock()) else {
                    return;
                };
                snapshot_all(&store, &config)
            };
            let Ok(config) = state.config.lock() else {
                return;
            };
            summary::weekly(&config.summary, &snaps)
        };
        let body = match result {
            Ok(text) => text,
            Err(e) => format!("Summary unavailable — {e}"),
        };
        let _ = app
            .notification()
            .builder()
            .title("TokenHUD — weekly summary")
            .body(body)
            .show();
    });
}

fn show_settings(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(SETTINGS) {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
        .title("TokenHUD Settings")
        .inner_size(320.0, 320.0)
        .resizable(false)
        .build();
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Show / hide HUD", true, None::<&str>)?;
    let summary = MenuItem::with_id(app, "summary", "Weekly summary…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TokenHUD"))?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &summary,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("tokenhud")
        .tooltip("TokenHUD")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => toggle_hud(app),
            "settings" => show_settings(app),
            "summary" => show_summary(app),
            _ => {}
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

    // A monochrome template image so the icon adapts to light/dark menu bars.
    match tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
        Ok(icon) => {
            builder = builder.icon(icon).icon_as_template(true);
        }
        Err(_) => {
            if let Some(icon) = app.default_window_icon().cloned() {
                builder = builder.icon(icon);
            }
        }
    }
    builder.build(app)?;
    Ok(())
}

/// If a restored window position leaves the HUD off every monitor (a monitor was
/// unplugged, resolution changed…), snap it back to the primary's top-right.
fn ensure_on_screen(app: &AppHandle) {
    let Some(win) = app.get_webview_window(HUD) else {
        return;
    };
    let (Ok(pos), Ok(size), Ok(monitors)) = (
        win.outer_position(),
        win.outer_size(),
        win.available_monitors(),
    ) else {
        return;
    };

    let visible = monitors.iter().any(|m| {
        let mp = m.position();
        let ms = m.size();
        let (l, t) = (mp.x, mp.y);
        let (r, b) = (mp.x + ms.width as i32, mp.y + ms.height as i32);
        // At least a 48px sliver of the title area is on this monitor.
        pos.x + 48 < r && pos.x + size.width as i32 - 48 > l && pos.y + 8 < b && pos.y + 8 > t - 8
    });

    if !visible {
        if let Some(primary) = win.primary_monitor().ok().flatten() {
            let ms = primary.size();
            let mp = primary.position();
            let x = mp.x + ms.width as i32 - size.width as i32 - 24;
            let y = mp.y + 40;
            let _ = win.set_position(tauri::PhysicalPosition::new(x.max(mp.x), y));
        }
    }
}

fn tune_window(app: &AppHandle) {
    ensure_on_screen(app);

    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }

    #[cfg(target_os = "windows")]
    if let Some(win) = app.get_webview_window(HUD) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        };
        if let Ok(handle) = win.hwnd() {
            let hwnd = handle.0 as *mut core::ffi::c_void;
            unsafe {
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                let want = (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize;
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | want);
            }
        }
    }

    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!(
            "tokenhud: Wayland session detected — always-on-top is unreliable here; \
             use the tray icon to summon the HUD."
        );
    }
}

pub fn run() {
    let store: Shared<Store> =
        Arc::new(Mutex::new(Store::open_default().expect("open usage store")));
    let config: Shared<Config> = Arc::new(Mutex::new(Config::load()));
    let (worker_store, worker_config) = (store.clone(), config.clone());

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
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
        .manage(AppState { store, config })
        .invoke_handler(tauri::generate_handler![
            get_snapshots,
            get_config,
            set_caps,
            run_summary,
            open_settings
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            build_tray(&handle)?;
            tune_window(&handle);
            spawn_worker(handle, worker_store, worker_config);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running TokenHUD");
}
