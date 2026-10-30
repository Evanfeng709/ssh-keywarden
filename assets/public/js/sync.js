'use strict';

/**
 * 微软账号同步面板：配置客户端 ID → 设备码登录 → 立即同步。
 *
 * 设备码只在界面上出现一次，`device_code` 留在本地服务端轮询，
 * 不写进 DOM、日志或剪贴板；同步口令同理，只回传「是否记住」。
 */

import { $, api, toast, escapeHtml, openModal, closeModal, confirmModal, writeClipboardText } from './core.js';

let status = null;
let onSynced = () => {};
let modal = null;
let prompt = null; // 设备码视图 { userCode, verificationUri, at }
let waiting = false; // 登录轮询中
let busy = false; // 同步请求中
let conflicts = null;
let lastError = '';

export function initSync(opts = {}) {
  onSynced = opts.onSynced || (() => {});
  const btn = $('btnSync');
  if (!btn) return;
  btn.addEventListener('click', openSync);
  refreshStatus().catch(() => {});
}

async function refreshStatus() {
  status = await api('/api/sync/status');
  paintDot();
  return status;
}

function paintDot() {
  const btn = $('btnSync');
  if (!btn) return;
  const dot = btn.querySelector('.sync-dot');
  const s = status || {};
  if (dot) {
    dot.className = `sync-dot ${!s.configured ? 'off' : s.loggedIn ? 'on' : 'warn'}`;
  }
  btn.title = !s.configured
    ? '微软账号同步：还没配置 Azure 客户端 ID'
    : s.loggedIn
      ? `微软账号同步：${s.accountEmail || s.accountName || '已登录'}`
      : '微软账号同步：已配置，未登录';
}

async function openSync() {
  try {
    await refreshStatus();
  } catch (e) {
    toast(e.message, 'err', 6000);
    return;
  }
  lastError = '';
  conflicts = null;
  prompt = null;
  waiting = false;
  busy = false;
  modal = openModal({
    title: '微软账号同步',
    width: '560px',
    bodyHtml: '',
    footHtml: '',
    onClose() {
      modal = null;
    },
  });
  render();
}

/* ------------------------------------------------------------------ */
/* 视图                                                                */
/* ------------------------------------------------------------------ */

function bodyHtml() {
  const s = status || {};
  const err = lastError ? `<div class="sync-error">${escapeHtml(lastError)}</div>` : '';
  if (waiting && prompt) return codeView() + err;
  if (s.loggedIn) return loggedInView(s) + conflictsView() + err;
  return configView(s) + notLoggedInView(s) + err;
}

function footHtml() {
  const s = status || {};
  if (waiting) return `<button type="button" data-sync-close>收起</button>`;
  if (s.loggedIn) {
    return (
      `<button type="button" data-sync-logout>退出登录</button>` +
      `<button type="button" data-sync-force${busy ? ' disabled' : ''} title="跳过拉取，直接用本机数据覆盖云端。云端包解不开（换过口令 / 包损坏）时用这个换回同步">覆盖云端</button>` +
      `<button type="button" class="primary" data-sync-now${busy ? ' disabled' : ''}>${busy ? '同步中…' : '立即同步'}</button>`
    );
  }
  return (
    `<button type="button" data-sync-close>关闭</button>` +
    `<button type="button" class="primary" data-sync-login${!s.configured || busy ? ' disabled' : ''}>登录微软账号</button>`
  );
}

function configView(s) {
  return `
    <div class="form-section">
      <div class="form-section-head">
        <h3>微软账号 · OneDrive</h3>
        <span>只同步主机、备注与主机指纹，不上传私钥</span>
      </div>
      <div class="field">
        <label>Azure 应用程序(客户端) ID</label>
        <div class="row">
          <input type="text" id="syncClientId" spellcheck="false" autocomplete="off"
                 placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
                 value="${escapeHtml(s.clientId || '')}" />
          <button type="button" id="syncSaveClient" style="flex:0 0 auto">保存</button>
        </div>
        <div class="hint">这是公开信息、不是密钥；同一个应用可以在多台设备上共用。</div>
      </div>
      ${s.configured ? '' : helpView()}
    </div>`;
}

function helpView() {
  return `
    <details class="sync-help">
      <summary>还没有 Azure 应用？点开看注册步骤</summary>
      <ol>
        <li>打开 portal.azure.com → Microsoft Entra ID → 应用注册 → 新注册；</li>
        <li>受支持的账户类型选「仅个人 Microsoft 账户」（或「任何组织目录…和个人 Microsoft 帐户」）；</li>
        <li>「Authentication (Preview)」→ 高级设置 → 允许公共客户端流 = 是；</li>
        <li>「清单」里把 <code>api.requestedAccessTokenVersion</code> 设为 <code>2</code>，再改账户类型；</li>
        <li>「API 权限」加 Microsoft Graph 委托权限：<code>Files.ReadWrite.AppFolder</code>、
            <code>User.Read</code>、<code>offline_access</code>；</li>
        <li>把「应用程序(客户端) ID」粘到上面并保存。</li>
      </ol>
    </details>`;
}

function notLoggedInView(s) {
  return `
    <div class="field">
      <label>登录</label>
      <div class="hint">点下面的「登录微软账号」，浏览器里输一次 8 位码即可；不需要密码，也不开端口。</div>
    </div>`;
}

function loggedInView(s) {
  const when = s.lastSyncAt ? `上次同步：${fmtTime(s.lastSyncAt)}` : '还没有同步过';
  const idBlock = s.clientId
    ? `<div class="sync-meta">客户端 ID <code>${escapeHtml(s.clientId)}</code></div>`
    : '';
  return `
    <div class="form-section">
      <div class="form-section-head">
        <h3>${escapeHtml(s.accountName || 'Microsoft 账户')}</h3>
        <span>${escapeHtml(s.accountEmail || '')}</span>
      </div>
      ${idBlock}
      <div class="field">
        <label>同步口令</label>
        <input type="password" id="syncPass" autocomplete="off" spellcheck="false"
               placeholder="${s.hasSyncKey ? '已记住口令，留空沿用' : '给同步包加密的口令，手机端要填同一个'}" />
        <div class="hint">${when}。口令只在本机加密用，不会发给微软；忘了就只能删掉云端的 ssh-keywarden.kwsync 重来。</div>
      </div>
      <label class="sync-check">
        <input type="checkbox" id="syncRemember" ${s.hasSyncKey ? 'checked' : ''} />
        记住口令（存在本机 ms-config.json）
      </label>
    </div>`;
}

function conflictsView() {
  if (!conflicts || !conflicts.length) return '';
  const rows = conflicts
    .map(
      (c) => `<li><code>${escapeHtml(c.key)}</code>
        <div class="hint">本机 ${escapeHtml(c.localFingerprint)}<br />云端 ${escapeHtml(c.remoteFingerprint)}</div></li>`,
    )
    .join('');
  return `
    <div class="sync-conflict">
      <div>以下主机的指纹云端与本机不一致，本次<b>没有上传</b>：</div>
      <ul>${rows}</ul>
      <div class="row">
        <button type="button" data-sync-keep>保留本机</button>
        <button type="button" class="primary" data-sync-trust${busy ? ' disabled' : ''}>全部覆盖后重传</button>
      </div>
    </div>`;
}

function codeView() {
  return `
    <div class="form-section">
      <div class="form-section-head">
        <h3>在浏览器里完成登录</h3>
        <span>${escapeHtml(prompt.verificationUri || 'microsoft.com/devicelogin')}</span>
      </div>
      <div class="hint">打开登录页，输入下面这串代码，选个人微软账号并同意。</div>
      <div class="sync-code">${escapeHtml(prompt.userCode)}</div>
      <div class="row">
        <button type="button" data-sync-copy>复制代码</button>
        <button type="button" data-sync-open>打开登录页</button>
      </div>
      <div class="hint sync-wait">等待你在浏览器里完成授权…（最长 5 分钟，完成前请不要关闭这个窗口）</div>
    </div>`;
}

/* ------------------------------------------------------------------ */
/* 渲染与交互                                                          */
/* ------------------------------------------------------------------ */

function render() {
  if (!modal || !modal.isConnected) return;
  modal.querySelector('.modal-body').innerHTML = bodyHtml();
  modal.querySelector('.modal-foot').innerHTML = footHtml();

  const on = (sel, fn) => {
    const el = modal.querySelector(sel);
    if (el) el.addEventListener('click', fn);
  };

  on('[data-sync-close]', () => closeModal(modal));
  on('#syncSaveClient', saveClient);
  on('[data-sync-login]', startLogin);
  on('[data-sync-now]', () => doSync([]));
  on('[data-sync-logout]', logout);
  on('[data-sync-force]', forceUpload);
  on('[data-sync-copy]', copyCode);
  on('[data-sync-open]', openLoginPage);
  on('[data-sync-keep]', () => {
    conflicts = null;
    toast('已保留本机指纹，本次不上传', 'info');
    render();
  });
  on('[data-sync-trust]', () => doSync((conflicts || []).map((c) => c.key)));

  const input = modal.querySelector('#syncClientId');
  if (input) {
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') saveClient();
    });
  }
}

async function saveClient() {
  const input = modal && modal.querySelector('#syncClientId');
  if (!input) return;
  const value = input.value.trim();
  try {
    status = await api('/api/sync/config', { method: 'POST', body: { clientId: value } });
    lastError = '';
    paintDot();
    toast(value ? '客户端 ID 已保存' : '客户端 ID 已清除', 'ok');
  } catch (e) {
    lastError = e.message;
  }
  render();
}

async function startLogin() {
  if (waiting) return;
  lastError = '';
  prompt = null;
  waiting = true;
  render();
  try {
    const p = await api('/api/sync/login/start', { method: 'POST', body: {} });
    prompt = p;
    render();
  } catch (e) {
    waiting = false;
    lastError = e.message;
    render();
    return;
  }

  try {
    await api('/api/sync/login/complete', { method: 'POST', body: { timeoutSec: 300 } });
    prompt = null;
    await refreshStatus();
    toast('已登录微软账号', 'ok');
  } catch (e) {
    lastError = e.message;
  } finally {
    waiting = false;
    render();
  }
}

async function copyCode() {
  if (!prompt) return;
  const ok = await writeClipboardText(prompt.userCode);
  toast(ok ? '代码已复制' : '复制失败，请手动选中', ok ? 'ok' : 'warn');
}

function openLoginPage() {
  const url = (prompt && prompt.verificationUri) || 'https://microsoft.com/devicelogin';
  window.open(url, '_blank');
}

/**
 * 用本机覆盖云端。
 *
 * 存在的理由：云端包一旦解不开（换过同步口令、或上传时被截断），普通同步会在
 * 拉取那一步直接失败，用户就再也没有退路 —— 既不能同步，也不能清掉那份坏包。
 * 这个按钮就是那条退路，属于破坏性操作，必须二次确认。
 */
async function forceUpload() {
  const ok = await confirmModal({
    title: '用本机覆盖云端？',
    message:
      '会跳过拉取，直接用这台设备上的主机、备注和私钥重写 OneDrive 里的同步包。\n\n' +
      '云端原来那份会被替换且无法恢复。只在「确认本机数据才是准的」时候用 —— ' +
      '例如换过同步口令、导致旧包已经解不开。',
    danger: true,
    okText: '覆盖云端',
  });
  if (!ok) return;
  doSync([], true);
}

async function doSync(trustChanged, force) {
  if (busy) return;
  const passEl = modal && modal.querySelector('#syncPass');
  const rememberEl = modal && modal.querySelector('#syncRemember');
  const passphrase = passEl ? passEl.value : '';
  const rememberKey = rememberEl ? rememberEl.checked : false;

  busy = true;
  lastError = '';
  conflicts = null;
  render();
  try {
    const r = await api('/api/sync/now', {
      method: 'POST',
      body: {
        passphrase: passphrase || undefined,
        rememberKey,
        trustChanged,
        forceUpload: force || undefined,
      },
    });
    await refreshStatus();
    const left = r.report ? r.report.knownHostConflicts : [];
    if (left && left.length) {
      conflicts = left;
    } else {
      conflicts = null;
      const p = r.report || {};
      toast(
        `同步完成：主机 +${p.addedHosts || 0} ~${p.updatedHosts || 0} -${p.deletedHosts || 0}，备注 ${p.syncedNotes || 0} 条`,
        'ok',
        5000,
      );
    }
    try {
      await onSynced();
    } catch {}
  } catch (e) {
    lastError = e.message;
    toast(e.message, 'err', 8000);
  } finally {
    busy = false;
    render();
  }
}

async function logout() {
  const ok = await confirmModal({
    title: '退出微软账号',
    message: '只删除本机的登录令牌，云端的同步包保留；下次登录后可以继续同步。',
    danger: true,
    okText: '退出登录',
  });
  if (!ok) return;
  try {
    status = await api('/api/sync/logout', { method: 'POST', body: {} });
    paintDot();
    lastError = '';
    toast('已退出登录', 'ok');
  } catch (e) {
    lastError = e.message;
  }
  render();
}

function fmtTime(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
