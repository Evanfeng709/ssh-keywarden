'use strict';
import { state, escapeHtml, shortFp, toast, logLine, api, stripPubSuffix } from './core.js';
import { attachDirPicker } from './files.js';
import { renderHosts } from './hosts.js';

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
    const pair = stripPubSuffix(entry.path);
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

export { applySwitchResult, keySourceForm };
