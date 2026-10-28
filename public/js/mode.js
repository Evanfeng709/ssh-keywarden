'use strict';

import { $, state, api, toast, logLine } from './core.js';
import { renderServer, alignSplitter } from './keys.js';
import { openTerminal, getTermCtl, applyTermHeight } from './terminal.js';
import { refreshSshdBadge } from './sshd.js';

const MODE_KEY = 'kw.mode';
let switching = false;

function readMode() {
  try {
    const ui = window.keywarden && window.keywarden.ui;
    const value = ui ? ui.initial.mode : localStorage.getItem(MODE_KEY);
    return value === 'term' ? 'term' : 'keys';
  } catch {
    return 'keys';
  }
}

function setMode(mode, { persist = true } = {}) {
  state.mode = mode;
  document.body.classList.toggle('mode-term', mode === 'term');
  for (const btn of document.querySelectorAll('#modeSwitch button')) {
    btn.classList.toggle('on', btn.dataset.mode === mode);
    btn.setAttribute('aria-pressed', String(btn.dataset.mode === mode));
  }
  document.querySelector('.server-panel .panel-head h2').textContent = mode === 'term' ? 'SSH 终端' : '服务器密钥';
  if (mode === 'term' && $('layout').classList.contains('local-open')) $('btnLocal').click();
  renderServer();
  if (mode === 'term' && state.session) openTerminal();
  if (mode === 'keys') {
    applyTermHeight();
    if (state.session && !state.sshd) refreshSshdBadge();
  }
  requestAnimationFrame(() => {
    alignSplitter();
    const ctl = getTermCtl();
    if (ctl) {
      ctl.fit.fit();
      ctl.sendResize();
    }
  });

  if (!persist) return;
  const ui = window.keywarden && window.keywarden.ui;
  if (ui) {
    ui.set({ mode }).catch(() => toast('视图模式保存失败', 'warn'));
  } else {
    try {
      localStorage.setItem(MODE_KEY, mode);
    } catch {}
  }
}

async function switchMode(next) {
  if (switching || state.connecting || next === state.mode) return;
  const session = state.session;
  if (next === 'keys' && session && session.keysLoaded === false) {
    switching = true;
    const buttons = document.querySelectorAll('#modeSwitch button');
    buttons.forEach((b) => (b.disabled = true));
    try {
      const data = await api('/api/refresh', { method: 'POST', body: { sid: session.sid } });
      // 补读期间可能已经断开或换了主机，旧响应不能恢复旧会话。
      if (state.session !== session || state.connecting) return;
      state.session = data;
      state.selected.clear();
      logLine(`已读取服务器密钥：${data.stats.total} 把`, 'ok');
    } catch (e) {
      if (state.session === session) toast(`读取密钥失败，保留终端：${e.message}`, 'err', 6000);
      return;
    } finally {
      switching = false;
      buttons.forEach((b) => (b.disabled = state.connecting));
    }
  }
  setMode(next);
}

export function initMode() {
  for (const btn of document.querySelectorAll('#modeSwitch button')) {
    btn.addEventListener('click', () => switchMode(btn.dataset.mode));
  }
  setMode(readMode(), { persist: false });
}
