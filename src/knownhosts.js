'use strict';
/**
 * 主机指纹（known_hosts 式 TOFU）。
 * 首次连接由用户确认指纹后写入；之后密钥变化必须再次确认，防止中间人冒充。
 */

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { DATA_DIR } = require('./store');

const FILE = path.join(DATA_DIR, 'known_hosts.json');

function hostId(host, port) {
  return `${String(host || '').trim().toLowerCase()}:${Number(port) || 22}`;
}

function fingerprintOfBlob(blob) {
  if (!Buffer.isBuffer(blob)) blob = Buffer.from(blob);
  return `SHA256:${crypto.createHash('sha256').update(blob).digest('base64').replace(/=+$/, '')}`;
}

function keyTypeOfBlob(blob) {
  if (!Buffer.isBuffer(blob) || blob.length < 8) return '';
  const len = blob.readUInt32BE(0);
  if (len < 3 || len > 64 || blob.length < 4 + len) return '';
  const type = blob.subarray(4, 4 + len).toString('ascii');
  return /^[a-z0-9@.-]+$/i.test(type) ? type : '';
}

function readAll() {
  try {
    const raw = JSON.parse(fs.readFileSync(FILE, 'utf8'));
    return raw && typeof raw === 'object' && !Array.isArray(raw) ? raw : {};
  } catch {
    return {};
  }
}

function writeAll(all) {
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const tmp = `${FILE}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(all, null, 2), { encoding: 'utf8', mode: 0o600 });
  // 和 src/store.js 的 writeJsonAtomic 对齐：mode 只在新建时生效，
  // 覆盖上次崩溃残留的 .tmp 时要再补一刀
  try {
    fs.chmodSync(tmp, 0o600);
  } catch {}
  fs.renameSync(tmp, FILE);
}

function get(host, port) {
  return readAll()[hostId(host, port)] || null;
}

function list() {
  return Object.values(readAll());
}

function describe(host, port, blob) {
  return {
    host: String(host || '').trim(),
    port: Number(port) || 22,
    keyType: keyTypeOfBlob(blob),
    fingerprint: fingerprintOfBlob(blob),
  };
}

function save(host, port, blob) {
  const all = readAll();
  const id = hostId(host, port);
  const prev = all[id];
  const entry = {
    ...describe(host, port, blob),
    publicKey: Buffer.from(blob).toString('base64'),
    addedAt: prev ? prev.addedAt : Date.now(),
    updatedAt: Date.now(),
  };
  all[id] = entry;
  writeAll(all);
  return entry;
}

class HostKeyError extends Error {
  constructor(code, details) {
    super(
      code === 'HOSTKEY_CHANGED'
        ? `主机密钥已变化：${details.host}:${details.port} 返回的指纹与本地记录不一致。若非你主动重装或更换过服务器，请取消连接并检查网络`
        : `首次连接 ${details.host}:${details.port}，请核对主机指纹后确认`
    );
    this.name = 'HostKeyError';
    this.code = code;
    this.details = details;
  }
}

/**
 * 校验远端主机密钥。
 * @param {{host:string, port?:number, blob:Buffer, trustFingerprint?:string}} input
 * @returns {{status:'match'|'trusted'|'updated', current:object, previous?:object}}
 */
function verify({ host, port, blob, trustFingerprint }) {
  const current = describe(host, port, blob);
  const entry = get(host, port);
  const confirmed = typeof trustFingerprint === 'string' && trustFingerprint === current.fingerprint;

  if (!entry) {
    if (confirmed) return { status: 'trusted', current: save(host, port, blob) };
    throw new HostKeyError('HOSTKEY_UNKNOWN', current);
  }
  if (entry.fingerprint === current.fingerprint) return { status: 'match', current, previous: entry };
  if (confirmed) {
    save(host, port, blob);
    return { status: 'updated', current, previous: entry };
  }
  throw new HostKeyError('HOSTKEY_CHANGED', {
    ...current,
    previousFingerprint: entry.fingerprint,
    previousUpdatedAt: entry.updatedAt,
  });
}

module.exports = { FILE, fingerprintOfBlob, keyTypeOfBlob, get, list, save, verify, HostKeyError };
