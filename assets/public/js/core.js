'use strict';

/* ------------------------------------------------------------------ */
/* 基础：状态、HTTP、提示、弹窗                                          */
/* ------------------------------------------------------------------ */

export const $ = (id) => document.getElementById(id);

export const state = {
  hosts: [],
  settings: {},
  keyNotes: {},
  localKeys: { dir: '', entries: [], errors: [] },
  session: null,
  selected: new Set(),
  sshd: null,
  mode: 'keys',
  connecting: false,
};

export function escapeHtml(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/* 会话断开后的界面复位由 connect.js 注册，避免 core 反向依赖业务模块 */
let sessionLostHandler = () => {};
export function setSessionLostHandler(fn) {
  sessionLostHandler = fn;
}

export async function api(path, { method = 'GET', body } = {}) {
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
  if (json && json.code === 'SESSION_LOST') sessionLostHandler();
  if (!res.ok || json.ok === false) {
    const err = new Error(json.error || `请求失败（HTTP ${res.status}）`);
    err.code = json.code;
    err.details = json.details;
    err.status = res.status;
    throw err;
  }
  return json.data;
}

export function toast(msg, type = 'info', ms = 3600) {
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

export function logLine(msg, type = 'info') {
  const el = document.createElement('div');
  el.className = `log-line ${type}`;
  const t = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  el.innerHTML = `<span class="t">${t}</span><span class="m">${escapeHtml(msg)}</span>`;
  const body = $('log');
  body.appendChild(el);
  body.scrollTop = body.scrollHeight;
}

export function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function shortFp(fp) {
  if (!fp) return '—';
  return fp.length > 28 ? `${fp.slice(0, 22)}…${fp.slice(-6)}` : fp;
}

/* ------------------------------------------------------------------ */
/* 剪贴板：桌面版走主进程 IPC（不依赖网页权限），网页版退回浏览器 API       */
/* ------------------------------------------------------------------ */

/** 读不到返回 null，和「剪贴板本来就是空的」区分开。 */
export async function readClipboardText() {
  const kw = window.keywarden;
  if (kw && typeof kw.readClipboard === 'function') {
    try {
      return (await kw.readClipboard()) || '';
    } catch {
      /* 落到下面的网页通道 */
    }
  }
  try {
    return (await navigator.clipboard.readText()) || '';
  } catch {
    return null;
  }
}

export async function writeClipboardText(text) {
  const kw = window.keywarden;
  if (kw && typeof kw.writeClipboard === 'function') {
    try {
      await kw.writeClipboard(text);
      return true;
    } catch {
      /* 落到下面的网页通道 */
    }
  }
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** 由公钥路径反推配对私钥路径：只认 `.pub` 后缀，一次改完到处生效 */
export function stripPubSuffix(p) {
  return String(p == null ? '' : p).replace(/\.pub$/i, '');
}

/* ------------------------------------------------------------------ */
/* 弹窗                                                                */
/* ------------------------------------------------------------------ */

let modalStack = [];
const closeHooks = new WeakMap();

export function openModal({ title, bodyHtml, footHtml, onMount, onClose, width }) {
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
  if (onClose) closeHooks.set(mask, onClose);
  $('modalRoot').appendChild(mask);
  modalStack.push(mask);
  if (onMount) onMount(mask);
  return mask;
}

export function closeModal(mask) {
  if (!mask) mask = modalStack[modalStack.length - 1];
  if (!mask) return;
  modalStack = modalStack.filter((m) => m !== mask);
  const onClose = closeHooks.get(mask);
  closeHooks.delete(mask);
  if (onClose) {
    try {
      onClose();
    } catch {}
  }
  mask.remove();
}

/**
 * 替代原生 confirm()。
 * 原生弹窗在无边框窗口里是一块系统模样的方框，和主题完全割裂，还会同步阻塞渲染线程
 * （终端正在大量输出时整个界面会卡住）；这里复用 openModal，返回 Promise<boolean>。
 *
 *   if (!(await confirmModal({ title: '删除密钥', message, danger: true }))) return;
 */
export function confirmModal({
  title = '确认操作',
  message = '',
  danger = false,
  okText = '确定',
  cancelText = '取消',
} = {}) {
  return new Promise((resolve) => {
    let settled = false;
    const done = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    openModal({
      title,
      width: '520px',
      bodyHtml: `<div class="confirm-msg">${escapeHtml(message)}</div>`,
      footHtml:
        `<button type="button" data-confirm-cancel>${escapeHtml(cancelText)}</button>` +
        `<button type="button" class="${danger ? 'danger' : 'primary'}" data-confirm-ok>${escapeHtml(okText)}</button>`,
      onMount: (m) => {
        m.querySelector('[data-confirm-ok]').addEventListener('click', () => {
          done(true);
          closeModal(m);
        });
        m.querySelector('[data-confirm-cancel]').addEventListener('click', () => {
          done(false);
          closeModal(m);
        });
        m.querySelector('[data-confirm-ok]').focus();
      },
      /* Esc、右上角 ×、点遮罩都会走到这里 —— 一律按取消处理 */
      onClose: () => done(false),
    });
  });
}

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeModal();
});
