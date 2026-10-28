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
  systemTheme: {
    get: () => ipcRenderer.invoke('kw:system-theme'),
    onChange: (cb) => {
      ipcRenderer.on('kw:system-theme-changed', (_event, dark) => cb(Boolean(dark)));
    },
  },
  theme: {
    /* 同步读取：首帧 <head> 脚本要用它定主题 */
    initial: ipcRenderer.sendSync('kw:theme-get') || '',
    get: () => ipcRenderer.sendSync('kw:theme-get') || '',
    set: (choice) => ipcRenderer.invoke('kw:theme-set', choice),
  },
  ui: {
    initial: ipcRenderer.sendSync('kw:ui-get') || { mode: '', termHeight: 0 },
    get: () => ipcRenderer.sendSync('kw:ui-get') || { mode: '', termHeight: 0 },
    set: (patch) => ipcRenderer.invoke('kw:ui-set', patch),
  },
});
