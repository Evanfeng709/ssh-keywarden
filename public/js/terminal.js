'use strict';
import { $, state, toast } from './core.js';
import { doConnect } from './connect.js';
import { alignSplitter } from './keys.js';

/* ------------------------------------------------------------------ */
/* 内置 SSH 终端                                                       */
/* ------------------------------------------------------------------ */

let termCtl = null;
let pendingTerminal = false;

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

  const term = new window.Terminal({
    cursorBlink: true,
    cursorStyle: 'bar',
    fontSize: 13,
    lineHeight: 1.2,
    fontFamily: '"Cascadia Mono", Consolas, "Courier New", monospace',
    scrollback: 8000,
    theme: {
      background: '#131920',
      foreground: '#d7e0ea',
      cursor: '#4c8dff',
      selectionBackground: '#26436b',
      black: '#1a1f26',
      red: '#f85149',
      green: '#3fb950',
      yellow: '#d6a034',
      blue: '#4c8dff',
      magenta: '#bc8cff',
      cyan: '#39c5cf',
      white: '#c9d1d9',
      brightBlack: '#5c6773',
      brightRed: '#ff7b72',
      brightGreen: '#56d364',
      brightYellow: '#e3b341',
      brightBlue: '#79b8ff',
      brightMagenta: '#d2a8ff',
      brightCyan: '#56d4dd',
      brightWhite: '#f0f6fc',
    },
  });
  const fit = new window.FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open($('termBody'));
  fit.fit();

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
