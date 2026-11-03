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
  setAllSelected,
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
  resetTermHeight,
  applyTermHeight,
  getTermCtl,
} from './terminal.js';
import { initMode } from './mode.js';
import { initSync } from './sync.js';

/* ------------------------------------------------------------------ */
/* 事件绑定 + 启动                                                     */
/* ------------------------------------------------------------------ */

function setLocalPanel(open) {
  $('layout').classList.toggle('local-open', open);
  $('btnLocal').setAttribute('aria-pressed', String(open));
  $('btnLocal').title = open ? '收起本地密钥库' : '显示本地密钥库';
  const navLocal = document.querySelector('#sidebar .nav-item[data-nav="local"]');
  if (navLocal) navLocal.classList.toggle('active', open);
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
  initSync({ onSynced: reloadAll });
  $('btnRescan').addEventListener('click', rescanLocal);
  bindLocalTools();
  bindServerTools();

  $('btnRefresh').addEventListener('click', refreshSession);
  $('btnDisconnect').addEventListener('click', disconnect);
  $('btnAdd').addEventListener('click', openAddModal);
  $('btnBackups').addEventListener('click', openBackupsModal);
  // 登录方式徽章是动态渲染的，且挂在面板标题上（不在表格里），所以用 document 级委托
  document.addEventListener('click', (e) => {
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
      if (e.button !== 0) return;
      dragging = true;
      startY = e.clientY;
      startH = panel.offsetHeight;
      document.body.style.cursor = 'row-resize';
      document.body.style.userSelect = 'none';
      e.preventDefault();
    });

    document.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      applyTermHeight(Math.max(180, startH - (e.clientY - startY)));
      if (getTermCtl()) getTermCtl().fit.fit();
    });

    document.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      saveTermHeight(panel.offsetHeight);
      alignSplitter();
    });

    // 双击恢复自适应：表格按内容收缩，终端吃掉剩余
    splitter.addEventListener('dblclick', () => {
      resetTermHeight();
      requestAnimationFrame(() => {
        alignSplitter();
        const ctl = getTermCtl();
        if (ctl) {
          ctl.fit.fit();
          ctl.sendResize();
        }
      });
    });
  })();
  $('btnDelete').addEventListener('click', () => askDelete([...state.selected]));

  // 批量改复选框、行样式和 state.selected，只重算一次选择 UI
  // （原来逐行 dispatchEvent('change')，N 把钥匙会各跑一次 updateSelectionUi）
  $('checkAll').addEventListener('change', (e) => setAllSelected(e.target.checked));

  $('btnClearLog').addEventListener('click', (e) => {
    e.stopPropagation();
    $('log').innerHTML = '';
    const latest = $('logLatest');
    if (latest) {
      latest.textContent = '日志已清空';
      latest.className = 'log-latest';
    }
  });

  // 底部 Termius 状态栏点击展开/收起详细日志抽屉
  const logToggle = $('logToggle');
  if (logToggle) {
    logToggle.addEventListener('click', (e) => {
      if (e.target.closest('#btnClearLog')) return;
      const bar = $('logbar');
      const open = bar.classList.toggle('log-open');
      const btn = $('btnToggleLog');
      if (btn) {
        btn.setAttribute('aria-expanded', String(open));
        btn.textContent = open ? '日志 ▾' : '日志 ▴';
      }
      requestAnimationFrame(() => {
        alignSplitter();
        const ctl = getTermCtl();
        if (ctl) {
          ctl.fit.fit();
          ctl.sendResize();
        }
      });
    });
  }

  // 左侧 Termius 导航栏
  const sidebar = $('sidebar');
  if (sidebar) {
    sidebar.addEventListener('click', (e) => {
      const btn = e.target.closest('.nav-item[data-nav]');
      if (!btn) return;
      const nav = btn.dataset.nav;
      if (nav === 'hosts' || nav === 'term') {
        // 已由 initMode() 绑定 [data-mode] 点击切换
      } else if (nav === 'local') {
        setLocalPanel(!$('layout').classList.contains('local-open'));
      } else if (nav === 'new-host') {
        openHostModal(null);
      } else if (nav === 'sync') {
        $('btnSync').click();
      } else if (nav === 'settings') {
        openSettingsModal();
      }
    });
  }

  // 主机库卡片矩阵（Vault Grid）直连与编辑
  $('layout').addEventListener('click', (e) => {
    const connBtn = e.target.closest('[data-vault-connect]');
    if (connBtn) {
      const id = connBtn.dataset.id;
      const targetMode = connBtn.dataset.vaultConnect;
      if (id) {
        $('hostSelect').value = id;
        const m = document.querySelector(`[data-mode="${targetMode}"]`);
        if (m && state.mode !== targetMode) m.click();
        doConnect(id);
      }
      return;
    }
    const editBtn = e.target.closest('[data-vault-edit]');
    if (editBtn) {
      openHostModal(editBtn.dataset.vaultEdit);
      return;
    }
    if (e.target.closest('[data-vault-add]')) {
      openHostModal(null);
      return;
    }
    if (e.target.closest('[data-vault-sync]')) {
      $('btnSync').click();
    }
  });

  $('layout').addEventListener('dblclick', (e) => {
    const card = e.target.closest('.vault-card[data-host-id]');
    if (!card || e.target.closest('button')) return;
    const id = card.dataset.hostId;
    if (id) {
      $('hostSelect').value = id;
      doConnect(id);
    }
  });

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

/** 重新拉一遍整页状态：首次启动和同步完成后都走这里 */
async function reloadAll() {
  const s = await api('/api/state');
  // 后端报的是 semver 形态（2026.10.30-1，Cargo 只认三段），展示统一成 v2026.10.30.1
  if (s.version) $('version').textContent = 'v' + String(s.version).replace(/-/g, '.');
  state.hosts = s.hosts;
  state.settings = s.settings;
  state.keyNotes = s.keyNotes || {};
  state.localKeys = s.localKeys;
  renderHosts();
  renderLocal();
  renderServer();
  return s;
}

async function boot() {
  initSystemTheme();
  setSessionLostHandler(handleSessionLost);
  bind();
  initMode();
  initTermHeight();
  window.addEventListener('resize', alignSplitter);

  try {
    const s = await reloadAll();

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
