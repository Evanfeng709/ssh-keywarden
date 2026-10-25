'use strict';
import { $, state, escapeHtml, shortFp, toast, api } from './core.js';
import { openReplaceModal, askDelete } from './actions.js';
import { sshdBadgeHtml } from './sshd.js';

/* ------------------------------------------------------------------ */
/* 本地密钥库                                                          */
/* ------------------------------------------------------------------ */

const LOCAL_KIND_LABEL = { private: '私钥', public: '公钥' };
let localKind = 'all';
let localQuery = '';
let localVisible = [];
let localToolsBound = false;

let keyFilter = 'all';
let keyQuery = '';
let keySid = '';
let serverToolsBound = false;

async function rescanLocal() {
  try {
    const data = await api(`/api/local-keys?dir=${encodeURIComponent(state.settings.localKeyDir || '')}&force=1`);
    state.localKeys = data;
    renderLocal();
    renderServer();
  } catch (e) {
    toast(`扫描本地密钥失败：${e.message}`, 'err');
  }
}

function bindLocalTools() {
  if (localToolsBound) return;
  localToolsBound = true;

  const search = $('localSearch');
  search.addEventListener('input', () => {
    localQuery = search.value;
    renderLocal();
  });
  search.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    localQuery = '';
    search.value = '';
    renderLocal();
    search.blur();
  });
  $('localSearchClear').addEventListener('click', () => {
    localQuery = '';
    search.value = '';
    renderLocal();
    search.focus();
  });

  $('localFilter').addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-kind]');
    if (!btn) return;
    localKind = btn.dataset.kind;
    renderLocal();
  });

  $('localDirBtn').addEventListener('click', () => {
    const dir = state.localKeys.dir;
    if (dir) copyLocalText(dir, '目录');
  });

  const list = $('localList');
  list.addEventListener('click', (e) => {
    if (e.target.closest('[data-local-reset]')) {
      localQuery = '';
      localKind = 'all';
      $('localSearch').value = '';
      renderLocal();
      $('localSearch').focus();
      return;
    }
    const item = e.target.closest('.local-item');
    if (!item) return;
    const entry = localVisible[Number(item.dataset.idx)];
    if (!entry) return;
    if (e.target.closest('.local-fp')) {
      if (entry.fingerprint) copyLocalText(entry.fingerprint, '指纹');
      else toast('这把钥匙没有可用指纹', 'warn', 2200);
      return;
    }
    copyLocalText(entry.path, '路径');
  });

  list.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const item = e.target.closest('.local-item');
    if (!item) return;
    e.preventDefault();
    const entry = localVisible[Number(item.dataset.idx)];
    if (entry) copyLocalText(entry.path, '路径');
  });
}

/** 目录只保留头尾，中间省略；完整值放 title / 点击复制 */
function midPath(text, max = 40) {
  if (!text || text.length <= max) return text || '';
  const head = Math.ceil((max - 1) * 0.58);
  const tail = max - 1 - head;
  return `${text.slice(0, head)}…${text.slice(-tail)}`;
}

function leafOf(dir) {
  const parts = String(dir || '').split(/[\\/]/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : String(dir || '');
}

async function copyLocalText(text, label) {
  try {
    await navigator.clipboard.writeText(text);
    toast(`${label}已复制`, 'ok', 1600);
  } catch {
    toast(text, 'info', 5000);
  }
}

function localRow(entry) {
  const idx = localVisible.push(entry) - 1;
  const li = document.createElement('li');
  li.className = 'local-item';
  li.dataset.idx = String(idx);
  li.tabIndex = 0;
  li.title = `点击复制完整路径\n${entry.path}`;

  const line1 = document.createElement('div');
  line1.className = 'local-line1';
  const name = document.createElement('span');
  name.className = 'local-name';
  name.textContent = entry.name;
  name.title = entry.name;
  const badges = document.createElement('span');
  badges.className = 'local-badges';
  if (entry.kind === 'private' && entry.encrypted) {
    const badge = document.createElement('span');
    badge.className = 'local-badge';
    badge.title = '私钥已加密，连接时需要 passphrase';
    badge.innerHTML = '<svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true"><rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/></svg>加密';
    badges.appendChild(badge);
  }
  line1.append(name, badges);

  const line2 = document.createElement('div');
  line2.className = 'local-line2';
  const fp = document.createElement('span');
  fp.className = `local-fp${entry.fingerprint ? '' : ' nofp'}`;
  fp.textContent = entry.fingerprint ? shortFp(entry.fingerprint) : '指纹未知';
  fp.title = entry.fingerprint
    ? `${entry.fingerprint}（点击复制完整指纹）`
    : '没有同名 .pub 公钥，无法计算指纹';
  const dirLeaf = document.createElement('span');
  dirLeaf.className = 'local-dir-leaf';
  dirLeaf.textContent = leafOf(entry.dir);
  dirLeaf.title = entry.dir;
  line2.append(fp, dirLeaf);

  li.append(line1, line2);
  const note = noteOf(entry.fingerprint);
  if (note) {
    const line3 = document.createElement('div');
    line3.className = 'local-note';
    line3.textContent = note;
    line3.title = `本地备注：${note}`;
    li.appendChild(line3);
  }
  return li;
}

function renderLocal() {
  const { dir, entries, errors } = state.localKeys;
  const list = $('localList');
  list.innerHTML = '';
  localVisible = [];

  $('localCount').textContent = String(entries.length);

  const dirBtn = $('localDirBtn');
  const dirText = $('localDir');
  dirText.textContent = dir ? midPath(dir) : '未设置密钥目录';
  dirText.title = dir || '';
  dirBtn.disabled = !dir;
  dirBtn.title = dir ? `点击复制密钥目录\n${dir}` : '未设置密钥目录';

  const priv = entries.filter((e) => e.kind === 'private').length;
  const counts = { all: entries.length, private: priv, public: entries.length - priv };
  for (const btn of $('localFilter').querySelectorAll('button[data-kind]')) {
    btn.classList.toggle('on', btn.dataset.kind === localKind);
    btn.querySelector('b').textContent = String(counts[btn.dataset.kind] || 0);
  }
  $('localTools').classList.toggle('hidden', entries.length === 0);
  $('localSearchClear').classList.toggle('hidden', !localQuery);

  const errBox = $('localErrors');
  if (errors && errors.length) {
    errBox.textContent = `⚠ ${errors.length} 个文件读取失败`;
    errBox.title = errors.map((x) => `${x.path || ''}：${x.message}`).join('\n');
  } else {
    errBox.textContent = '';
    errBox.removeAttribute('title');
  }

  const query = localQuery.trim().toLowerCase();
  const filtered = entries.filter((e) => {
    if (localKind !== 'all' && e.kind !== localKind) return false;
    if (!query) return true;
    return [e.name, e.fingerprint, e.path, e.dir, e.type, e.comment, noteOf(e.fingerprint)]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
      .includes(query);
  });

  if (!filtered.length) {
    const li = document.createElement('li');
    li.className = 'local-empty';
    const title = document.createElement('div');
    title.className = 'etitle';
    const hint = document.createElement('div');
    hint.className = 'ehint';
    if (!entries.length) {
      title.textContent = dir ? '这个目录里没找到密钥文件' : '请先在设置里指定本地密钥目录';
      hint.textContent = dir ? '支持递归扫描私钥与 .pub 公钥' : '点右上角 📁 选择目录后会重新扫描';
      li.append(title, hint);
    } else {
      title.textContent = '没有匹配的密钥';
      hint.textContent = '换个关键词，或清空筛选';
      const reset = document.createElement('button');
      reset.type = 'button';
      reset.dataset.localReset = '';
      reset.textContent = '清空筛选';
      li.append(title, hint, reset);
    }
    list.appendChild(li);
    return;
  }

  const sorted = [...filtered].sort((a, b) => (b.mtime || 0) - (a.mtime || 0));
  for (const kind of ['private', 'public']) {
    const group = sorted.filter((e) => e.kind === kind);
    if (!group.length) continue;
    const head = document.createElement('li');
    head.className = 'local-group';
    const label = document.createElement('span');
    label.textContent = LOCAL_KIND_LABEL[kind];
    const n = document.createElement('span');
    n.textContent = String(group.length);
    head.append(label, n);
    list.appendChild(head);
    for (const entry of group) list.appendChild(localRow(entry));
  }
}

/** 找出某指纹对应的本地文件（优先公钥条目） */
function localMatch(fingerprint) {
  if (!fingerprint) return null;
  const hits = state.localKeys.entries.filter((e) => e.fingerprint === fingerprint);
  if (!hits.length) return null;
  return hits.find((e) => e.kind === 'public') || hits[0];
}

/** 一次渲染只建一次映射，避免每行都全量扫本地密钥库 */
function buildLocalMap() {
  const map = new Map();
  for (const entry of state.localKeys.entries) {
    if (!entry.fingerprint) continue;
    const prev = map.get(entry.fingerprint);
    if (!prev || (prev.kind !== 'public' && entry.kind === 'public')) {
      map.set(entry.fingerprint, entry);
    }
  }
  return map;
}

function noteOf(fingerprint) {
  const entry = fingerprint && state.keyNotes ? state.keyNotes[fingerprint] : null;
  return entry && typeof entry.note === 'string' ? entry.note : '';
}

/** 本地条目是否就是主机配置里指定的登录私钥 */
function isLoginKey(entry, identityFile) {
  if (!entry || !identityFile) return false;
  const norm = (p) => String(p).replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  return norm(entry.path.replace(/\.pub$/i, '')) === norm(identityFile);
}

/* ------------------------------------------------------------------ */
/* 服务器密钥表                                                        */
/* ------------------------------------------------------------------ */

function setConnected(connected) {
  const dot = $('connState');
  dot.className = `state-dot${connected ? ' on' : ''}`;
  $('btnRefresh').disabled = !connected;
  $('btnBackups').disabled = !connected;
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

function bindServerTools() {
  if (serverToolsBound) return;
  serverToolsBound = true;

  const search = $('keySearch');
  search.addEventListener('input', () => {
    keyQuery = search.value;
    renderServer();
  });
  search.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    if (keyQuery) {
      keyQuery = '';
      search.value = '';
    } else if (keyFilter !== 'all') {
      keyFilter = 'all';
    }
    renderServer();
    search.blur();
  });
  $('keySearchClear').addEventListener('click', () => {
    keyQuery = '';
    search.value = '';
    renderServer();
    search.focus();
  });

  $('keyFilter').addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-filter]');
    if (!btn) return;
    keyFilter = btn.dataset.filter;
    renderServer();
  });

  $('keyBody').addEventListener('click', (e) => {
    if (e.target.closest('[data-filter-reset]')) {
      resetKeyFilters();
      return;
    }
    const editBtn = e.target.closest('[data-note-edit]');
    if (!editBtn) return;
    startNoteEdit(editBtn.closest('td'), editBtn.dataset.noteEdit);
  });
}

function resetKeyFilters(focus = true) {
  keyQuery = '';
  keyFilter = 'all';
  const search = $('keySearch');
  if (search) search.value = '';
  renderServer();
  if (focus && search) search.focus();
}

function startNoteEdit(cell, fingerprint) {
  if (!cell || cell.querySelector('.note-editor')) return;
  const prevNote = noteOf(fingerprint);
  const oldLine = cell.querySelector('.key-note');
  if (oldLine) oldLine.classList.add('hidden');

  const wrap = document.createElement('div');
  wrap.className = 'note-editor';
  const row = document.createElement('div');
  row.className = 'note-editor-row';
  const input = document.createElement('input');
  input.type = 'text';
  input.maxLength = 200;
  input.value = prevNote;
  input.placeholder = '例如：张三的 MacBook';
  input.setAttribute('aria-label', '本地备注');
  const save = document.createElement('button');
  save.type = 'button';
  save.className = 'note-save';
  save.textContent = '保存';
  const cancel = document.createElement('button');
  cancel.type = 'button';
  cancel.className = 'note-cancel';
  cancel.textContent = '取消';
  const hint = document.createElement('div');
  hint.className = 'note-editor-hint';
  hint.textContent = '只存本机；留空保存 = 删除备注';
  row.append(input, save, cancel);
  wrap.append(row, hint);
  cell.appendChild(wrap);
  input.focus();
  input.select();

  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    wrap.remove();
    if (oldLine) oldLine.classList.remove('hidden');
  };

  const commit = async () => {
    if (closed) return;
    save.disabled = true;
    cancel.disabled = true;
    try {
      const data = await api('/api/key-notes', {
        method: 'POST',
        body: { fingerprint, note: input.value },
      });
      state.keyNotes = data.keyNotes || {};
      const wrapEl = document.querySelector('.table-wrap');
      const top = wrapEl ? wrapEl.scrollTop : 0;
      renderServer();
      renderLocal();
      if (wrapEl) wrapEl.scrollTop = top;
      const next = noteOf(fingerprint);
      toast(
        next ? '备注已保存' : prevNote ? '备注已删除' : '没有需要保存的备注',
        next || prevNote ? 'ok' : 'info',
        1800
      );
    } catch (e) {
      toast(`保存备注失败：${e.message}`, 'err', 5000);
      save.disabled = false;
      cancel.disabled = false;
      input.focus();
    }
  };

  save.addEventListener('click', commit);
  cancel.addEventListener('click', close);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  });
}

function renderServer() {
  const s = state.session;
  const body = $('keyBody');
  const empty = $('emptyState');
  body.innerHTML = '';

  if (!s) {
    keySid = '';
    $('serverMeta').textContent = '未连接';
    setConnected(false);
    state.sshd = null;
    $('keyTools').classList.add('hidden');
    empty.classList.remove('hidden');
    empty.querySelector('p').textContent = '还没有连接服务器';
    renderNotice('');
    return;
  }

  if (keySid !== s.sid) {
    keySid = s.sid;
    keyQuery = '';
    keyFilter = 'all';
    const search = $('keySearch');
    if (search) search.value = '';
  }

  empty.classList.add('hidden');
  setConnected(true);

  const usedFp = s.usedKeyFp;
  // 徽章代表「主机配置里的登录私钥」，也就是下次连接会用的那把；
  // 凭据弹窗临时换私钥只影响本次会话，不能拿 s.host.identityFile 冒充配置。
  const savedHost = s.host.id ? state.hosts.find((h) => h.id === s.host.id) : null;
  const identityFile = (savedHost ? savedHost.identityFile : s.host.identityFile) || '';
  const loginTitle = savedHost
    ? '主机配置里的登录私钥，下次连接会用它'
    : '本次连接使用的私钥（临时连接，未写入主机配置）';
  renderServerMeta(s);

  renderNotice(
    !s.fileExists
      ? '这台服务器还没有 authorized_keys 文件（第一次添加时会自动创建 ~/.ssh 并在写入前备份）。'
      : ''
  );

  const keys = s.items.filter((i) => i.kind === 'key');
  const fpSet = new Set(keys.map((i) => i.fingerprint));
  for (const fp of [...state.selected]) {
    if (!fpSet.has(fp)) state.selected.delete(fp);
  }

  const localMap = buildLocalMap();
  const query = keyQuery.trim().toLowerCase();
  const active = Boolean(query) || keyFilter !== 'all';
  const counts = {
    all: keys.length,
    unmatched: keys.filter((i) => !localMap.has(i.fingerprint)).length,
    dup: keys.filter((i) => i.duplicateOf != null).length,
    used: usedFp ? keys.filter((i) => i.fingerprint === usedFp).length : 0,
  };

  const tools = $('keyTools');
  tools.classList.toggle('hidden', keys.length === 0);
  let visibleKeys = keys;
  if (keys.length) {
    for (const btn of $('keyFilter').querySelectorAll('button[data-filter]')) {
      btn.classList.toggle('on', btn.dataset.filter === keyFilter);
      btn.querySelector('b').textContent = String(counts[btn.dataset.filter] || 0);
    }
    $('keySearchClear').classList.toggle('hidden', !keyQuery);

    visibleKeys = keys.filter((item) => {
      if (keyFilter === 'unmatched' && localMap.has(item.fingerprint)) return false;
      if (keyFilter === 'dup' && item.duplicateOf == null) return false;
      if (keyFilter === 'used' && !(usedFp && item.fingerprint === usedFp)) return false;
      if (!query) return true;
      const local = localMap.get(item.fingerprint);
      return [item.comment, item.fingerprint, item.type, item.options, local?.name, local?.path, noteOf(item.fingerprint)]
        .filter(Boolean)
        .join(' ')
        .toLowerCase()
        .includes(query);
    });

    $('keyViewCount').textContent = `显示 ${visibleKeys.length} / 共 ${keys.length} 把`;
    const visibleFps = new Set(visibleKeys.map((i) => i.fingerprint));
    const hiddenSelected = [...state.selected].filter((fp) => !visibleFps.has(fp)).length;
    const selHidden = $('keySelHidden');
    selHidden.classList.toggle('hidden', hiddenSelected === 0);
    selHidden.textContent = hiddenSelected
      ? `已选 ${state.selected.size} · ${hiddenSelected} 条在筛选外`
      : '';
    selHidden.title = hiddenSelected
      ? '删除会同时作用于被筛掉的钥匙，请核对删除确认里的清单'
      : '';

    if (!visibleKeys.length) {
      body.innerHTML = `<tr><td colspan="7" class="filter-empty">没有匹配的密钥
        <button type="button" data-filter-reset>清空筛选</button></td></tr>`;
      updateSelectionUi();
      alignSplitter();
      return;
    }
  }

  if (!keys.length) {
    body.innerHTML = `<tr><td colspan="7" style="padding:26px;text-align:center;color:var(--text-faint)">
      服务器上还没有任何授权密钥。点「＋ 新增密钥」把本机的公钥装上去。</td></tr>`;
  }

  const items = active ? visibleKeys : s.items;
  for (const item of items) {
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

    if (item.kind !== 'key') continue;

    const checked = state.selected.has(item.fingerprint);
    const local = localMap.get(item.fingerprint) || null;
    const isUsed = usedFp && item.fingerprint === usedFp;
    const isLogin = isLoginKey(local, identityFile);
    const note = noteOf(item.fingerprint);
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
        <div class="comment-line">
          <span class="comment-text">${escapeHtml(item.comment || '(无注释)')}</span>
          <button type="button" class="note-btn ${note ? 'has' : ''}" data-note-edit="${escapeHtml(item.fingerprint)}"
            title="${note ? '编辑本地备注' : '添加本地备注（仅保存在本机）'}"
            aria-label="${note ? '编辑本地备注' : '添加本地备注'}">✎</button>
        </div>
        ${item.options ? `<span class="options">选项：${escapeHtml(item.options)}</span>` : ''}
        ${
          note
            ? `<button type="button" class="key-note" data-note-edit="${escapeHtml(item.fingerprint)}"
                title="点击编辑本地备注：${escapeHtml(note)}"><span class="note-tag">备注</span>${escapeHtml(note)}</button>`
            : ''
        }
      </td>
      <td class="local-cell ${local ? 'hit' : ''}">
        ${local ? escapeHtml(local.name) : '—'}${
          isLogin ? `<span class="badge login" title="${loginTitle}">登录</span>` : ''
        }
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

    tr.querySelector('.fp').addEventListener('click', async () => {
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

export {
  rescanLocal,
  bindLocalTools,
  bindServerTools,
  renderLocal,
  localMatch,
  setConnected,
  alignSplitter,
  renderServer,
  renderNotice,
  updateSelectionUi,
};
