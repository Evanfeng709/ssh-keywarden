'use strict';

import { $, state, toast, logLine, api, setSessionLostHandler } from './core.js';
import { openHostModal, openSettingsModal, renderHosts } from './hosts.js';
import {
  renderLocal,
  renderServer,
  alignSplitter,
  rescanLocal,
  bindLocalTools,
  bindServerTools,
} from './keys.js';
import { doConnect, refreshSession, disconnect, handleSessionLost } from './connect.js';
import { openAddModal, askDelete } from './actions.js';
import { openBackupsModal } from './backups.js';
import { toggleSshdFromBadge } from './sshd.js';
import { initSystemTheme } from './theme.js';
import {
  openTerminal,
  hideTerminal,
  termVisible,
  initTermHeight,
  saveTermHeight,
  getTermCtl,
  TERM_HEIGHT_KEY,
} from './terminal.js';

/* ------------------------------------------------------------------ */
/* 事件绑定 + 启动                                                     */
/* ------------------------------------------------------------------ */

function setLocalPanel(open) {
  $('layout').classList.toggle('local-open', open);
  $('btnLocal').setAttribute('aria-pressed', String(open));
  $('btnLocal').title = open ? '收起本地密钥库' : '显示本地密钥库';
  if (open) renderLocal();
  // 主区域变窄，终端列数要跟着重排
  requestAnimationFrame(() => {
    alignSplitter();
    const ctl = getTermCtl();
    if (ctl) {
      ctl.fit.fit();
      ctl.sendResize();
    }
  });
}

/** 桌面版：无边框窗口靠顶栏里的自绘按钮操作 */
function bindWindowControls() {
  if (!window.keywarden || !window.keywarden.isDesktop) return;
  document.body.classList.add('desktop');

  const wc = window.keywarden.windowControls;
  if (!wc) return;

  $('winMin').addEventListener('click', () => wc.minimize());
  $('winMax').addEventListener('click', () => wc.toggleMaximize());
  $('winClose').addEventListener('click', () => wc.close());

  const apply = (v) => {
    const max = Boolean(v);
    document.body.classList.toggle('maximized', max);
    $('winMax').title = max ? '还原' : '最大化';
    $('winMax').setAttribute('aria-label', max ? '还原' : '最大化');
  };
  wc.isMaximized().then(apply).catch(() => {});
  wc.onMaximizeChange(apply);
}

function bind() {
  bindWindowControls();

  $('btnConnect').addEventListener('click', () => {
    const id = $('hostSelect').value;
    if (!id) return openHostModal();
    doConnect(id);
  });

  $('btnLocal').addEventListener('click', () => {
    setLocalPanel(!$('layout').classList.contains('local-open'));
  });

  $('btnHosts').addEventListener('click', () => {
    const id = $('hostSelect').value;
    openHostModal(state.hosts.length ? id : null);
  });

  $('btnSettings').addEventListener('click', openSettingsModal);
  $('btnRescan').addEventListener('click', rescanLocal);
  bindLocalTools();
  bindServerTools();

  $('btnRefresh').addEventListener('click', refreshSession);
  $('btnDisconnect').addEventListener('click', disconnect);
  $('btnAdd').addEventListener('click', openAddModal);
  $('btnBackups').addEventListener('click', openBackupsModal);
  // 状态列里的登录方式徽章是动态渲染的，用事件委托
  $('keyBody').addEventListener('click', (e) => {
    if (e.target.closest('[data-sshd-toggle]')) toggleSshdFromBadge();
  });
  $('btnTerm').addEventListener('click', () => {
    if (termVisible()) hideTerminal();
    else openTerminal();
  });
  $('termClose').addEventListener('click', hideTerminal);
  $('termClear').addEventListener('click', () => {
    if (getTermCtl()) getTermCtl().term.clear();
  });

  // 拖动分隔条调整终端高度
  (() => {
    const splitter = $('termSplitter');
    const panel = $('termPanel');
    let dragging = false;
    let startY = 0;
    let startH = 0;

    splitter.addEventListener('mousedown', (e) => {
      dragging = true;
      startY = e.clientY;
      startH = panel.offsetHeight;
      document.body.style.cursor = 'row-resize';
      document.body.style.userSelect = 'none';
      e.preventDefault();
    });

    document.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      const next = Math.max(160, Math.min(startH - (e.clientY - startY), window.innerHeight - 240));
      panel.style.height = `${next}px`;
      if (getTermCtl()) getTermCtl().fit.fit();
    });

    document.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      saveTermHeight(panel.offsetHeight);
      if (getTermCtl()) {
        getTermCtl().fit.fit();
        getTermCtl().sendResize();
      }
    });
  })();
  $('btnDelete').addEventListener('click', () => askDelete([...state.selected]));

  $('checkAll').addEventListener('change', (e) => {
    const checkboxes = document.querySelectorAll('#keyBody input[type="checkbox"]');
    checkboxes.forEach((cb) => {
      cb.checked = e.target.checked;
      cb.dispatchEvent(new Event('change'));
    });
  });

  $('btnClearLog').addEventListener('click', () => ($('log').innerHTML = ''));

  // 主机下拉里双击 = 快速编辑
  $('hostSelect').addEventListener('dblclick', () => openHostModal($('hostSelect').value));

  // 切换了另一台服务器却还连着旧的，容易误操作 —— 直接断开，让用户重新连
  $('hostSelect').addEventListener('change', () => {
    if (!state.session) return;
    const sel = state.hosts.find((h) => h.id === $('hostSelect').value);
    if (!sel) return;
    const cur = state.session.host;
    const same =
      sel.host === cur.host && sel.username === cur.username && (sel.port || 22) === (cur.port || 22);
    if (!same) {
      logLine(`已切到「${sel.name}」，与当前连接的服务器不是同一台，自动断开`, 'warn');
      toast('已切换服务器，请重新连接', 'info');
      disconnect();
    }
  });
}

async function boot() {
  initSystemTheme();
  setSessionLostHandler(handleSessionLost);
  bind();
  initTermHeight();

  // 没手动拖过就跟着窗口走（拖过的话尊重用户的选择）
  window.addEventListener('resize', () => {
    if (!localStorage.getItem(TERM_HEIGHT_KEY)) initTermHeight();
    alignSplitter();
  });

  try {
    const s = await api('/api/state');
    if (s.version) $('version').textContent = s.version;
    state.hosts = s.hosts;
    state.settings = s.settings;
    state.keyNotes = s.keyNotes || {};
    state.localKeys = s.localKeys;
    renderHosts();
    renderLocal();
    renderServer();

    if (!s.hosts.length) {
      logLine('还没有配置主机。点右上角「管理主机」添加一台，或用「连接」按钮直接填。', 'info');
    } else {
      logLine(`已加载 ${s.hosts.length} 台主机配置，本地密钥库 ${s.localKeys.entries.length} 个文件`, 'info');
    }
  } catch (e) {
    logLine(`初始化失败：${e.message}`, 'err');
    toast(e.message, 'err', 8000);
  }
}

boot();
