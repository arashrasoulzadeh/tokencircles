//! TokenHUD desktop overlay: owns the store + watcher, streams snapshots to the
//! webview, raises threshold notifications, and adds the OS-level niceties an
//! always-on-top HUD needs — a tray icon, a global toggle shortcut, a settings
//! window, and per-platform window tweaks.

mod hud;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hud::{
    circle_edge_position, mode_str, pending_alerts, rescue_position, window_is_reachable, Rect,
    CARD_SIZE, CIRCLE_SIZE,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_notification::NotificationExt;
use tokenhud_core::{
    aggregate::ToolSnapshot,
    config::{Config, HudMode, ScreenSide},
    has_remote_providers, refresh, snapshot_all,
    store::Store,
    summary,
    watch::Watcher,
    watch_roots, Scope,
};

/// Steady tick. Every tick does an incremental local rescan (only changed log
/// files are re-read, so it's cheap) — this keeps the numbers current even if a
/// filesystem event is missed or the session is idle.
const TICK: Duration = Duration::from_secs(15);
/// Every this-many ticks (~5 min) also poll the opt-in remote providers.
const REMOTE_EVERY_TICKS: u32 = 20;

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

/// Replace the whole config from the settings window: persist, re-lay-out, and
/// re-emit a snapshot so the HUD reflects new caps immediately.
#[tauri::command]
fn set_config(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    config: Config,
) -> Result<(), String> {
    {
        let mut cur = state.config.lock().map_err(|e| e.to_string())?;
        *cur = config;
        cur.save().map_err(|e| e.to_string())?;
    }
    relayout(&app);
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

/// Pop the shared menu at the cursor — the HUD's right-click menu.
#[tauri::command]
fn show_context_menu(app: AppHandle) {
    if let (Some(win), Ok(menu)) = (app.get_webview_window(HUD), build_menu(&app)) {
        let _ = win.popup_menu(&menu);
    }
}

#[tauri::command]
fn set_mode(app: AppHandle, state: tauri::State<'_, AppState>, mode: String) -> Result<(), String> {
    let new = if mode == "circle" {
        HudMode::Circle
    } else {
        HudMode::Card
    };
    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        config.ui.mode = new;
        config.save().map_err(|e| e.to_string())?;
    }
    relayout(&app);
    Ok(())
}

#[tauri::command]
fn set_circle_side(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    side: String,
) -> Result<(), String> {
    let new = if side == "right" {
        ScreenSide::Right
    } else {
        ScreenSide::Left
    };
    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        config.ui.circle_side = new;
        config.save().map_err(|e| e.to_string())?;
    }
    relayout(&app);
    Ok(())
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

/// Raise a desktop notification for each newly-crossed usage threshold.
fn check_thresholds(app: &AppHandle, snaps: &[ToolSnapshot], fired: &mut HashSet<String>) {
    for body in pending_alerts(snaps, fired) {
        let _ = app
            .notification()
            .builder()
            .title("TokenHUD")
            .body(body)
            .show();
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

    // Fast path: incremental local rescan on every debounced filesystem change.
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

    // Steady tick: every TICK an incremental local rescan (only changed files),
    // plus the opt-in remotes every REMOTE_EVERY_TICKS. Covers missed fs events
    // and keeps the reset countdown current while idle.
    std::thread::spawn(move || {
        let mut n: u32 = 0;
        loop {
            std::thread::sleep(TICK);
            n = n.wrapping_add(1);
            let scope = if n.is_multiple_of(REMOTE_EVERY_TICKS) && has_remote_providers() {
                Scope::IncludeRemote
            } else {
                Scope::LocalOnly
            };
            refresh_and_emit(&app, &store, &config, scope, &fired);
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

/// Flip between card and circle mode, persist, and re-lay-out.
fn cycle_mode(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let Ok(mut config) = state.config.lock() else {
            return;
        };
        config.ui.mode = match config.ui.mode {
            HudMode::Card => HudMode::Circle,
            HudMode::Circle => HudMode::Card,
        };
        let _ = config.save();
    }
    if let Some(win) = app.get_webview_window(HUD) {
        let _ = win.show();
    }
    relayout(app);
}

/// Move circle mode to the other screen edge.
fn cycle_side(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let Ok(mut config) = state.config.lock() else {
            return;
        };
        config.ui.circle_side = config.ui.circle_side.flipped();
        let _ = config.save();
    }
    relayout(app);
}

fn show_settings(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(SETTINGS) {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
        .title("TokenHUD Settings")
        .inner_size(400.0, 640.0)
        .min_inner_size(360.0, 420.0)
        .resizable(true)
        .build();
}

/// The shared menu used by both the tray icon and the HUD's right-click menu.
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let toggle = MenuItem::with_id(app, "toggle", "Show / hide HUD", true, None::<&str>)?;
    let mode = MenuItem::with_id(app, "mode", "Switch card / circle mode", true, None::<&str>)?;
    let side = MenuItem::with_id(
        app,
        "side",
        "Circle: move to other side",
        true,
        None::<&str>,
    )?;
    let summary = MenuItem::with_id(app, "summary", "Weekly summary…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TokenHUD"))?;
    Menu::with_items(
        app,
        &[
            &toggle,
            &mode,
            &side,
            &summary,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

/// Route a menu item id from either the tray or the right-click menu.
fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "toggle" => toggle_hud(app),
        "mode" => cycle_mode(app),
        "side" => cycle_side(app),
        "settings" => show_settings(app),
        "summary" => show_summary(app),
        _ => {}
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;

    // Menu item events are handled by the app-level `on_menu_event` so the
    // tray and the HUD's right-click menu share one code path.
    let mut builder = TrayIconBuilder::with_id("tokenhud")
        .tooltip("TokenHUD")
        .menu(&menu)
        .show_menu_on_left_click(false)
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

    let win_rect = Rect::new(
        pos.x as f64,
        pos.y as f64,
        size.width as f64,
        size.height as f64,
    );
    let screens: Vec<Rect> = monitors
        .iter()
        .map(|m| {
            let (mp, ms) = (m.position(), m.size());
            Rect::new(mp.x as f64, mp.y as f64, ms.width as f64, ms.height as f64)
        })
        .collect();

    if !window_is_reachable(win_rect, &screens) {
        if let Some(primary) = win.primary_monitor().ok().flatten() {
            let (mp, ms) = (primary.position(), primary.size());
            let prim = Rect::new(mp.x as f64, mp.y as f64, ms.width as f64, ms.height as f64);
            let (x, y) = rescue_position(prim, (size.width as f64, size.height as f64));
            let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
        }
    }
}

/// Resize the HUD for the chosen mode and, in circle mode, pin it to the
/// configured screen edge, vertically centred. The frontend picks up the
/// layout from the `mode` event and its own `get_config` call.
fn apply_mode(app: &AppHandle, mode: HudMode, side: ScreenSide) {
    let Some(win) = app.get_webview_window(HUD) else {
        return;
    };
    let _ = app.emit("mode", mode_str(mode));

    match mode {
        HudMode::Card => {
            let _ = win.set_min_size(Some(LogicalSize::new(CARD_SIZE.0, 120.0)));
            let _ = win.set_size(LogicalSize::new(CARD_SIZE.0, CARD_SIZE.1));
            ensure_on_screen(app);
        }
        HudMode::Circle => {
            let _ = win.set_min_size(Some(LogicalSize::new(CIRCLE_SIZE.0, 120.0)));
            let _ = win.set_size(LogicalSize::new(CIRCLE_SIZE.0, CIRCLE_SIZE.1));
            if let Some(primary) = win.primary_monitor().ok().flatten() {
                let scale = primary.scale_factor();
                let ms = primary.size().to_logical::<f64>(scale);
                let mp = primary.position().to_logical::<f64>(scale);
                let monitor = Rect::new(mp.x, mp.y, ms.width, ms.height);
                let (x, y) = circle_edge_position(monitor, CIRCLE_SIZE, side);
                let _ = win.set_position(LogicalPosition::new(x, y));
            }
        }
    }
    let _ = win.set_always_on_top(true);
    // Circle mode is click-through: the rings never eat a click. Right-clicks
    // are still caught by the global mouse watcher (see spawn_rightclick_watcher).
    let _ = win.set_ignore_cursor_events(matches!(mode, HudMode::Circle));
    let _ = win.show();
}

/// Re-apply the stored mode + side (used after a config change).
fn relayout(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (mode, side) = state
        .config
        .lock()
        .map(|c| (c.ui.mode, c.ui.circle_side))
        .unwrap_or((HudMode::Card, ScreenSide::Left));
    apply_mode(app, mode, side);
}

/// While the HUD is click-through (circle mode), a background watcher is the
/// only way a right-click on the rings can reach us: poll the global mouse and,
/// on a right-button press inside the HUD's rectangle, pop the context menu.
///
/// On macOS this needs Accessibility permission; without it `device_query`
/// panics, so we probe once, catch it, and quietly fall back to the tray menu.
fn spawn_rightclick_watcher(app: AppHandle) {
    use device_query::{DeviceQuery, DeviceState};
    std::thread::spawn(move || {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let probe = std::panic::catch_unwind(|| {
            let ds = DeviceState::new();
            let _ = ds.get_mouse();
            ds
        });
        std::panic::set_hook(prev_hook);
        let ds = match probe {
            Ok(ds) => ds,
            Err(_) => {
                eprintln!(
                    "tokenhud: no Accessibility permission — right-click on the circle \
                     won't work; use the tray icon, or grant it in System Settings › \
                     Privacy & Security › Accessibility."
                );
                return;
            }
        };

        let mut was_down = false;
        loop {
            std::thread::sleep(Duration::from_millis(40));
            let mouse = ds.get_mouse();
            // device_query: index 3 is the right button.
            let down = mouse.button_pressed.get(3).copied().unwrap_or(false);
            let pressed_edge = down && !was_down;
            was_down = down;
            if !pressed_edge {
                continue;
            }

            let state = app.state::<AppState>();
            let is_circle = state
                .config
                .lock()
                .map(|c| c.ui.mode == HudMode::Circle)
                .unwrap_or(false);
            if !is_circle {
                continue;
            }

            let Some(win) = app.get_webview_window(HUD) else {
                continue;
            };
            let (Ok(pos), Ok(size), scale) = (
                win.outer_position(),
                win.outer_size(),
                win.scale_factor().unwrap_or(1.0),
            ) else {
                continue;
            };
            let p = pos.to_logical::<f64>(scale);
            let s = size.to_logical::<f64>(scale);
            let (mx, my) = (mouse.coords.0 as f64, mouse.coords.1 as f64);
            let pad = 6.0;
            let inside = mx >= p.x - pad
                && mx <= p.x + s.width + pad
                && my >= p.y - pad
                && my <= p.y + s.height + pad;
            if inside {
                show_context_menu(app.clone());
            }
        }
    });
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
            set_config,
            set_mode,
            set_circle_side,
            run_summary,
            open_settings,
            show_context_menu
        ])
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .setup(move |app| {
            let handle = app.handle().clone();
            build_tray(&handle)?;
            tune_window(&handle);
            relayout(&handle);
            spawn_rightclick_watcher(handle.clone());
            spawn_worker(handle, worker_store, worker_config);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running TokenHUD");
}
