#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod esphome;
mod ha_ws;
mod proxy;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
};

// ---- App state ----

#[derive(Clone)]
struct AppState {
    config: Arc<Mutex<config::Config>>,
    proxy_port: Arc<std::sync::Mutex<u16>>,
    active_camera_idx: Arc<std::sync::Mutex<i32>>,
    broadcast_tx: broadcast::Sender<()>,
    close_timer: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    // Handles for restarting background tasks
    esphome_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ha_ws_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl AppState {
    fn new(cfg: config::Config) -> Self {
        let (broadcast_tx, _) = broadcast::channel(8);
        Self {
            config: Arc::new(Mutex::new(cfg)),
            proxy_port: Arc::new(std::sync::Mutex::new(0)),
            active_camera_idx: Arc::new(std::sync::Mutex::new(-1)),
            broadcast_tx,
            close_timer: Arc::new(Mutex::new(None)),
            esphome_handle: Arc::new(Mutex::new(None)),
            ha_ws_handle: Arc::new(Mutex::new(None)),
        }
    }
}

// ---- Tray management ----

fn build_tray(app: &tauri::App, cameras: &[config::Camera]) -> tauri::Result<()> {
    let menu = build_tray_menu(app.handle(), cameras)?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("HA Camera Viewer")
        .menu(&menu)
        .on_menu_event({
            let cameras: Vec<config::Camera> = cameras.to_vec();
            move |app, event| {
                let id = event.id().as_ref();
                match id {
                    "settings" => open_settings(app),
                    "quit" => app.exit(0),
                    s if s.starts_with("camera_") => {
                        if let Ok(idx) = s["camera_".len()..].parse::<usize>() {
                            if let Some(cam) = cameras.get(idx) {
                                let cam = cam.clone();
                                let app = app.clone();
                                tauri::async_runtime::spawn(async move {
                                    let state: tauri::State<'_, AppState> = app.state();
                                    let config = state.config.lock().await.clone();
                                    let proxy_port = *state.proxy_port.lock().unwrap();
                                    let timeout = config.manual_timeout;
                                    let idx_i32 = idx as i32;
                                    *state.active_camera_idx.lock().unwrap() = idx_i32;
                                    let _ = state.broadcast_tx.send(());
                                    esphome::do_show_camera(&app, &cam, &config, proxy_port).await;
                                    start_close_timer(&app, timeout).await;
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .build(app)?;
    Ok(())
}

fn build_tray_menu(
    app: &tauri::AppHandle,
    cameras: &[config::Camera],
) -> tauri::Result<Menu<tauri::Wry>> {
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = Vec::new();

    let camera_items: Vec<MenuItem<tauri::Wry>> = cameras
        .iter()
        .enumerate()
        .map(|(i, cam)| {
            MenuItem::with_id(app, format!("camera_{}", i), &cam.name, true, None::<&str>)
                .unwrap()
        })
        .collect();
    for item in &camera_items {
        items.push(item);
    }

    let no_cameras = MenuItem::with_id(app, "no_cameras", "No cameras configured", false, None::<&str>)?;
    if cameras.is_empty() {
        items.push(&no_cameras);
    }

    let sep = PredefinedMenuItem::separator(app)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    items.push(&sep);
    items.push(&settings);
    items.push(&sep2);
    items.push(&quit);

    Menu::with_items(app, &items)
}

fn rebuild_tray_menu(app: &tauri::AppHandle, cameras: &[config::Camera]) {
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(menu) = build_tray_menu(app, cameras) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

// ---- Window helpers ----

fn open_settings(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.set_focus();
        return;
    }
    match WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("HA Camera Viewer — Settings")
        .inner_size(560.0, 720.0)
        .resizable(false)
        .build()
    {
        Ok(_) => {}
        Err(e) => eprintln!("settings window error: {}", e),
    }
}

async fn start_close_timer(app: &tauri::AppHandle, timeout_secs: u32) {
    let state: tauri::State<'_, AppState> = app.state();
    let mut timer = state.close_timer.lock().await;
    if let Some(h) = timer.take() {
        h.abort();
    }
    if timeout_secs > 0 {
        let app = app.clone();
        *timer = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(timeout_secs as u64)).await;
            let state: tauri::State<'_, AppState> = app.state();
            *state.active_camera_idx.lock().unwrap() = -1;
            let _ = state.broadcast_tx.send(());
            esphome::do_hide_popup(&app);
        }));
    }
}

// ---- Background service management ----

async fn restart_services(app: &tauri::AppHandle, state: &AppState) {
    let config = state.config.lock().await.clone();

    // Restart ESPHome
    {
        let mut handle = state.esphome_handle.lock().await;
        if let Some(h) = handle.take() { h.abort(); }

        let esp_state = esphome::EspHomeState {
            config: state.config.clone(),
            active_camera_idx: state.active_camera_idx.clone(),
            broadcast_tx: state.broadcast_tx.clone(),
        };
        let app_clone = app.clone();
        let port = config.esp_port;
        let proxy_port = state.proxy_port.clone();

        *handle = Some(tokio::spawn(async move {
            esphome::run_server(port, esp_state, app_clone, proxy_port).await;
        }));
    }

    // Restart HA WebSocket
    {
        let mut handle = state.ha_ws_handle.lock().await;
        if let Some(h) = handle.take() { h.abort(); }

        if !config.ha_url.is_empty() && !config.ha_token.is_empty() {
            let url = config.ha_url.clone();
            let token = config.ha_token.clone();
            *handle = Some(tokio::spawn(async move {
                ha_ws::run(url, token).await;
            }));
        }
    }
}

// ---- IPC commands ----

#[tauri::command]
async fn get_config(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let config = state.config.lock().await;
    let mut json = serde_json::to_value(&*config).map_err(|e| e.to_string())?;

    use tauri_plugin_autostart::ManagerExt;
    let start_with_windows = app.autolaunch().is_enabled().unwrap_or(false);
    json["startWithWindows"] = serde_json::Value::Bool(start_with_windows);

    Ok(json)
}

#[tauri::command]
async fn save_config(
    config: serde_json::Value,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Handle startWithWindows (not part of Config struct)
    if let Some(start) = config["startWithWindows"].as_bool() {
        use tauri_plugin_autostart::ManagerExt;
        let mgr = app.autolaunch();
        if start { mgr.enable().map_err(|e| e.to_string())?; }
        else { mgr.disable().map_err(|e| e.to_string())?; }
    }

    // Parse and save Config (unknown fields like startWithWindows are ignored)
    let new_config: config::Config = serde_json::from_value(config).map_err(|e| e.to_string())?;
    {
        let mut cfg = state.config.lock().await;
        *cfg = new_config.clone();
    }
    config::save(&new_config)?;

    // Rebuild tray and restart services
    rebuild_tray_menu(&app, &new_config.cameras);
    restart_services(&app, &state).await;

    Ok(())
}

#[tauri::command]
async fn close_popup(state: tauri::State<'_, AppState>, app: tauri::AppHandle) -> Result<(), ()> {
    let mut timer = state.close_timer.lock().await;
    if let Some(h) = timer.take() { h.abort(); }
    *state.active_camera_idx.lock().unwrap() = -1;
    let _ = state.broadcast_tx.send(());
    esphome::do_hide_popup(&app);
    Ok(())
}

#[tauri::command]
async fn extend_timer(
    seconds: i32,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), ()> {
    {
        let mut timer = state.close_timer.lock().await;
        if let Some(h) = timer.take() { h.abort(); }
        if seconds > 0 {
            let app_clone = app.clone();
            *timer = Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(seconds as u64)).await;
                let state: tauri::State<'_, AppState> = app_clone.state();
                *state.active_camera_idx.lock().unwrap() = -1;
                let _ = state.broadcast_tx.send(());
                esphome::do_hide_popup(&app_clone);
            }));
        }
    }
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.emit("timer-extended", seconds);
    }
    Ok(())
}

// ---- Entry point ----

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .setup(|app| {
            let cfg = config::load();
            let state = AppState::new(cfg.clone());
            app.manage(state.clone());

            // Start proxy
            let config_arc = state.config.clone();
            let proxy_port_arc = state.proxy_port.clone();
            tauri::async_runtime::spawn(async move {
                let port = proxy::start(config_arc).await;
                *proxy_port_arc.lock().unwrap() = port;
            });

            // Build tray
            build_tray(app, &cfg.cameras)?;

            // Start ESPHome + HA WS
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                restart_services(&app_handle, &app_handle.state::<AppState>()).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            close_popup,
            extend_timer,
        ])
        .run(tauri::generate_context!())
        .expect("tauri app error");
}
