'use strict';
import { $, state, escapeHtml, toast, logLine, openModal, closeModal, api } from './core.js';
import { renderServer } from './keys.js';
import { refreshSshdBadge } from './sshd.js';
import { destroyTerminal, openTerminal, consumePendingTerminal } from './terminal.js';

/* ------------------------------------------------------------------ */
/* 连接 / 刷新 / 断开                                                  */
/* ------------------------------------------------------------------ */

async function doConnect(hostId, extra = {}) {
  if (state.connecting) return false;
  state.connecting = true;
  const btn = $('btnConnect');
  const label = btn.querySelector('.lbl');
  btn.disabled = true;
  label.textContent = '连接中…';
  $('hostSelect').disabled = true;
  document.querySelectorAll('#modeSwitch button').forEach((b) => (b.disabled = true));
  logLine(`正在连接 ${hostId ? (state.hosts.find((h) => h.id === hostId) || {}).name || hostId : '主机'}…`, 'info');

  const withKeys = state.mode !== 'term';
  try {
    const data = await api('/api/connect', { method: 'POST', body: { hostId, ...extra, withKeys } });
    const openTerm = consumePendingTerminal() || !withKeys;
    const previous = state.session;
    destroyTerminal();
    state.session = data;
    state.sshd = null;
    if (previous) api('/api/disconnect', { method: 'POST', body: { sid: previous.sid } }).catch(() => {});
    state.selected.clear();
    renderServer();
    logLine(
      withKeys
        ? `已连接 ${data.host.username}@${data.host.host}（${data.server.uname || '未知系统'}）· ${data.remotePath} · 共 ${data.stats.total} 把钥匙`
        : `已连接 ${data.host.username}@${data.host.host}（${data.server.uname || '未知系统'}）· 纯终端模式，未读取密钥`,
      'ok'
    );
    toast(`已连接 ${data.host.host}`, 'ok');
    if (withKeys) refreshSshdBadge();

    if (openTerm) openTerminal();
    return true;
  } catch (e) {
    if (e.code === 'HOSTKEY_UNKNOWN' || e.code === 'HOSTKEY_CHANGED') {
      logLine(`主机指纹待确认：${e.details ? `${e.details.host}:${e.details.port}` : e.message}`, 'warn');
      openHostKeyModal(e, hostId, extra);
      return false;
    }
    logLine(`连接失败：${e.message}`, 'err');
    openCredModal(e.message, hostId);
    return false;
  } finally {
    state.connecting = false;
    btn.disabled = false;
    label.textContent = '连接';
    $('hostSelect').disabled = false;
    document.querySelectorAll('#modeSwitch button').forEach((b) => (b.disabled = false));
  }
}

/** 首次连接 / 指纹变化时的确认弹窗；确认后带上 trustFingerprint 重连 */
function openHostKeyModal(err, hostId, extra) {
  const d = err.details || {};
  const changed = err.code === 'HOSTKEY_CHANGED';
  const bodyHtml = `
    <div class="warn-box">${escapeHtml(err.message)}</div>
    <div class="field">
      <label>主机</label>
      <div class="hostfp">${escapeHtml(`${d.host || ''}:${d.port || 22}`)}</div>
    </div>
    <div class="field">
      <label>密钥类型</label>
      <div class="hostfp">${escapeHtml(d.keyType || '未知')}</div>
    </div>
    <div class="field">
      <label>服务器返回的指纹</label>
      <div class="hostfp">${escapeHtml(d.fingerprint || '')}</div>
    </div>
    ${
      changed
        ? `<div class="field">
      <label>本地原先记录的指纹</label>
      <div class="hostfp">${escapeHtml(d.previousFingerprint || '')}</div>
    </div>`
        : ''
    }
    <div class="hint">${
      changed
        ? '如果你没有重装或更换过服务器，此时连接可能被中间人劫持，请取消并检查网络。'
        : '请通过云厂商控制台或服务器本地 <code>ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub</code> 核对，指纹一致再连接。'
    }</div>`;

  openModal({
    title: changed ? '主机指纹已变化' : '确认主机指纹',
    bodyHtml,
    footHtml: `<button data-close>取消</button><button class="primary" id="hk_trust">${
      changed ? '我已确认，仍然连接' : '指纹一致，信任并连接'
    }</button>`,
    onMount(m) {
      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));
      m.querySelector('#hk_trust').addEventListener('click', () => {
        closeModal(m);
        doConnect(hostId, { ...extra, trustFingerprint: d.fingerprint });
      });
    },
  });
}

function openCredModal(errMsg, hostId) {
  const host = state.hosts.find((h) => h.id === hostId);
  if (!host) {
    toast(errMsg, 'err', 6000);
    return;
  }
  const bodyHtml = `
    <div class="warn-box">${escapeHtml(errMsg)}</div>
    <div class="field">
      <label>私钥 passphrase（私钥没加密就留空）</label>
      <input type="password" id="c_pass" placeholder="私钥密码" autocomplete="off" />
    </div>
    <div class="field">
      <label>登录密码（改用密码认证时填）</label>
      <input type="password" id="c_pwd" placeholder="SSH 登录密码，只在内存中使用" autocomplete="off" />
      <div class="hint">密码不会写入配置文件，进程退出即消失。</div>
    </div>
    <div class="field">
      <label>私钥文件</label>
      <input type="text" id="c_key" value="${escapeHtml(host.identityFile || '')}" placeholder="留空则用密码登录" />
    </div>`;

  openModal({
    title: `连接 ${host.name}`,
    bodyHtml,
    footHtml: `<button data-close>取消</button><button class="primary" id="c_go">连接</button>`,
    onMount(m) {
      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));
      m.querySelector('#c_go').addEventListener('click', async () => {
        const passphrase = m.querySelector('#c_pass').value;
        const password = m.querySelector('#c_pwd').value;
        const identityFile = m.querySelector('#c_key').value.trim();
        closeModal(m);
        // 必须带 host.id 重连：保留主机绑定后，新增密钥时的「切换本机登录密钥」才能写进配置。
        // identityFile 始终提交（空串 = 这次改用密码登录）。
        await doConnect(host.id, {
          identityFile,
          passphrase: passphrase || undefined,
          password: password || undefined,
        });
      });
    },
  });
}

async function refreshSession() {
  if (!state.session) return;
  try {
    state.session = await api('/api/refresh', { method: 'POST', body: { sid: state.session.sid } });
    state.selected.clear();
    renderServer();
    logLine(`已刷新：服务器上现有 ${state.session.stats.total} 把钥匙`, 'ok');
  } catch (e) {
    logLine(`刷新失败：${e.message}`, 'err');
    toast(e.message, 'err');
  }
}

async function disconnect() {
  if (!state.session) return;
  const session = state.session;
  destroyTerminal();
  state.session = null;
  state.sshd = null;
  state.selected.clear();
  renderServer();
  try {
    await api('/api/disconnect', { method: 'POST', body: { sid: session.sid } });
  } catch {}
  logLine('已断开连接', 'info');
}

/** SSH 连接被服务器断开后，把界面拉回「未连接」状态，而不是留着一个坏会话 */
function handleSessionLost() {
  if (!state.session) return;
  logLine('SSH 连接已断开，请重新连接', 'warn');
  destroyTerminal();
  state.session = null;
  state.selected.clear();
  renderServer();

  const empty = $('emptyState');
  empty.classList.remove('hidden');
  const p = empty.querySelector('p');
  const hint = empty.querySelector('.hint');
  if (p) p.textContent = '连接已断开';
  if (hint) hint.textContent = '服务器主动断开了连接（常见于空闲超时）。点右上角「连接」重新连上即可。';
}
export { doConnect, openHostKeyModal, openCredModal, refreshSession, disconnect, handleSessionLost };
