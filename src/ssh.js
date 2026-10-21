'use strict';
/**
 * SSH 传输层：连接、执行命令、读/原子写远端文件。
 *
 * 为什么不用系统 ssh.exe：本机 Node 起子进程会 EBUSY（踩过多次），
 * 所以全部走 ssh2 的纯 JS 实现。
 */

const { Client } = require('ssh2');

const CONNECT_TIMEOUT = 20000;
const EXEC_TIMEOUT = 30000;

/** 判断错误是不是「连接已经断了」这一类 */
function isDisconnectError(msg) {
  return /Unable to exec|No response from server|Client is not connected|Not connected|Connection lost|ECONNRESET|EPIPE|Channel open failure|connection is closed|Socket is closed|Channel closed/i.test(
    String(msg || '')
  );
}

function friendlyError(err, cfg = {}) {
  const msg = String((err && err.message) || err);
  const host = cfg.host || '目标主机';
  // 断线类优先判断：这类错误发生在连接已建立之后，跟地址端口无关
  if (isDisconnectError(msg)) {
    return 'SSH 连接已断开（服务器可能主动断开了空闲连接或网络中断），请重新连接后再操作';
  }
  if (/ECONNREFUSED/i.test(msg)) return `连接被拒绝：${host}:${cfg.port || 22} 上没有 SSH 服务，或防火墙拦了`;
  if (/ENOTFOUND|EAI_AGAIN/i.test(msg)) return `域名解析失败：${host}`;
  if (/ETIMEDOUT|timed out/i.test(msg)) return `连接超时：${host}:${cfg.port || 22} 无响应（内网地址需确认是否同一网络）`;
  if (/All configured authentication methods failed/i.test(msg)) return `认证失败：私钥/密码不被服务器接受（检查用户名、私钥是否匹配、authorized_keys 是否含这把钥匙）`;
  if (/Encrypted private OpenSSH key detected|no passphrase given/i.test(msg)) {
    return cfg.passphrase
      ? `私钥密码（passphrase）不正确，这把私钥解不开。请再确认一下`
      : `这把私钥已加密，需要填写 passphrase 才能使用`;
  }
  if (/Cannot parse privateKey|Bad passphrase|Invalid PEM|bad decrypt/i.test(msg)) {
    return cfg.passphrase ? `私钥解析失败，passphrase 可能不正确（${msg}）` : `私钥无法解析：${msg}`;
  }
  if (/Host key verification/i.test(msg)) return `主机指纹校验失败：${msg}`;
  return msg;
}

/**
 * 建立 SSH 连接
 * @param {{host:string,port?:number,username:string,privateKey?:string|Buffer,passphrase?:string,password?:string}} cfg
 */
function connect(cfg) {
  return new Promise((resolve, reject) => {
    const client = new Client();
    let settled = false;
    let hostKeyError = null;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      try {
        client.end();
      } catch {}
      reject(new Error(`连接超时（${CONNECT_TIMEOUT / 1000}s）：${cfg.host}:${cfg.port || 22} 无响应`));
    }, CONNECT_TIMEOUT + 2000);

    const finish = (err) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (err) {
        try {
          client.end();
        } catch {}
        if (hostKeyError) reject(hostKeyError);
        else reject(new Error(friendlyError(err, cfg)));
      } else {
        resolve(client);
      }
    };

    client.on('ready', () => finish(null));
    client.on('error', (e) => finish(e));
    client.on('close', () => finish(new Error('连接被服务器关闭')));
    // 服务器只开 keyboard-interactive 时的兜底
    client.on('keyboard-interactive', (name, instr, lang, prompts, done) => {
      done(prompts.map(() => cfg.password || ''));
    });

    const options = {
      host: cfg.host,
      port: cfg.port || 22,
      username: cfg.username,
      privateKey: cfg.privateKey || undefined,
      passphrase: cfg.passphrase || undefined,
      password: cfg.password || undefined,
      readyTimeout: CONNECT_TIMEOUT,
      tryKeyboard: Boolean(cfg.password),
      keepaliveInterval: 15000,
    };

    if (cfg.hostVerifier) {
      options.hostVerifier = (key) => {
        try {
          return cfg.hostVerifier(key) !== false;
        } catch (e) {
          hostKeyError = e;
          return false;
        }
      };
    }

    try {
      client.connect(options);
    } catch (e) {
      finish(e);
    }
  });
}

/** 在已连接的 client 上执行命令，返回 { code, stdout, stderr } */
function exec(client, command, opts = {}) {
  const { timeout = EXEC_TIMEOUT, input = null } = opts;
  return new Promise((resolve, reject) => {
    let done = false;
    const timer = setTimeout(() => {
      if (done) return;
      done = true;
      reject(
        new Error(
          `命令执行超时（${Math.round(timeout / 1000)}s）：服务器已接受登录，但执行「${command.slice(0, 40)}」无响应。常见原因：受限账号（只允许 sftp/单命令）、服务器负载过高、或网络不稳定`
        )
      );
    }, timeout);

    client.exec(command, (err, stream) => {
      if (err) {
        clearTimeout(timer);
        done = true;
        return reject(new Error(friendlyError(err)));
      }
      let stdout = '';
      let stderr = '';
      stream.on('data', (d) => (stdout += d.toString()));
      stream.stderr.on('data', (d) => (stderr += d.toString()));
      stream.on('error', (e) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        reject(new Error(friendlyError(e)));
      });
      stream.on('close', (code) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        resolve({ code, stdout, stderr });
      });
      // 关闭写侧：命令不读 stdin 时相当于 EOF；需要喂数据时一并送完
      try {
        stream.end(input == null ? undefined : input);
      } catch {}
    });
  });
}

/**
 * 读远端文本文件；文件不存在返回 null，空文件返回 ''。
 * 走 base64 而不是 SFTP：不依赖 sftp 子系统（部分 sshd/受限账号会关掉它）。
 */
async function readRemoteText(client, remotePath) {
  assertSafePath(remotePath);
  const res = await exec(
    client,
    `if [ -f ${q(remotePath)} ]; then base64 < ${q(remotePath)}; else echo __KW_MISSING__; fi`
  );
  const out = res.stdout.replace(/\s+/g, '');
  if (out === '__KW_MISSING__') return null;
  if (res.code !== 0) {
    throw new Error(`读取 ${remotePath} 失败：${res.stderr.trim() || `退出码 ${res.code}`}`);
  }
  return Buffer.from(out, 'base64').toString('utf8');
}

/** shell 单引号转义 */
function q(s) {
  return `'${String(s).replace(/'/g, `'\\''`)}'`;
}

function assertSafePath(p) {
  if (!p || typeof p !== 'string') throw new Error('远端路径为空');
  if (/[\r\n\0]/.test(p)) throw new Error('远端路径包含非法字符');
  return p;
}

/**
 * 原子写：先写同目录临时文件 → mv 覆盖 → chmod 600
 * 避免写一半断电/断线把 authorized_keys 写坏，导致自己再也进不去。
 */
async function writeRemoteTextAtomic(client, remotePath, content) {
  assertSafePath(remotePath);
  const tmp = `${remotePath}.tmp-${Date.now()}`;
  const payload = Buffer.from(content, 'utf8').toString('base64');

  const w = await exec(client, `base64 -d > ${q(tmp)}`, { input: payload });
  if (w.code !== 0) {
    throw new Error(`写入临时文件失败（退出码 ${w.code}）：${w.stderr.trim() || '无输出'}`);
  }

  const res = await exec(client, `mv -f ${q(tmp)} ${q(remotePath)} && chmod 600 ${q(remotePath)}`);
  if (res.code !== 0) {
    await exec(client, `rm -f ${q(tmp)}`).catch(() => {});
    throw new Error(`替换文件失败（退出码 ${res.code}）：${res.stderr.trim() || '无输出'}`);
  }
}

/** 备份磁盘上的文件，返回备份文件名；文件不存在返回 null */
async function backupFile(client, remotePath) {
  assertSafePath(remotePath);
  const stamp = new Date()
    .toLocaleString('sv-SE', { timeZone: 'Asia/Shanghai' })
    .replace(/[-: ]/g, '')
    .slice(0, 14);
  const target = `${remotePath}.bak.${stamp}`;
  const res = await exec(
    client,
    `if [ -f ${q(remotePath)} ]; then cp ${q(remotePath)} ${q(target)} && chmod 600 ${q(target)} && echo "__OK__"; else echo "__SKIP__"; fi`
  );
  if (res.stdout.includes('__OK__')) return target;
  if (res.stdout.includes('__SKIP__')) return null;
  throw new Error(`备份失败：${res.stderr.trim() || res.stdout.trim() || '未知原因'}`);
}

/** 拿家目录 */
async function resolveHome(client) {
  const res = await exec(client, 'printf "%s" "$HOME"');
  let home = res.stdout.trim();
  if (!home) {
    const alt = await exec(client, 'getent passwd "$(id -u)" | cut -d: -f6');
    home = alt.stdout.trim();
  }
  if (!home || home === '/') throw new Error('无法确定服务器上的用户家目录（$HOME 为空）');
  return home;
}

/** 确保 ~/.ssh 存在且权限正确 */
async function ensureSshDir(client, home) {
  const dir = `${home}/.ssh`;
  const res = await exec(client, `mkdir -p ${q(dir)} && chmod 700 ${q(dir)} && echo __OK__`);
  if (!res.stdout.includes('__OK__')) {
    throw new Error(`无法创建 ${dir}：${res.stderr.trim() || '权限不足'}`);
  }
  return dir;
}

/** 服务器基础信息。
 *  这些只是给界面展示用的，**任何一条失败都不算致命** —— 有些受限账号/精简系统
 *  不让跑 uname 或响应极慢，不能因为拿不到系统名就把整个连接判死。 */
async function probeServer(client) {
  const out = { uname: '', user: '', hostname: '' };
  const probes = [
    ['uname -srm 2>/dev/null', 'uname', 6000],
    ['id -un 2>/dev/null', 'user', 6000],
    ['hostname 2>/dev/null', 'hostname', 6000],
  ];
  for (const [cmd, key, timeout] of probes) {
    try {
      const r = await exec(client, cmd, { timeout });
      if (r.code === 0 && r.stdout.trim()) out[key] = r.stdout.trim();
    } catch (e) {
      // 探测失败不致命，字段留空即可
    }
  }
  return out;
}

module.exports = {
  connect,
  exec,
  readRemoteText,
  writeRemoteTextAtomic,
  backupFile,
  resolveHome,
  ensureSshDir,
  probeServer,
  friendlyError,
  isDisconnectError,
  assertSafePath,
};
