# HA Camera Viewer

A Windows system tray app that connects to Home Assistant and displays camera feeds in a popup window, triggered by HA automations.

Built with **Tauri** — uses the Windows WebView2 runtime (pre-installed on Windows 10/11) instead of bundling a browser. Installer is under 4 MB.

No MQTT or manual helpers needed — it simulates an ESPHome device so HA discovers it natively.

---

## Features

- Runs in the Windows system tray
- Pop-up camera viewer in the bottom-right corner of your screen
- Shows any HA camera entity (MJPEG stream)
- Multiple cameras — each appears as its own switch in HA
- Triggered from HA automations (turn a switch on to show a camera, off to hide)
- Auto-close timer with progress bar, +30s / Keep Open buttons
- Adjustable window size and position
- Start with Windows option

---

## Download

Grab the installer from the [Releases](../../releases) page.

---

## Setup

### 1. Install and configure the app

1. Run the installer and launch from the Start Menu or Desktop shortcut
2. Right-click the tray icon → **Settings**
3. Enter your **HA URL** (e.g. `http://192.168.1.100:8123` or your Nabu Casa URL)
4. Enter a **Long-Lived Access Token** (HA → Profile → Long-Lived Access Tokens → Create Token)
5. Add your cameras — give each one a **Name** (shown in HA) and the HA **Camera Entity ID**
6. Click **Save Settings**

### 2. Add the device in Home Assistant

1. Go to **Settings → Devices & Services → Add Integration → ESPHome**
2. Enter your **PC's local IP address** and port **6053**
3. Click Submit — HA discovers the device as "HA Camera Viewer"

HA will now show one **toggle switch per camera** (e.g. `switch.driveway`, `switch.backyard`).

### 3. Use in automations

Turn a camera switch ON to show it, OFF to hide the popup:

```yaml
- action: switch.turn_on
  target:
    entity_id: switch.driveway
```

Switch cameras by turning one on — the previous one turns off automatically.

---

## Firewall

On first run Windows may prompt to allow network access. Click **Allow** so HA can reach the ESPHome API on port 6053.

If HA can't connect, run this in PowerShell (as Administrator):

```powershell
New-NetFirewallRule -DisplayName "HA Camera Viewer ESPHome API" -Direction Inbound -Protocol TCP -LocalPort 6053 -Action Allow -Profile Any
```

---

## How it works

- **Tauri** app running as a tray process (~3 MB installer, no bundled browser)
- Implements the **ESPHome native API** (TCP port 6053) — HA discovers it as a real ESPHome device with no manual helper setup
- A local HTTP proxy adds the HA Bearer token to camera stream requests (MJPEG)
- Connects to HA via WebSocket for auth
- Settings stored in `%APPDATA%\ha-camera-viewer\config.json`

---

## Building from source

Requires [Rust](https://rustup.rs/) and [Node.js](https://nodejs.org/).

```bash
npm install
npm run build
# Installer appears in: src-tauri/target/release/bundle/nsis/
```

> Windows 10/11 includes WebView2 by default. No extra runtime download needed.

---

## Tech stack

- [Tauri](https://tauri.app/) v2 — app framework (Rust backend + WebView2 frontend)
- [Tokio](https://tokio.rs/) — async Rust runtime
- [Axum](https://github.com/tokio-rs/axum) — local HTTP proxy for camera streams
- [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) — HA WebSocket connection
- ESPHome native API — implemented from scratch in Rust (see `src-tauri/src/esphome.rs`)
