use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, Mutex};
use tauri::Emitter;
use crate::config::{Camera, Config};

// Message type IDs (verified from live HA traffic)
const T_HELLO_REQUEST: u32 = 1;
const T_HELLO_RESPONSE: u32 = 2;
const T_CONNECT_REQUEST: u32 = 3;
const T_CONNECT_RESPONSE: u32 = 4;
const T_DISCONNECT_REQUEST: u32 = 5;
const T_DISCONNECT_RESPONSE: u32 = 6;
const T_PING_REQUEST: u32 = 7;
const T_PING_RESPONSE: u32 = 8;
const T_GET_TIME_REQUEST: u32 = 9;
const T_GET_TIME_RESPONSE: u32 = 10;
const T_LIST_ENTITIES_REQUEST: u32 = 11;
const T_LIST_ENTITIES_SWITCH_RESPONSE: u32 = 17;
const T_LIST_ENTITIES_DONE_RESPONSE: u32 = 19;
const T_SUBSCRIBE_STATES_REQUEST: u32 = 20;
const T_SWITCH_STATE_RESPONSE: u32 = 26;
const T_SWITCH_COMMAND_REQUEST: u32 = 33;

fn cam_key(index: usize) -> u32 {
    (0xCA00u32).wrapping_add(index as u32).wrapping_add(1)
}

fn slugify(name: &str) -> String {
    let s: String = name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    s.trim_matches('_').to_string()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

// ---- Protobuf varint helpers ----

fn encode_varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        if n < 128 {
            out.push(n as u8);
            break;
        }
        out.push((n & 0x7F | 0x80) as u8);
        n >>= 7;
    }
    out
}

fn decode_varint(buf: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        if pos >= buf.len() { return None; }
        let b = buf[pos];
        pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        shift += 7;
        if b & 0x80 == 0 { break; }
    }
    Some((v, pos))
}

fn pb_str(fnum: u32, s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = encode_varint(((fnum << 3) | 2) as u64);
    out.extend(encode_varint(bytes.len() as u64));
    out.extend_from_slice(bytes);
    out
}

fn pb_bool(fnum: u32, v: bool) -> Vec<u8> {
    let mut out = encode_varint(((fnum << 3) | 0) as u64);
    out.extend(encode_varint(if v { 1 } else { 0 }));
    out
}

fn pb_fixed32(fnum: u32, n: u32) -> Vec<u8> {
    let mut out = encode_varint(((fnum << 3) | 5) as u64);
    out.extend_from_slice(&n.to_le_bytes());
    out
}

fn pb_varint_field(fnum: u32, n: u64) -> Vec<u8> {
    let mut out = encode_varint(((fnum << 3) | 0) as u64);
    out.extend(encode_varint(n));
    out
}

// Frame: 0x00 | varint(payload_len) | varint(msg_type) | payload
fn encode_frame(msg_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x00u8];
    out.extend(encode_varint(payload.len() as u64));
    out.extend(encode_varint(msg_type as u64));
    out.extend_from_slice(payload);
    out
}

// Returns (consumed_bytes, vec of (msg_type, payload))
fn decode_frames(buf: &[u8]) -> (usize, Vec<(u32, Vec<u8>)>) {
    let mut frames = Vec::new();
    let mut pos = 0;
    loop {
        let start = pos;
        if pos >= buf.len() || buf[pos] != 0x00 { break; }
        pos += 1;
        let (payload_len, p) = match decode_varint(buf, pos) {
            Some(v) => v,
            None => { pos = start; break; }
        };
        pos = p;
        let (msg_type, payload_start) = match decode_varint(buf, pos) {
            Some(v) => v,
            None => { pos = start; break; }
        };
        if payload_start + payload_len as usize > buf.len() {
            pos = start;
            break;
        }
        let payload = buf[payload_start..payload_start + payload_len as usize].to_vec();
        pos = payload_start + payload_len as usize;
        frames.push((msg_type as u32, payload));
    }
    (pos, frames)
}

fn parse_switch_command(payload: &[u8]) -> Option<(u32, bool)> {
    let mut pos = 0;
    let mut key: Option<u32> = None;
    let mut state: Option<bool> = None;

    while pos < payload.len() {
        let (tag, p) = decode_varint(payload, pos)?;
        pos = p;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u32;

        match (field, wire) {
            (1, 5) => {
                if pos + 4 > payload.len() { return None; }
                key = Some(u32::from_le_bytes(payload[pos..pos+4].try_into().ok()?));
                pos += 4;
            }
            (2, 0) => {
                let (v, p) = decode_varint(payload, pos)?;
                pos = p;
                state = Some(v != 0);
            }
            (_, 0) => { let (_, p) = decode_varint(payload, pos)?; pos = p; }
            (_, 2) => { let (len, p) = decode_varint(payload, pos)?; pos = p + len as usize; }
            (_, 5) => { pos += 4; }
            _ => return None,
        }
    }
    Some((key?, state?))
}

// ---- Server ----

#[derive(Clone)]
pub struct EspHomeState {
    pub config: Arc<Mutex<Config>>,
    pub active_camera_idx: Arc<std::sync::Mutex<i32>>,
    pub broadcast_tx: broadcast::Sender<()>,
}

pub async fn run_server(
    port: u16,
    state: EspHomeState,
    app: tauri::AppHandle,
    proxy_port: Arc<std::sync::Mutex<u16>>,
) {
    let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await {
        Ok(l) => {
            eprintln!("[ESPHome] Listening on :{}", port);
            l
        }
        Err(e) => {
            eprintln!("[ESPHome] Bind error on port {}: {}", port, e);
            return;
        }
    };

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                eprintln!("[ESPHome] Client connected from {}", addr);
                let state = state.clone();
                let app = app.clone();
                let proxy_port = proxy_port.clone();
                tokio::spawn(handle_connection(stream, state, app, proxy_port));
            }
            Err(_) => break,
        }
    }
}

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    state: EspHomeState,
    app: tauri::AppHandle,
    proxy_port: Arc<std::sync::Mutex<u16>>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut rx = state.broadcast_tx.subscribe();

    loop {
        tokio::select! {
            result = stream.read(&mut tmp) => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        let (consumed, frames) = decode_frames(&buf);
                        buf.drain(..consumed);
                        for (msg_type, payload) in frames {
                            if !handle_message(&mut stream, msg_type, &payload, &state, &app, &proxy_port).await {
                                return;
                            }
                        }
                    }
                }
            }
            Ok(_) = rx.recv() => {
                // Broadcast signal: push current states to this client
                send_states(&mut stream, &state).await;
            }
        }
    }
    eprintln!("[ESPHome] Client disconnected");
}

async fn send_states(stream: &mut tokio::net::TcpStream, state: &EspHomeState) {
    let cameras = {
        let cfg = state.config.lock().await;
        cfg.cameras.clone()
    };
    let active = *state.active_camera_idx.lock().unwrap();
    for (i, _) in cameras.iter().enumerate() {
        let payload: Vec<u8> = [
            pb_fixed32(1, cam_key(i)),
            pb_bool(2, active == i as i32),
        ].concat();
        let frame = encode_frame(T_SWITCH_STATE_RESPONSE, &payload);
        let _ = stream.write_all(&frame).await;
    }
}

// Returns false if the connection should close
async fn handle_message(
    stream: &mut tokio::net::TcpStream,
    msg_type: u32,
    payload: &[u8],
    state: &EspHomeState,
    app: &tauri::AppHandle,
    proxy_port: &Arc<std::sync::Mutex<u16>>,
) -> bool {
    match msg_type {
        T_HELLO_REQUEST => {
            let resp: Vec<u8> = [
                pb_varint_field(2, 1),
                pb_varint_field(3, 10),
                pb_str(4, "ha-camera-viewer 2.0.0"),
                pb_str(5, "HA Camera Viewer"),
            ].concat();
            let _ = stream.write_all(&encode_frame(T_HELLO_RESPONSE, &resp)).await;
        }

        T_CONNECT_REQUEST => {
            let resp = pb_bool(1, false); // invalid_password = false
            let _ = stream.write_all(&encode_frame(T_CONNECT_RESPONSE, &resp)).await;
        }

        T_PING_REQUEST => {
            let _ = stream.write_all(&encode_frame(T_PING_RESPONSE, &[])).await;
        }

        T_GET_TIME_REQUEST => {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let resp = pb_varint_field(1, secs);
            let _ = stream.write_all(&encode_frame(T_GET_TIME_RESPONSE, &resp)).await;
        }

        T_DISCONNECT_REQUEST => {
            let _ = stream.write_all(&encode_frame(T_DISCONNECT_RESPONSE, &[])).await;
            return false;
        }

        T_LIST_ENTITIES_REQUEST => {
            let cameras = {
                let cfg = state.config.lock().await;
                cfg.cameras.clone()
            };
            for (i, cam) in cameras.iter().enumerate() {
                let slug = slugify(&cam.name);
                let resp: Vec<u8> = [
                    pb_str(1, &format!("cam_{}", slug)),
                    pb_fixed32(2, cam_key(i)),
                    pb_str(3, &cam.name),
                    pb_str(4, &format!("ha_cam_viewer_{}", slug)),
                    pb_str(5, "mdi:cctv"),
                    pb_bool(6, false),
                ].concat();
                let _ = stream.write_all(&encode_frame(T_LIST_ENTITIES_SWITCH_RESPONSE, &resp)).await;
            }
            let _ = stream.write_all(&encode_frame(T_LIST_ENTITIES_DONE_RESPONSE, &[])).await;
        }

        T_SUBSCRIBE_STATES_REQUEST => {
            send_states(stream, state).await;
        }

        T_SWITCH_COMMAND_REQUEST => {
            if let Some((key, on)) = parse_switch_command(payload) {
                let cameras = {
                    let cfg = state.config.lock().await;
                    cfg.cameras.clone()
                };
                let idx = cameras.iter().enumerate()
                    .find(|(i, _)| cam_key(*i) == key)
                    .map(|(i, _)| i);

                if let Some(idx) = idx {
                    if on {
                        *state.active_camera_idx.lock().unwrap() = idx as i32;
                        let _ = state.broadcast_tx.send(());
                        let cam = cameras[idx].clone();
                        let config = state.config.lock().await.clone();
                        let port = *proxy_port.lock().unwrap();
                        let app = app.clone();
                        tokio::spawn(async move {
                            do_show_camera(&app, &cam, &config, port).await;
                        });
                    } else {
                        *state.active_camera_idx.lock().unwrap() = -1;
                        let _ = state.broadcast_tx.send(());
                        do_hide_popup(app);
                    }
                }
            }
        }

        _ => {}
    }
    true
}

pub async fn do_show_camera(
    app: &tauri::AppHandle,
    cam: &Camera,
    config: &Config,
    proxy_port: u16,
) {
    use tauri::{Manager, WebviewWindowBuilder, WebviewUrl};

    let url = format!(
        "http://127.0.0.1:{}/api/camera_proxy_stream/{}",
        proxy_port, cam.entity_id
    );

    let window = if let Some(w) = app.get_webview_window("popup") {
        let _ = w.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(
            config.width, config.height,
        )));
        let _ = w.set_always_on_top(config.always_on_top);
        w
    } else {
        match WebviewWindowBuilder::new(app, "popup", WebviewUrl::App("popup.html".into()))
            .decorations(false)
            .always_on_top(config.always_on_top)
            .skip_taskbar(true)
            .inner_size(config.width as f64, config.height as f64)
            .visible(false)
            .build()
        {
            Ok(w) => w,
            Err(e) => { eprintln!("popup window error: {}", e); return; }
        }
    };

    // Position bottom-right of primary monitor
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let wa = monitor.work_area();
        let x = wa.position.x + wa.size.width as i32 - config.width as i32 - 16;
        let y = wa.position.y + wa.size.height as i32 - config.height as i32 - 16;
        let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)));
    }

    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("show-camera", serde_json::json!({
        "cameraUrl": url,
        "cameraEntityId": cam.entity_id,
        "timeout": config.timeout,
        "showBar": config.show_timer_bar,
    }));
}

pub fn do_hide_popup(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.hide();
    }
}
