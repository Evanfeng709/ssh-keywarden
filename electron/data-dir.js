'use strict';
/**
 * 桌面版数据目录：解析、创建、从旧位置迁移。
 * 不依赖 electron，方便单测。
 */

const fs = require('fs');
const path = require('path');

const DATA_FILES = ['config.json', 'known_hosts.json', 'key-notes.json'];

function resolveDataDir({ userData, env = process.env }) {
  return env.KW_DATA_DIR ? path.resolve(env.KW_DATA_DIR) : path.join(userData, 'data');
}

function ensureDataDir(dir) {
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

/**
 * 目标目录还没有 config.json 时，从候选目录复制已有的三个数据文件。
 * 候选目录按顺序尝试，第一个含有 config.json 的生效。
 * @returns {{migrated:boolean, from:string, copied:string[]}}
 */
function migrateData(targetDir, candidates = []) {
  if (fs.existsSync(path.join(targetDir, 'config.json'))) {
    return { migrated: false, from: '', copied: [] };
  }
  const seen = new Set();
  for (const candidate of candidates) {
    if (!candidate) continue;
    const src = path.resolve(candidate);
    if (seen.has(src) || src === path.resolve(targetDir)) continue;
    seen.add(src);
    if (!fs.existsSync(path.join(src, 'config.json'))) continue;

    ensureDataDir(targetDir);
    const copied = [];
    for (const name of DATA_FILES) {
      const from = path.join(src, name);
      if (!fs.existsSync(from)) continue;
      const to = path.join(targetDir, name);
      fs.copyFileSync(from, to);
      try {
        fs.chmodSync(to, 0o600);
      } catch {
        /* Windows 上 chmod 有限，忽略 */
      }
      copied.push(name);
    }
    return { migrated: copied.length > 0, from: src, copied };
  }
  return { migrated: false, from: '', copied: [] };
}

module.exports = { resolveDataDir, ensureDataDir, migrateData, DATA_FILES };
