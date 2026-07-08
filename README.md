# HA Camera Viewer

A Windows system tray app that connects to Home Assistant and displays camera feeds in a popup window, triggered by HA automations.

Built with Electron. No MQTT or manual helpers needed — it simulates an ESPHome device so HA discovers it natively.

---

## Features

- Runs in the Windows system tray
- Pop-up camera viewer in the bottom-right corner of your screen
- Shows any HA camera entity (MJPEG stream)
- Multiple cameras — each appears as its own toggle in HA
- Triggered from HA automations (turn on a switch entity to show a camera)
- Auto-close timer with progress bar, +30s / Keep Open buttons
- Adjustable window size
- Works over local network and remotely via Nabu Casa

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

Switch cameras by turning one on — the previous one turns off and the feed updates automatically.

---

## Firewall

On first run Windows may ask to allow network access. Click **Allow** so HA can reach the ESPHome API on port 6053.

If HA can't connect, run this in PowerShell (as admin):
```powershell
New-NetFirewallRule -DisplayName "HA Camera Viewer ESPHome API" -Direction Inbound -Protocol TCP -LocalPort 6053 -Action Allow -Profile Any
```

---

## How it works

- **Electron** app running as a tray process
- Implements the **ESPHome native API** (TCP port 6053) so HA discovers it as a real ESPHome device with no manual helper setup
- A local HTTP proxy adds the HA Bearer token to camera stream requests (MJPEG)
- Connects to HA via WebSocket for auth and future state sync
- Settings stored in `%APPDATA%\ha-camera-viewer\config.json`

---

## Building from source

```bash
npm install
npm start          # run in dev mode
npm run build      # build NSIS installer (requires running as Administrator on Windows)
```

> **Note:** The build must run as Administrator on Windows to allow electron-builder to create the symlinks it needs for the winCodeSign toolchain.

---

## Tech stack

- [Electron](https://www.electronjs.org/) v29
- [ws](https://github.com/websockets/ws) — HA WebSocket connection
- ESPHome native API — implemented from scratch (see `esphome-api.js`)
- Node.js built-in `net`, `http`, `https` — TCP server + camera proxy
