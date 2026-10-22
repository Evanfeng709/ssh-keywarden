'use strict';
/**
 * authorized_keys 备份：列表、差异、恢复路径校验。
 *
 * 只认同目录下的 <remotePath>.bak.YYYYMMDDHHMMSS。
 * 用户传的名字永远只当前缀校验用，实际读取路径由 remotePath 重新拼出来，
 * 避免把接口变成任意远端文件读取器。
 */

const path = require('path');
const ssh = require('./ssh');
const { parseAuthorizedKeys, keyItems } = require('./authorized');

const STAMP_RE = /^\d{14}$/;

function displayTime(stamp) {
  return `${stamp.slice(0, 4)}-${stamp.slice(4, 6)}-${stamp.slice(6, 8)} ${stamp.slice(
    8,
    10
  )}:${stamp.slice(10, 12)}:${stamp.slice(12, 14)}`;
}

/** 校验「basename.bak.14位时间戳」，合法返回展示信息，非法返回 null */
function parseBackupName(remotePath, name) {
  const base = path.posix.basename(String(remotePath || ''));
  if (!base || typeof name !== 'string' || !name) return null;
  if (/[\\/]/.test(name) || name !== path.posix.basename(name)) return null;
  const prefix = `${base}.bak.`;
  if (!name.startsWith(prefix)) return null;
  const stamp = name.slice(prefix.length);
  if (!STAMP_RE.test(stamp)) return null;
  return { name, stamp, displayTime: displayTime(stamp) };
}

/** 只根据 remotePath + 合法名字拼路径；不接受任何完整路径参数 */
function backupPathFor(remotePath, name) {
  const parsed = parseBackupName(remotePath, name);
  if (!parsed) throw new Error('备份文件名不合法');
  ssh.assertSafePath(remotePath);
  return `${remotePath}.bak.${parsed.stamp}`;
}

/** 列出备份文件名。远端只跑 sh 内建 for/printf，不依赖 ls/find/SFTP */
async function listBackups(client, remotePath) {
  ssh.assertSafePath(remotePath);
  const command = `for f in ${ssh.quote(remotePath)}.bak.*; do printf '%s\\n' "$f"; done`;
  const res = await ssh.exec(client, command);
  if (res.code !== 0) {
    throw new Error(`读取备份列表失败：${res.stderr.trim() || `退出码 ${res.code}`}`);
  }

  const base = path.posix.basename(remotePath);
  const prefix = `${remotePath}.bak.`;
  const seen = new Set();
  const out = [];
  for (const raw of res.stdout.split(/\r?\n/)) {
    const full = raw.replace(/\r$/, '');
    if (!full.startsWith(prefix)) continue;
    const name = `${base}.bak.${full.slice(prefix.length)}`;
    const parsed = parseBackupName(remotePath, name);
    if (!parsed || seen.has(name)) continue;
    seen.add(name);
    out.push(parsed);
  }
  out.sort((a, b) => b.stamp.localeCompare(a.stamp));
  return out;
}

function keyMap(text) {
  const map = new Map();
  const parsed = parseAuthorizedKeys(text || '');
  for (const item of keyItems(parsed.items)) {
    if (!map.has(item.fingerprint)) {
      map.set(item.fingerprint, {
        fingerprint: item.fingerprint,
        type: item.type || '',
        comment: item.comment || '',
        options: item.options || '',
      });
    }
  }
  return { map, parsed };
}

/**
 * added = 备份里有、当前没有（恢复后会新增）
 * removed = 当前有、备份没有（恢复后会移除）
 */
function diffBackup(currentText, backupText, usedKeyFp, currentFileExists = true) {
  const cur = keyMap(currentText);
  const bak = keyMap(backupText);
  const sortKeys = (arr) =>
    arr.sort((a, b) => (a.comment || a.fingerprint).localeCompare(b.comment || b.fingerprint));

  return {
    backupKeyCount: bak.map.size,
    currentKeyCount: cur.map.size,
    sameCount: [...bak.map.keys()].filter((fp) => cur.map.has(fp)).length,
    added: sortKeys([...bak.map.values()].filter((k) => !cur.map.has(k.fingerprint))),
    removed: sortKeys([...cur.map.values()].filter((k) => !bak.map.has(k.fingerprint))),
    backupMalformed: bak.parsed.items.filter((i) => i.kind === 'malformed').length,
    currentMalformed: cur.parsed.items.filter((i) => i.kind === 'malformed').length,
    emptyBackup: bak.map.size === 0,
    usedKeyInBackup: usedKeyFp ? bak.map.has(usedKeyFp) : null,
    currentFileExists,
  };
}

module.exports = {
  parseBackupName,
  backupPathFor,
  listBackups,
  diffBackup,
  displayTime,
};
