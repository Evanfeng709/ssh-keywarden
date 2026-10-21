'use strict';
import { $, state, escapeHtml, shortFp, toast, logLine, openModal, closeModal, api } from './core.js';
import { keySourceForm, applySwitchResult } from './keyform.js';
import { mountSshdBar } from './sshd.js';
import { renderServer, rescanLocal } from './keys.js';

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
          if (data.changed) {
            logLine(`新增成功：${data.fingerprint}`, 'ok');
            toast('密钥已写入服务器', 'ok');
            if (data.privateKeyPath) toast(`私钥已保存：${data.privateKeyPath}`, 'info', 6000);
          } else {
            logLine(data.message, 'warn');
            toast(data.message, 'warn', 5000);
          }
          await applySwitchResult(data, body.switchToNew);
          renderServer();
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
              renderServer();
            }
          } else if (data.removedOld) {
            logLine('更换完成：新钥匙已生效，旧钥匙已移除', 'ok');
            toast('更换完成', 'ok');
            await applySwitchResult(data, true);
            renderServer();
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

export { openAddModal, openReplaceModal, askDelete };
