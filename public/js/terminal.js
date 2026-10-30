'use strict';
import { $, state, toast, readClipboardText, writeClipboardText } from './core.js';
import { doConnect } from './connect.js';
import { alignSplitter } from './keys.js';
import { termPalette, resolvedTheme, onThemeChange } from './theme.js';

/* ------------------------------------------------------------------ */
/* 内置 SSH 终端                                                       */
/* ------------------------------------------------------------------ */

let termCtl = null;
let pendingTerminal = false;
let clipboardBound = false;
let themeBound = false;

/* readClipboardText / writeClipboardText 已下沉到 core.js —— 桌面版走主进程 IPC、
   网页版退回 navigator.clipboard 的那套逻辑，全站只留一份。 */

async function pasteIntoTerminal(term) {
  const text = await readClipboardText();
  if (text == null) {
    toast('读不到剪贴板内容，试试直接按 Ctrl+V', 'warn', 2400);
    return;
  }
  if (!text) {
    toast('剪贴板是空的', 'warn', 1800);
    return;
  }
  term.paste(text);
  term.focus();
}

async function copySelection(term) {
  const sel = term.getSelection();
  if (!sel) {
    toast('先选中要复制的内容', 'warn', 1800);
    return;
  }
  const ok = await writeClipboardText(sel);
  toast(ok ? '已复制选中内容' : '复制失败', ok ? 'ok' : 'err', 1600);
  term.focus();
}

/** 终端始终在位，粘贴/复制入口只绑一次 */
function bindClipboardUi() {
  if (clipboardBound) return;
  clipboardBound = true;

  // 浏览器原生粘贴（Ctrl+V / 右键粘贴）时，别让 xterm 的 textarea 自己吞掉
  $('termBody').addEventListener(
    'paste',
    (e) => {
      if (!termCtl) return;
      const text = e.clipboardData ? e.clipboardData.getData('text') : '';
      if (!text) return;
      e.preventDefault();
      e.stopPropagation();
      termCtl.term.paste(text);
    },
    true
  );

  $('termCopy').addEventListener('click', () => {
    if (termCtl) copySelection(termCtl.term);
  });
  $('termPaste').addEventListener('click', () => {
    if (termCtl) pasteIntoTerminal(termCtl.term);
  });
}

const TERM_HEIGHT_KEY = 'kw.termHeight';
let savedHeight = 0;

function readSavedHeight() {
  try {
    const ui = window.keywarden && window.keywarden.ui;
    const h = Number(ui ? ui.initial.termHeight : localStorage.getItem(TERM_HEIGHT_KEY));
    return Number.isFinite(h) && h >= 180 ? Math.round(h) : 0;
  } catch {
    return 0;
  }
}

function applyTermHeight(px = savedHeight) {
  const panel = $('termPanel');
  const host = panel.parentElement;
  host.classList.toggle('term-manual', px > 0);
  if (!px) {
    panel.style.height = '';
    return;
  }
  const reserved = [...host.children]
    .filter((el) => el !== panel && !el.classList.contains('table-wrap'))
    .reduce((sum, el) => sum + el.getBoundingClientRect().height, 0);
  // 保留表头和至少一行的位置，缩小窗口时不能把密钥表挤出视口。
  const max = Math.max(180, host.clientHeight - reserved - 90);
  panel.style.height = `${Math.max(180, Math.min(Math.round(px), max))}px`;
}

function initTermHeight() {
  savedHeight = readSavedHeight();
  applyTermHeight(savedHeight);
  const observer = new ResizeObserver(() => {
    if (state.mode !== 'term') applyTermHeight(savedHeight);
  });
  observer.observe($('termPanel').parentElement);
  observer.observe(document.querySelector('.server-panel .panel-head'));
  observer.observe($('keyTools'));
}

function saveTermHeight(px) {
  savedHeight = px > 0 ? Math.max(180, Math.round(px)) : 0;
  applyTermHeight(savedHeight);
  const ui = window.keywarden && window.keywarden.ui;
  if (ui) {
    ui.set({ termHeight: savedHeight }).catch(() => toast('终端高度保存失败', 'warn'));
  } else {
    try {
      localStorage.setItem(TERM_HEIGHT_KEY, String(savedHeight));
    } catch {}
  }
}

function resetTermHeight() {
  saveTermHeight(0);
}

function termVisible() {
  return !$('termPanel').classList.contains('hidden');
}

function setTermVisible(v) {
  $('termPanel').classList.toggle('hidden', !v);
  $('termSplitter').classList.toggle('hidden', !v);
  const host = $('termPanel').parentElement;
  if (host) host.classList.toggle('term-open', v);
  $('btnTerm').classList.toggle('active', v);
  if (v) {
    if (state.mode !== 'term') applyTermHeight();
    requestAnimationFrame(alignSplitter);
  }
}

function bytesToB64(bytes) {
  let bin = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}

function b64ToBytes(b64) {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function setTermStatus(text, kind = '') {
  const dot = $('termDot');
  const label = $('termStatus');

  if (dot) {
    dot.className = `term-dot${kind ? ' ' + kind : ''}`;
    dot.title = text;
  }
  if (label) {
    // 正常状态只用圆点表示，出错/警告才把文字显示出来
    if (kind === 'err' || kind === 'warn') {
      label.textContent = text;
      label.className = `term-status ${kind}`;
    } else {
      label.textContent = '';
      label.className = 'term-status hidden';
    }
  }
}

function openTerminal() {
  // 没连接就先连上，连完再自动打开（doConnect 成功后会回调进来）
  if (!state.session) {
    const hostId = $('hostSelect').value;
    if (!hostId) {
      toast('先添加并选择一台服务器', 'warn');
      return;
    }
    pendingTerminal = true;
    doConnect(hostId);
    return;
  }

  setTermVisible(true);

  if (termCtl) {
    requestAnimationFrame(() => {
      if (!termCtl) return;
      termCtl.fit.fit();
      termCtl.sendResize();
    });
    return;
  }
  if (!window.Terminal || !window.FitAddon) {
    toast('终端组件未加载（public/vendor 下的 xterm 文件缺失）', 'err', 6000);
    return;
  }

  setTermStatus('连接中…', 'warn');
  bindClipboardUi();

  const term = new window.Terminal({
    cursorBlink: true,
    cursorStyle: 'bar',
    fontSize: 13,
    lineHeight: 1.2,
    fontFamily: '"Cascadia Mono", Consolas, "Courier New", monospace',
    scrollback: 8000,
    theme: termPalette(resolvedTheme()),
  });
  const fit = new window.FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open($('termBody'));
  fit.fit();
  term.focus();

  // Ctrl+V / Ctrl+Shift+V / Shift+Insert 粘贴，Ctrl+Shift+C 复制；
  // Ctrl+C 保持原样（有选中内容时 xterm 自己会复制，没有就是 SIGINT）
  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== 'keydown') return true;
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.shiftKey && (e.key === 'C' || e.key === 'c')) {
      copySelection(term);
      return false;
    }
    if (ctrl && (e.key === 'v' || e.key === 'V')) {
      pasteIntoTerminal(term);
      return false;
    }
    if (e.shiftKey && e.key === 'Insert') {
      pasteIntoTerminal(term);
      return false;
    }
    return true;
  });

  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const qs = `sid=${encodeURIComponent(state.session.sid)}&cols=${term.cols}&rows=${term.rows}`;
  const ws = new WebSocket(`${proto}://${location.host}/api/terminal?${qs}`);

  const sendResize = () => {
    if (ws.readyState === 1) ws.send(JSON.stringify({ t: 'resize', cols: term.cols, rows: term.rows }));
  };

  ws.onopen = () => {
    setTermStatus('已连接', 'ok');
    fit.fit();
    sendResize();
  };

  ws.onmessage = (ev) => {
    let msg;
    try {
      msg = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (msg.t === 'out') {
      term.write(b64ToBytes(msg.d));
    } else if (msg.t === 'error') {
      term.write(`\r\n\x1b[31m${msg.message}\x1b[0m\r\n`);
      setTermStatus('出错', 'err');
    } else if (msg.t === 'exit') {
      setTermStatus('会话结束', 'warn');
    }
  };

  ws.onclose = (e) => {
    const lost = e.code === 4001 || e.code === 4002;
    setTermStatus(lost ? '会话已失效' : '已断开', 'err');
    term.write(`\r\n\x1b[90m[连接已关闭，code=${e.code}]\x1b[0m\r\n`);
  };

  ws.onerror = () => setTermStatus('连接错误', 'err');

  term.onData((d) => {
    if (ws.readyState === 1) {
      ws.send(JSON.stringify({ t: 'in', d: bytesToB64(new TextEncoder().encode(d)) }));
    }
  });

  term.onResize(() => sendResize());

  const resizeObserver = new ResizeObserver(() => {
    if (termVisible()) {
      fit.fit();
      sendResize();
    }
  });
  resizeObserver.observe($('termBody'));

  termCtl = { term, fit, ws, resizeObserver, sendResize };

  // 换主题时终端跟着换（xterm 不认 CSS 变量，只能整表替换）
  if (!themeBound) {
    themeBound = true;
    onThemeChange((id) => {
      if (termCtl) termCtl.term.options.theme = termPalette(id);
    });
  }
}

/** 收起：只隐藏，shell 与终端实例保留，再点「终端」立刻回来 */
function hideTerminal() {
  setTermVisible(false);
}

/** 彻底销毁：断开连接、切换服务器时用 */
function destroyTerminal() {
  pendingTerminal = false;
  if (termCtl) {
    termCtl.resizeObserver.disconnect();
    termCtl.ws.onclose = null;
    termCtl.ws.onerror = null;
    try {
      termCtl.ws.close();
    } catch {}
    try {
      termCtl.term.dispose();
    } catch {}
    termCtl = null;
  }
  $('termBody').innerHTML = '';
  setTermVisible(false);
  setTermStatus('未连接');
}

export function getTermCtl() {
  return termCtl;
}

/** 连上后是否要自动拉起终端（取一次就清空） */
export function consumePendingTerminal() {
  const pending = pendingTerminal;
  pendingTerminal = false;
  return pending;
}

export {
  openTerminal,
  hideTerminal,
  destroyTerminal,
  initTermHeight,
  saveTermHeight,
  resetTermHeight,
  applyTermHeight,
  termVisible,
  setTermVisible,
};
