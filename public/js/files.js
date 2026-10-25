'use strict';
import { $, state, escapeHtml, fmtBytes, toast, openModal, closeModal, api } from './core.js';

/** 桌面版由 preload 注入；浏览器模式为 undefined */
const desktopBridge = typeof window !== 'undefined' ? window.keywarden : null;

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

  // 桌面版直接调系统选择框，路径绝对且支持 OneDrive 占位符
  if (desktopBridge && desktopBridge.isDesktop) {
    const pick = mode === 'dir' ? desktopBridge.pickDir : desktopBridge.pickFile;
    pick({ start: opts.start || '', title: opts.title || '' })
      .then((picked) => {
        if (picked) opts.onPick(picked);
      })
      .catch((e) => toast(e.message || '打开系统选择框失败', 'err'));
    return null;
  }

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

export { dirOfPath, openFilePicker, attachDirPicker };
