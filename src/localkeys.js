'use strict';
/**
 * 本地密钥库扫描：把目录里的私钥/公钥读出来，供 UI 展示与「从本地选择」使用。
 */

const fs = require('fs');
const path = require('path');
const { parsePublicKeyLine, fingerprintOfBlob, inspectPrivateKey } = require('./keygen');
const { utils } = require('ssh2');

const MAX_FILE_SIZE = 128 * 1024;
const SKIP_DIRS = new Set(['node_modules', '.git', '$RECYCLE.BIN', 'System Volume Information']);

function looksLikePublic(text) {
  const line = text.split(/\r?\n/).find((l) => l.trim() && !l.trim().startsWith('#'));
  if (!line) return false;
  return parsePublicKeyLine(line.trim()) !== null;
}

/**
 * 扫描目录（递归，限深度），返回 { entries, errors }
 * entry: { path, name, dir, kind:'private'|'public', type, fingerprint, comment, encrypted, size, mtime }
 */
function scanKeys(rootDir, options = {}) {
  const maxDepth = options.maxDepth == null ? 3 : options.maxDepth;
  const entries = [];
  const errors = [];

  if (!rootDir || !fs.existsSync(rootDir)) {
    return { entries, errors: [{ path: rootDir || '', message: '目录不存在' }] };
  }

  const walk = (dir, depth) => {
    let list;
    try {
      list = fs.readdirSync(dir, { withFileTypes: true });
    } catch (e) {
      errors.push({ path: dir, message: e.message });
      return;
    }
    for (const item of list) {
      const full = path.join(dir, item.name);
      if (item.isDirectory()) {
        if (depth < maxDepth && !SKIP_DIRS.has(item.name)) walk(full, depth + 1);
        continue;
      }
      if (!item.isFile()) continue;
      let stat;
      try {
        stat = fs.statSync(full);
      } catch (e) {
        errors.push({ path: full, message: e.message });
        continue;
      }
      if (stat.size === 0 || stat.size > MAX_FILE_SIZE) continue;

      let text;
      try {
        text = fs.readFileSync(full, 'utf8');
      } catch (e) {
        errors.push({ path: full, message: e.message });
        continue;
      }

      const isPrivate = /-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(text);
      const isPublicFile = !isPrivate && looksLikePublic(text);

      if (isPrivate) {
        const info = inspectPrivateKey(text);
        entries.push({
          path: full,
          name: item.name,
          dir,
          kind: 'private',
          type: info.type || '',
          fingerprint: matchPublicSibling(full, text),
          encrypted: info.encrypted,
          error: info.encrypted ? '' : info.error || '',
          size: stat.size,
          mtime: stat.mtimeMs,
        });
      } else if (isPublicFile) {
        const line = text
          .split(/\r?\n/)
          .map((l) => l.trim())
          .find((l) => l && !l.startsWith('#'));
        const parsed = line ? parsePublicKeyLine(line) : null;
        if (!parsed) continue;
        entries.push({
          path: full,
          name: item.name,
          dir,
          kind: 'public',
          type: parsed.type,
          fingerprint: parsed.fingerprint,
          comment: parsed.comment,
          valid: parsed.valid,
          publicKeyLine: `${parsed.type} ${parsed.b64}${parsed.comment ? ' ' + parsed.comment : ''}`,
          size: stat.size,
          mtime: stat.mtimeMs,
        });
      }
    }
  };

  walk(rootDir, 0);
  return { entries, errors };
}

/**
 * 私钥本身没有「指纹」字段，靠同目录同名 .pub 反推。
 * 拿不到就返回空字符串（UI 显示「未知」），不臆造。
 */
function matchPublicSibling(privPath, privText) {
  const candidates = [`${privPath}.pub`];
  const base = privPath.replace(/\.(rsa|ed25519|ecdsa|dsa)$/i, '');
  if (base !== privPath) candidates.push(`${base}.pub`);

  for (const pubPath of candidates) {
    try {
      const pubText = fs.readFileSync(pubPath, 'utf8');
      const line = pubText.split(/\r?\n/).map((l) => l.trim()).find((l) => l && !l.startsWith('#'));
      const parsed = line ? parsePublicKeyLine(line) : null;
      if (parsed) return parsed.fingerprint;
    } catch {
      /* 没有 .pub 就跳过 */
    }
  }

  // 退一步：从私钥自己算（未加密时可用）
  try {
    const k = utils.parseKey(privText);
    const key = Array.isArray(k) ? k[0] : k;
    if (!(key instanceof Error)) return fingerprintOfBlob(key.getPublicSSH());
  } catch {
    /* ignore */
  }
  return '';
}

module.exports = { scanKeys };
