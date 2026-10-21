'use strict';
/**
 * 公钥解析 / 指纹 / 本地密钥对生成
 *
 * 说明：ssh2 的 parseKey 不接受 PKCS#8 形式的 ed25519 私钥（实测报
 * "Unsupported key format"），所以 ed25519 私钥按 OpenSSH 容器格式
 * (openssh-key-v1, 无密码) 自行拼装 —— 已用 parseKey 回读验证公钥一致。
 */

const crypto = require('crypto');
const { utils } = require('ssh2');
const bcryptPbkdf = require('bcrypt-pbkdf');

const KEY_TYPE_RE =
  /^(ssh-ed25519|ssh-rsa|ssh-dss|ecdsa-sha2-nistp(?:256|384|521)|sk-ssh-ed25519@openssh\.com|sk-ecdsa-sha2-nistp256@openssh\.com)$/;

function u32(n) {
  const b = Buffer.allocUnsafe(4);
  b.writeUInt32BE(n >>> 0, 0);
  return b;
}

function sshStr(v) {
  const b = Buffer.isBuffer(v) ? v : Buffer.from(String(v), 'utf8');
  return Buffer.concat([u32(b.length), b]);
}

/**
 * RFC4251 mpint 编码：去掉多余前导零；若最高位为 1 则补一个 0x00。
 * RSA 的 n 常常最高位是 1，直接用 sshStr 会与 ssh2 算出的公钥 blob 不一致。
 */
function mpint(raw) {
  let b = Buffer.isBuffer(raw) ? raw : Buffer.from(raw);
  let i = 0;
  while (i < b.length - 1 && b[i] === 0) i++;
  b = b.subarray(i);
  if (b[0] & 0x80) b = Buffer.concat([Buffer.from([0]), b]);
  return sshStr(b);
}

function fingerprintOfBlob(blob) {
  return 'SHA256:' + crypto.createHash('sha256').update(blob).digest('base64').replace(/=+$/, '');
}

/** 一行公钥 → 结构化条目；无法识别返回 null */
function parsePublicKeyLine(line) {
  const tokens = String(line || '').trim().split(/\s+/).filter(Boolean);
  if (!tokens.length) return null;

  let idx = -1;
  for (let i = 0; i < tokens.length; i++) {
    if (KEY_TYPE_RE.test(tokens[i])) {
      idx = i;
      break;
    }
  }
  if (idx < 0 || idx + 1 >= tokens.length) return null;

  const type = tokens[idx];
  const b64 = tokens[idx + 1];
  const options = idx > 0 ? tokens.slice(0, idx).join(' ') : '';
  const comment = tokens.slice(idx + 2).join(' ');
  const blob = Buffer.from(b64, 'base64');
  if (!blob.length) return null;

  let realType = type;
  let fingerprint = fingerprintOfBlob(blob);
  let valid = true;
  let error = '';
  try {
    const k = utils.parseKey(`${type} ${b64}`);
    const key = Array.isArray(k) ? k[0] : k;
    if (key instanceof Error) {
      valid = false;
      error = key.message;
    } else {
      realType = key.type || type;
      fingerprint = fingerprintOfBlob(key.getPublicSSH());
    }
  } catch (e) {
    valid = false;
    error = e.message;
  }

  return { type: realType, b64, options, comment, fingerprint, valid, error };
}

/** 多行公钥文本 → 条目数组（跳过空行与注释行） */
function parsePublicKeys(text) {
  const out = [];
  for (const raw of String(text || '').split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const p = parsePublicKeyLine(line);
    if (p) out.push(p);
  }
  return out;
}

function normalizeComment(c) {
  return String(c || '')
    .replace(/[\r\n]+/g, ' ')
    .replace(/\s+/g, '_')
    .trim()
    .slice(0, 80);
}

/**
 * 生成 ed25519 密钥对，返回 OpenSSH 文本私钥 + 公钥行
 */
/** 私钥块补齐到指定块大小（加密时按 cipher 的 16 字节，明文按 8 字节） */
function padBlock(block, blockSize) {
  const padLen = (blockSize - (block.length % blockSize)) % blockSize;
  if (!padLen) return block;
  return Buffer.concat([block, Buffer.from(Array.from({ length: padLen }, (_, i) => i + 1))]);
}

/**
 * 组装 openssh-key-v1 容器。
 * 给了 passphrase 就用 aes256-ctr + bcrypt KDF 加密私钥块 —— 与 `ssh-keygen -N <pass>` 的做法一致。
 */
function buildOpenSSHContainer(pubBlob, blockPlain, passphrase) {
  const head = Buffer.from('openssh-key-v1\0', 'utf8');

  if (!passphrase) {
    return Buffer.concat([
      head,
      sshStr('none'),
      sshStr('none'),
      sshStr(Buffer.alloc(0)),
      u32(1),
      sshStr(pubBlob),
      sshStr(blockPlain),
    ]);
  }

  const rounds = 16;
  const salt = crypto.randomBytes(16);
  const keyiv = Buffer.alloc(32 + 16); // 32 字节 AES key + 16 字节 IV
  const passBuf = Buffer.from(passphrase, 'utf8');
  bcryptPbkdf.pbkdf(passBuf, passBuf.length, salt, salt.length, keyiv, keyiv.length, rounds);

  const cipher = crypto.createCipheriv('aes-256-ctr', keyiv.subarray(0, 32), keyiv.subarray(32, 48));
  const enc = Buffer.concat([cipher.update(blockPlain), cipher.final()]);

  return Buffer.concat([
    head,
    sshStr('aes256-ctr'),
    sshStr('bcrypt'),
    sshStr(Buffer.concat([sshStr(salt), u32(rounds)])),
    u32(1),
    sshStr(pubBlob),
    sshStr(enc),
  ]);
}

function armorPrivateKey(container) {
  const b64 = container.toString('base64').match(/.{1,70}/g).join('\n');
  return `-----BEGIN OPENSSH PRIVATE KEY-----\n${b64}\n-----END OPENSSH PRIVATE KEY-----\n`;
}

function generateEd25519(comment, passphrase) {
  const c = normalizeComment(comment) || 'ssh-keywarden';
  const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
  const pubRaw = Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url');
  const privRaw = Buffer.from(privateKey.export({ format: 'jwk' }).d, 'base64url');

  const typeBuf = Buffer.from('ssh-ed25519');
  const pubBlob = Buffer.concat([sshStr(typeBuf), sshStr(pubRaw)]);

  const check = crypto.randomBytes(4);
  const rawBlock = Buffer.concat([
    check,
    check,
    sshStr(typeBuf),
    sshStr(pubRaw),
    sshStr(Buffer.concat([privRaw, pubRaw])),
    sshStr(Buffer.from(c, 'utf8')),
  ]);
  const block = padBlock(rawBlock, passphrase ? 16 : 8);

  const privateKeyPem = armorPrivateKey(buildOpenSSHContainer(pubBlob, block, passphrase));
  const publicKeyLine = `ssh-ed25519 ${pubBlob.toString('base64')} ${c}`;

  assertRoundTrip(privateKeyPem, pubBlob, passphrase);
  return {
    type: 'ssh-ed25519',
    privateKeyPem,
    publicKeyLine,
    fingerprint: fingerprintOfBlob(pubBlob),
    comment: c,
    encrypted: Boolean(passphrase),
  };
}

/**
 * 生成 RSA 密钥对。不给 passphrase 时导出 PKCS#1 PEM（ssh2 原生支持）；
 * 给了 passphrase 就改用 OpenSSH 容器格式加密（PEM 的传统加密方式 ssh2 不认）。
 */
function generateRsa(comment, bits = 3072, passphrase) {
  const c = normalizeComment(comment) || 'ssh-keywarden';
  const { publicKey, privateKey } = crypto.generateKeyPairSync('rsa', { modulusLength: bits });
  const pubJwk = publicKey.export({ format: 'jwk' });
  const privJwk = privateKey.export({ format: 'jwk' });

  const e = Buffer.from(pubJwk.e, 'base64url');
  const n = Buffer.from(pubJwk.n, 'base64url');
  const pubBlob = Buffer.concat([sshStr('ssh-rsa'), mpint(e), mpint(n)]);

  let privateKeyPem;
  if (passphrase) {
    // OpenSSH 的 RSA 私钥块字段顺序：n, e, d, iqmp, p, q
    const check = crypto.randomBytes(4);
    const rawBlock = Buffer.concat([
      check,
      check,
      sshStr('ssh-rsa'),
      mpint(n),
      mpint(e),
      mpint(Buffer.from(privJwk.d, 'base64url')),
      mpint(Buffer.from(privJwk.qi, 'base64url')),
      mpint(Buffer.from(privJwk.p, 'base64url')),
      mpint(Buffer.from(privJwk.q, 'base64url')),
      sshStr(Buffer.from(c, 'utf8')),
    ]);
    const block = padBlock(rawBlock, 16);
    privateKeyPem = armorPrivateKey(buildOpenSSHContainer(pubBlob, block, passphrase));
  } else {
    privateKeyPem = privateKey.export({ type: 'pkcs1', format: 'pem' });
  }

  const publicKeyLine = `ssh-rsa ${pubBlob.toString('base64')} ${c}`;

  assertRoundTrip(privateKeyPem, pubBlob, passphrase);
  return {
    type: 'ssh-rsa',
    privateKeyPem,
    publicKeyLine,
    fingerprint: fingerprintOfBlob(pubBlob),
    comment: c,
    encrypted: Boolean(passphrase),
  };
}

/** 生成后立刻校验：私钥能被 ssh2 解析，且公钥与预期 blob 完全一致 */
function assertRoundTrip(privateKeyPem, expectedBlob, passphrase) {
  const k = utils.parseKey(privateKeyPem, passphrase || undefined);
  const key = Array.isArray(k) ? k[0] : k;
  if (key instanceof Error) {
    throw new Error(`生成的私钥无法被解析：${key.message}`);
  }
  if (Buffer.compare(key.getPublicSSH(), expectedBlob) !== 0) {
    throw new Error('生成的私钥与公钥不匹配（内部错误）');
  }
  // 设了密码就得真的挡住：不给 passphrase 必须解析失败
  if (passphrase) {
    const plain = utils.parseKey(privateKeyPem);
    const pk = Array.isArray(plain) ? plain[0] : plain;
    if (!(pk instanceof Error)) {
      throw new Error('加密私钥竟然能在无密码时解析（内部错误）');
    }
  }
}

/** 读取私钥文本，返回 { encrypted, type } */
function inspectPrivateKey(text) {
  const k = utils.parseKey(text);
  const key = Array.isArray(k) ? k[0] : k;
  if (key instanceof Error) {
    const encrypted = /passphrase|Encrypted/i.test(key.message);
    return { encrypted, type: '', error: key.message };
  }
  return { encrypted: false, type: key.type || '' };
}

module.exports = {
  KEY_TYPE_RE,
  fingerprintOfBlob,
  parsePublicKeyLine,
  parsePublicKeys,
  normalizeComment,
  generateEd25519,
  generateRsa,
  inspectPrivateKey,
  sshStr,
  u32,
};
