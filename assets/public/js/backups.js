'use strict';
import { state, escapeHtml, shortFp, toast, logLine, openModal, closeModal, confirmModal, api } from './core.js';
import { renderServer } from './keys.js';

let backupModal = null;

function closeBackupModal(mask) {
  if (backupModal === mask) backupModal = null;
  closeModal(mask);
}

function keyLine(kind, item) {
  return `<div class="bak-key ${kind}">
    <span class="bak-key-sign">${kind === 'add' ? '+' : '−'}</span>
    <span class="bak-key-type">${escapeHtml(item.type || '未知')}</span>
    <span class="bak-key-fp" title="${escapeHtml(item.fingerprint)}">${escapeHtml(
      shortFp(item.fingerprint)
    )}</span>
    <span class="bak-key-comment">${escapeHtml(item.comment || '(无注释)')}</span>
  </div>`;
}

function diffHtml(diff) {
  const rows = [
    ...diff.added.map((item) => keyLine('add', item)),
    ...diff.removed.map((item) => keyLine('del', item)),
  ];
  return rows.length
    ? rows.join('')
    : '<div class="bak-same">钥匙集合与当前一致；恢复仍可能改变注释 / 选项 / 文件格式</div>';
}

async function openBackupsModal() {
  if (!state.session) {
    toast('先连接服务器再查看备份', 'warn');
    return;
  }
  if (backupModal && document.body.contains(backupModal)) return;

  let list = [];
  let selectedName = '';
  let restoreBusy = false;

  const mask = openModal({
    title: 'authorized_keys 历史备份',
    width: '800px',
    bodyHtml: `
      <div class="bak-hint">备份与 authorized_keys 同目录。每次新增 / 更换 / 删除 / 恢复都会自动生成；恢复前会再备份一次当前文件。</div>
      <div class="bak-body">
        <div id="bakList" class="bak-list"><div class="bak-empty">正在读取备份…</div></div>
        <div id="bakDetail" class="bak-detail"><div class="bak-empty">选择一份备份查看差异</div></div>
      </div>`,
    footHtml: `<button data-close>关闭</button><button id="bakReload">刷新列表</button>`,
    onMount(m) {
      backupModal = m;
      const listEl = m.querySelector('#bakList');
      const detailEl = m.querySelector('#bakDetail');

      m.querySelectorAll('[data-close]').forEach((btn) =>
        btn.addEventListener('click', () => closeBackupModal(m))
      );
      m.querySelector('#bakReload').addEventListener('click', loadList);

      function renderList() {
        listEl.innerHTML = list
          .map(
            (b) => `<button type="button" class="bak-item ${b.name === selectedName ? 'on' : ''}"
              data-name="${escapeHtml(b.name)}" title="${escapeHtml(b.name)}">
              <span class="bak-item-time">${escapeHtml(b.displayTime)}</span>
              <span class="bak-item-name">${escapeHtml(b.name)}</span>
            </button>`
          )
          .join('');
        for (const btn of listEl.querySelectorAll('[data-name]')) {
          btn.addEventListener('click', () => selectBackup(btn.dataset.name));
        }
      }

      function renderDetail(diff) {
        const blocked = diff.emptyBackup;
        const needsForce = diff.usedKeyInBackup === false;
        let warn = '';
        if (blocked) {
          warn =
            '<div class="danger-box">这份备份里没有任何可用的密钥，恢复会清空 authorized_keys，已拒绝。</div>';
        } else if (needsForce) {
          warn =
            '<div class="warn-box">这份备份不包含本次连接正在使用的钥匙。恢复后本会话仍能操作，但<b>下次连接会失败</b>。</div>';
        } else if (diff.usedKeyInBackup === null) {
          warn =
            '<div class="warn-box">本次是密码登录，无法判断恢复后钥匙是否还能登录；请自行确认备份里至少有你能用的钥匙。</div>';
        }

        detailEl.classList.remove('hidden');
        detailEl.innerHTML = `
          <div class="bak-detail-head">
            <div class="bak-detail-title">
              <b>${escapeHtml(diff.displayTime)}</b>
              <span>${escapeHtml(diff.name)}</span>
            </div>
            <button type="button" class="primary" id="bakRestore" ${blocked ? 'disabled' : ''}>恢复到此备份</button>
          </div>
          <div class="bak-summary">
            <span>备份 <b>${diff.backupKeyCount}</b> 把</span>
            <span>当前 <b>${diff.currentKeyCount}</b> 把</span>
            <span class="add">+${diff.added.length} 恢复后新增</span>
            <span class="del">−${diff.removed.length} 恢复后移除</span>
          </div>
          ${warn}
          <div class="bak-diff">${diffHtml(diff)}</div>
          ${
            needsForce
              ? `<label class="bak-force"><input type="checkbox" id="bakForce" />
                  我确认：恢复后当前登录钥匙会从文件中消失，下次连接可能失败</label>`
              : ''
          }
          <div class="bak-note">恢复前会自动备份当前文件；如果恢复错了，可以再从备份列表恢复回来。</div>`;

        const restoreBtn = detailEl.querySelector('#bakRestore');
        const forceBox = detailEl.querySelector('#bakForce');
        if (forceBox) {
          restoreBtn.disabled = true;
          forceBox.addEventListener('change', () => {
            restoreBtn.disabled = !forceBox.checked;
          });
        }
        restoreBtn.addEventListener('click', () =>
          restoreSelected(diff, Boolean(forceBox && forceBox.checked))
        );
      }

      async function selectBackup(name) {
        selectedName = name;
        renderList();
        detailEl.innerHTML = '<div class="bak-empty">正在读取差异…</div>';
        try {
          const diff = await api(
            `/api/backups/diff?sid=${encodeURIComponent(state.session.sid)}&name=${encodeURIComponent(name)}`
          );
          if (selectedName !== name) return;
          renderDetail(diff);
        } catch (e) {
          if (selectedName !== name) return;
          detailEl.innerHTML = `<div class="bak-error">读取差异失败：${escapeHtml(e.message)}</div>`;
        }
      }

      async function restoreSelected(diff, force) {
        if (restoreBusy) return;
        const lines = [
          `确定用 ${diff.displayTime} 的备份覆盖服务器上的 authorized_keys？`,
          '',
          `恢复后：${diff.backupKeyCount} 把钥匙（新增 ${diff.added.length}，移除 ${diff.removed.length}）`,
          '写入前会自动备份当前文件，恢复错了可以再恢复回来。',
        ];
        if (force) {
          lines.push('', '⚠ 当前登录钥匙不在备份里，恢复后下次连接会失败。');
        }
        if (!(await confirmModal({ title: '恢复备份', message: lines.join('\n'), danger: force, okText: '恢复' }))) return;

        restoreBusy = true;
        const btn = detailEl.querySelector('#bakRestore');
        if (btn) {
          btn.disabled = true;
          btn.textContent = '恢复中…';
        }
        try {
          const data = await api('/api/backups/restore', {
            method: 'POST',
            body: { sid: state.session.sid, name: diff.name, force },
          });
          (data.steps || []).forEach((s) => logLine(s, 'step'));
          if (data.session) state.session = data.session;
          renderServer();

          if (data.needsForce) {
            logLine(data.message, 'warn');
            toast(data.message, 'warn', 7000);
            restoreBusy = false;
            selectBackup(diff.name); // 重新拉差异，显示强制勾选框
            return;
          }
          if (data.changed) {
            logLine('备份恢复完成', 'ok');
            toast('已恢复到所选备份', 'ok');
            closeBackupModal(mask);
            return;
          }
          logLine(data.message || '未做改动', 'warn');
          toast(data.message || '未做改动', 'info', 5000);
          restoreBusy = false;
          if (btn) {
            btn.disabled = false;
            btn.textContent = '恢复到此备份';
          }
        } catch (e) {
          logLine(`备份恢复失败：${e.message}`, 'err');
          toast(e.message, 'err', 6000);
          restoreBusy = false;
          if (btn) {
            btn.disabled = false;
            btn.textContent = '恢复到此备份';
          }
        }
      }

      async function loadList() {
        listEl.innerHTML = '<div class="bak-empty">正在读取备份…</div>';
        detailEl.innerHTML = '<div class="bak-empty">选择一份备份查看差异</div>';
        try {
          const data = await api(`/api/backups?sid=${encodeURIComponent(state.session.sid)}`);
          list = data.backups || [];
          if (!list.length) {
            listEl.innerHTML =
              '<div class="bak-empty">还没有历史备份<br><span>每次新增 / 更换 / 删除 / 恢复后都会自动生成</span></div>';
            return;
          }
          selectedName = list[0].name;
          renderList();
          selectBackup(selectedName);
        } catch (e) {
          listEl.innerHTML = `<div class="bak-error">读取备份列表失败：${escapeHtml(e.message)}</div>`;
        }
      }

      loadList();
    },
  });

  return mask;
}

export { openBackupsModal };
