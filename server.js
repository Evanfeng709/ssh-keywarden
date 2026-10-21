'use strict';
/**
 * SSH Keywarden —— 本地 SSH 授权密钥管理台
 *
 * 安全边界：
 *   1. 只监听 127.0.0.1，且严格校验 Host 头（防 DNS rebinding）
 *   2. 会话令牌走 SameSite=Strict Cookie（防浏览器跨站请求伪造）
 *   3. 密码只存在内存里，不落盘；配置只存「怎么连」
 */

const http = require('http');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const os = require('os');

const store = require('./src/store');
const keygen = require('./src/keygen');
const sshconfig = require('./src/sshconfig');
const sshd = require('./src/sshd');
const { APP_VERSION } = require('./src/version');
const { parseAuthorizedKeys, keyItems, renderWithout, appendKey } = require('./src/authorized');
const { scanKeys } = require('./src/localkeys');
const ssh = require('./src/ssh');

const PORT = Number(process.env.PORT) || 7791;
const BIND = '127.0.0.1';
const TOKEN = crypto.randomBytes(18).toString('hex');
const SESSION_IDLE_MS = 30 * 60 * 1000;
const PUBLIC_DIR = path.join(__dirname, 'public');

/** 内存会话：sid -> {...} */
const sessions = new Map();

setInterval(() => {
  const now = Date.now();
  for (const [sid, s] of sessions) {
    if (now - s.lastUsed > SESSION_IDLE_MS) {
      try {
        s.client.end();
      } catch {}
      sessions.delete(sid);
    }
  }
}, 60_000).unref();

/* ------------------------------------------------------------------ */
/* 工具                                                                */
/* ------------------------------------------------------------------ */

function log(...args) {
  const ts = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  console.log(`[${ts}]`, ...args);
}

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Cache-Control': 'no-store',
    'X-Content-Type-Options': 'nosniff',
  });
  res.end(body);
}

function ok(res, data) {
  sendJson(res, 200, { ok: true, data });
}

function fail(res, message, status = 400, extra = {}) {
  sendJson(res, status, { ok: false, error: String(message || '未知错误'), ...extra });
}

function readJson(req) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on('data', (c) => {
      size += c.length;
      if (size > 2 * 1024 * 1024) {
        reject(new Error('请求体过大'));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on('end', () => {
      const text = Buffer.concat(chunks).toString('utf8').trim();
      if (!text) return resolve({});
      try {
        resolve(JSON.parse(text));
      } catch {
        reject(new Error('请求体不是合法 JSON'));
      }
    });
    req.on('error', reject);
  });
}

function parseCookies(header) {
  const out = {};
  String(header || '')
    .split(';')
    .forEach((pair) => {
      const i = pair.indexOf('=');
      if (i > 0) out[pair.slice(0, i).trim()] = decodeURIComponent(pair.slice(i + 1).trim());
    });
  return out;
}

function expandPath(p, home) {
  if (!p) return `${home}/.ssh/authorized_keys`;
  const t = String(p).trim();
  if (t === '~') return home;
  if (t.startsWith('~/')) return `${home}/${t.slice(2)}`;
  if (t.startsWith('/')) return t;
  return `${home}/${t}`;
}

function stamp() {
  return new Date().toLocaleString('sv-SE', { timeZone: 'Asia/Shanghai' }).replace(/[-: ]/g, '').slice(0, 14);
}

/* ---------------- 本机文件浏览（给「浏览…」按钮用） ---------------- */

function expandLocalPath(p) {
  const t = String(p || '').trim();
  if (!t) return os.homedir();
  if (t === '~') return os.homedir();
  if (t.startsWith('~/') || t.startsWith('~\\')) return path.join(os.homedir(), t.slice(2));
  return path.resolve(t);
}

function listDrives() {
  if (process.platform !== 'win32') return ['/'];
  const out = [];
  for (let c = 67; c <= 90; c++) {
    const d = `${String.fromCharCode(c)}:\\`;
    try {
      if (fs.existsSync(d)) out.push(d);
    } catch {
      /* 空光驱会抛，跳过 */
    }
  }
  return out;
}

/** 判断一个普通文件像不像 SSH 密钥（只看头部，不整文件读） */
function sniffKeyFile(fullPath, size) {
  if (size <= 0 || size > 128 * 1024) return null;
  let head = '';
  try {
    const fd = fs.openSync(fullPath, 'r');
    const buf = Buffer.alloc(2048);
    const n = fs.readSync(fd, buf, 0, buf.length, 0);
    fs.closeSync(fd);
    head = buf.subarray(0, n).toString('utf8');
  } catch {
    return null;
  }
  if (/-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(head)) return 'private';
  if (/^(ssh-ed25519|ssh-rsa|ssh-dss|ecdsa-sha2-|sk-ssh-|sk-ecdsa-)/m.test(head)) return 'public';
  return null;
}

function listLocalDir(rawDir) {
  const dir = expandLocalPath(rawDir);

  let st;
  try {
    st = fs.statSync(dir);
  } catch {
    throw new Error(`目录不存在或无权访问：${dir}`);
  }
  if (!st.isDirectory()) throw new Error(`这不是目录：${dir}`);

  let dirents;
  try {
    dirents = fs.readdirSync(dir, { withFileTypes: true });
  } catch (e) {
    throw new Error(`无法读取目录：${e.message}`);
  }

  const entries = [];
  const skipped = [];

  for (const ent of dirents) {
    const full = path.join(dir, ent.name);
    let s;
    try {
      s = fs.statSync(full);
    } catch {
      skipped.push(ent.name);
      continue;
    }

    if (s.isDirectory()) {
      entries.push({ name: ent.name, path: full, type: 'dir' });
    } else if (s.isFile()) {
      const item = { name: ent.name, path: full, type: 'file', size: s.size, mtime: s.mtimeMs };
      const kind = sniffKeyFile(full, s.size);
      if (kind) item.keyKind = kind;
      entries.push(item);
    }
  }

  entries.sort((a, b) => {
    if (a.type !== b.type) return a.type === 'dir' ? -1 : 1;
    return a.name.localeCompare(b.name, 'zh-CN', { numeric: true });
  });

  const parent = path.dirname(dir);
  return {
    dir,
    parent: parent === dir ? null : parent,
    entries,
    skipped,
    home: os.homedir(),
    drives: listDrives(),
  };
}

function safeName(s) {
  return (
    String(s || '')
      // 只替换 Windows 文件名的非法字符，中文/字母/数字/点/横线都保留
      .replace(/[\\/:*?"<>|]+/g, '_')
      .replace(/\s+/g, '_')
      .replace(/^\.+/, '')
      .slice(0, 40) || 'key'
  );
}

/** 从 ~/.ssh/config 猜一个默认私钥目录（找不到就空着） */
function detectKeyDir() {
  try {
    const hosts = sshconfig.importFromSshConfig();
    const dirs = hosts
      .map((h) => h.identityFile)
      .filter(Boolean)
      .map((p) => path.dirname(p))
      .filter((d) => fs.existsSync(d));
    if (!dirs.length) return '';

    // 私钥目录常有多层（如 密钥/1o&通用/id_rsa），向上浮一两级取「密钥文件最多」的那层
    let best = dirs[0];
    let bestCount = countKeys(best);
    let cur = best;
    for (let i = 0; i < 2; i++) {
      const parent = path.dirname(cur);
      if (!parent || parent === cur || parent === os.homedir()) break;
      const c = countKeys(parent);
      if (c > bestCount) {
        best = parent;
        bestCount = c;
      }
      cur = parent;
    }
    return best;
  } catch {
    return '';
  }
}

function countKeys(dir) {
  try {
    return scanKeys(dir, { maxDepth: 1 }).entries.length;
  } catch {
    return 0;
  }
}

function currentSettings() {
  const s = store.getSettings();
  if (!s.localKeyDir) {
    const guess = detectKeyDir();
    if (guess) {
      try {
        return store.updateSettings({ localKeyDir: guess });
      } catch {
        return { ...s, localKeyDir: guess };
      }
    }
  }
  return s;
}

/* ------------------------------------------------------------------ */
/* 认证材料                                                            */
/* ------------------------------------------------------------------ */

function buildAuth(target) {
  const auth = {};
  if (target.identityFile) {
    if (!fs.existsSync(target.identityFile)) {
      throw new Error(`私钥文件不存在：${target.identityFile}`);
    }
    auth.privateKey = fs.readFileSync(target.identityFile);
    // 加密私钥必须把 passphrase 一起交给 ssh2，否则直接报
    // "Encrypted private OpenSSH key detected, but no passphrase given"
    if (target.passphrase) auth.passphrase = String(target.passphrase);
  }
  if (target.password) auth.password = String(target.password);
  if (!auth.privateKey && !auth.password) {
    throw new Error('没有可用凭据：请选择私钥文件，或填写登录密码');
  }
  return auth;
}

/** 私钥 → 公钥指纹（用于「禁止删掉当前连接用的钥匙」） */
function fingerprintOfPrivateKey(privateKey, passphrase) {
  try {
    const { utils } = require('ssh2');
    // 加密私钥必须带上 passphrase，否则解析失败 → 指纹为空 → 防误删保护形同虚设
    const k = utils.parseKey(privateKey, passphrase || undefined);
    const key = Array.isArray(k) ? k[0] : k;
    if (key instanceof Error) return '';
    return keygen.fingerprintOfBlob(key.getPublicSSH());
  } catch {
    return '';
  }
}

/* ------------------------------------------------------------------ */
/* 业务：连接 / 读 / 增 / 换 / 删                                       */
/* ------------------------------------------------------------------ */

async function openSession(body) {
  let target;
  if (body.hostId) {
    const h = store.listHosts().find((x) => x.id === body.hostId);
    if (!h) throw new Error('主机配置不存在，请刷新页面');
    target = { ...h, passphrase: body.passphrase, password: body.password };
  } else {
    target = {
      name: body.name || body.host,
      host: body.host,
      port: Number(body.port) || 22,
      username: body.username,
      identityFile: body.identityFile,
      remotePath: body.remotePath,
      passphrase: body.passphrase,
      password: body.password,
    };
  }
  if (!target.host) throw new Error('主机地址不能为空');
  if (!target.username) throw new Error('用户名不能为空');

  const auth = buildAuth(target);

  let authMethod = auth.privateKey ? 'key' : 'password';
  log(
    `连接 ${target.username}@${target.host}:${target.port || 22} （${
      auth.privateKey ? (auth.passphrase ? '私钥+passphrase' : '私钥') : '密码'
    }认证）`
  );

  const connCfg = {
    host: target.host,
    port: target.port || 22,
    username: target.username,
    ...auth,
  };

  let client;
  try {
    client = await ssh.connect(connCfg);
  } catch (e) {
    // 私钥解析不了（加密却没给/给错 passphrase）但有密码 → 用密码兜底重试一次
    const keyBroken = /passphrase|Encrypted private|Cannot parse privateKey/i.test(e.message);
    if (keyBroken && auth.privateKey && auth.password) {
      log('私钥不可用，改用密码认证重试');
      client = await ssh.connect({
        host: target.host,
        port: target.port || 22,
        username: target.username,
        password: auth.password,
      });
      authMethod = 'password';
    } else {
      throw e;
    }
  }

  try {
    let server = { uname: '', user: '', hostname: '' };
    try {
      server = await ssh.probeServer(client);
    } catch (e) {
      // 系统信息只是展示用：受限账号或响应慢的服务器拿不到也不影响密钥管理
      log(`系统信息探测失败（不影响使用）：${e.message}`);
    }
    const home = await ssh.resolveHome(client);
    const remotePath = expandPath(target.remotePath, home);

    const raw = await ssh.readRemoteText(client, remotePath).catch((e) => {
      throw new Error(`读取 ${remotePath} 失败：${e.message}`);
    });

    const sid = crypto.randomUUID();
    const session = {
      sid,
      client,
      target,
      home,
      remotePath,
      server,
      fileExists: raw !== null,
      usedKeyFp: authMethod === 'key' ? fingerprintOfPrivateKey(auth.privateKey, auth.passphrase) : '',
      authMethod,
      createdAt: Date.now(),
      lastUsed: Date.now(),
    };
    sessions.set(sid, session);

    // 服务器主动断开（空闲超时、网络抖动）时打标记，后续操作直接提示重连，而不是抛底层英文错误
    client.on('close', () => {
      session.dead = true;
      if (sessions.has(sid)) log(`会话已断开：${target.host}`);
    });

    return sessionPayload(session, raw === null ? '' : raw);
  } catch (e) {
    try {
      client.end();
    } catch {}
    throw e;
  }
}

function sessionPayload(session, rawText) {
  const parsed = parseAuthorizedKeys(rawText);
  return {
    sid: session.sid,
    host: {
      name: session.target.name || session.target.host,
      host: session.target.host,
      port: session.target.port || 22,
      username: session.target.username,
      identityFile: session.target.identityFile || '',
    },
    server: session.server,
    home: session.home,
    remotePath: session.remotePath,
    fileExists: session.fileExists,
    authMethod: session.authMethod,
    usedKeyFp: session.usedKeyFp,
    items: parsed.items
      .filter((i) => i.kind !== 'blank')
      .map((i) => ({
        index: i.index,
        kind: i.kind,
        type: i.type || '',
        fingerprint: i.fingerprint || '',
        comment: i.comment || '',
        options: i.options || '',
        b64: i.b64 || '',
        duplicateOf: i.duplicateOf == null ? null : i.duplicateOf,
        reason: i.reason || '',
        text: i.text || '',
      })),
    stats: {
      total: keyItems(parsed.items).length,
      malformed: parsed.items.filter((i) => i.kind === 'malformed').length,
      comments: parsed.items.filter((i) => i.kind === 'comment').length,
    },
  };
}

function sessionLost(msg) {
  const e = new Error(msg);
  e.code = 'SESSION_LOST';
  return e;
}

function mustSession(body) {
  const s = sessions.get(body.sid);
  if (!s) throw sessionLost('会话已失效（超过 30 分钟未操作或服务重启），请重新连接');
  if (s.dead) {
    sessions.delete(s.sid);
    throw sessionLost('SSH 连接已断开，请重新连接');
  }
  s.lastUsed = Date.now();
  return s;
}

/** 找本地某个公钥对应的私钥文件（用于更换时做真实登录验证） */
function findPrivateKeyFor(publicKeyPath) {
  if (!publicKeyPath) return null;
  const candidates = [publicKeyPath.replace(/\.pub$/i, '')];
  for (const p of candidates) {
    if (fs.existsSync(p)) return p;
  }
  return null;
}

async function verifyLogin(session, privateKey, passphrase) {
  const client = await ssh.connect({
    host: session.target.host,
    port: session.target.port || 22,
    username: session.target.username,
    privateKey,
    passphrase,
  });
  try {
    const res = await ssh.exec(client, 'printf __LOGIN_OK__');
    if (!res.stdout.includes('__LOGIN_OK__')) {
      throw new Error('新密钥登录测试未通过（命令回显异常）');
    }
  } finally {
    try {
      client.end();
    } catch {}
  }
}

/** 保存密钥对到本地目录 */
function saveKeyPair(dir, gen, nameLabel) {
  const targetDir = dir || path.join(os.homedir(), '.ssh');
  fs.mkdirSync(targetDir, { recursive: true });

  // 命名：服务器名_日期.key（如 myhost_20261019.key），一眼看出是谁的钥匙；
  // .key 后缀让 Windows 资源管理器显示「KEY 文件」而不是一串数字
  const base = `${safeName(nameLabel)}_${stamp().slice(0, 8)}.key`;
  let privPath = path.join(targetDir, base);
  let i = 2;
  while (fs.existsSync(privPath) || fs.existsSync(`${privPath}.pub`)) {
    privPath = path.join(targetDir, `${safeName(nameLabel)}_${stamp().slice(0, 8)}_${i++}.key`);
  }
  fs.writeFileSync(privPath, gen.privateKeyPem, { encoding: 'utf8', mode: 0o600 });
  fs.writeFileSync(`${privPath}.pub`, `${gen.publicKeyLine}\n`, { encoding: 'utf8', mode: 0o644 });
  return { privateKey: privPath, publicKey: `${privPath}.pub` };
}

/** 解析「要新增的那把公钥」，统一成 publicKeyLine + 可选本地私钥 */
function resolveNewKey(session, body, settings) {
  const mode = body.mode;

  if (mode === 'generate') {
    const generator = body.generator === 'rsa' ? 'rsa' : 'ed25519';
    const passphrase = String(body.passphrase || '');
    const gen =
      generator === 'rsa'
        ? keygen.generateRsa(body.comment, 3072, passphrase || undefined)
        : keygen.generateEd25519(body.comment, passphrase || undefined);
    const dir = body.saveDir || settings.saveDir || settings.localKeyDir || path.join(os.homedir(), '.ssh');
    const saved = saveKeyPair(dir, gen, session.target.name || session.target.host);
    return {
      publicKeyLine: gen.publicKeyLine,
      fingerprint: gen.fingerprint,
      type: gen.type,
      privateKey: gen.privateKeyPem,
      privateKeyPath: saved.privateKey,
      steps: [
        `已生成 ${gen.type} 密钥对${gen.encrypted ? '（私钥已用 passphrase 加密）' : ''}`,
        `私钥：${saved.privateKey}`,
        `公钥：${saved.publicKey}`,
      ],
    };
  }

  if (mode === 'local') {
    if (!body.path) throw new Error('没有指定本地公钥文件');
    const p = body.path;
    if (!fs.existsSync(p)) throw new Error(`本地文件不存在：${p}`);

    let pubText;
    let privPath = null;
    if (/\.pub$/i.test(p)) {
      pubText = fs.readFileSync(p, 'utf8');
      privPath = findPrivateKeyFor(p);
    } else {
      // 直接选了私钥：从私钥导出公钥行
      const priv = fs.readFileSync(p, 'utf8');
      const { utils } = require('ssh2');
      const k = utils.parseKey(priv, body.passphrase || undefined);
      const key = Array.isArray(k) ? k[0] : k;
      if (key instanceof Error) throw new Error(`无法解析私钥 ${p}：${key.message}`);
      const fp = keygen.fingerprintOfBlob(key.getPublicSSH());
      pubText = `${key.type} ${key.getPublicSSH().toString('base64')} ${key.comment || ''}`.trim();
      privPath = p;
      const parsed = keygen.parsePublicKeyLine(pubText);
      return {
        publicKeyLine: `${parsed.type} ${parsed.b64}${parsed.comment ? ' ' + parsed.comment : ''}`,
        fingerprint: fp,
        type: parsed.type,
        privateKey: priv,
        privateKeyPath: p,
        steps: [`读取本地私钥：${p}`],
      };
    }

    const keys = keygen.parsePublicKeys(pubText);
    if (!keys.length) throw new Error(`文件里没有可识别的公钥：${p}`);
    const k = keys[0];
    return {
      publicKeyLine: `${k.type} ${k.b64}${k.comment ? ' ' + k.comment : ''}`,
      fingerprint: k.fingerprint,
      type: k.type,
      privateKey: privPath ? fs.readFileSync(privPath) : null,
      privateKeyPath: privPath,
      steps: [`读取本地公钥：${p}`],
    };
  }

  if (mode === 'paste') {
    const keys = keygen.parsePublicKeys(body.publicKey || '');
    if (!keys.length) throw new Error('粘贴的内容里没有识别到公钥（应以 ssh-ed25519 / ssh-rsa 等开头）');
    if (!keys[0].valid) throw new Error(`公钥格式无效：${keys[0].error || '结构损坏'}`);
    const k = keys[0];
    return {
      publicKeyLine: `${k.type} ${k.b64}${k.comment ? ' ' + k.comment : ''}`,
      fingerprint: k.fingerprint,
      type: k.type,
      privateKey: null,
      privateKeyPath: null,
      steps: ['已解析粘贴的公钥'],
    };
  }

  throw new Error(`未知的新增方式：${mode}`);
}

/** 写盘 + 回读校验的公共流程 */
async function commitKeys(session, newText, steps) {
  const dir = path.posix.dirname(session.remotePath);
  const mk = await ssh.exec(session.client, `mkdir -p '${dir}' && chmod 700 '${dir}'`);
  if (mk.code !== 0) throw new Error(`创建目录 ${dir} 失败：${mk.stderr.trim() || '权限不足'}`);

  const backup = await ssh.backupFile(session.client, session.remotePath);
  steps.push(backup ? `已备份原文件：${backup}` : '目标文件原本不存在，无需备份');

  await ssh.writeRemoteTextAtomic(session.client, session.remotePath, newText);
  steps.push(`已写入 ${session.remotePath}`);

  const readback = await ssh.readRemoteText(session.client, session.remotePath);
  if (readback === null) throw new Error('写入后回读失败：文件不存在（请检查目录权限）');
  if (readback !== newText) {
    // 有些 sshd 环境会规整换行，允许末尾换行差异，但内容不一致必须报错
    if (readback.replace(/\s+$/, '') !== newText.replace(/\s+$/, '')) {
      throw new Error('写入后回读内容与预期不完全一致，请检查服务器');
    }
  }
  session.fileExists = true;
  return readback;
}

/** 把主机配置里的「登录私钥」指向新文件 */
function switchHostIdentity(session, privateKeyPath) {
  if (!privateKeyPath) return { switched: false, reason: '没有本地私钥文件路径' };
  const hostId = session.target && session.target.id;
  if (!hostId) return { switched: false, reason: '当前是临时连接（没保存成主机），配置未更新' };
  const host = store.listHosts().find((h) => h.id === hostId);
  if (!host) return { switched: false, reason: '主机配置已不存在' };

  store.upsertHost({ ...host, identityFile: privateKeyPath });
  session.target.identityFile = privateKeyPath;
  return { switched: true, hostId, identityFile: privateKeyPath };
}

/**
 * 新增密钥后，可选把登录密钥切换成新的那把。
 * 切换前必须用新私钥真连一次 —— 否则等于拿一把没验证过的钥匙去开门。
 */
async function maybeSwitchIdentity(session, fresh, body, steps) {
  if (!body.switchToNew) return { switched: false, skippedByUser: true };
  if (!fresh.privateKey) {
    const reason = '这把公钥在本地没有对应的私钥文件';
    steps.push(`跳过切换：${reason}`);
    return { switched: false, reason };
  }
  try {
    await verifyLogin(session, fresh.privateKey, body.passphrase);
    steps.push('已用新私钥实际登录验证通过');
  } catch (e) {
    const reason = `新私钥登录验证未通过 —— ${e.message}`;
    steps.push(`跳过切换：${reason}`);
    return { switched: false, reason };
  }
  const info = switchHostIdentity(session, fresh.privateKeyPath);
  if (info.switched) steps.push(`已把本机登录密钥切换为新钥匙：${info.identityFile}`);
  else steps.push(`未更新主机配置：${info.reason}`);
  return info;
}

async function addKey(session, body) {
  const settings = currentSettings();
  const steps = [];
  const fresh = resolveNewKey(session, body, settings);
  steps.push(...fresh.steps);

  const raw = session.fileExists
    ? (await ssh.readRemoteText(session.client, session.remotePath)) || ''
    : '';
  const parsed = parseAuthorizedKeys(raw);

  const result = appendKey(parsed, fresh.publicKeyLine);
  if (!result.appended) {
    // 钥匙本来就在服务器上：也可以直接切换到它
    const switchInfo = await maybeSwitchIdentity(session, fresh, body, steps);
    return {
      changed: false,
      message: result.reason,
      fingerprint: fresh.fingerprint,
      switched: switchInfo.switched,
      identityFile: switchInfo.identityFile || '',
      switchReason: switchInfo.reason || '',
      steps,
      session: sessionPayload(session, raw),
    };
  }
  steps.push('公钥不在服务器上，准备写入');

  const newText = result.text;
  await commitKeys(session, newText, steps);

  const verifyParsed = parseAuthorizedKeys(await ssh.readRemoteText(session.client, session.remotePath));
  const found = keyItems(verifyParsed.items).some((i) => i.fingerprint === fresh.fingerprint);
  if (!found) throw new Error('回读校验失败：新公钥没有出现在 authorized_keys 里');
  steps.push(`校验通过：指纹 ${fresh.fingerprint}`);

  const switchInfo = await maybeSwitchIdentity(session, fresh, body, steps);

  return {
    changed: true,
    fingerprint: fresh.fingerprint,
    type: fresh.type,
    privateKeyPath: fresh.privateKeyPath,
    switched: switchInfo.switched,
    identityFile: switchInfo.identityFile || '',
    switchReason: switchInfo.reason || '',
    steps,
    session: sessionPayload(session, await ssh.readRemoteText(session.client, session.remotePath)),
  };
}

async function replaceKey(session, body) {
  const steps = [];
  const oldFp = String(body.oldFingerprint || '');
  if (!oldFp) throw new Error('没有指定要更换的旧密钥');

  const raw = session.fileExists ? (await ssh.readRemoteText(session.client, session.remotePath)) || '' : '';
  const parsed = parseAuthorizedKeys(raw);
  const oldItem = keyItems(parsed.items).find((i) => i.fingerprint === oldFp);
  if (!oldItem) throw new Error('服务器上找不到这把旧密钥（可能已被删除或文件已变化），请刷新后重试');

  const settings = currentSettings();
  const fresh = resolveNewKey(session, body, settings);
  steps.push(...fresh.steps);

  if (fresh.fingerprint === oldFp) {
    throw new Error('新密钥和旧密钥是同一把（指纹相同），无需更换');
  }

  // 1) 先加新
  const withNew = appendKey(parsed, fresh.publicKeyLine);
  let text = withNew.text;
  if (withNew.appended) {
    await commitKeys(session, text, steps);
    steps.push('第 1 步完成：新密钥已加入');
  } else {
    steps.push(`第 1 步跳过：${withNew.reason}`);
  }

  // 2) 验证新钥匙真的能登录（拿得到私钥才验证）
  let verified = false;
  let verifyError = '';
  if (fresh.privateKey) {
    try {
      await verifyLogin(session, fresh.privateKey, body.passphrase);
      verified = true;
      steps.push('第 2 步完成：用新私钥实际登录测试通过');
    } catch (e) {
      verifyError = e.message;
      steps.push(`第 2 步失败：新私钥登录测试未通过 —— ${verifyError}`);
    }
  } else {
    verifyError = '新密钥没有对应的本地私钥，无法自动验证能否登录';
    steps.push(`第 2 步跳过：${verifyError}`);
  }

  if (!verified && !body.force) {
    return {
      changed: withNew.appended,
      removedOld: false,
      needsForce: true,
      message: `${verifyError}。为防把自己锁在门外，旧密钥【没有删除】。确认新钥匙能登录后，可勾选强制完成。`,
      steps,
      session: sessionPayload(session, await ssh.readRemoteText(session.client, session.remotePath)),
    };
  }

  // 3) 删旧
  const curRaw = await ssh.readRemoteText(session.client, session.remotePath);
  const curParsed = parseAuthorizedKeys(curRaw || '');
  const dropIndexes = curParsed.items
    .filter((i) => i.kind === 'key' && i.fingerprint === oldFp)
    .map((i) => i.index);
  if (!dropIndexes.length) {
    steps.push('旧密钥在回读时已不存在，无需删除');
    return {
      changed: true,
      removedOld: true,
      steps,
      session: sessionPayload(session, curRaw || ''),
    };
  }
  const remaining = keyItems(curParsed.items).length - dropIndexes.length;
  if (remaining <= 0) {
    throw new Error('旧密钥是服务器上最后一根钥匙，拒绝删除（否则你将无法再登录）');
  }

  const finalText = renderWithout(curParsed, dropIndexes);
  await commitKeys(session, finalText, steps);
  steps.push('第 3 步完成：旧密钥已移除');
  if (session.usedKeyFp === oldFp) {
    session.usedKeyFp = fresh.fingerprint;
    steps.push('当前会话的钥匙标记已切换到新密钥');
  }

  // 换完必须把本机登录密钥也换过去：旧公钥已从服务器删掉，配置还指着旧私钥会导致下次登录失败
  let switched = false;
  let identityFile = '';
  let switchReason = '';
  if (fresh.privateKeyPath) {
    const info = switchHostIdentity(session, fresh.privateKeyPath);
    if (info.switched) {
      switched = true;
      identityFile = info.identityFile;
      steps.push(`已把本机登录密钥切换为新钥匙：${info.identityFile}`);
    } else {
      switchReason = info.reason;
      steps.push(`未更新主机配置：${info.reason}`);
    }
  } else {
    switchReason = '新密钥在本地没有对应私钥文件';
    steps.push('注意：新密钥在本地没有对应私钥文件，请手动把主机私钥改成对应文件，否则下次连接会失败');
  }

  const fin = await ssh.readRemoteText(session.client, session.remotePath);
  const stillThere = keyItems(parseAuthorizedKeys(fin || '').items).some((i) => i.fingerprint === oldFp);
  if (stillThere) throw new Error('回读校验失败：旧密钥仍然存在');

  return {
    changed: true,
    removedOld: true,
    switched,
    identityFile,
    switchReason,
    steps,
    session: sessionPayload(session, fin || ''),
  };
}

async function deleteKeys(session, body) {
  const steps = [];
  const targets = Array.isArray(body.fingerprints) ? body.fingerprints.filter(Boolean) : [];
  if (!targets.length) throw new Error('没有选择要删除的密钥');

  const raw = session.fileExists ? (await ssh.readRemoteText(session.client, session.remotePath)) || '' : '';
  const parsed = parseAuthorizedKeys(raw);

  const dropItems = parsed.items.filter((i) => i.kind === 'key' && targets.includes(i.fingerprint));
  if (!dropItems.length) throw new Error('服务器上找不到选中的密钥，请刷新后重试');

  if (session.usedKeyFp && targets.includes(session.usedKeyFp) && !body.force) {
    return {
      changed: false,
      needsForce: true,
      message: '选中的密钥里包含【本次连接正在使用】的那把，删掉它会立刻把你锁在门外。确认要删请勾选强制删除。',
      steps,
      session: sessionPayload(session, raw),
    };
  }

  const remaining = keyItems(parsed.items).length - dropItems.length;
  if (remaining <= 0) {
    throw new Error('不能把 authorized_keys 清空 —— 那等于把服务器锁死。请至少保留一把能登录的钥匙');
  }

  const text = renderWithout(parsed, dropItems.map((i) => i.index));
  steps.push(`准备删除 ${dropItems.length} 条：${[...new Set(targets)].join(', ')}`);
  await commitKeys(session, text, steps);

  const fin = await ssh.readRemoteText(session.client, session.remotePath);
  const left = keyItems(parseAuthorizedKeys(fin || '').items);
  const leaked = left.filter((i) => targets.includes(i.fingerprint));
  if (leaked.length) throw new Error(`删除未完全生效，仍有 ${leaked.length} 条匹配的密钥`);

  steps.push(`剩余 ${left.length} 把钥匙`);
  return { changed: true, steps, session: sessionPayload(session, fin || '') };
}

/* ------------------------------------------------------------------ */
/* 内置终端（WebSocket ⇄ SSH shell）                                    */
/* ------------------------------------------------------------------ */

let WebSocketServer = null;
try {
  ({ WebSocketServer } = require('ws'));
} catch {
  console.warn('[警告] 未安装 ws，内置终端不可用（npm install ws）');
}

const wss = WebSocketServer ? new WebSocketServer({ noServer: true }) : null;

function handleTerminal(ws, url) {
  const sid = url.searchParams.get('sid');
  const session = sessions.get(sid);
  if (!session || session.dead) {
    ws.send(JSON.stringify({ t: 'error', message: '会话已失效，请重新连接服务器' }));
    try {
      ws.close(4001, 'session lost');
    } catch {}
    return;
  }

  const clamp = (v, min, max, dft) => Math.max(min, Math.min(max, Number(v) || dft));
  const cols = clamp(url.searchParams.get('cols'), 20, 400, 100);
  const rows = clamp(url.searchParams.get('rows'), 5, 200, 30);

  let stream = null;
  const pending = [];

  function applyInput(msg) {
    if (!stream) return;
    if (msg.t === 'in' && msg.d) {
      try {
        stream.write(Buffer.from(msg.d, 'base64'));
      } catch {}
    } else if (msg.t === 'resize') {
      try {
        stream.setWindow(Number(msg.rows) || rows, Number(msg.cols) || cols, 0, 0);
      } catch {}
    }
  }

  // 必须先挂 message 监听：客户端常在 onopen 就发首屏输入，这时 shell 还没就绪
  ws.on('message', (raw) => {
    let msg;
    try {
      msg = JSON.parse(raw.toString());
    } catch {
      return;
    }
    session.lastUsed = Date.now();
    if (!stream) {
      pending.push(msg);
      return;
    }
    applyInput(msg);
  });

  ws.on('close', () => {
    try {
      if (stream) stream.end();
    } catch {}
    log('终端已关闭');
  });
  ws.on('error', () => {
    try {
      if (stream) stream.end();
    } catch {}
  });

  session.client.shell({ term: 'xterm-256color', cols, rows, width: cols * 8, height: rows * 17 }, (err, sh) => {
    // 注意：ws 实例上没有 OPEN 属性（常量在构造函数上），这里直接用 1
    const send = (obj) => {
      if (ws.readyState === 1) ws.send(JSON.stringify(obj));
    };

    if (err) {
      send({ t: 'error', message: ssh.friendlyError(err, session.target) });
      try {
        ws.close(4002, 'shell failed');
      } catch {}
      return;
    }

    stream = sh; // 交给外层 applyInput 用
    session.lastUsed = Date.now();
    log(`打开终端：${session.target.username}@${session.target.host}`);

    // 统一 base64 传输，避免多字节 UTF-8 被 WebSocket 分帧切坏
    sh.on('data', (d) => send({ t: 'out', d: Buffer.from(d).toString('base64') }));
    sh.stderr.on('data', (d) => send({ t: 'out', d: Buffer.from(d).toString('base64') }));
    sh.on('close', () => {
      send({ t: 'exit' });
      try {
        ws.close(4000, 'shell closed');
      } catch {}
    });

    // shell 就绪后，把此前先到的输入补发出去
    pending.forEach(applyInput);
    pending.length = 0;
  });
}

/* upgrade 事件的绑定放在 server 创建之后（见文件下方） */

/* ------------------------------------------------------------------ */
/* HTTP                                                                */
/* ------------------------------------------------------------------ */

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.ico': 'image/x-icon',
};

const ALLOWED_STATIC = new Set(['/index.html', '/app.js', '/style.css']);
const ALLOWED_VENDOR = /^\/vendor\/[A-Za-z0-9._-]+\.(js|css)$/;

function hostAllowed(req) {
  const h = String(req.headers.host || '').toLowerCase();
  return h === `127.0.0.1:${PORT}` || h === `localhost:${PORT}` || h === `127.0.0.1` || h === `localhost`;
}

function cookieValid(req) {
  const c = parseCookies(req.headers.cookie);
  if (c.kk === TOKEN) return true;
  return req.headers['x-token'] === TOKEN;
}

async function handleApi(req, res, pathname, url) {
  if (!cookieValid(req)) {
    return fail(res, '未授权：请从工具启动时打印的地址打开页面', 401);
  }

  const method = req.method.toUpperCase();

  if (pathname === '/api/state' && method === 'GET') {
    const settings = currentSettings();
    const scan = settings.localKeyDir ? scanKeys(settings.localKeyDir) : { entries: [], errors: [] };
    return ok(res, {
      version: APP_VERSION,
      hosts: store.listHosts(),
      settings,
      localKeys: { dir: settings.localKeyDir, entries: scan.entries, errors: scan.errors },
      sessions: [...sessions.values()].map((s) => ({
        sid: s.sid,
        host: s.target.host,
        remotePath: s.remotePath,
      })),
    });
  }

  if (pathname === '/api/fs/list' && method === 'GET') {
    const dir = url.searchParams.get('dir') || '';
    const data = listLocalDir(dir);
    const settings = currentSettings();
    data.shortcuts = [
      { label: '主目录', path: os.homedir() },
      { label: '.ssh', path: path.join(os.homedir(), '.ssh') },
      settings.localKeyDir ? { label: '密钥库', path: settings.localKeyDir } : null,
      settings.saveDir ? { label: '保存目录', path: settings.saveDir } : null,
    ].filter((x) => x && fs.existsSync(x.path));
    return ok(res, data);
  }

  if (pathname === '/api/sshd' && method === 'GET') {
    const session = mustSession({ sid: url.searchParams.get('sid') });
    return ok(res, await sshd.inspect(session.client));
  }

  if (pathname === '/api/sshd/password-auth' && method === 'POST') {
    const body = await readJson(req);
    const session = mustSession(body);
    const enableKeyOnly = Boolean(body.enable);

    const before = await sshd.inspect(session.client);
    if (!before.available) throw new Error(before.reason);
    if (!before.canWrite) {
      throw new Error('需要 root 或免密 sudo 才能修改 sshd 配置');
    }

    // 安全闸门：开启「仅密钥登录」前，必须确认自己现在就是用密钥进来的
    if (enableKeyOnly) {
      const raw = (await ssh.readRemoteText(session.client, session.remotePath)) || '';
      const keys = keyItems(parseAuthorizedKeys(raw).items);
      if (!keys.length) {
        throw new Error('服务器上没有任何授权密钥，开启仅密钥登录会让你彻底进不去，已拒绝');
      }
      if (session.authMethod !== 'key') {
        throw new Error('本次是密码登录。开启仅密钥登录后，这个会话一断就再也连不回来 —— 请先用密钥成功登录一次再操作');
      }
      if (!session.usedKeyFp) {
        throw new Error('无法确认本次连接所用的密钥，为安全起见已拒绝');
      }
      if (!keys.some((k) => k.fingerprint === session.usedKeyFp)) {
        throw new Error('本次连接所用的钥匙不在 authorized_keys 里，开启后会把你锁死，已拒绝');
      }
    }

    const result = await sshd.setKeyOnly(session.client, enableKeyOnly, before);
    const after = await sshd.inspect(session.client);
    const effective = enableKeyOnly ? !after.passwordAuth : after.passwordAuth;

    log(
      `sshd 密码登录已${enableKeyOnly ? '关闭' : '开启'}（${session.target.username}@${session.target.host}，备份 ${result.backup || '无'}）`
    );

    return ok(res, {
      ...result,
      status: after,
      effective,
      warning: effective
        ? ''
        : '配置已写入，但检测到密码登录状态没变 —— 可能被 sshd_config.d 里的 drop-in 覆盖，请检查该目录',
    });
  }

  if (pathname === '/api/local-keys' && method === 'GET') {
    const dir = url.searchParams.get('dir') || currentSettings().localKeyDir;
    if (!dir) return ok(res, { dir: '', entries: [], errors: [] });
    const scan = scanKeys(dir);
    return ok(res, { dir, entries: scan.entries, errors: scan.errors });
  }

  if (pathname === '/api/hosts' && method === 'POST') {
    const body = await readJson(req);
    return ok(res, store.upsertHost(body));
  }

  if (pathname === '/api/hosts' && method === 'DELETE') {
    const id = url.searchParams.get('id');
    store.removeHost(id);
    return ok(res, true);
  }

  if (pathname === '/api/settings' && method === 'POST') {
    const body = await readJson(req);
    const allowed = {};
    for (const k of ['localKeyDir', 'saveDir', 'remotePath']) {
      if (typeof body[k] === 'string') allowed[k] = body[k].trim();
    }
    return ok(res, store.updateSettings(allowed));
  }

  if (pathname === '/api/connect' && method === 'POST') {
    const body = await readJson(req);
    const session = await openSession(body);
    return ok(res, session);
  }

  if (pathname === '/api/refresh' && method === 'POST') {
    const body = await readJson(req);
    const session = mustSession(body);
    const raw = await ssh.readRemoteText(session.client, session.remotePath);
    session.fileExists = raw !== null;
    return ok(res, sessionPayload(session, raw || ''));
  }

  if (pathname === '/api/add' && method === 'POST') {
    const body = await readJson(req);
    const session = mustSession(body);
    return ok(res, await addKey(session, body));
  }

  if (pathname === '/api/replace' && method === 'POST') {
    const body = await readJson(req);
    const session = mustSession(body);
    return ok(res, await replaceKey(session, body));
  }

  if (pathname === '/api/delete' && method === 'POST') {
    const body = await readJson(req);
    const session = mustSession(body);
    return ok(res, await deleteKeys(session, body));
  }

  if (pathname === '/api/disconnect' && method === 'POST') {
    const body = await readJson(req);
    const s = sessions.get(body.sid);
    if (s) {
      try {
        s.client.end();
      } catch {}
      sessions.delete(body.sid);
    }
    return ok(res, true);
  }

  return fail(res, `未知接口：${method} ${pathname}`, 404);
}

const server = http.createServer(async (req, res) => {
  try {
    if (!hostAllowed(req)) {
      return fail(res, '拒绝访问：Host 头不合法（本工具只绑定 127.0.0.1）', 421);
    }

    const url = new URL(req.url, `http://${req.headers.host}`);
    let pathname;
    try {
      pathname = decodeURIComponent(url.pathname);
    } catch {
      pathname = url.pathname;
    }

    if (pathname.startsWith('/api/')) {
      return await handleApi(req, res, pathname, url);
    }

    // 静态资源
    if (pathname === '/') pathname = '/index.html';
    if (!ALLOWED_STATIC.has(pathname) && !ALLOWED_VENDOR.test(pathname)) {
      res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' });
      return res.end('Not found');
    }
    const filePath = path.join(PUBLIC_DIR, pathname.slice(1));
    if (!fs.existsSync(filePath)) {
      res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' });
      return res.end('Not found');
    }
    const ext = path.extname(filePath).toLowerCase();
    const headers = {
      'Content-Type': MIME[ext] || 'application/octet-stream',
      'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff',
    };
    if (pathname === '/index.html') {
      headers['Set-Cookie'] = `kk=${TOKEN}; Path=/; HttpOnly; SameSite=Strict`;
    }
    res.writeHead(200, headers);
    res.end(fs.readFileSync(filePath));
  } catch (e) {
    const isSessionError = e.code === 'SESSION_LOST';
    const lost = isSessionError || ssh.isDisconnectError(e.message) || /连接已断开/.test(e.message || '');
    // 会话自身抛出的提示已经是人话，原样返回；只有底层英文错误才替换
    const message = isSessionError || !lost ? e.message : 'SSH 连接已断开（服务器可能主动断开了空闲连接），请重新连接后再操作';
    log(lost ? '会话已断开:' : '请求处理失败:', e.message);
    if (!res.headersSent) {
      return fail(res, message, lost ? 409 : 400, lost ? { code: 'SESSION_LOST' } : {});
    }
    try {
      res.end();
    } catch {}
  }
});

/* 终端 WebSocket 握手：同样的 Host + Cookie 校验，校验不过直接掐掉 */
if (wss) {
  server.on('upgrade', (req, socket, head) => {
    try {
      const url = new URL(req.url, `http://${req.headers.host || 'localhost'}`);
      const okReq = hostAllowed(req) && cookieValid(req) && url.pathname === '/api/terminal';
      if (!okReq) {
        socket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
        socket.destroy();
        return;
      }
      wss.handleUpgrade(req, socket, head, (ws) => handleTerminal(ws, url));
    } catch {
      try {
        socket.destroy();
      } catch {}
    }
  });
}

server.on('error', (e) => {
  if (e.code === 'EADDRINUSE') {
    console.error(`端口 ${PORT} 已被占用。可以换端口启动：set PORT=7792 && node server.js`);
    process.exit(1);
  }
  console.error('服务启动失败:', e.message);
  process.exit(1);
});

/** 首次启动：把 ~/.ssh/config 里现成的主机导进来，省得手抄 */
function bootstrapHosts() {
  try {
    if (store.listHosts().length > 0) return;
    const imported = sshconfig.importFromSshConfig();
    let n = 0;
    for (const h of imported) {
      try {
        store.upsertHost(h);
        n++;
      } catch (e) {
        log(`导入主机 ${h.name} 失败：${e.message}`);
      }
    }
    if (n) log(`已从 ~/.ssh/config 自动导入 ${n} 台主机`);
  } catch (e) {
    log('自动导入主机失败:', e.message);
  }
}

server.listen(PORT, BIND, () => {
  bootstrapHosts();
  const settings = currentSettings();
  log('SSH Keywarden 已启动');
  console.log(`   打开：http://${BIND}:${PORT}`);
  console.log(`   本地密钥目录：${settings.localKeyDir || '(未设置，请在页面右上角设置)'}`);
  if (!fs.existsSync(path.join(PUBLIC_DIR, 'vendor', 'xterm.js'))) {
    console.log('   提示：public/vendor/xterm.js 缺失，内置终端不可用（可从 node_modules/@xterm/xterm/lib 复制）');
  }
  console.log('   按 Ctrl+C 停止');
});

function shutdown() {
  for (const s of sessions.values()) {
    try {
      s.client.end();
    } catch {}
  }
  sessions.clear();
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 500).unref();
}

process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
process.on('uncaughtException', (e) => {
  log('未捕获异常:', e.stack || e.message);
});
process.on('unhandledRejection', (e) => {
  log('未处理的 Promise 拒绝:', (e && e.stack) || e);
});
