'use strict';
import { $, state, escapeHtml, shortFp, toast, api } from './core.js';
import { openReplaceModal, askDelete } from './actions.js';
import { sshdBadgeHtml } from './sshd.js';

/* ------------------------------------------------------------------ */
/* 本地密钥库                                                          */
/* ------------------------------------------------------------------ */

async function rescanLocal() {
  try {
    const data = await api(`/api/local-keys?dir=${encodeURIComponent(state.settings.localKeyDir || '')}`);
    state.localKeys = data;
    renderLocal();
    renderServer();
  } catch (e) {
    toast(`扫描本地密钥失败：${e.message}`, 'err');
  }
}

function renderLocal() {
  const { dir, entries, errors } = state.localKeys;
  $('localDir').textContent = dir || '未设置密钥目录';
  $('localDir').title = dir || '';

  const list = $('localList');
  list.innerHTML = '';

  const sorted = [...entries].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'private' ? -1 : 1;
    return (b.mtime || 0) - (a.mtime || 0);
  });

  if (!sorted.length) {
    list.innerHTML = `<div class="key-pick"><div class="none">${
      dir ? '这个目录里没找到密钥文件' : '请先在设置里指定本地密钥目录'
    }</div></div>`;
  }

  for (const e of sorted) {
    const li = document.createElement('li');
    li.title = '点击复制路径';
    const tags = [];
    if (e.kind === 'private') tags.push(e.encrypted ? '<span class="tag">已加密</span>' : '<span class="tag on">私钥</span>');
    else tags.push('<span class="tag">公钥</span>');

    li.innerHTML = `
      <div class="lname">
        <span>${escapeHtml(e.name)}</span>
        <span>${tags.join(' ')}</span>
      </div>
      <div class="lfp">${escapeHtml(shortFp(e.fingerprint))}</div>
      <div class="lpath">${escapeHtml(e.dir)}</div>`;
    li.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(e.path);
        toast('路径已复制', 'ok', 1800);
      } catch {
        toast(e.path, 'info', 5000);
      }
    });
    list.appendChild(li);
  }

  const errBox = $('localErrors');
  errBox.innerHTML = errors && errors.length
    ? errors.slice(0, 4).map((x) => `⚠ ${escapeHtml(x.path || '')}：${escapeHtml(x.message)}`).join('<br>')
    : '';
}

/** 找出某指纹对应的本地文件（优先公钥条目） */
function localMatch(fingerprint) {
  if (!fingerprint) return null;
  const hits = state.localKeys.entries.filter((e) => e.fingerprint === fingerprint);
  if (!hits.length) return null;
  return hits.find((e) => e.kind === 'public') || hits[0];
}

/* ------------------------------------------------------------------ */
/* 服务器密钥表                                                        */
/* ------------------------------------------------------------------ */

function setConnected(connected) {
  const dot = $('connState');
  dot.className = `state-dot${connected ? ' on' : ''}`;
  $('btnRefresh').disabled = !connected;
  $('btnAdd').disabled = !connected;
  $('btnDelete').disabled = !connected || state.selected.size === 0;
  $('btnDisconnect').disabled = !connected;
  // 终端按钮保持可点：没连接时点一下会明确提示「请先连接服务器」，
  // 比灰着让人以为坏了要好
  $('btnTerm').classList.toggle('dim', !connected);
  $('btnTerm').title = connected ? '打开 SSH 终端' : '先连接服务器才能开终端';
}

/** 拖动条的视觉标记与「状态」列对齐
 *  注意：参照物必须用 .server-panel（始终可见）。终端面板收起时 display:none，
 *  拿它量宽度会得 0，对齐就永远不生效。 */
function alignSplitter() {
  const th = document.querySelector('.key-table th.col-status');
  const splitter = $('termSplitter');
  const host = $('termPanel') && $('termPanel').parentElement;
  if (!th || !splitter || !host) return;
  const hr = host.getBoundingClientRect();
  if (!hr.width) return;
  const tr = th.getBoundingClientRect();
  const x = tr.left - hr.left + tr.width / 2;
  splitter.style.setProperty('--align-x', `${Math.max(40, Math.min(x, hr.width - 60))}px`);
}

/** 服务器信息条：主机（重点，可复制）/ 路径（可复制）/ 系统名，分段展示 */
function renderServerMeta(s) {
  const el = $('serverMeta');
  if (!el) return;

  const dotCls = s.authMethod === 'key' ? 'ok' : 'warn';
  const host = `${s.host.username}@${s.host.host}:${s.host.port}`;
  const path = s.remotePath || '';
  const uname = (s.server && s.server.uname) || '';

  const copyIcon =
    '<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
  const parts = [
    `<span class="meta-host"><span class="meta-dot ${dotCls}" title="${
      s.authMethod === 'key' ? '密钥登录' : '密码登录'
    }"></span>${escapeHtml(host)}<button class="meta-copy" data-copy-host title="复制登录信息">${copyIcon}</button></span>`,
  ];
  if (path) {
    parts.push('<span class="meta-sep"></span>');
    parts.push(`<button class="meta-path" title="点击复制路径">${escapeHtml(path)}</button>`);
  }
  if (uname) {
    parts.push('<span class="meta-sep"></span>');
    parts.push(`<span class="meta-sys">${escapeHtml(uname)}</span>`);
  }
  el.innerHTML = parts.join('');

  const copyBtn = el.querySelector('[data-copy-host]');
  if (copyBtn) {
    copyBtn.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(host);
        toast('登录信息已复制', 'ok', 1500);
      } catch {
        toast(host, 'info', 4000);
      }
    });
  }

  const pathBtn = el.querySelector('.meta-path');
  if (pathBtn) {
    pathBtn.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(path);
        toast('路径已复制', 'ok', 1500);
      } catch {
        toast(path, 'info', 4000);
      }
    });
  }
}

function renderServer() {
  const s = state.session;
  const body = $('keyBody');
  const empty = $('emptyState');
  body.innerHTML = '';

  if (!s) {
    $('serverMeta').textContent = '未连接';
    setConnected(false);
    state.sshd = null;
    empty.classList.remove('hidden');
    empty.querySelector('p').textContent = '还没有连接服务器';
    renderNotice('');
    return;
  }

  empty.classList.add('hidden');
  setConnected(true);

  const usedFp = s.usedKeyFp;
  renderServerMeta(s);

  renderNotice(
    !s.fileExists
      ? '这台服务器还没有 authorized_keys 文件（第一次添加时会自动创建 ~/.ssh 并在写入前备份）。'
      : ''
  );

  const keys = s.items.filter((i) => i.kind === 'key');
  if (!keys.length) {
    body.innerHTML = `<tr><td colspan="7" style="padding:26px;text-align:center;color:var(--text-faint)">
      服务器上还没有任何授权密钥。点「＋ 新增密钥」把本机的公钥装上去。</td></tr>`;
  }

  for (const item of s.items) {
    if (item.kind === 'comment') {
      const tr = document.createElement('tr');
      tr.className = 'comment-row';
      tr.innerHTML = `<td></td><td colspan="6"># ${escapeHtml(item.text || '')}</td>`;
      body.appendChild(tr);
      continue;
    }

    if (item.kind === 'malformed') {
      const tr = document.createElement('tr');
      tr.className = 'malformed-row';
      tr.innerHTML = `
        <td></td>
        <td><span class="badge bad">无法解析</span></td>
        <td></td>
        <td></td>
        <td colspan="3" style="font-family:var(--mono);font-size:11.5px;color:#c99">
          ${escapeHtml(item.reason || '格式异常')}
        </td>`;
      body.appendChild(tr);
      continue;
    }

    const checked = state.selected.has(item.fingerprint);
    const local = localMatch(item.fingerprint);
    const isUsed = usedFp && item.fingerprint === usedFp;
    const statusBits = [];
    if (isUsed) statusBits.push('<span class="used-dot" title="本次连接所用的钥匙"></span>');
    if (item.duplicateOf != null) statusBits.push('<span class="badge dup">重复</span>');

    const tr = document.createElement('tr');
    if (checked) tr.classList.add('selected');
    tr.innerHTML = `
      <td class="col-check"><input type="checkbox" data-fp="${escapeHtml(item.fingerprint)}" ${checked ? 'checked' : ''} /></td>
      <td class="col-type"><span class="badge">${escapeHtml(item.type || '未知')}</span></td>
      <td>
        <div class="fp" data-copy="${escapeHtml(item.fingerprint)}" title="点击复制完整指纹">${escapeHtml(item.fingerprint || '—')}</div>
      </td>
      <td class="status-cell">${sshdBadgeHtml()}${statusBits.join(' ') || (state.sshd && state.sshd.available ? '' : '<span class="dash">—</span>')}</td>
      <td class="comment-cell">
        ${escapeHtml(item.comment || '(无注释)')}
        ${item.options ? `<span class="options">选项：${escapeHtml(item.options)}</span>` : ''}
      </td>
      <td class="local-cell ${local ? 'hit' : ''}">
        ${local ? escapeHtml(local.name) : '—'}
      </td>
      <td class="col-op">
        <button class="row-btn" data-replace="${escapeHtml(item.fingerprint)}" title="用新密钥替换这一把">更换</button>
        <button class="row-btn danger" data-del="${escapeHtml(item.fingerprint)}" title="从服务器删除">删除</button>
      </td>`;

    tr.querySelector('input[type="checkbox"]').addEventListener('change', (ev) => {
      if (ev.target.checked) state.selected.add(item.fingerprint);
      else state.selected.delete(item.fingerprint);
      tr.classList.toggle('selected', ev.target.checked);
      updateSelectionUi();
    });

    tr.querySelector('.fp').addEventListener('click', async (ev) => {
      try {
        await navigator.clipboard.writeText(item.fingerprint);
        toast('指纹已复制', 'ok', 1600);
      } catch {
        toast(item.fingerprint, 'info', 4000);
      }
    });

    tr.querySelector('[data-replace]').addEventListener('click', () => openReplaceModal(item.fingerprint));
    tr.querySelector('[data-del]').addEventListener('click', () => askDelete([item.fingerprint]));

    body.appendChild(tr);
  }

  updateSelectionUi();
  alignSplitter();
}

function renderNotice(text, kind = '') {
  const el = $('notice');
  if (!text) {
    el.className = 'notice hidden';
    el.textContent = '';
    return;
  }
  el.className = `notice ${kind}`;
  el.textContent = text;
}

function updateSelectionUi() {
  const n = state.selected.size;
  $('selCount').textContent = String(n);
  $('selCount').classList.toggle('hidden', n === 0);
  $('btnDelete').disabled = !state.session || n === 0;
  const all = document.querySelectorAll('#keyBody input[type="checkbox"]');
  const checked = document.querySelectorAll('#keyBody input[type="checkbox"]:checked');
  const ca = $('checkAll');
  ca.checked = all.length > 0 && checked.length === all.length;
  ca.indeterminate = checked.length > 0 && checked.length < all.length;
}

export { rescanLocal, renderLocal, localMatch, setConnected, alignSplitter, renderServer, renderNotice, updateSelectionUi };
