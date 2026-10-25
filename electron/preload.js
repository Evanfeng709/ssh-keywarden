'use strict';
/**
 * 预加载：只向页面暴露三个桌面能力，不开放 Node。
 */

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('keywarden', {
  isDesktop: true,
  pickFile: (opts) => ipcRenderer.invoke('kw:pick-file', opts || {}),
  pickDir: (opts) => ipcRenderer.invoke('kw:pick-dir', opts || {}),
  openDataDir: () => ipcRenderer.invoke('kw:open-data-dir'),
});
