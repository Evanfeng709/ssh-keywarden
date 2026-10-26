'use strict';
import { $, state, toast } from './core.js';
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

/** 桌面版走主进程剪贴板（不依赖浏览器权限），网页版退回 navigator.clipboard。
 *  读不到返回 null（和「剪贴板是空的」区分开）。 */
async function readClipboardText() {
  const kw = window.keywarden;
  if (kw && typeof kw.readClipboard === 'function') {
    try {
      return (await kw.readClipboard()) || '';
    } catch {
      /* 落到下面的网页通道 */
    }
  }
  try {
    return (await navigator.clipboard.readText()) || '';
  } catch {
    return null;
  }
}

async function writeClipboardText(text) {
  const kw = window.keywarden;
  if (kw && typeof kw.writeClipboard === 'function') {
    try {
      await kw.writeClipboard(text);
      return true;
    } catch {
      /* 落到下面的网页通道 */
    }
  }
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

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

/** 终端默认高度：窗口的 42%，用户拖动过就记住 */
export const TERM_HEIGHT_KEY = 'kw.termHeight';

function initTermHeight() {
  const panel = $('termPanel');
  if (!panel) return;

  const saved = Number(localStorage.getItem(TERM_HEIGHT_KEY));
  if (saved && saved >= 160) {
    panel.style.height = `${saved}px`;
    return;
  }

  // 没拖过：占内容区（服务器密钥面板）的一半
  const host = panel.parentElement;
  const base = host && host.clientHeight > 200 ? host.clientHeight : Math.round(window.innerHeight * 0.6);
  panel.style.height = `${Math.round(base * 0.5)}px`;
}

function saveTermHeight(px) {
  try {
    localStorage.setItem(TERM_HEIGHT_KEY, String(Math.round(px)));
  } catch {}
}

function termVisible() {
  return !$('termPanel').classList.contains('hidden');
}

function setTermVisible(v) {
  $('termPanel').classList.toggle('hidden', !v);
  $('termSplitter').classList.toggle('hidden', !v);
  $('btnTerm').classList.toggle('active', v);
  if (v) {
    // 展开瞬间列宽才稳定，下一帧再对齐
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

  const onWinResize = () => {
    fit.fit();
    sendResize();
  };
  window.addEventListener('resize', onWinResize);

  termCtl = { term, fit, ws, onWinResize, sendResize };

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
  if (termCtl) {
    window.removeEventListener('resize', termCtl.onWinResize);
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
  termVisible,
  setTermVisible,
};
