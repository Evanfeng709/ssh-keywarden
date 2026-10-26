'use strict';
/**
 * 预加载：只向页面暴露桌面能力，不开放 Node。
 */

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('keywarden', {
  isDesktop: true,
  pickFile: (opts) => ipcRenderer.invoke('kw:pick-file', opts || {}),
  pickDir: (opts) => ipcRenderer.invoke('kw:pick-dir', opts || {}),
  openDataDir: () => ipcRenderer.invoke('kw:open-data-dir'),
  windowControls: {
    minimize: () => ipcRenderer.send('kw:win-minimize'),
    toggleMaximize: () => ipcRenderer.send('kw:win-toggle-maximize'),
    close: () => ipcRenderer.send('kw:win-close'),
    isMaximized: () => ipcRenderer.invoke('kw:win-is-maximized'),
    onMaximizeChange: (cb) => {
      ipcRenderer.on('kw:win-max-changed', (_event, max) => cb(Boolean(max)));
    },
  },
  readClipboard: () => ipcRenderer.invoke('kw:read-clipboard'),
  writeClipboard: (text) => ipcRenderer.invoke('kw:write-clipboard', text),
});
