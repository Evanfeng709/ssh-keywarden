'use strict';

/* ------------------------------------------------------------------ */
/* 基础                                                                */
/* ------------------------------------------------------------------ */

const $ = (id) => document.getElementById(id);

const state = {
  hosts: [],
  settings: {},
  localKeys: { dir: '', entries: [], errors: [] },
  session: null,
  selected: new Set(),
};

function escapeHtml(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

async function api(path, { method = 'GET', body } = {}) {
  const res = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });

  let json;
  try {
    json = await res.json();
  } catch {
    throw new Error(`服务返回了非 JSON 内容（HTTP ${res.status}）`);
  }
  if (res.status === 401) throw new Error('会话未授权，请刷新页面（F5）后重试');
  if (json && json.code === 'SESSION_LOST') handleSessionLost();
  if (!res.ok || json.ok === false) throw new Error(json.error || `请求失败（HTTP ${res.status}）`);
  return json.data;
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

  $('connState').className = 'state-dot err';
  $('connState').title = '连接已断开';
}

function toast(msg, type = 'info', ms = 3600) {
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.textContent = msg;
  $('toastRoot').appendChild(el);
  setTimeout(() => {
    el.style.transition = 'opacity .25s';
    el.style.opacity = '0';
    setTimeout(() => el.remove(), 260);
  }, ms);
}

function logLine(msg, type = 'info') {
  const el = document.createElement('div');
  el.className = `log-line ${type}`;
  const t = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  el.innerHTML = `<span class="t">${t}</span><span class="m">${escapeHtml(msg)}</span>`;
  const body = $('log');
  body.appendChild(el);
  body.scrollTop = body.scrollHeight;
}

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function shortFp(fp) {
  if (!fp) return '—';
  return fp.length > 28 ? `${fp.slice(0, 22)}…${fp.slice(-6)}` : fp;
}

/* ------------------------------------------------------------------ */
/* 弹窗                                                                */
/* ------------------------------------------------------------------ */

let modalStack = [];

function openModal({ title, bodyHtml, footHtml, onMount, width }) {
  const mask = document.createElement('div');
  mask.className = 'modal-mask';
  mask.innerHTML = `
    <div class="modal" ${width ? `style="width:${width}"` : ''}>
      <div class="modal-head">
        <span>${escapeHtml(title)}</span>
        <button class="close-x" data-close>×</button>
      </div>
      <div class="modal-body"></div>
      <div class="modal-foot">${footHtml || ''}</div>
    </div>`;
  mask.querySelector('.modal-body').innerHTML = bodyHtml;
  mask.addEventListener('mousedown', (e) => {
    if (e.target === mask) closeModal(mask);
  });
  mask.querySelector('[data-close]').addEventListener('click', () => closeModal(mask));
  $('modalRoot').appendChild(mask);
  modalStack.push(mask);
  if (onMount) onMount(mask);
  return mask;
}

function closeModal(mask) {
  if (!mask) mask = modalStack[modalStack.length - 1];
  if (!mask) return;
  modalStack = modalStack.filter((m) => m !== mask);
  mask.remove();
}

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeModal();
});

/* ------------------------------------------------------------------ */
/* 本机文件 / 目录选择器                                               */
/* ------------------------------------------------------------------ */

/** 取路径的父目录（同时兼容 \ 和 /） */
function dirOfPath(p) {
  const s = String(p || '').trim();
  if (!s) return '';
  const m = s.match(/^(.*)[\\/][^\\/]*$/);
  return m ? m[1] : s;
}

/**
 * 应用内文件浏览器：浏览本机目录、挑一个文件或目录。
 * 不用 <input type="file"> 是因为浏览器拿不到绝对路径。
 * @param {{mode?:'file'|'dir', start?:string, title?:string, onPick:(p:string)=>void}} opts
 */
function openFilePicker(opts) {
  const mode = opts.mode === 'dir' ? 'dir' : 'file';
  let curDir = '';
  let selectedFile = '';
  let parentDir = null;

  const bodyHtml = `
    <div class="picker">
      <div class="picker-bar">
        <button class="icon-btn" id="pk_up" title="上一级">↑</button>
        <input type="text" id="pk_path" spellcheck="false" placeholder="输入路径后回车" />
        <button id="pk_go">转到</button>
      </div>
      <div class="picker-quick" id="pk_quick"></div>
      <div class="picker-list" id="pk_list"><div class="none">加载中…</div></div>
      <div class="picker-sel" id="pk_sel">未选择</div>
    </div>`;

  const mask = openModal({
    title: opts.title || (mode === 'dir' ? '选择目录' : '选择私钥文件'),
    width: '760px',
    bodyHtml,
    footHtml: `<button data-close>取消</button>
      <button class="primary" id="pk_ok">${mode === 'dir' ? '使用当前目录' : '选择此文件'}</button>`,
    onMount(m) {
      const listEl = m.querySelector('#pk_list');
      const pathEl = m.querySelector('#pk_path');
      const selEl = m.querySelector('#pk_sel');
      const upBtn = m.querySelector('#pk_up');

      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));

      function updateSel() {
        if (mode === 'dir') {
          selEl.innerHTML = `当前目录：<b>${escapeHtml(curDir)}</b>`;
        } else if (selectedFile) {
          selEl.innerHTML = `已选：<b>${escapeHtml(selectedFile)}</b>`;
        } else {
          selEl.textContent = '点一下文件即可选中，双击直接确定';
        }
      }

      function doOk() {
        if (mode === 'dir') {
          if (!curDir) return toast('还没有进入任何目录', 'warn');
          opts.onPick(curDir);
          return closeModal(m);
        }
        if (!selectedFile) return toast('请先点选一个文件', 'warn');
        opts.onPick(selectedFile);
        closeModal(m);
      }

      function renderList(data) {
        listEl.innerHTML = '';
        if (!data.entries.length) {
          listEl.innerHTML = '<div class="none">这个目录是空的</div>';
          return;
        }
        for (const ent of data.entries) {
          const row = document.createElement('div');
          row.className = 'pk-row';

          if (ent.type === 'dir') {
            row.innerHTML = `<span class="pk-icon">📁</span>
              <span class="pk-name">${escapeHtml(ent.name)}</span>
              <span class="pk-meta"></span>`;
            row.addEventListener('click', () => load(ent.path));
            row.addEventListener('dblclick', () => load(ent.path));
          } else {
            const kindLabel = ent.keyKind === 'private' ? '私钥' : ent.keyKind === 'public' ? '公钥' : '';
            row.innerHTML = `<span class="pk-icon">${ent.keyKind ? '🗝️' : '📄'}</span>
              <span class="pk-name ${ent.keyKind ? 'is-key' : ''}">${escapeHtml(ent.name)}</span>
              <span class="pk-meta">${kindLabel} ${fmtBytes(ent.size || 0)}</span>`;
            row.addEventListener('click', () => {
              listEl.querySelectorAll('.pk-row').forEach((x) => x.classList.remove('selected'));
              row.classList.add('selected');
              selectedFile = ent.path;
              updateSel();
            });
            row.addEventListener('dblclick', () => {
              selectedFile = ent.path;
              updateSel();
              doOk();
            });
          }
          listEl.appendChild(row);
        }
        listEl.scrollTop = 0;
      }

      function renderQuick(data) {
        const bar = m.querySelector('#pk_quick');
        bar.innerHTML = '';
        const add = (label, p) => {
          if (!p) return;
          const b = document.createElement('button');
          b.type = 'button';
          b.className = 'chip';
          b.textContent = label;
          b.title = p;
          b.addEventListener('click', () => load(p));
          bar.appendChild(b);
        };
        (data.shortcuts || []).forEach((s) => add(s.label, s.path));
        (data.drives || []).forEach((d) => add(d.replace(/[\\/]+$/, ''), d));
      }

      async function load(dir) {
        listEl.innerHTML = '<div class="none">加载中…</div>';
        try {
          const data = await api(`/api/fs/list?dir=${encodeURIComponent(dir || '')}`);
          curDir = data.dir;
          parentDir = data.parent;
          selectedFile = '';
          pathEl.value = data.dir;
          upBtn.disabled = !data.parent;
          updateSel();
          renderQuick(data);
          renderList(data);
        } catch (e) {
          listEl.innerHTML = `<div class="none" style="color:#ff9b95">${escapeHtml(e.message)}</div>`;
        }
      }

      upBtn.addEventListener('click', () => parentDir && load(parentDir));
      m.querySelector('#pk_go').addEventListener('click', () => load(pathEl.value.trim()));
      pathEl.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') load(pathEl.value.trim());
      });
      m.querySelector('#pk_ok').addEventListener('click', doOk);

      load(opts.start || state.settings.localKeyDir || '');
    },
  });

  return mask;
}

/** 给「输入框 + 浏览按钮」这对控件接上目录选择器 */
function attachDirPicker(mask, inputId, btnId) {
  const input = mask.querySelector(`#${inputId}`);
  const btn = mask.querySelector(`#${btnId}`);
  if (!input || !btn) return;
  btn.addEventListener('click', () => {
    openFilePicker({
      mode: 'dir',
      start: input.value.trim(),
      onPick: (p) => {
        input.value = p;
      },
    });
  });
}

/* ------------------------------------------------------------------ */
/* 主机                                                                */
/* ------------------------------------------------------------------ */

function renderHosts() {
  const sel = $('hostSelect');
  const prev = sel.value;
  sel.innerHTML = '';

  if (!state.hosts.length) {
    const opt = document.createElement('option');
    opt.value = '';
    opt.textContent = '（还没有主机，点 ⚙ 添加）';
    sel.appendChild(opt);
    return;
  }

  for (const h of state.hosts) {
    const opt = document.createElement('option');
    opt.value = h.id;
    opt.textContent = `${h.name} — ${h.username}@${h.host}:${h.port}`;
    sel.appendChild(opt);
  }
  if (prev && state.hosts.some((h) => h.id === prev)) sel.value = prev;
}

function openHostModal(hostId) {
  let editingId =
    hostId && state.hosts.some((h) => h.id === hostId) ? hostId : state.hosts[0] ? state.hosts[0].id : null;

  const mask = openModal({
    title: '服务器管理',
    width: '720px',
    bodyHtml: `<div class="srv-tabs" id="srvTabs"></div><div id="srvForm"></div>`,
    footHtml: `
      <button class="danger" id="h_delete">删除这台</button>
      <span style="flex:1"></span>
      <button data-close>关闭</button>
      <button class="primary" id="h_save">保存</button>`,
    onMount(m) {
      const tabsEl = m.querySelector('#srvTabs');
      const formEl = m.querySelector('#srvForm');
      const delBtn = m.querySelector('#h_delete');

      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));

      const currentHost = () => (editingId ? state.hosts.find((x) => x.id === editingId) || null : null);

      function renderTabs() {
        tabsEl.innerHTML = '';
        for (const h of state.hosts) {
          const b = document.createElement('button');
          b.type = 'button';
          b.className = `srv-tab${editingId === h.id ? ' active' : ''}`;
          b.textContent = h.name || h.host;
          b.title = `${h.username}@${h.host}:${h.port}`;
          b.addEventListener('click', () => {
            editingId = h.id;
            renderTabs();
            renderForm();
          });
          tabsEl.appendChild(b);
        }
        const add = document.createElement('button');
        add.type = 'button';
        add.className = 'srv-tab add';
        add.textContent = '＋ 新建服务器';
        add.addEventListener('click', () => {
          editingId = null;
          renderTabs();
          renderForm();
        });
        tabsEl.appendChild(add);
      }

      function renderForm() {
        const h = currentHost() || {
          name: '',
          host: '',
          port: 22,
          username: '',
          identityFile: '',
          remotePath: '',
        };
        const isNew = !currentHost();
        delBtn.style.visibility = isNew ? 'hidden' : 'visible';

        formEl.innerHTML = `
          <div class="field">
            <label>名称</label>
            <input type="text" id="h_name" placeholder="例如 nas、vps-hk" value="${escapeHtml(h.name)}" />
          </div>
          <div class="field">
            <label>地址</label>
            <div class="row">
              <input type="text" id="h_host" placeholder="example.com 或 203.0.113.10" value="${escapeHtml(h.host)}" style="flex:2" />
              <input type="number" id="h_port" placeholder="22" value="${h.port || 22}" style="flex:0 0 90px" />
            </div>
          </div>
          <div class="field">
            <label>用户名</label>
            <input type="text" id="h_user" placeholder="admin / root" value="${escapeHtml(h.username)}" />
          </div>
          <div class="field">
            <label>私钥文件（可选，留空则连接时用密码）</label>
            <div class="row">
              <input type="text" id="h_key" placeholder="C:\\Users\\...\\id_ed25519" value="${escapeHtml(h.identityFile)}" style="flex:1" />
              <button type="button" id="h_key_browse" style="flex:0 0 auto">浏览…</button>
            </div>
            <div class="hint">密码不会被保存。私钥路径可以是 OneDrive 里的密钥，路径带空格也没问题。</div>
          </div>
          <div class="field">
            <label>authorized_keys 路径（可选）</label>
            <input type="text" id="h_remote" placeholder="留空 = ~/.ssh/authorized_keys" value="${escapeHtml(h.remotePath || '')}" />
          </div>`;

        formEl.querySelector('#h_key_browse').addEventListener('click', () => {
          const cur = formEl.querySelector('#h_key').value.trim();
          openFilePicker({
            mode: 'file',
            start: cur ? dirOfPath(cur) : state.settings.localKeyDir || '',
            onPick: (p) => {
              formEl.querySelector('#h_key').value = p;
            },
          });
        });
      }

      m.querySelector('#h_save').addEventListener('click', async () => {
        try {
          const payload = {
            id: editingId || undefined,
            name: formEl.querySelector('#h_name').value,
            host: formEl.querySelector('#h_host').value.trim(),
            port: Number(formEl.querySelector('#h_port').value) || 22,
            username: formEl.querySelector('#h_user').value.trim(),
            identityFile: formEl.querySelector('#h_key').value.trim(),
            remotePath: formEl.querySelector('#h_remote').value.trim(),
          };
          if (!payload.host) return toast('请填写服务器地址', 'warn');
          if (!payload.username) return toast('请填写用户名', 'warn');

          const saved = await api('/api/hosts', { method: 'POST', body: payload });
          state.hosts = await api('/api/state').then((s) => s.hosts);
          editingId = saved.id;
          renderHosts();
          $('hostSelect').value = saved.id;
          renderTabs();
          renderForm();
          toast(`已保存：${saved.name}`, 'ok');
          logLine(`服务器已保存：${saved.name} (${saved.username}@${saved.host}:${saved.port})`, 'ok');
        } catch (e) {
          toast(e.message, 'err');
        }
      });

      delBtn.addEventListener('click', async () => {
        const h = currentHost();
        if (!h) return;
        if (!confirm(`确定删除服务器「${h.name}」的配置？（不会影响服务器上的密钥）`)) return;
        try {
          await api(`/api/hosts?id=${encodeURIComponent(h.id)}`, { method: 'DELETE' });
          state.hosts = await api('/api/state').then((s) => s.hosts);
          editingId = state.hosts[0] ? state.hosts[0].id : null;
          renderHosts();
          renderTabs();
          renderForm();
          toast('已删除', 'ok');
          logLine(`服务器配置已删除：${h.name}`, 'ok');
        } catch (e) {
          toast(e.message, 'err');
        }
      });

      renderTabs();
      renderForm();
    },
  });
  return mask;
}

function openSettingsModal() {
  const s = state.settings;
  const bodyHtml = `
    <div class="field">
      <label>本地密钥库目录</label>
      <div class="row">
        <input type="text" id="s_keydir" placeholder="例如 C:\\Users\\xxx\\OneDrive\\密钥" value="${escapeHtml(s.localKeyDir || '')}" style="flex:1" />
        <button type="button" id="s_keydir_browse" style="flex:0 0 auto">浏览…</button>
      </div>
      <div class="hint">用来扫出本机所有私钥/公钥，跟服务器上的钥匙做指纹比对。</div>
    </div>
    <div class="field">
      <label>新生成密钥的保存目录</label>
      <div class="row">
        <input type="text" id="s_savedir" placeholder="留空 = 本地密钥库目录" value="${escapeHtml(s.saveDir || '')}" style="flex:1" />
        <button type="button" id="s_savedir_browse" style="flex:0 0 auto">浏览…</button>
      </div>
    </div>
    <div class="field">
      <label>默认 authorized_keys 路径</label>
      <input type="text" id="s_remote" placeholder="留空 = ~/.ssh/authorized_keys" value="${escapeHtml(s.remotePath || '')}" />
    </div>`;

  const foot = `<button data-close>取消</button><button class="primary" id="s_save">保存</button>`;

  openModal({
    title: '设置',
    bodyHtml,
    footHtml: foot,
    onMount(m) {
      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));
      attachDirPicker(m, 's_keydir', 's_keydir_browse');
      attachDirPicker(m, 's_savedir', 's_savedir_browse');
      m.querySelector('#s_save').addEventListener('click', async () => {
        try {
          state.settings = await api('/api/settings', {
            method: 'POST',
            body: {
              localKeyDir: m.querySelector('#s_keydir').value.trim(),
              saveDir: m.querySelector('#s_savedir').value.trim(),
              remotePath: m.querySelector('#s_remote').value.trim(),
            },
          });
          closeModal(m);
          await rescanLocal();
          toast('设置已保存', 'ok');
        } catch (e) {
          toast(e.message, 'err');
        }
      });
    },
  });
}

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

/* ------------------------------------------------------------------ */
/* 连接 / 刷新 / 断开                                                  */
/* ------------------------------------------------------------------ */

async function doConnect(hostId, extra = {}) {
  const btn = $('btnConnect');
  btn.disabled = true;
  btn.textContent = '连接中…';
  logLine(`正在连接 ${hostId ? (state.hosts.find((h) => h.id === hostId) || {}).name || hostId : '主机'}…`, 'info');

  try {
    const data = await api('/api/connect', { method: 'POST', body: { hostId, ...extra } });
    destroyTerminal(); // 换了服务器，旧终端必须关掉
    state.session = data;
    state.selected.clear();
    $('connState').className = 'state-dot on';
    $('connState').title = '已连接';
    renderServer();
    logLine(
      `已连接 ${data.host.username}@${data.host.host}（${data.server.uname || '未知系统'}）· ${data.remotePath} · 共 ${data.stats.total} 把钥匙`,
      'ok'
    );
    toast(`已连接 ${data.host.host}`, 'ok');
    refreshSshdBadge();

    // 用户是从「终端」按钮进来的，连上后直接把终端拉起来
    if (pendingTerminal) {
      pendingTerminal = false;
      setTimeout(() => openTerminal(), 60);
    }
    return true;
  } catch (e) {
    $('connState').className = 'state-dot err';
    logLine(`连接失败：${e.message}`, 'err');
    openCredModal(e.message, hostId);
    return false;
  } finally {
    btn.disabled = false;
    btn.textContent = '连接';
  }
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
        await doConnect(null, {
          host: host.host,
          port: host.port,
          username: host.username,
          identityFile: identityFile || undefined,
          remotePath: host.remotePath || undefined,
          passphrase: passphrase || undefined,
          password: password || undefined,
          name: host.name,
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
  try {
    await api('/api/disconnect', { method: 'POST', body: { sid: state.session.sid } });
  } catch {}
  destroyTerminal();
  logLine('已断开连接', 'info');
  state.session = null;
  state.selected.clear();
  renderServer();
}

/* ------------------------------------------------------------------ */
/* 密钥来源表单（生成 / 本地 / 粘贴）                                   */
/* ------------------------------------------------------------------ */

/** 新增/更换后同步「本机登录密钥」的切换结果；勾了切换却没切成，必须明确报出来 */
async function applySwitchResult(data, switchRequested) {
  if (data && data.switched && data.identityFile) {
    try {
      state.hosts = await api('/api/state').then((s) => s.hosts);
      renderHosts();
      if (state.session) state.session.host.identityFile = data.identityFile;
    } catch {
      /* 列表刷新失败不影响主流程 */
    }
    logLine(`已切换登录密钥：${data.identityFile}`, 'ok');
    toast('本机已改用这把新密钥登录（下次连接生效）', 'ok', 5000);
    return;
  }
  if (switchRequested) {
    const why = (data && data.switchReason) || '原因见上方步骤';
    logLine(`本机登录密钥未切换：${why}`, 'warn');
    toast(`公钥已写入服务器，但本机登录密钥没有切换：${why}`, 'warn', 8000);
  }
}

function keySourceForm(prefix, opts = {}) {
  const defaultDir = state.settings.saveDir || state.settings.localKeyDir || '';
  const hostLabel = state.session ? state.session.host.name : 'key';
  const today = new Date().toISOString().slice(0, 10).replace(/-/g, '');
  const defaultComment = `${(state.session && state.session.server && state.session.server.user) || 'user'}@${hostLabel}-${today}`;

  // 更换弹窗不放这个勾选框：那边完成后一定会切换登录密钥（旧钥匙已从服务器删掉，不切下次就登不进去）
  const switchBlock =
    opts.showSwitch === false
      ? ''
      : `
    <div class="field" style="margin-top:14px;margin-bottom:0">
      <div class="inline">
        <input type="checkbox" id="${prefix}_switch" checked />
        <label for="${prefix}_switch" style="margin:0;font-size:12.5px">写完后把本机登录密钥切换成这把（下次连接即用它）</label>
      </div>
      <div class="hint">切换前会先用新私钥真连一次验证；验证不通过就保持原配置不动。</div>
    </div>`;

  const html = `
    <div class="tabs" data-tabs>
      <button class="active" data-tab="generate">生成新密钥</button>
      <button data-tab="local">本地已有</button>
      <button data-tab="paste">粘贴公钥</button>
    </div>

    <div class="tab-pane active" data-pane="generate">
      <div class="field">
        <label>算法</label>
        <select id="${prefix}_gen">
          <option value="ed25519">Ed25519（推荐，短、快、现代）</option>
          <option value="rsa">RSA 3072（兼容老服务器）</option>
        </select>
      </div>
      <div class="field">
        <label>私钥密码 passphrase（可选，明文显示便于核对）</label>
        <input type="text" id="${prefix}_pass" placeholder="留空 = 私钥不加密" autocomplete="off" spellcheck="false" />
        <div class="hint">设了密码的私钥，登录时要用它解密（本工具会在切换密钥时自动带上）。密码不落盘。</div>
      </div>
      <div class="field">
        <label>注释（公钥末尾的标签，便于以后辨认）</label>
        <input type="text" id="${prefix}_comment" value="${escapeHtml(defaultComment)}" />
      </div>
      <div class="field">
        <label>保存目录</label>
        <div class="row">
          <input type="text" id="${prefix}_dir" value="${escapeHtml(defaultDir)}" placeholder="留空 = 用户目录\\.ssh" style="flex:1" />
          <button type="button" id="${prefix}_dir_browse" style="flex:0 0 auto">浏览…</button>
        </div>
        <div class="hint">私钥和 .pub 会一起写到这里，私钥权限 600。</div>
      </div>
    </div>

    <div class="tab-pane" data-pane="local">
      <div class="key-pick" id="${prefix}_pick"></div>
      <div class="hint" style="margin-top:6px">选一个本地公钥（.pub）或私钥，工具会自动推出对应的公钥行。</div>
      <div class="field">
        <label id="${prefix}_loc_pass_label">私钥密码 passphrase（可选）</label>
        <input type="text" id="${prefix}_loc_pass" placeholder="配对私钥带密码时填写，明文显示便于核对" autocomplete="off" spellcheck="false" />
        <div class="hint" id="${prefix}_loc_pass_hint">${
          opts.showSwitch === false
            ? '更换完会把本机登录密钥改成这把，验证新私钥能否登录时要用它——配对私钥带密码就必须填。'
            : '只有勾了「切换本机登录密钥」才需要它——工具要用这把私钥真连一次验证。'
        }</div>
      </div>
    </div>

    <div class="tab-pane" data-pane="paste">
      <div class="field">
        <label>公钥内容</label>
        <textarea id="${prefix}_paste" rows="5" placeholder="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... user@host"></textarea>
      </div>
    </div>

    ${switchBlock}`;

  let selectedPath = null;
  let selectedNeedsPass = false;
  let activeTab = 'generate';

  /** 选中项的配对私钥是否是 passphrase 加密的 —— 决定了切换登录密钥时要不要密码 */
  function pairedPrivEncrypted(entry) {
    if (entry.kind === 'private') return Boolean(entry.encrypted);
    const pair = entry.path.replace(/\.pub$/i, '');
    const priv = (state.localKeys.entries || []).find((x) => x.kind === 'private' && x.path === pair);
    return Boolean(priv && priv.encrypted);
  }

  function mount(root) {
    attachDirPicker(root, `${prefix}_dir`, `${prefix}_dir_browse`);

    const tabs = root.querySelector('[data-tabs]');
    tabs.querySelectorAll('button').forEach((b) => {
      b.addEventListener('click', () => {
        tabs.querySelectorAll('button').forEach((x) => x.classList.remove('active'));
        b.classList.add('active');
        activeTab = b.dataset.tab;
        root.querySelectorAll('.tab-pane').forEach((p) => p.classList.toggle('active', p.dataset.pane === activeTab));
      });
    });

    // 本地密钥选择
    const pick = root.querySelector(`#${prefix}_pick`);
    const candidates = state.localKeys.entries
      .filter((e) => (e.kind === 'public' && e.valid !== false) || (e.kind === 'private' && !e.encrypted))
      .sort((a, b) => (a.kind === b.kind ? (b.mtime || 0) - (a.mtime || 0) : a.kind === 'public' ? -1 : 1));

    if (!candidates.length) {
      pick.innerHTML = '<div class="none">本地密钥库里没有可用的公钥，请先在设置里指定目录</div>';
    } else {
      for (const e of candidates) {
        const div = document.createElement('div');
        div.className = 'item';
        div.dataset.path = e.path;
        div.innerHTML = `<div class="n">${escapeHtml(e.name)} <span style="color:var(--text-faint)">· ${
          e.kind === 'public' ? '公钥' : '私钥'
        }</span></div><div class="f">${escapeHtml(shortFp(e.fingerprint))}</div>`;
        div.addEventListener('click', () => {
          pick.querySelectorAll('.item').forEach((x) => x.classList.remove('selected'));
          div.classList.add('selected');
          selectedPath = e.path;
          selectedNeedsPass = pairedPrivEncrypted(e);
          const lbl = root.querySelector(`#${prefix}_loc_pass_label`);
          if (lbl) {
            lbl.textContent = selectedNeedsPass
              ? '私钥密码 passphrase（这把公钥的配对私钥带密码，切换登录密钥时必填）'
              : '私钥密码 passphrase（可选）';
          }
        });
        pick.appendChild(div);
      }
    }
  }

  function payload() {
    const switchEl = document.getElementById(`${prefix}_switch`);
    // 更换弹窗没有勾选框（那边是无条件切换的），按「要切换」算才能正确要求 passphrase
    const switchToNew = opts.showSwitch === false ? true : switchEl ? switchEl.checked : false;

    if (activeTab === 'generate') {
      return {
        mode: 'generate',
        generator: document.getElementById(`${prefix}_gen`).value,
        comment: document.getElementById(`${prefix}_comment`).value,
        saveDir: document.getElementById(`${prefix}_dir`).value.trim(),
        passphrase: document.getElementById(`${prefix}_pass`).value || '',
        switchToNew,
      };
    }
    if (activeTab === 'local') {
      if (!selectedPath) throw new Error('请先在「本地已有」里选一把密钥');
      const passEl = document.getElementById(`${prefix}_loc_pass`);
      const passphrase = passEl ? passEl.value : '';
      if (switchToNew && selectedNeedsPass && !passphrase) {
        throw new Error(
          '这把公钥的配对私钥带 passphrase 加密：填写密码后才能切换本机登录密钥（切换前要用它真连一次验证），或取消勾选「切换本机登录密钥」'
        );
      }
      return { mode: 'local', path: selectedPath, passphrase, switchToNew };
    }
    const text = document.getElementById(`${prefix}_paste`).value.trim();
    if (!text) throw new Error('请粘贴公钥内容');
    return { mode: 'paste', publicKey: text, switchToNew };
  }

  return { html, mount, payload };
}

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

/* ------------------------------------------------------------------ */
/* 新增                                                                */
/* ------------------------------------------------------------------ */

function openAddModal() {
  if (!state.session) return;
  const form = keySourceForm('add');

  const mask = openModal({
    title: '新增密钥到服务器',
    bodyHtml:
      `<div id="sshdBar" class="sshd-bar"><span class="sshd-loading">正在读取服务器登录方式…</span></div>` +
      form.html +
      `<div id="add_feedback"></div>`,
    footHtml: `<button data-close>取消</button><button class="primary" id="add_go">写入服务器</button>`,
    onMount(m) {
      form.mount(m);
      mountSshdBar(m, 'sshdBar');
      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));
      m.querySelector('#add_go').addEventListener('click', async () => {
        const btn = m.querySelector('#add_go');
        let body;
        try {
          body = form.payload();
        } catch (e) {
          toast(e.message, 'warn');
          return;
        }
        btn.disabled = true;
        btn.textContent = '写入中…';
        try {
          const data = await api('/api/add', { method: 'POST', body: { sid: state.session.sid, ...body } });
          (data.steps || []).forEach((s) => logLine(s, 'step'));
          state.session = data.session;
          renderServer();
          if (data.changed) {
            logLine(`新增成功：${data.fingerprint}`, 'ok');
            toast('密钥已写入服务器', 'ok');
            if (data.privateKeyPath) toast(`私钥已保存：${data.privateKeyPath}`, 'info', 6000);
          } else {
            logLine(data.message, 'warn');
            toast(data.message, 'warn', 5000);
          }
          await applySwitchResult(data, body.switchToNew);
          closeModal(m);
          await rescanLocal();
        } catch (e) {
          logLine(`新增失败：${e.message}`, 'err');          toast(e.message, 'err', 6000);
          m.querySelector('#add_feedback').innerHTML = `<div class="danger-box">${escapeHtml(e.message)}</div>`;
          btn.disabled = false;
          btn.textContent = '写入服务器';
        }
      });
    },
  });
  return mask;
}

/* ------------------------------------------------------------------ */
/* 更换                                                                */
/* ------------------------------------------------------------------ */

function openReplaceModal(oldFp) {
  if (!state.session) return;
  const keys = state.session.items.filter((i) => i.kind === 'key');
  if (!keys.length) {
    toast('服务器上没有可更换的密钥，请先新增一把', 'warn');
    return;
  }
  const form = keySourceForm('rep', { showSwitch: false });

  const oldOptions = keys
    .map(
      (k) =>
        `<option value="${escapeHtml(k.fingerprint)}" ${k.fingerprint === oldFp ? 'selected' : ''}>${escapeHtml(
          shortFp(k.fingerprint)
        )} · ${escapeHtml(k.type)} · ${escapeHtml(k.comment || '无注释')}</option>`
    )
    .join('');

  const bodyHtml = `
    <div class="warn-box">
      流程是「先加新钥匙 → 用新私钥真实登录验证 → 再删旧钥匙」。任何一步失败都会保留旧钥匙，不会把你锁在门外。<br />
      完成后会自动把本机登录密钥改成这把新钥匙（旧钥匙已从服务器删掉，不改下次就登不进去）。
    </div>
    <div class="field">
      <label>要替换掉哪一把（旧密钥）</label>
      <select id="rep_old">${oldOptions}</select>
    </div>
    ${form.html}
    <div class="field" style="margin-top:14px">
      <div class="inline">
        <input type="checkbox" id="rep_force" />
        <label for="rep_force" style="margin:0;font-size:12.5px">新钥匙无法自动验证时，仍然强制删除旧钥匙</label>
      </div>
    </div>`;

  openModal({
    title: '更换服务器登录密钥',
    bodyHtml,
    footHtml: `<button data-close>取消</button><button class="primary" id="rep_go">开始更换</button>`,
    onMount(m) {
      form.mount(m);
      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));
      m.querySelector('#rep_go').addEventListener('click', async () => {
        let body;
        try {
          body = form.payload();
        } catch (e) {
          toast(e.message, 'warn');
          return;
        }
        const oldFingerprint = m.querySelector('#rep_old').value;
        const force = m.querySelector('#rep_force').checked;
        const btn = m.querySelector('#rep_go');
        btn.disabled = true;
        btn.textContent = '执行中…';
        try {
          const data = await api('/api/replace', {
            method: 'POST',
            body: { sid: state.session.sid, oldFingerprint, force, ...body },
          });
          (data.steps || []).forEach((s) => logLine(s, 'step'));
          state.session = data.session;
          renderServer();

          if (data.needsForce) {
            logLine(data.message, 'warn');
            const again = confirm(`${data.message}\n\n确定要强制删除旧密钥吗？`);
            if (again) {
              btn.disabled = true;
              btn.textContent = '强制完成…';
              const retry = await api('/api/replace', {
                method: 'POST',
                body: { sid: state.session.sid, oldFingerprint, force: true, ...body },
              });
              (retry.steps || []).forEach((s) => logLine(s, 'step'));
              state.session = retry.session;
              renderServer();
              logLine('更换完成（强制模式）', 'ok');
              toast('更换完成', 'ok');
              await applySwitchResult(retry, true);
            }
          } else if (data.removedOld) {
            logLine('更换完成：新钥匙已生效，旧钥匙已移除', 'ok');
            toast('更换完成', 'ok');
            await applySwitchResult(data, true);
          } else {
            logLine(data.message || '未做改动', 'warn');
            toast(data.message || '未做改动', 'warn', 5000);
          }
          closeModal(m);
          await rescanLocal();
        } catch (e) {
          logLine(`更换失败：${e.message}`, 'err');
          toast(e.message, 'err', 6000);
          btn.disabled = false;
          btn.textContent = '开始更换';
        }
      });
    },
  });
}

/* ------------------------------------------------------------------ */
/* 删除                                                                */
/* ------------------------------------------------------------------ */

async function askDelete(fingerprints) {
  if (!state.session) return;
  const list = fingerprints || [...state.selected];
  if (!list.length) return;

  const info = list
    .map((fp) => {
      const item = state.session.items.find((i) => i.fingerprint === fp);
      return `· ${item ? item.comment || '(无注释)' : ''}  ${shortFp(fp)}`;
    })
    .join('\n');

  const used = state.session.usedKeyFp && list.includes(state.session.usedKeyFp);
  const extra = used ? '\n\n⚠ 其中包含【本次连接正在使用】的钥匙，删掉会立刻断开且无法再登录。' : '';
  if (!confirm(`确定从服务器删除这 ${list.length} 把钥匙？\n\n${info}${extra}\n\n写入前会自动备份原文件。`)) return;

  try {
    const data = await api('/api/delete', {
      method: 'POST',
      body: { sid: state.session.sid, fingerprints: list, force: used ? confirm('再次确认：强制删除当前连接用的钥匙？') : false },
    });
    (data.steps || []).forEach((s) => logLine(s, 'step'));
    state.session = data.session;
    state.selected.clear();
    renderServer();
    if (data.needsForce) {
      logLine(data.message, 'warn');
      toast(data.message, 'warn', 6000);
    } else {
      logLine('删除完成', 'ok');
      toast('已删除', 'ok');
    }
  } catch (e) {
    logLine(`删除失败：${e.message}`, 'err');
    toast(e.message, 'err', 6000);
  }
}

/* ------------------------------------------------------------------ */
/* 内置 SSH 终端                                                       */
/* ------------------------------------------------------------------ */

let termCtl = null;
let pendingTerminal = false;

/** 终端默认高度：窗口的 42%，用户拖动过就记住 */
const TERM_HEIGHT_KEY = 'kk.termHeight';

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

/* ------------------------------------------------------------------ */
/* 事件绑定 + 启动                                                     */
/* ------------------------------------------------------------------ */

function bind() {
  $('btnConnect').addEventListener('click', () => {
    const id = $('hostSelect').value;
    if (!id) return openHostModal();
    doConnect(id);
  });

  $('btnHosts').addEventListener('click', () => {
    const id = $('hostSelect').value;
    openHostModal(state.hosts.length ? id : null);
  });

  $('btnSettings').addEventListener('click', openSettingsModal);
  $('btnRescan').addEventListener('click', rescanLocal);

  $('btnRefresh').addEventListener('click', refreshSession);
  $('btnDisconnect').addEventListener('click', disconnect);
  $('btnAdd').addEventListener('click', openAddModal);
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
    if (termCtl) termCtl.term.clear();
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
      if (termCtl) termCtl.fit.fit();
    });

    document.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      saveTermHeight(panel.offsetHeight);
      if (termCtl) {
        termCtl.fit.fit();
        termCtl.sendResize();
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
    state.localKeys = s.localKeys;
    renderHosts();
    renderLocal();
    renderServer();

    if (!s.hosts.length) {
      logLine('还没有配置主机。点右上角 ⚙ 添加一台，或用「连接」按钮直接填。', 'info');
    } else {
      logLine(`已加载 ${s.hosts.length} 台主机配置，本地密钥库 ${s.localKeys.entries.length} 个文件`, 'info');
    }
  } catch (e) {
    logLine(`初始化失败：${e.message}`, 'err');
    toast(e.message, 'err', 8000);
  }
}

boot();
