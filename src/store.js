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

function write(cfg) {
  ensureDir();
  const tmp = `${CONFIG_PATH}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(cfg, null, 2), 'utf8');
  fs.renameSync(tmp, CONFIG_PATH); // 原子替换
  return cfg;
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

module.exports = { listHosts, upsertHost, removeHost, getSettings, updateSettings, DATA_DIR, CONFIG_PATH };
