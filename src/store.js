'use strict';
/**
 * 配置持久化：主机列表 + 全局设置。
 * 只存「怎么连」（地址/端口/用户名/私钥路径），**绝不存密码**。
 */

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const DATA_DIR = process.env.KW_DATA_DIR || path.join(__dirname, '..', 'data');
const CONFIG_PATH = path.join(DATA_DIR, 'config.json');
const NOTES_PATH = path.join(DATA_DIR, 'key-notes.json');
const NOTE_MAX = 200;
const FP_RE = /^SHA256:[A-Za-z0-9+/]{43}$/;

const DEFAULT_CONFIG = {
  settings: {
    localKeyDir: '',
    saveDir: '',
    remotePath: '', // 留空 = ~/.ssh/authorized_keys
  },
  hosts: [],
};

function ensureDir() {
  if (!fs.existsSync(DATA_DIR)) fs.mkdirSync(DATA_DIR, { recursive: true });
}

function read() {
  ensureDir();
  if (!fs.existsSync(CONFIG_PATH)) return JSON.parse(JSON.stringify(DEFAULT_CONFIG));
  try {
    const raw = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8'));
    return {
      settings: { ...DEFAULT_CONFIG.settings, ...(raw.settings || {}) },
      hosts: Array.isArray(raw.hosts) ? raw.hosts : [],
    };
  } catch (e) {
    // 配置坏了不能静默吞掉：备份坏文件，再用默认值继续
    const bad = `${CONFIG_PATH}.bad-${Date.now()}`;
    try {
      fs.copyFileSync(CONFIG_PATH, bad);
    } catch {}
    console.error(`[config] 配置文件解析失败（${e.message}），已备份到 ${bad}，使用默认配置`);
    return JSON.parse(JSON.stringify(DEFAULT_CONFIG));
  }
}

function writeJsonAtomic(file, value) {
  ensureDir();
  const tmp = `${file}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2), { encoding: 'utf8', mode: 0o600 });
  fs.renameSync(tmp, file); // 原子替换
  return value;
}

function write(cfg) {
  return writeJsonAtomic(CONFIG_PATH, cfg);
}

function sanitizeNote(note) {
  return String(note == null ? '' : note)
    .replace(/[\r\n\t]+/g, ' ')
    .replace(/[\u0000-\u001f\u007f]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

/** 读取本地备注；文件损坏时备份后按空表继续，不能拖垮整个配置 */
function readKeyNotes() {
  ensureDir();
  if (!fs.existsSync(NOTES_PATH)) return {};
  try {
    const raw = JSON.parse(fs.readFileSync(NOTES_PATH, 'utf8'));
    const src = raw && typeof raw === 'object' && raw.notes && typeof raw.notes === 'object' ? raw.notes : {};
    const out = {};
    for (const [fingerprint, entry] of Object.entries(src)) {
      if (!FP_RE.test(fingerprint) || !entry || typeof entry.note !== 'string') continue;
      const note = sanitizeNote(entry.note);
      if (!note) continue;
      out[fingerprint] = { note, updatedAt: Number(entry.updatedAt) || 0 };
    }
    return out;
  } catch (e) {
    const bad = `${NOTES_PATH}.bad-${Date.now()}`;
    try {
      fs.copyFileSync(NOTES_PATH, bad);
    } catch {}
    console.error(`[notes] 备注文件解析失败（${e.message}），已备份到 ${bad}，按空备注继续`);
    return {};
  }
}

/** 保存/删除一条备注；空备注 = 删除。返回完整备注表 */
function setKeyNote(fingerprint, note) {
  const fp = String(fingerprint || '').trim();
  if (!FP_RE.test(fp)) throw new Error('指纹格式不正确，备注未保存');
  const text = sanitizeNote(note);
  if (text.length > NOTE_MAX) throw new Error(`备注最多 ${NOTE_MAX} 个字符`);

  const notes = readKeyNotes();
  if (!text) delete notes[fp];
  else notes[fp] = { note: text, updatedAt: Date.now() };
  writeJsonAtomic(NOTES_PATH, { version: 1, notes });
  return notes;
}

function listHosts() {
  return read().hosts;
}

function upsertHost(input) {
  const cfg = read();
  const host = {
    id: input.id || crypto.randomUUID(),
    name: String(input.name || input.host || '未命名').trim(),
    host: String(input.host || '').trim(),
    port: Number(input.port) || 22,
    username: String(input.username || '').trim(),
    identityFile: String(input.identityFile || '').trim(),
    remotePath: String(input.remotePath || '').trim(),
    updatedAt: Date.now(),
  };
  if (!host.host) throw new Error('主机地址不能为空');
  if (!host.username) throw new Error('用户名不能为空');

  const idx = cfg.hosts.findIndex((h) => h.id === host.id);
  if (idx >= 0) {
    host.createdAt = cfg.hosts[idx].createdAt || Date.now();
    cfg.hosts[idx] = host;
  } else {
    host.createdAt = Date.now();
    cfg.hosts.push(host);
  }
  write(cfg);
  return host;
}

function removeHost(id) {
  const cfg = read();
  const before = cfg.hosts.length;
  cfg.hosts = cfg.hosts.filter((h) => h.id !== id);
  if (cfg.hosts.length === before) throw new Error('主机不存在');
  write(cfg);
  return true;
}

function getSettings() {
  return read().settings;
}

function updateSettings(patch) {
  const cfg = read();
  cfg.settings = { ...cfg.settings, ...patch };
  write(cfg);
  return cfg.settings;
}

module.exports = {
  listHosts,
  upsertHost,
  removeHost,
  getSettings,
  updateSettings,
  listKeyNotes: readKeyNotes,
  setKeyNote,
  DATA_DIR,
  CONFIG_PATH,
  NOTES_PATH,
  NOTE_MAX,
};
