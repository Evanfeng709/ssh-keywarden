'use strict';
import { $, state, escapeHtml, toast, logLine, openModal, closeModal, confirmModal, api } from './core.js';
import { dirOfPath, openFilePicker, attachDirPicker } from './files.js';
import { rescanLocal } from './keys.js';
import { THEME_CHOICES, currentChoice, applyTheme } from './theme.js';

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
    opt.textContent = '（还没有主机，点「管理主机」添加）';
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
        if (
          !(await confirmModal({
            title: '删除服务器配置',
            message: `确定删除服务器「${h.name}」的配置？（不会影响服务器上的密钥）`,
            danger: true,
            okText: '删除',
          }))
        )
          return;
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

function themeCardHtml(t) {
  const thumb = t.pair
    ? `<span class="theme-thumb" aria-hidden="true">${t.pair
        .map((id) => `<span class="tt-half" data-theme="${id}"></span>`)
        .join('')}</span>`
    : `<span class="theme-thumb" data-theme="${t.id}" aria-hidden="true">
        <span class="tt-side"></span>
        <span class="tt-main"><i></i><i></i><i></i></span>
      </span>`;
  return `<button type="button" class="theme-card" data-choice="${t.id}" aria-pressed="false" title="${escapeHtml(
    t.name
  )}：${escapeHtml(t.desc)}">${thumb}<span class="theme-name">${escapeHtml(t.name)}<span class="tick">✓</span></span><span class="theme-desc">${escapeHtml(
    t.desc
  )}</span></button>`;
}

function openSettingsModal() {
  const s = state.settings;
  const isDesktop = Boolean(window.keywarden && window.keywarden.isDesktop);
  const desktopBlock = isDesktop
    ? `
    <div class="field">
      <label>桌面版数据</label>
      <div class="row">
        <button type="button" id="s_open_data" style="flex:0 0 auto">打开数据目录</button>
      </div>
      <div class="hint">配置、主机指纹与备注都在这里；从旧版本迁移时，把 config.json / known_hosts.json / key-notes.json 复制进去即可。</div>
    </div>`
    : '';
  const bodyHtml = `
    <div class="form-section">
      <div class="form-section-head">
        <h3>外观</h3>
        <span>点一下立即预览，保存后记住</span>
      </div>
      <div class="theme-grid" id="themeGrid">${THEME_CHOICES.map(themeCardHtml).join('')}</div>
    </div>
    <div class="form-section">
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
      </div>${desktopBlock}
    </div>`;

  const foot = `<button data-close>取消</button><button class="primary" id="s_save">保存</button>`;

  const initial = currentChoice();
  let picked = initial;
  let committed = false;

  openModal({
    title: '设置',
    bodyHtml,
    footHtml: foot,
    onClose() {
      // 取消 / 关闭 / 点遮罩：主题回到打开弹窗时的那套
      if (!committed) applyTheme(initial, { persist: false });
    },
    onMount(m) {
      const grid = m.querySelector('#themeGrid');
      const markChoice = (id) => {
        grid.querySelectorAll('.theme-card').forEach((card) => {
          const on = card.dataset.choice === id;
          card.classList.toggle('on', on);
          card.setAttribute('aria-pressed', String(on));
        });
      };
      markChoice(initial);
      grid.addEventListener('click', (e) => {
        const card = e.target.closest('.theme-card');
        if (!card) return;
        picked = card.dataset.choice;
        applyTheme(picked, { persist: false });
        markChoice(picked);
      });

      m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m)));
      attachDirPicker(m, 's_keydir', 's_keydir_browse');
      attachDirPicker(m, 's_savedir', 's_savedir_browse');
      const openData = m.querySelector('#s_open_data');
      if (openData) {
        openData.addEventListener('click', async () => {
          const err = await window.keywarden.openDataDir();
          if (err) toast(err, 'err');
        });
      }
      m.querySelector('#s_save').addEventListener('click', async () => {
        committed = true;
        applyTheme(picked, { persist: true });
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

export { renderHosts, openHostModal, openSettingsModal };
