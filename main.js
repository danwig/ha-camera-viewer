const { app, BrowserWindow, Tray, Menu, ipcMain, screen, nativeImage } = require('electron');
const path = require('path');
const fs = require('fs');
const http = require('http');
const https = require('https');
const WebSocket = require('ws');
const { ESPHomeServer } = require('./esphome-api');

// ---- Local camera proxy server ----

let proxyServer = null;
let proxyPort = 0;

function startProxy() {
  proxyServer = http.createServer((req, res) => {
    const haBase = (config.haUrl || '').replace(/\/$/, '');
    const targetUrl = haBase + req.url;
    const parsed = new URL(targetUrl);
    const mod = parsed.protocol === 'https:' ? https : http;
    const proxyReq = mod.request(targetUrl, {
      method: req.method,
      headers: { 'Authorization': `Bearer ${config.haToken}` }
    }, (proxyRes) => {
      res.writeHead(proxyRes.statusCode, proxyRes.headers);
      proxyRes.pipe(res);
    });
    proxyReq.on('error', () => { if (!res.headersSent) res.writeHead(502).end(); });
    proxyReq.end();
  });
  proxyServer.listen(0, '127.0.0.1', () => {
    proxyPort = proxyServer.address().port;
    console.log(`Camera proxy running on port ${proxyPort}`);
  });
}

// ---- Config ----

const CONFIG_PATH = path.join(app.getPath('userData'), 'config.json');
const DEFAULT_CONFIG = {
  haUrl: 'http://homeassistant.local:8123',
  haToken: '',
  cameras: [],           // [{ name, entityId }]
  espPort: 6053,
  width: 640,
  height: 480,
  timeout: 30,
  manualTimeout: 0,
  alwaysOnTop: true,
  showTimerBar: true
};

let config = {};

function loadConfig() {
  try {
    if (fs.existsSync(CONFIG_PATH)) {
      config = { ...DEFAULT_CONFIG, ...JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8')) };
    } else {
      config = { ...DEFAULT_CONFIG };
    }
  } catch {
    config = { ...DEFAULT_CONFIG };
  }
  if (!Array.isArray(config.cameras)) config.cameras = [];
}

function saveConfig(updates) {
  config = { ...config, ...updates };
  fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2));
}

function haBase() {
  return (config.haUrl || '').replace(/\/$/, '');
}

// ---- Window management ----

let popupWindow = null;
let settingsWindow = null;
let closeTimer = null;

function getPopupPosition() {
  const { workArea } = screen.getPrimaryDisplay();
  return {
    x: workArea.x + workArea.width - config.width - 16,
    y: workArea.y + workArea.height - config.height - 16
  };
}

function createPopupWindow() {
  if (popupWindow && !popupWindow.isDestroyed()) return popupWindow;
  const { x, y } = getPopupPosition();
  popupWindow = new BrowserWindow({
    width: config.width,
    height: config.height,
    x, y,
    frame: false,
    alwaysOnTop: config.alwaysOnTop,
    skipTaskbar: true,
    resizable: false,
    show: false,
    backgroundColor: '#111318',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      webSecurity: false
    }
  });
  popupWindow.loadFile('renderer/popup.html');
  popupWindow.on('closed', () => { popupWindow = null; clearTimeout(closeTimer); });
  return popupWindow;
}

function showCamera(cameraEntityId, timeout) {
  if (!cameraEntityId) return;
  const win = createPopupWindow();
  const { x, y } = getPopupPosition();
  win.setPosition(x, y);
  win.setSize(config.width, config.height);

  const actualTimeout = (timeout === undefined) ? config.timeout : timeout;
  const cameraUrl = `http://127.0.0.1:${proxyPort}/api/camera_proxy_stream/${cameraEntityId}`;

  const send = () => {
    win.webContents.send('show-camera', {
      cameraUrl, cameraEntityId, timeout: actualTimeout, showBar: config.showTimerBar
    });
  };

  win.show();
  win.focus();
  if (win.webContents.isLoading()) {
    win.webContents.once('did-finish-load', send);
  } else {
    send();
  }

  clearTimeout(closeTimer);
  if (actualTimeout > 0) {
    closeTimer = setTimeout(() => hidePopup(true), actualTimeout * 1000);
  }

  if (espServer) {
    const idx = (config.cameras || []).findIndex(c => c.entityId === cameraEntityId);
    espServer.setActiveCamera(idx);
  }
}

function hidePopup(fromTimer = false) {
  clearTimeout(closeTimer);
  if (popupWindow && !popupWindow.isDestroyed()) popupWindow.hide();
  if (espServer) espServer.setActiveCamera(-1);
}

function openSettings() {
  if (settingsWindow && !settingsWindow.isDestroyed()) { settingsWindow.focus(); return; }
  settingsWindow = new BrowserWindow({
    width: 560,
    height: 720,
    title: 'HA Camera Viewer — Settings',
    resizable: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false
    }
  });
  settingsWindow.loadFile('renderer/settings.html');
  settingsWindow.on('closed', () => { settingsWindow = null; });
}

// ---- ESPHome device simulation ----

let espServer = null;
function startESPHome() {
  if (espServer) { espServer.stop(); espServer = null; }

  espServer = new ESPHomeServer({
    port: config.espPort || 6053,
    deviceName: 'HA Camera Viewer',
    getCameras: () => config.cameras || [],

    onShowCamera: (cam) => {
      showCamera(cam.entityId, config.timeout);
    },

    onHideCamera: () => {
      hidePopup(false);
    }
  });

  espServer.start();
}

// ---- HA WebSocket (for camera proxy auth + optional state triggers) ----

let haWs = null;
let wsMsgId = 1;
// eslint-disable-next-line no-unused-vars

function sendHaMsg(msg) {
  if (haWs && haWs.readyState === WebSocket.OPEN) {
    haWs.send(JSON.stringify({ id: wsMsgId++, ...msg }));
  }
}

function connectHA() {
  if (!config.haToken || !config.haUrl) return;
  if (haWs) { try { haWs.terminate(); } catch {} }

  const wsUrl = haBase().replace(/^http/, 'ws') + '/api/websocket';
  try { haWs = new WebSocket(wsUrl); } catch (e) {
    console.error('WS connect failed:', e.message);
    setTimeout(connectHA, 15000);
    return;
  }

  haWs.on('open', () => console.log('HA WebSocket connected'));
  haWs.on('message', (raw) => {
    try {
      const msg = JSON.parse(raw);
      if (msg.type === 'auth_required') {
        haWs.send(JSON.stringify({ type: 'auth', access_token: config.haToken }));
      } else if (msg.type === 'auth_ok') {
        console.log('HA auth OK');
      }
    } catch {}
  });
  haWs.on('close', () => { setTimeout(connectHA, 15000); });
  haWs.on('error', (e) => console.error('HA WS error:', e.message));
}

// ---- Tray ----

let tray = null;

function buildTrayMenu() {
  const cameras = config.cameras || [];
  const cameraItems = cameras.length > 0
    ? cameras.map(cam => ({
        label: cam.name || cam.entityId,
        click: () => showCamera(cam.entityId, config.manualTimeout)
      }))
    : [{ label: 'No cameras configured', enabled: false }];

  const template = [
    ...cameraItems,
    { type: 'separator' },
    { label: 'Settings', click: openSettings },
    { type: 'separator' },
    { label: 'Quit', click: () => app.exit(0) }
  ];

  tray.setContextMenu(Menu.buildFromTemplate(template));
}

function createTray() {
  const iconPath = path.join(__dirname, 'assets', 'icon.png');
  const icon = fs.existsSync(iconPath)
    ? nativeImage.createFromPath(iconPath).resize({ width: 16, height: 16 })
    : nativeImage.createEmpty();

  tray = new Tray(icon);
  tray.setToolTip('HA Camera Viewer');

  tray.on('click', () => {
    if (popupWindow && !popupWindow.isDestroyed() && popupWindow.isVisible()) {
      hidePopup(false);
    } else {
      const cam = config.cameras[0];
      if (cam) showCamera(cam.entityId, config.manualTimeout);
      else openSettings();
    }
  });

  buildTrayMenu();
}

// ---- IPC ----

ipcMain.handle('get-config', () => config);
ipcMain.handle('save-config', (_, updates) => {
  saveConfig(updates);
  connectHA();
  startESPHome();
  buildTrayMenu();
  return { ok: true };
});
ipcMain.on('close-popup', () => hidePopup(false));
ipcMain.on('extend-timer', (_, seconds) => {
  clearTimeout(closeTimer);
  if (seconds > 0) closeTimer = setTimeout(() => hidePopup(true), seconds * 1000);
  if (popupWindow && !popupWindow.isDestroyed()) {
    popupWindow.webContents.send('timer-extended', seconds);
  }
});

// ---- App startup ----

app.whenReady().then(() => {
  app.setAppUserModelId('com.ha.cameraviewer');
  loadConfig();
  startProxy();
  createTray();
  connectHA();
  startESPHome();
});

app.on('window-all-closed', (e) => e.preventDefault());
app.on('before-quit', () => {
  if (haWs) try { haWs.terminate(); } catch {}
  if (proxyServer) try { proxyServer.close(); } catch {}
  if (espServer) try { espServer.stop(); } catch {}
});
