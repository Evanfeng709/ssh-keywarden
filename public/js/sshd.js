'use strict';
import { state, escapeHtml, toast, logLine, api } from './core.js';
import { renderServer } from './keys.js';

/* ------------------------------------------------------------------ */
/* 服务器登录方式（是否仅密钥登录）                                     */
/* ------------------------------------------------------------------ */

async function mountSshdBar(m, barId) {
  const bar = m.querySelector(`#${barId}`);
  if (!bar || !state.session) return;

  const renderSshd = (st) => {
    if (!st || !st.available) {
      bar.innerHTML = `<span class="sshd-err">${escapeHtml((st && st.reason) || '无法读取 sshd 配置')}</span>`;
      return;
    }
    const label = SSHD_STATE_LABEL[st.state] || { text: '未知', cls: '' };
    const dotCls = st.state === 'key-only' ? 'ok' : st.state === 'both' ? 'warn' : 'err';
    const badgeCls = st.state === 'key-only' ? 'ok' : st.state === 'both' ? 'warn' : 'bad';

    bar.innerHTML = `
      <div class="sshd-text">
        <span class="dot ${dotCls}"></span>
        <span>密码登录：<b>${st.pwAllowed ? '允许' : '已禁止'}</b></span>
        <span class="sep">·</span>
        <span>公钥登录：<b>${st.pubkeyAuth ? '允许' : '已禁止'}</b></span>
        <span class="badge ${badgeCls}">${escapeHtml(label.text.replace(/^[^\u4e00-\u9fa5]+\s*/, ''))}</span>
      </div>
      <label class="switch" title="${st.canWrite ? '切换后立即生效' : '需要 root 或免密 sudo 才能修改'}">
        <input type="checkbox" id="sshdToggle" ${st.keyOnly ? 'checked' : ''} ${st.canWrite ? '' : 'disabled'} />
        <span class="slider"></span>
      </label>
      <span class="sshd-label">仅密钥登录</span>`;

    const toggle = bar.querySelector('#sshdToggle');
    toggle.addEventListener('change', async (ev) => {
      const enable = ev.target.checked;
      const ok = confirm(
        enable
          ? '开启后服务器只接受密钥登录，密码登录会被关闭。\n\n前提：你现在就是用密钥登录，且这把钥匙已确认可用。\n\n确定继续？'
          : '重新允许密码登录？服务器安全性会下降。'
      );
      if (!ok) {
        ev.target.checked = !enable;
        return;
      }

      toggle.disabled = true;
      try {
        const r = await api('/api/sshd/password-auth', {
          method: 'POST',
          body: { sid: state.session.sid, enable },
        });
        renderSshd(r.status);
        if (r.effective) {
          toast(enable ? '已开启仅密钥登录' : '已允许密码登录', 'ok');
          logLine(
            `${enable ? '已开启' : '已关闭'}仅密钥登录（备份：${r.backup || '无'}${
              r.reloaded ? '' : '；sshd 未自动重载，可能需要手动重启'
            }）`,
            'ok'
          );
        } else {
          toast(r.warning || '配置已写入但未生效', 'warn', 8000);
          logLine(r.warning || 'sshd 配置可能被 drop-in 覆盖', 'warn');
        }
      } catch (e) {
        ev.target.checked = !enable;
        toggle.disabled = false;
        toast(e.message, 'err', 8000);
        logLine(`切换登录方式失败：${e.message}`, 'err');
      }
    });
  };

  bar.innerHTML = '<span class="sshd-loading">正在读取服务器登录方式…</span>';
  try {
    const st = await api(`/api/sshd?sid=${encodeURIComponent(state.session.sid)}`);
    renderSshd(st);
  } catch (e) {
    bar.innerHTML = `<span class="sshd-err">读取登录方式失败：${escapeHtml(e.message)}</span>`;
  }
}

/* ------------------------------------------------------------------ */
/* 主界面上的登录方式徽章                                               */
/* ------------------------------------------------------------------ */

// 四态文案：一句话说清「现在到底谁能进来」
const SSHD_STATE_LABEL = {
  both: { text: '🔓 密码 + 密钥都可登录', cls: 'warn' },
  'key-only': { text: '🔒 仅密钥登录', cls: 'ok' },
  'password-only': { text: '⚠️ 仅密码登录（密钥已关闭）', cls: 'danger' },
  none: { text: '⛔ 公钥和密码都已关闭', cls: 'danger' },
};

/** 状态列里的登录方式徽章（行内，绿点左侧） */
function sshdBadgeHtml() {
  const st = state.sshd;
  if (!st || !st.available) return '';
  const label = SSHD_STATE_LABEL[st.state];
  if (!label) return '';
  const title = st.canWrite ? '点击切换登录方式' : st.reason || '需要 root 或免密 sudo 才能修改';
  return `<button class="sshd-badge ${label.cls}" data-sshd-toggle ${st.canWrite ? '' : 'disabled'} title="${escapeHtml(title)}">${escapeHtml(label.text)}</button>`;
}

async function refreshSshdBadge() {
  if (!state.session) {
    state.sshd = null;
    renderServer();
    return;
  }
  try {
    state.sshd = await api(`/api/sshd?sid=${encodeURIComponent(state.session.sid)}`);
  } catch {
    state.sshd = null;
  }
  renderServer();
}

async function toggleSshdFromBadge() {
  if (!state.session) return;
  try {
    const st =
      state.sshd && state.sshd.available
        ? state.sshd
        : await api(`/api/sshd?sid=${encodeURIComponent(state.session.sid)}`);

    if (!st.available) return toast(st.reason, 'warn', 6000);

    const enable = !st.keyOnly;
    const ok = confirm(
      enable
        ? '开启「仅密钥登录」后，服务器将不再接受密码登录。\n\n前提：你现在就是用密钥登录，且这把钥匙已确认可用。\n\n确定继续？'
        : '重新允许密码登录？服务器安全性会下降。'
    );
    if (!ok) return;

    const r = await api('/api/sshd/password-auth', {
      method: 'POST',
      body: { sid: state.session.sid, enable },
    });

    state.sshd = r.status;
    await refreshSshdBadge();

    if (r.effective) {
      toast(enable ? '已开启仅密钥登录' : '已允许密码登录', 'ok');
      logLine(`${enable ? '已开启' : '已关闭'}仅密钥登录（备份：${r.backup || '无'}）`, 'ok');
    } else {
      toast(r.warning || '配置已写入但未生效', 'warn', 8000);
      logLine(r.warning || 'sshd 配置可能被 drop-in 覆盖', 'warn');
    }
  } catch (e) {
    toast(e.message, 'err', 8000);
    logLine(`切换登录方式失败：${e.message}`, 'err');
  }
}

export { mountSshdBar, sshdBadgeHtml, refreshSshdBadge, toggleSshdFromBadge };
