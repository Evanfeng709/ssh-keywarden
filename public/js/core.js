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

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeModal();
});
