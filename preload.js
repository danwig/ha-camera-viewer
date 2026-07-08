const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('ha', {
  getConfig: () => ipcRenderer.invoke('get-config'),
  saveConfig: (cfg) => ipcRenderer.invoke('save-config', cfg),
  closePopup: () => ipcRenderer.send('close-popup'),
  extendTimer: (seconds) => ipcRenderer.send('extend-timer', seconds),
  onShowCamera: (cb) => ipcRenderer.on('show-camera', (_, data) => cb(data)),
  onTimerExtended: (cb) => ipcRenderer.on('timer-extended', (_, seconds) => cb(seconds)),
});
