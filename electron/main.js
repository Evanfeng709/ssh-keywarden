'use strict';
/**
 * Electron 主进程：内置启动本地服务，只给这个窗口发会话 Cookie。
 * 渲染进程无 Node、无导航，所有本机文件操作走 IPC。
 */

const { app, BrowserWindow, Menu, dialog, ipcMain, session, shell, clipboard, nativeTheme } = require('electron');
const fs = require('fs');
const path = require('path');
const { ensureDataDir, migrateData } = require('./data-dir');

const APP_ID = 'io.github.evanfeng709.ssh-keywarden';
app.setAppUserModelId(APP_ID);

let mainWindow = null;
let serverInfo = null;
let appDataDir = '';
let baseUrl = '';
let quitting = false;

function resolveAppDataDir() {
  if (process.env.KW_DATA_DIR) {
    return { dir: path.resolve(process.env.KW_DATA_DIR), migrateFrom: [] };
  }
  if (!app.isPackaged) {
    // 开发态（npm run app）沿用仓库里的 data，和 npm start 共用一份配置
    return { dir: path.join(__dirname, '..', 'data'), migrateFrom: [] };
  }
  return {
    dir: path.join(app.getPath('userData'), 'data'),
    migrateFrom: [path.join(path.dirname(app.getPath('exe')), 'data')],
  };
}

/**
 * 主题记在数据目录里，不依赖 localStorage：桌面版每次启动都是随机端口，
 * origin 一变 localStorage 就是空的，用户的主题选择会跟着丢。
 */
const THEME_CHOICES = new Set(['obsidian', 'midnight', 'graphite', 'cloud', 'linen', 'mist', 'auto', 'auto-soft']);
let themeChoice = '';

function loadThemeChoice() {
  try {
    const raw = JSON.parse(fs.readFileSync(path.join(appDataDir, 'theme.json'), 'utf8'));
    if (raw && THEME_CHOICES.has(raw.choice)) return raw.choice;
  } catch {
    /* 没选过或文件损坏就用默认 */
  }
  return '';
}

function saveThemeChoice(choice) {
  try {
    fs.writeFileSync(path.join(appDataDir, 'theme.json'), JSON.stringify({ choice }), 'utf8');
  } catch {
    /* 存不下也只是这一轮不记住，不打断使用 */
  }
}

function loadWindowState() {
  try {
    const raw = JSON.parse(fs.readFileSync(path.join(appDataDir, 'window-state.json'), 'utf8'));
    if (raw && Number(raw.width) >= 900 && Number(raw.height) >= 600) return raw;
  } catch {
    /* 没有或损坏就用默认尺寸 */
  }
  return { width: 1280, height: 820 };
}

function saveWindowState() {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  try {
    const bounds = mainWindow.getNormalBounds();
    fs.writeFileSync(path.join(appDataDir, 'window-state.json'), JSON.stringify(bounds), 'utf8');
  } catch {
    /* 窗口状态存不下不影响使用 */
  }
}

function registerIpc() {
  ipcMain.handle('kw:pick-file', async (_event, opts = {}) => {
    const result = await dialog.showOpenDialog(mainWindow, {
      title: opts.title || '选择私钥文件',
      defaultPath: opts.start || undefined,
      properties: ['openFile'],
    });
    return result.canceled || !result.filePaths[0] ? '' : result.filePaths[0];
  });

  ipcMain.handle('kw:pick-dir', async (_event, opts = {}) => {
    const result = await dialog.showOpenDialog(mainWindow, {
      title: opts.title || '选择目录',
      defaultPath: opts.start || undefined,
      properties: ['openDirectory', 'createDirectory'],
    });
    return result.canceled || !result.filePaths[0] ? '' : result.filePaths[0];
  });

  ipcMain.handle('kw:open-data-dir', async () => {
    ensureDataDir(appDataDir);
    const err = await shell.openPath(appDataDir);
    return err || '';
  });

  // 无边框窗口：顶栏里的最小化 / 最大化 / 关闭
  ipcMain.on('kw:win-minimize', () => {
    if (mainWindow && !mainWindow.isDestroyed()) mainWindow.minimize();
  });
  ipcMain.on('kw:win-toggle-maximize', () => {
    if (!mainWindow || mainWindow.isDestroyed()) return;
    if (mainWindow.isMaximized()) mainWindow.unmaximize();
    else mainWindow.maximize();
  });
  ipcMain.on('kw:win-close', () => {
    if (mainWindow && !mainWindow.isDestroyed()) mainWindow.close();
  });
  ipcMain.handle('kw:win-is-maximized', () => Boolean(mainWindow && !mainWindow.isDestroyed() && mainWindow.isMaximized()));

  // 终端粘贴：主进程剪贴板不受网页权限限制
  ipcMain.handle('kw:read-clipboard', () => clipboard.readText());
  ipcMain.handle('kw:write-clipboard', (_event, text) => {
    clipboard.writeText(String(text == null ? '' : text));
  });

  // 「跟随系统」主题：和系统设置里的深浅色同步
  ipcMain.handle('kw:system-theme', () => nativeTheme.shouldUseDarkColors);
  nativeTheme.on('updated', () => {
    if (!mainWindow || mainWindow.isDestroyed()) return;
    mainWindow.webContents.send('kw:system-theme-changed', nativeTheme.shouldUseDarkColors);
  });

  // 首帧脚本要同步拿到主题，避免启动时先闪一帧默认深色
  ipcMain.on('kw:theme-get', (event) => {
    event.returnValue = themeChoice;
  });
  ipcMain.handle('kw:theme-set', (_event, choice) => {
    if (THEME_CHOICES.has(choice)) {
      themeChoice = choice;
      saveThemeChoice(choice);
    }
    return themeChoice;
  });
}

function hardenWebContents(win) {
  const origin = new URL(baseUrl).origin;

  win.webContents.on('will-navigate', (event, target) => {
    try {
      if (new URL(target).origin !== origin) event.preventDefault();
    } catch {
      event.preventDefault();
    }
  });

  win.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:\/\//i.test(url)) shell.openExternal(url);
    return { action: 'deny' };
  });

  win.webContents.session.setPermissionRequestHandler((_wc, permission, callback) => {
    callback(permission === 'clipboard-sanitized-write');
  });
}

function createWindow() {
  const state = loadWindowState();
  const iconPath = path.join(__dirname, '..', 'build', 'icon.png');

  mainWindow = new BrowserWindow({
    ...state,
    minWidth: 1024,
    minHeight: 680,
    frame: false,
    show: false,
    backgroundColor: '#0d1117',
    title: 'SSH Keywarden',
    autoHideMenuBar: true,
    ...(fs.existsSync(iconPath) ? { icon: iconPath } : {}),
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: false,
    },
  });

  hardenWebContents(mainWindow);
  mainWindow.once('ready-to-show', () => mainWindow.show());
  mainWindow.on('close', saveWindowState);
  const sendMaxState = () => {
    if (!mainWindow || mainWindow.isDestroyed()) return;
    mainWindow.webContents.send('kw:win-max-changed', mainWindow.isMaximized());
  };
  mainWindow.on('maximize', sendMaxState);
  mainWindow.on('unmaximize', sendMaxState);
  mainWindow.on('closed', () => {
    mainWindow = null;
  });
  mainWindow.loadURL(baseUrl).catch((e) => {
    dialog.showErrorBox('页面加载失败', e.message);
  });
}

async function boot() {
  const resolved = resolveAppDataDir();
  appDataDir = ensureDataDir(resolved.dir);
  themeChoice = loadThemeChoice();

  if (resolved.migrateFrom.length) {
    const migrated = migrateData(appDataDir, resolved.migrateFrom);
    if (migrated.migrated) console.log(`[desktop] 已从 ${migrated.from} 迁移 ${migrated.copied.join(', ')}`);
  }

  // 必须在 require server 之前设置：store.js 在模块加载时读取 KW_DATA_DIR
  process.env.KW_DATA_DIR = appDataDir;
  process.env.KW_NO_AUTO_COOKIE = '1';
  process.env.KW_SCAN_CACHE_MS = '60000';

  const { startServer } = require('../server.js');

  await app.whenReady();

  try {
    serverInfo = await startServer({ port: 0, autoCookie: false, scanCacheMs: 60_000 });
  } catch (e) {
    dialog.showErrorBox('SSH Keywarden 启动失败', e.message);
    app.quit();
    return;
  }

  baseUrl = `http://127.0.0.1:${serverInfo.port}/`;
  await session.defaultSession.cookies.set({
    url: baseUrl,
    name: 'kw',
    value: serverInfo.token,
    httpOnly: true,
    sameSite: 'strict',
  });

  if (app.isPackaged) Menu.setApplicationMenu(null);
  registerIpc();
  createWindow();

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
}

const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (!mainWindow) return;
    if (mainWindow.isMinimized()) mainWindow.restore();
    mainWindow.focus();
  });

  app.on('window-all-closed', () => {
    if (process.platform !== 'darwin') app.quit();
  });

  app.on('before-quit', (event) => {
    if (quitting || !serverInfo) return;
    event.preventDefault();
    quitting = true;
    serverInfo
      .close()
      .catch(() => {})
      .finally(() => app.quit());
  });

  boot().catch((e) => {
    dialog.showErrorBox('SSH Keywarden 启动失败', e.stack || e.message);
    app.quit();
  });
}
