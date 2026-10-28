'use strict';
/**
 * 端到端测试：在本机起一个真实的 SSH 服务端（ssh2 Server 实现，内存文件系统），
 * 然后通过 HTTP 接口跑完「查看 / 新增 / 更换 / 删除」四条主流程。
 *
 * 跑法：node tests/e2e.js
 * 说明：不碰任何真实服务器，全部在本机 127.0.0.1 完成；端口与数据目录都用临时的。
 */

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const { Server } = require('ssh2');
const WSClient = require('ws');

const keygen = require('../src/keygen');
const { parseAuthorizedKeys } = require('../src/authorized');

/* ------------------------------------------------------------------ */
/* 1. 临时环境                                                         */
/* ------------------------------------------------------------------ */

const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-e2e-'));
const DATA_DIR = path.join(TMP, 'data');
const KEYS_DIR = path.join(TMP, 'keys');
fs.mkdirSync(DATA_DIR, { recursive: true });
fs.mkdirSync(KEYS_DIR, { recursive: true });

const HOME = '/home/tester';
const AUTH_PATH = `${HOME}/.ssh/authorized_keys`;
const MOCK_PASSWORD = 'tester-pass';

// 加密私钥夹具（passphrase: test-pass-123，由 ssh-keygen 生成，仅测试用）
const FIXTURES = path.join(__dirname, 'fixtures');
const ENC_KEY_PATH = path.join(FIXTURES, 'enc_ed25519');
const ENC_PUB_LINE = fs.readFileSync(`${ENC_KEY_PATH}.pub`, 'utf8').trim();
const ENC_FP = keygen.parsePublicKeyLine(ENC_PUB_LINE).fingerprint;

// 钥匙：A=现有登录用，C=待粘贴，D=更换用
const keyA = keygen.generateEd25519('laptop-a');
const keyC = keygen.generateEd25519('pasted-c');
const keyD = keygen.generateEd25519('replacement-d');

function writeLocal(gen, base) {
  const p = path.join(KEYS_DIR, base);
  fs.writeFileSync(p, gen.privateKeyPem, { mode: 0o600 });
  fs.writeFileSync(`${p}.pub`, `${gen.publicKeyLine}\n`);
  return p;
}
const keyAPath = writeLocal(keyA, 'id_a');
const keyDPath = writeLocal(keyD, 'id_d');

/* ------------------------------------------------------------------ */
/* 2. 内存 SSH 服务端                                                  */
/* ------------------------------------------------------------------ */

const FS = new Map(); // 路径 -> 文本
const DIRS = new Set();
// 只统计客户端发起的远端命令；夹具初始化及 SSH 服务端认证读取不计入。
const remoteOps = { reads: 0, writes: 0, homeReads: 0, sshdProbes: 0 };
let denyRemoteReads = false;

const SEED = [
  keyA.publicKeyLine,
  '# 下面这条注释应当原样保留',
  'garbage-line-that-cannot-be-parsed',
  keyC.publicKeyLine, // 稍后会故意让它重复加入
  ENC_PUB_LINE, // 加密私钥夹具对应的公钥
].join('\n') + '\n';
FS.set(AUTH_PATH, SEED);
DIRS.add(`${HOME}/.ssh`);

// 注意：ssh2 Server 的 ctx.key.data 已经是完整公钥 blob（含 algo 长度前缀），
// 直接算指纹即可，不要再包一层。
function fpOfCtxKey(key) {
  return keygen.fingerprintOfBlob(key.data);
}

const MOCK_SSHD_OUT = [
  '###UID 0',
  '###SUDO no',
  '###EXISTS yes',
  '###READ yes',
  '###BEGIN /etc/ssh/sshd_config',
  'PasswordAuthentication yes',
  'PubkeyAuthentication yes',
  'PermitRootLogin prohibit-password',
  '###END',
].join('\n');

function mockExec(cmd, stdin) {
  const c = cmd.trim();
  let m;

  // 探测 sshd 配置的脚本是 base64 传过去再 sh -s 执行的，先解出来看是哪个
  const b64run = c.match(/^echo '([A-Za-z0-9+/=]+)' \| base64 -d \| (?:sudo -n )?sh -s$/);
  if (b64run) {
    const script = Buffer.from(b64run[1], 'base64').toString('utf8');
    if (script.includes('###UID')) {
      remoteOps.sshdProbes++;
      return { out: MOCK_SSHD_OUT };
    }
    return { out: '', err: 'mock: 未模拟该脚本', code: 1 };
  }

  if (c === 'printf "%s" "$HOME"' || c === 'getent passwd "$(id -u)" | cut -d: -f6') {
    remoteOps.homeReads++;
    return { out: HOME };
  }
  if (c === 'uname -srm 2>/dev/null') return { out: 'Linux 6.1.0-mock x86_64' };
  if (c === 'id -un 2>/dev/null') return { out: 'tester' };
  if (c === 'hostname 2>/dev/null') return { out: 'mock-nas' };
  if (c.startsWith('uname -srm')) return { out: 'Linux 6.1.0-mock x86_64\n___\nroot\n___\nmock-nas' };
  if (c === 'printf __LOGIN_OK__') return { out: '__LOGIN_OK__' };
  if (c.startsWith('chmod ')) {
    remoteOps.writes++;
    return { out: '' };
  }

  // 备份列表：for f in '<authorized_keys>'.bak.*; do printf '%s\n' "$f"; done
  if ((m = c.match(/^for f in '([^']+)'\.bak\.\*; do printf '%s\\n' "\$f"; done$/))) {
    const prefix = `${m[1]}.bak.`;
    const files = [...FS.keys()].filter((k) => k.startsWith(prefix)).sort();
    return { out: files.map((f) => `${f}\n`).join('') };
  }

  if ((m = c.match(/^mkdir -p '([^']+)'/))) {
    remoteOps.writes++;
    DIRS.add(m[1]);
    return { out: c.includes('echo __OK__') ? '__OK__' : '' };
  }

  if (c.includes('__SKIP__')) {
    remoteOps.writes++;
    const cp = c.match(/cp '([^']+)' '([^']+)'/);
    if (!cp) return { out: '', err: 'mock: 备份命令无法解析', code: 1 };
    if (!FS.has(cp[1])) return { out: '__SKIP__' };
    FS.set(cp[2], FS.get(cp[1]));
    return { out: '__OK__' };
  }

  if (c.includes('__KW_MISSING__')) {
    remoteOps.reads++;
    const r = c.match(/base64 < '([^']+)'/);
    if (denyRemoteReads && r && r[1] === AUTH_PATH) {
      return { out: '', err: 'Permission denied', code: 1 };
    }
    if (!r || !FS.has(r[1])) return { out: '__KW_MISSING__' };
    return { out: Buffer.from(FS.get(r[1]), 'utf8').toString('base64') };
  }

  if ((m = c.match(/^base64 -d > '([^']+)'/))) {
    remoteOps.writes++;
    FS.set(m[1], Buffer.from(stdin.replace(/\s+/g, ''), 'base64').toString('utf8'));
    return { out: '' };
  }

  if ((m = c.match(/^mv -f '([^']+)' '([^']+)'/))) {
    remoteOps.writes++;
    if (!FS.has(m[1])) return { out: '', err: 'mv: cannot stat: No such file', code: 1 };
    FS.set(m[2], FS.get(m[1]));
    FS.delete(m[1]);
    return { out: '' };
  }

  if ((m = c.match(/^rm -f '([^']+)'/))) {
    remoteOps.writes++;
    FS.delete(m[1]);
    return { out: '' };
  }

  return { out: '', err: `mock: 未实现的命令 -> ${c}`, code: 127 };
}

const hostKeyPair = keygen.generateEd25519('mock-host');
const hostKey = hostKeyPair.privateKeyPem;
const HOST_KEY_FP = keygen.parsePublicKeyLine(hostKeyPair.publicKeyLine).fingerprint;

const mockClients = [];
const mockServer = new Server({ hostKeys: [hostKey] }, (client) => {
  mockClients.push(client);
  client.on('error', () => {}); // 指纹被拒时 KEX 中断，ssh2 连接对象会 emit error
  client.on('authentication', (ctx) => {
    if (ctx.method === 'password') {
      const ok = ctx.password === MOCK_PASSWORD;
      if (process.env.KW_DEBUG) console.log(`  [mock] password ${ok ? '接受' : '拒绝'}`);
      return ok ? ctx.accept() : ctx.reject(['publickey', 'password']);
    }
    if (ctx.method !== 'publickey') {
      if (process.env.KW_DEBUG) console.log('  [mock] 拒绝认证方式:', ctx.method);
      return ctx.reject();
    }
    const fp = fpOfCtxKey(ctx.key);
    const parsed = parseAuthorizedKeys(FS.get(AUTH_PATH) || '');
    const allowed = parsed.items.some((i) => i.kind === 'key' && i.fingerprint === fp);
    if (process.env.KW_DEBUG) {
      console.log(`  [mock] publickey ${ctx.key.algo} fp=${fp} sig=${ctx.signature ? 'yes' : 'no'} allowed=${allowed}`);
      parsed.items.filter((i) => i.kind === 'key').forEach((i) => console.log(`       文件里有 ${i.fingerprint}`));
    }
    return allowed ? ctx.accept() : ctx.reject(['publickey']);
  });

  client.on('ready', () => {});

  client.on('session', (accept) => {
    const session = accept();

    // 终端用：pty 申请与窗口变化都要放行
    session.on('pty', (acceptPty) => acceptPty());
    session.on('window-change', (acceptWin) => acceptWin && acceptWin());
    session.on('shell', (acceptShell) => {
      const stream = acceptShell();
      stream.write('mock-shell ready\r\n$ ');
      stream.on('data', (d) => {
        const text = d.toString();
        if (text.includes('exit')) {
          stream.exit(0);
          stream.end();
          return;
        }
        stream.write(text); // 回显
        stream.write('\r\n$ ');
      });
    });

    session.on('exec', (acceptExec, rejectExec, info) => {
      const stream = acceptExec();
      let stdin = '';
      stream.on('data', (d) => (stdin += d.toString()));
      stream.on('error', () => {});
      stream.on('end', () => {
        let r;
        try {
          r = mockExec(info.command, stdin);
        } catch (e) {
          r = { out: '', err: e.message, code: 1 };
        }
        if (r.out) stream.write(r.out);
        if (r.err) stream.stderr.write(r.err);
        stream.exit(r.code || 0);
        stream.end();
      });
    });
  });
});

// 指纹被用户拒绝时客户端会在 KEX 中途断开；ssh2 的 mock server 会因此 emit error，
// 不接住就会变成未捕获异常（真实服务器不会因此崩溃）
mockServer.on('error', () => {});

/* ------------------------------------------------------------------ */
/* 3. 断言工具                                                         */
/* ------------------------------------------------------------------ */

let passed = 0;
let failed = 0;
function test(name, fn) {
  try {
    fn();
    passed++;
    console.log(`  ✓ ${name}`);
  } catch (e) {
    failed++;
    console.log(`  ✗ ${name}\n      ${e.message}`);
  }
}
function assertRejects(fn, re, name) {
  return fn().then(
    () => {
      failed++;
      console.log(`  ✗ ${name}\n      期望抛错但没有`);
    },
    (e) => {
      if (re && !re.test(e.message)) {
        failed++;
        console.log(`  ✗ ${name}\n      错误信息不匹配：${e.message}`);
      } else {
        passed++;
        console.log(`  ✓ ${name}（被拦下：${e.message}）`);
      }
    }
  );
}

/* ------------------------------------------------------------------ */
/* 4. HTTP 客户端                                                      */
/* ------------------------------------------------------------------ */

let COOKIE = '';

function request(method, urlPath, body) {
  return new Promise((resolve, reject) => {
    const data = body ? JSON.stringify(body) : null;
    const req = http.request(
      {
        host: '127.0.0.1',
        port: process.env.PORT,
        path: urlPath,
        method,
        headers: {
          ...(data ? { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(data) } : {}),
          ...(COOKIE ? { Cookie: COOKIE } : {}),
        },
      },
      (res) => {
        let raw = '';
        res.on('data', (d) => (raw += d));
        res.on('end', () => {
          const sc = res.headers['set-cookie'];
          if (sc && sc[0]) COOKIE = sc[0].split(';')[0];
          let json = null;
          try {
            json = JSON.parse(raw);
          } catch {
            /* 首页是 HTML */
          }
          resolve({ status: res.statusCode, json, raw, headers: res.headers });
        });
      }
    );
    req.on('error', reject);
    if (data) req.write(data);
    req.end();
  });
}

async function call(path, body) {
  const r = await request('POST', path, body);
  if (!r.json) throw new Error(`非 JSON 响应（HTTP ${r.status}）`);
  if (!r.json.ok) throw new Error(r.json.error);
  return r.json.data;
}

// 同一条 WebSocket 在 refresh 前后分别发不同输入，防止旧输出冒充刷新后的回显。
function terminalEcho(ws, text, expectPrompt = false) {
  return new Promise((resolve, reject) => {
    let output = '';
    const timer = setTimeout(() => finish(new Error('终端回显超时（5s）')), 5000);
    const finish = (err) => {
      clearTimeout(timer);
      ws.off('open', send);
      ws.off('message', onMessage);
      ws.off('error', finish);
      ws.off('close', onClose);
      ws.off('unexpected-response', onUnexpected);
      if (err) reject(err);
      else resolve(output);
    };
    const send = () => {
      ws.send(JSON.stringify({ t: 'in', d: Buffer.from(`${text}\n`, 'utf8').toString('base64') }), (err) => {
        if (err) finish(err);
      });
    };
    const onMessage = (raw) => {
      try {
        const msg = JSON.parse(raw.toString());
        if (msg.t === 'error') return finish(new Error(msg.message || '终端错误'));
        if (msg.t !== 'out') return;
        output += Buffer.from(msg.d, 'base64').toString('utf8');
        if (output.includes(text) && (!expectPrompt || output.includes('mock-shell ready'))) finish();
      } catch (e) {
        finish(e);
      }
    };
    const onClose = () => finish(new Error('终端在回显前关闭'));
    const onUnexpected = () => finish(new Error('终端握手被拒绝'));
    ws.on('message', onMessage);
    ws.once('error', finish);
    ws.once('close', onClose);
    ws.once('unexpected-response', onUnexpected);
    if (ws.readyState === WSClient.OPEN) send();
    else if (ws.readyState === WSClient.CONNECTING) ws.once('open', send);
    else onClose();
  });
}

/* ------------------------------------------------------------------ */
/* 5. 主流程                                                           */
/* ------------------------------------------------------------------ */

async function main() {
  // 预置配置，避免触发「从 ~/.ssh/config 导入主机」污染测试
  fs.writeFileSync(
    path.join(DATA_DIR, 'config.json'),
    JSON.stringify({
      settings: { localKeyDir: KEYS_DIR, saveDir: KEYS_DIR, remotePath: '' },
      hosts: [
        { id: 'seed-host', name: 'mock', host: '127.0.0.1', port: 0, username: 'tester', identityFile: '' },
      ],
    })
  );

  process.env.KW_DATA_DIR = DATA_DIR;
  process.env.PORT = String(18000 + Math.floor(Math.random() * 500));

  await new Promise((resolve) => mockServer.listen(0, '127.0.0.1', resolve));
  const mockPort = mockServer.address().port;

  // 起被测服务（同进程内调用，端口已用 env 指定）
  const { startServer, stopServer } = require('../server.js');
  const started = await startServer();
  process.env.PORT = String(started.port);

  // 等 HTTP 就绪
  for (let i = 0; i < 50; i++) {
    try {
      await request('GET', '/');
      break;
    } catch {
      await new Promise((r) => setTimeout(r, 100));
    }
  }

  console.log('\n[鉴权]');
  // 清掉 cookie 试试未授权访问
  const savedCookie = COOKIE;
  COOKIE = '';
  const unauth = await request('GET', '/api/state');
  test('没有会话 Cookie 时 API 返回 401', () => assert.strictEqual(unauth.status, 401));
  COOKIE = savedCookie;

  console.log('\n[前端静态资源]');
  const mainJs = await request('GET', '/js/main.js');
  test('ES module 拆分后的入口能正常访问', () => {
    assert.strictEqual(mainJs.status, 200);
    assert.ok(mainJs.raw.includes("from './core.js'"));
  });
  const coreJs = await request('GET', '/js/core.js');
  test('子模块能正常访问且 MIME 为 JavaScript', () => {
    assert.strictEqual(coreJs.status, 200);
    assert.match(coreJs.headers['content-type'], /javascript/);
  });
  const oldBundle = await request('GET', '/app.js');
  test('旧的单文件 app.js 不再对外提供', () => assert.strictEqual(oldBundle.status, 404));
  const indexHtml = await request('GET', '/');
  const keysJs = await request('GET', '/js/keys.js');
  const backupsJs = await request('GET', '/js/backups.js');
  test('本地密钥库工具条与模块已就位', () => {
    assert.ok(indexHtml.raw.includes('id="localSearch"'));
    assert.ok(indexHtml.raw.includes('id="localFilter"'));
    assert.strictEqual(keysJs.status, 200);
    assert.ok(keysJs.raw.includes('bindLocalTools'));
  });
  test('服务器搜索/筛选、备注入口与备份弹窗已接线', () => {
    assert.ok(indexHtml.raw.includes('id="keySearch"'));
    assert.ok(indexHtml.raw.includes('id="keyFilter"'));
    assert.ok(indexHtml.raw.includes('id="keyViewCount"'));
    assert.ok(indexHtml.raw.includes('id="btnBackups"'));
    assert.ok(keysJs.raw.includes('data-note-edit'), '备注入口应渲染在服务器表里');
    assert.strictEqual(backupsJs.status, 200);
    assert.ok(backupsJs.raw.includes('openBackupsModal'));
    assert.ok(mainJs.raw.includes("from './backups.js'"));
    assert.ok(mainJs.raw.includes("$('btnBackups')"));
  });

  console.log('\n[主机指纹 TOFU]');
  const firstTry = await request('POST', '/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
    name: 'mock',
  });
  test('首次连接未确认指纹时被拦下，并返回服务器指纹', () => {
    assert.strictEqual(firstTry.status, 409);
    assert.strictEqual(firstTry.json.code, 'HOSTKEY_UNKNOWN');
    assert.strictEqual(firstTry.json.details.fingerprint, HOST_KEY_FP);
    assert.strictEqual(firstTry.json.details.keyType, 'ssh-ed25519');
  });

  console.log('\n[查看]');
  const beforeDefaultConnect = { ...remoteOps };
  const s1 = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
    name: 'mock',
    trustFingerprint: HOST_KEY_FP,
  });

  test('不传 withKeys 时仍立即加载密钥，且连接不写远端文件', () => {
    assert.strictEqual(s1.keysLoaded, true);
    assert.strictEqual(remoteOps.reads, beforeDefaultConnect.reads + 1);
    assert.strictEqual(remoteOps.writes, beforeDefaultConnect.writes);
    assert.strictEqual(remoteOps.homeReads, beforeDefaultConnect.homeReads + 1);
    assert.strictEqual(remoteOps.sshdProbes, beforeDefaultConnect.sshdProbes);
  });

  test('连上后能读到服务器信息', () => {
    assert.strictEqual(s1.host.host, '127.0.0.1');
    assert.ok(s1.server.uname.includes('Linux'));
    assert.strictEqual(s1.remotePath, AUTH_PATH);
    assert.strictEqual(s1.fileExists, true);
  });

  test('识别出本次连接用的钥匙', () => assert.strictEqual(s1.usedKeyFp, keyA.fingerprint));

  test('密钥条目解析正确（3 把钥匙 + 1 注释 + 1 无法解析）', () => {
    const kinds = s1.items.map((i) => i.kind);
    assert.deepStrictEqual(kinds.sort(), ['comment', 'key', 'key', 'key', 'malformed']);
    assert.strictEqual(s1.stats.total, 3);
    assert.strictEqual(s1.stats.malformed, 1);
  });

  test('注释里能拿到原文本', () => {
    const c = s1.items.find((i) => i.kind === 'comment');
    assert.ok(c.text.includes('应当原样保留'));
  });

  console.log('\n[服务器登录方式检测]');

  const beforeSshdProbe = remoteOps.sshdProbes;
  const sshdRes = JSON.parse((await request('GET', `/api/sshd?sid=${encodeURIComponent(s1.sid)}`, null)).raw);

  test('能读出密码登录 / 公钥登录状态，并判定当前不是仅密钥登录', () => {
    assert.strictEqual(sshdRes.ok, true, sshdRes.error);
    assert.strictEqual(remoteOps.sshdProbes, beforeSshdProbe + 1);
    assert.strictEqual(sshdRes.data.available, true);
    assert.strictEqual(sshdRes.data.passwordAuth, true);
    assert.strictEqual(sshdRes.data.pubkeyAuth, true);
    assert.strictEqual(sshdRes.data.keyOnly, false);
    assert.strictEqual(sshdRes.data.state, 'both', '两种登录方式都开着时应为 both');
    assert.strictEqual(sshdRes.data.canWrite, true, 'mock 里是 root');
  });

  console.log('\n[纯终端：延迟加载授权密钥]');

  for (const denied of [false, true]) {
    const label = denied ? '授权文件拒读' : '授权文件可读';
    const connectBody = {
      host: '127.0.0.1',
      port: mockPort,
      username: 'tester',
      // 拒读用密码认证，明确区分「允许登录」与「允许读授权文件」。
      ...(denied ? { identityFile: '', password: MOCK_PASSWORD } : { identityFile: keyAPath }),
    };
    const beforeConnect = { ...remoteOps };
    const beforeFiles = new Map(FS);
    const beforeDirs = new Set(DIRS);
    let ws;
    denyRemoteReads = denied;
    try {
      const session = await call('/api/connect', { ...connectBody, withKeys: false });
      test(`${label}：withKeys:false 返回未加载状态及空列表`, () => {
        assert.ok(session.sid);
        assert.strictEqual(session.keysLoaded, false);
        assert.deepStrictEqual(session.items, []);
        assert.deepStrictEqual(session.stats, { total: 0, malformed: 0, comments: 0 });
        assert.strictEqual(session.authMethod, denied ? 'password' : 'key');
      });

      ws = new WSClient(
        `ws://127.0.0.1:${process.env.PORT}/api/terminal?sid=${encodeURIComponent(session.sid)}&cols=80&rows=24`,
        { headers: { Cookie: COOKIE } }
      );
      // HTTP 请求期间也接住错误；下一次回显会检查连接是否仍然可用。
      ws.on('error', () => {});
      const initialOut = await terminalEcho(ws, 'echo terminal-before-refresh', true);
      test(`${label}：未加载密钥也能收到 shell 提示符并回显输入`, () => {
        assert.ok(initialOut.includes('mock-shell ready'));
        assert.ok(initialOut.includes('echo terminal-before-refresh'));
      });
      test(`${label}：纯终端连接及 shell 不读写授权文件、不探测 sshd`, () => {
        assert.strictEqual(remoteOps.reads, beforeConnect.reads);
        assert.strictEqual(remoteOps.writes, beforeConnect.writes);
        assert.strictEqual(remoteOps.sshdProbes, beforeConnect.sshdProbes);
        assert.deepStrictEqual(FS, beforeFiles);
        assert.deepStrictEqual(DIRS, beforeDirs);
      });
      test(`${label}：首次 refresh 前不读取 home`, () => {
        assert.strictEqual(remoteOps.homeReads, beforeConnect.homeReads);
      });

      const sidQuery = `?sid=${encodeURIComponent(session.sid)}`;
      const beforeKeyActions = { ...remoteOps };
      for (const endpoint of ['/api/sshd', '/api/backups', '/api/backups/diff']) {
        const blocked = await request('GET', endpoint + sidQuery);
        test(`${label}：未加载密钥时拒绝 ${endpoint}`, () => {
          assert.strictEqual(blocked.json.ok, false);
          assert.match(blocked.json.error, /请先切换到密钥管理/);
        });
      }
      for (const endpoint of ['/api/sshd/password-auth', '/api/backups/restore', '/api/add', '/api/replace', '/api/delete']) {
        const blocked = await request('POST', endpoint, { sid: session.sid });
        test(`${label}：未加载密钥时拒绝 ${endpoint}`, () => {
          assert.strictEqual(blocked.json.ok, false);
          assert.match(blocked.json.error, /请先切换到密钥管理/);
        });
      }
      test(`${label}：被拦截的密钥操作不执行远端命令`, () => {
        assert.deepStrictEqual(remoteOps, beforeKeyActions);
      });

      if (denied) {
        const beforeDeniedRefresh = { ...remoteOps };
        const rejected = await request('POST', '/api/refresh', { sid: session.sid });
        test('拒读时 refresh 返回权限错误而非会话失效', () => {
          assert.ok(rejected.status >= 400 && rejected.status < 500);
          assert.strictEqual(rejected.json.ok, false);
          assert.match(rejected.json.error, /Permission denied/i);
          assert.notStrictEqual(rejected.json.code, 'SESSION_LOST');
          assert.strictEqual(remoteOps.reads, beforeDeniedRefresh.reads + 1);
          assert.strictEqual(remoteOps.writes, beforeDeniedRefresh.writes);
          assert.strictEqual(remoteOps.sshdProbes, beforeDeniedRefresh.sshdProbes);
        });
        const state = await request('GET', '/api/state');
        test('refresh 拒读后原 sid 仍在会话列表中', () => {
          assert.strictEqual(state.json.ok, true);
          assert.ok(state.json.data.sessions.some((s) => s.sid === session.sid));
        });
        const afterFailure = await terminalEcho(ws, 'echo terminal-after-denied-refresh');
        test('refresh 拒读后原 WebSocket shell 仍可收发', () => {
          assert.ok(afterFailure.includes('echo terminal-after-denied-refresh'));
        });

        const readsBeforeDefault = remoteOps.reads;
        await assertRejects(
          () => call('/api/connect', connectBody),
          /Permission denied/i,
          '默认密钥连接仍会因授权文件拒读而失败'
        );
        test('拒读时默认连接确实尝试读取授权文件', () => {
          assert.strictEqual(remoteOps.reads, readsBeforeDefault + 1);
        });
        denyRemoteReads = false;
      }

      const beforeRefresh = { ...remoteOps };
      const refreshed = await call('/api/refresh', { sid: session.sid });
      test(`${label}：refresh 在原 sid 上补读并填充密钥`, () => {
        assert.strictEqual(refreshed.sid, session.sid);
        assert.strictEqual(refreshed.keysLoaded, true);
        assert.strictEqual(refreshed.home, HOME);
        assert.strictEqual(refreshed.remotePath, AUTH_PATH);
        assert.strictEqual(refreshed.fileExists, true);
        assert.deepStrictEqual(refreshed.items, s1.items);
        assert.deepStrictEqual(refreshed.stats, s1.stats);
        assert.strictEqual(remoteOps.reads, beforeRefresh.reads + 1);
        assert.strictEqual(remoteOps.writes, beforeRefresh.writes);
        assert.strictEqual(remoteOps.sshdProbes, beforeRefresh.sshdProbes);
      });
      const afterRefresh = await terminalEcho(ws, 'echo terminal-after-loaded-refresh');
      test(`${label}：加载密钥后原 shell 仍可用且没有远端写入或 sshd 探测`, () => {
        assert.ok(afterRefresh.includes('echo terminal-after-loaded-refresh'));
        assert.strictEqual(remoteOps.writes, beforeConnect.writes);
        assert.strictEqual(remoteOps.sshdProbes, beforeConnect.sshdProbes);
        assert.deepStrictEqual(FS, beforeFiles);
        assert.deepStrictEqual(DIRS, beforeDirs);
      });
    } finally {
      denyRemoteReads = false;
      if (ws) ws.terminate();
    }
  }

  const beforeExplicitKeys = { ...remoteOps };
  const explicitKeys = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
    withKeys: true,
  });
  test('显式 withKeys:true 与默认连接一样立即加载密钥', () => {
    assert.strictEqual(explicitKeys.keysLoaded, true);
    assert.deepStrictEqual(explicitKeys.items, s1.items);
    assert.strictEqual(remoteOps.reads, beforeExplicitKeys.reads + 1);
    assert.strictEqual(remoteOps.writes, beforeExplicitKeys.writes);
  });

  console.log('\n[加密私钥 / passphrase]');

  await assertRejects(
    () => call('/api/connect', { host: '127.0.0.1', port: mockPort, username: 'tester', identityFile: ENC_KEY_PATH }),
    /已加密|passphrase/,
    '不填 passphrase：明确提示私钥已加密'
  );

  await assertRejects(
    () =>
      call('/api/connect', {
        host: '127.0.0.1',
        port: mockPort,
        username: 'tester',
        identityFile: ENC_KEY_PATH,
        passphrase: 'wrong-pass',
      }),
    /passphrase/,
    'passphrase 填错：明确提示密码不对'
  );

  const encOk = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: ENC_KEY_PATH,
    passphrase: 'test-pass-123',
  });

  test('填对 passphrase 能正常登录（说明 passphrase 真的透传给了 ssh2）', () => {
    assert.strictEqual(encOk.authMethod, 'key');
    assert.strictEqual(encOk.usedKeyFp, ENC_FP);
    assert.ok(encOk.items.length > 0);
  });

  const fallback = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: ENC_KEY_PATH,
    password: MOCK_PASSWORD,
  });

  test('私钥解不开但给了密码 → 自动降级用密码登录', () => {
    assert.strictEqual(fallback.authMethod, 'password');
    assert.strictEqual(fallback.usedKeyFp, '');
  });

  // 工具自己生成的加密私钥，要能真的用它登录
  const encGen = await call('/api/add', {
    sid: s1.sid,
    mode: 'generate',
    generator: 'ed25519',
    comment: 'enc-gen',
    saveDir: KEYS_DIR,
    passphrase: 'newpass123',
    switchToNew: false,
  });

  test('生成时设置的 passphrase 会写进私钥文件', () => {
    const raw = fs.readFileSync(encGen.privateKeyPath, 'utf8');
    assert.ok(raw.includes('BEGIN OPENSSH PRIVATE KEY'), '应是 OpenSSH 格式');
    const body = Buffer.from(raw.split('\n').slice(1, -2).join(''), 'base64');
    assert.ok(body.includes(Buffer.from('aes256-ctr')), '容器应声明 aes256-ctr');
    assert.ok(encGen.steps.some((s) => s.includes('passphrase 加密')), '步骤里应注明已加密');
  });

  const encConn = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: encGen.privateKeyPath,
    passphrase: 'newpass123',
  });

  test('用生成的加密私钥 + passphrase 能正常登录', () => {
    assert.strictEqual(encConn.authMethod, 'key');
    assert.strictEqual(encConn.usedKeyFp, encGen.fingerprint);
  });

  await call('/api/delete', { sid: s1.sid, fingerprints: [encGen.fingerprint] });

  console.log('\n[新增]');

  const beforeAdd = { ...remoteOps };
  const add1 = await call('/api/add', {
    sid: s1.sid,
    mode: 'paste',
    publicKey: `ssh-ed25519 ${keygen.parsePublicKeyLine(keygen.generateEd25519('x').publicKeyLine).b64} fresh-key`,
  });

  test('粘贴公钥 → 写入成功并回读校验通过', () => {
    assert.strictEqual(add1.changed, true);
    assert.ok(remoteOps.reads > beforeAdd.reads, '读计数器应捕获回读');
    assert.ok(remoteOps.writes > beforeAdd.writes, '写计数器应捕获原子写入及备份');
    assert.ok(FS.get(AUTH_PATH).includes('fresh-key'));
    assert.ok(add1.steps.some((s) => s.includes('已备份原文件')), '应当有备份步骤');
    assert.ok(add1.steps.some((s) => s.includes('校验通过')));
  });

  test('写入后备份文件真实存在', () => {
    const backups = [...FS.keys()].filter((k) => k.includes('.bak.'));
    assert.ok(backups.length >= 1, '应生成 .bak 备份');
    assert.ok(FS.get(backups[0]).includes(keyA.publicKeyLine), '备份内容应是写入前的版本');
  });

  test('原注释行与无法解析的行都没被动过', () => {
    const now = FS.get(AUTH_PATH);
    assert.ok(now.includes('# 下面这条注释应当原样保留'));
    assert.ok(now.includes('garbage-line-that-cannot-be-parsed'));
  });

  const addDup = await call('/api/add', { sid: s1.sid, mode: 'paste', publicKey: keyC.publicKeyLine });
  test('重复公钥不重复写入（幂等）', () => {
    assert.strictEqual(addDup.changed, false);
    assert.ok(addDup.message.includes('已在服务器上'));
    const b64 = keyC.publicKeyLine.split(' ')[1];
    const count = FS.get(AUTH_PATH).split(b64).length - 1;
    assert.strictEqual(count, 1);
  });

  await assertRejects(
    () => call('/api/add', { sid: s1.sid, mode: 'paste', publicKey: '这不是公钥' }),
    /没有识别到公钥/,
    '粘贴垃圾内容被拒绝'
  );

  console.log('\n[生成密钥后自动切换登录密钥]');

  // 先存一台主机，才能验证「配置被切换」
  const hostRec = await call('/api/hosts', {
    name: 'mock-switch',
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
  });

  const sGen = await call('/api/connect', { hostId: hostRec.id });
  const genRes = await call('/api/add', {
    sid: sGen.sid,
    mode: 'generate',
    generator: 'ed25519',
    comment: 'auto-switch',
    saveDir: KEYS_DIR,
    switchToNew: true,
  });

  test('生成并写入后，主机配置的私钥被换成新钥匙', () => {
    assert.strictEqual(genRes.switched, true, `未切换：${(genRes.steps || []).join(' | ')}`);
    assert.ok(fs.existsSync(genRes.identityFile), '新私钥文件应存在');
    assert.notStrictEqual(genRes.identityFile, keyAPath, '不能还是旧私钥');
    // 命名规则：服务器名_日期.key（可读、Windows 类型列显示 KEY 文件）
    const base = path.basename(genRes.identityFile);
    assert.ok(/^mock-switch_\d{8}(_\d+)?\.key$/.test(base), `命名应为 服务器名_日期.key，实际：${base}`);
  });

  const stateAfter = JSON.parse((await request('GET', '/api/state', null)).raw);
  const savedHost = stateAfter.data.hosts.find((h) => h.id === hostRec.id);

  test('落盘的配置里也是新私钥路径', () => {
    assert.strictEqual(savedHost.identityFile, genRes.identityFile);
  });

  const sGen2 = await call('/api/connect', { hostId: hostRec.id });

  test('用该主机重新连接时，确实走的是新钥匙', () => {
    assert.strictEqual(sGen2.usedKeyFp, genRes.fingerprint, '连接用的指纹应是新钥匙');
  });

  // 清理：删掉临时钥匙与临时主机。这把正是当前会话在用的，需要 force
  await call('/api/delete', { sid: sGen2.sid, fingerprints: [genRes.fingerprint], force: true });
  await request('DELETE', `/api/hosts?id=${encodeURIComponent(hostRec.id)}`, null);

  console.log('\n[本地已有：配对私钥带 passphrase 时的切换]');

  // 回归：ENC_KEY_PATH 是加密私钥夹具，其 .pub 可被「本地已有」选中。
  // 之前前端 local 分支根本不提交 passphrase，导致 verifyLogin 必失败，
  // 「写完后把本机登录密钥切换成这把」在这条路径上永远静默不生效。
  const hostEnc = await call('/api/hosts', {
    name: 'mock-enc-switch',
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
  });

  const sEnc = await call('/api/connect', { hostId: hostEnc.id });
  const noPass = await call('/api/add', {
    sid: sEnc.sid,
    mode: 'local',
    path: `${ENC_KEY_PATH}.pub`,
    switchToNew: true,
  });

  test('加密私钥没给 passphrase：不切换，且把原因说明白', () => {
    assert.strictEqual(noPass.switched, false, '缺 passphrase 不能算切换成功');
    assert.ok(
      /passphrase/i.test(noPass.switchReason || ''),
      `switchReason 要指出缺 passphrase，实际：${JSON.stringify(noPass.switchReason)}`
    );
  });

  const sEnc2 = await call('/api/connect', { hostId: hostEnc.id });
  const withPass = await call('/api/add', {
    sid: sEnc2.sid,
    mode: 'local',
    path: `${ENC_KEY_PATH}.pub`,
    passphrase: 'test-pass-123',
    switchToNew: true,
  });

  test('给了 passphrase：验证通过并真的切到加密私钥', () => {
    assert.strictEqual(withPass.switched, true, `未切换：${(withPass.steps || []).join(' | ')}`);
    assert.strictEqual(withPass.identityFile, ENC_KEY_PATH, `主机私钥应指向加密私钥，实际：${withPass.identityFile}`);
  });

  const savedEnc = JSON.parse((await request('GET', '/api/state', null)).raw).data.hosts.find(
    (h) => h.id === hostEnc.id
  );
  test('落盘配置也指向加密私钥', () => {
    assert.strictEqual(savedEnc.identityFile, ENC_KEY_PATH);
  });

  const sEnc3 = await call('/api/connect', { hostId: hostEnc.id, passphrase: 'test-pass-123' });
  test('切换后用该主机重连，走的就是这把加密私钥', () => {
    assert.strictEqual(sEnc3.usedKeyFp, ENC_FP);
  });

  await request('DELETE', `/api/hosts?id=${encodeURIComponent(hostEnc.id)}`, null);

  console.log('\n[凭据弹窗重连后切换登录密钥]');

  // 回归：私钥带 passphrase 时，连接会先失败、再由凭据弹窗重连。
  // 旧版前端重连时丢掉了 hostId，会话变成「临时连接」，新增密钥勾了切换也写不进主机配置。
  const hostCred = await call('/api/hosts', {
    name: 'mock-cred-bind',
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: ENC_KEY_PATH,
  });

  const credConn = await call('/api/connect', {
    hostId: hostCred.id,
    identityFile: ENC_KEY_PATH,
    passphrase: 'test-pass-123',
  });

  test('凭据弹窗重连保留主机绑定，并带上弹窗里编辑的私钥路径', () => {
    assert.strictEqual(credConn.authMethod, 'key');
    assert.strictEqual(credConn.usedKeyFp, ENC_FP);
  });

  const hostOverride = await call('/api/hosts', {
    name: 'mock-cred-override',
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
  });

  const overrideConn = await call('/api/connect', {
    hostId: hostOverride.id,
    identityFile: ENC_KEY_PATH,
    passphrase: 'test-pass-123',
  });
  const savedOverride = JSON.parse((await request('GET', '/api/state', null)).raw).data.hosts.find(
    (h) => h.id === hostOverride.id
  );

  test('带 hostId 的 identityFile 只覆盖本次连接，不改动主机配置', () => {
    assert.strictEqual(overrideConn.usedKeyFp, ENC_FP);
    assert.strictEqual(savedOverride.identityFile, keyAPath);
  });

  const pwdConn = await call('/api/connect', {
    hostId: hostOverride.id,
    identityFile: '',
    password: MOCK_PASSWORD,
  });

  test('显式传空 identityFile → 改走密码，不被主机配置里的私钥覆盖', () => {
    assert.strictEqual(pwdConn.authMethod, 'password');
    assert.strictEqual(pwdConn.usedKeyFp, '');
  });

  const credAdd = await call('/api/add', {
    sid: credConn.sid,
    mode: 'generate',
    generator: 'ed25519',
    comment: 'cred-switch',
    saveDir: KEYS_DIR,
    passphrase: 'newpass456',
    switchToNew: true,
  });

  test('凭据重连后新增加密密钥，切换登录密钥会落盘', () => {
    assert.strictEqual(credAdd.switched, true, `未切换：${(credAdd.steps || []).join(' | ')}`);
    assert.ok(credAdd.identityFile && credAdd.identityFile !== ENC_KEY_PATH);
  });

  const savedCred = JSON.parse((await request('GET', '/api/state', null)).raw).data.hosts.find(
    (h) => h.id === hostCred.id
  );

  test('落盘配置指向新生成的加密私钥', () => {
    assert.strictEqual(savedCred.identityFile, credAdd.identityFile);
  });

  const credRe = await call('/api/connect', { hostId: hostCred.id, passphrase: 'newpass456' });
  test('下次连接无需再指定 identityFile，用的就是新密钥', () => {
    assert.strictEqual(credRe.usedKeyFp, credAdd.fingerprint);
  });

  await call('/api/delete', { sid: credRe.sid, fingerprints: [credAdd.fingerprint], force: true });
  await request('DELETE', `/api/hosts?id=${encodeURIComponent(hostCred.id)}`, null);
  await request('DELETE', `/api/hosts?id=${encodeURIComponent(hostOverride.id)}`, null);

  console.log('\n[更换]');

  const before = FS.get(AUTH_PATH);
  const rep = await call('/api/replace', {
    sid: s1.sid,
    oldFingerprint: keyC.fingerprint,
    mode: 'local',
    path: `${keyDPath}.pub`,
  });

  test('更换：新钥匙写入、旧钥匙删除、返回标记正确', () => {
    assert.strictEqual(rep.changed, true);
    assert.strictEqual(rep.removedOld, true);
    const now = FS.get(AUTH_PATH);
    assert.ok(now.includes(keyD.publicKeyLine), '新钥匙应在文件中');
    assert.ok(!now.includes(keyC.publicKeyLine.split(' ')[1]), '旧钥匙应已移除');
    assert.ok(now.includes(keyA.publicKeyLine), '无关的钥匙不能被误删');
  });

  test('更换过程包含「先加新 → 验证登录 → 再删旧」三步', () => {
    const joined = rep.steps.join('\n');
    assert.ok(joined.includes('新密钥已加入'), '缺少加新步骤');
    assert.ok(joined.includes('实际登录测试通过'), '缺少登录验证步骤');
    assert.ok(joined.includes('旧密钥已移除'), '缺少删旧步骤');
  });

  test('更换时新钥匙在本地生成了可用私钥文件', () => {
    assert.ok(fs.existsSync(keyDPath), '私钥文件应存在');
    const pub = fs.readFileSync(`${keyDPath}.pub`, 'utf8');
    assert.ok(pub.includes(keyD.publicKeyLine));
  });

  console.log('\n[删除]');

  const del = await call('/api/delete', {
    sid: rep.session.sid,
    fingerprints: [keyD.fingerprint],
  });

  test('删除指定钥匙后文件里不再有它', () => {
    assert.strictEqual(del.changed, true);
    assert.ok(!FS.get(AUTH_PATH).includes(keyD.publicKeyLine.split(' ')[1]));
  });

  const guarded = await call('/api/delete', { sid: rep.session.sid, fingerprints: [keyA.fingerprint] });
  test('删除「本次连接正在用的钥匙」会先拦下并要求确认', () => {
    assert.strictEqual(guarded.changed, false);
    assert.strictEqual(guarded.needsForce, true);
    assert.ok(guarded.message.includes('锁在门外'));
    assert.ok(FS.get(AUTH_PATH).includes(keyA.publicKeyLine.split(' ')[1]), '文件不能被改动');
  });

  // 把前面粘贴进去的那把也删掉，让 keyA 成为最后一根，再验证防清空
  if (add1.fingerprint) {
    await call('/api/delete', { sid: rep.session.sid, fingerprints: [add1.fingerprint] });
  }
  await call('/api/delete', { sid: rep.session.sid, fingerprints: [ENC_FP] });
  test('删除后服务器只剩一把钥匙', () => {
    const left = parseAuthorizedKeys(FS.get(AUTH_PATH)).items.filter((i) => i.kind === 'key');
    assert.strictEqual(left.length, 1);
    assert.strictEqual(left[0].fingerprint, keyA.fingerprint);
  });

  await assertRejects(
    () => call('/api/delete', { sid: rep.session.sid, fingerprints: [keyA.fingerprint], force: true }),
    /不能把 authorized_keys 清空|锁死/,
    '服务器只剩一把钥匙时，强制删除也被拒绝（防锁死）'
  );

  await assertRejects(
    () => call('/api/delete', { sid: 'not-a-real-session', fingerprints: ['x'] }),
    /会话已失效/,
    '失效会话被拒绝'
  );

  console.log('\n[本地钥匙备注]');

  const noteSaved = await call('/api/key-notes', {
    fingerprint: keyA.fingerprint,
    note: '  运维用的主钥匙\n备用  ',
  });
  test('备注保存后回传归一化文本', () => {
    assert.strictEqual(noteSaved.keyNotes[keyA.fingerprint].note, '运维用的主钥匙 备用');
  });

  const stateNotes = JSON.parse((await request('GET', '/api/state', null)).raw).data.keyNotes;
  test('/api/state 会带出本机备注', () => {
    assert.strictEqual(stateNotes[keyA.fingerprint].note, '运维用的主钥匙 备用');
  });

  await assertRejects(
    () => call('/api/key-notes', { fingerprint: 'not-a-fingerprint', note: 'x' }),
    /指纹格式/,
    '备注拒绝非法指纹'
  );
  await assertRejects(
    () => call('/api/key-notes', { fingerprint: keyA.fingerprint, note: 'x'.repeat(201) }),
    /最多 200/,
    '备注超长被拒绝'
  );

  const noteCleared = await call('/api/key-notes', { fingerprint: keyA.fingerprint, note: '' });
  test('空备注等于删除', () => {
    assert.ok(!(keyA.fingerprint in noteCleared.keyNotes));
    const onDisk = JSON.parse(fs.readFileSync(path.join(DATA_DIR, 'key-notes.json'), 'utf8'));
    assert.ok(!(keyA.fingerprint in onDisk.notes));
  });

  console.log('\n[备份历史与恢复]');

  const bkKeep = 'authorized_keys.bak.20260102000000';
  const bkEmpty = 'authorized_keys.bak.20260101000000';
  const bkOther = 'authorized_keys.bak.20260101000001';
  FS.set(`${AUTH_PATH}.bak.20260102000000`, [keyA.publicKeyLine, keyC.publicKeyLine, '# 注释留在备份里'].join('\n') + '\n');
  FS.set(`${AUTH_PATH}.bak.20260101000000`, '# 只有注释\n');
  FS.set(`${AUTH_PATH}.bak.20260101000001`, `${keyC.publicKeyLine}\n`);

  const listRes = JSON.parse((await request('GET', `/api/backups?sid=${encodeURIComponent(s1.sid)}`, null)).raw);
  test('备份列表只列同目录合法备份，并按时间倒序', () => {
    assert.strictEqual(listRes.ok, true, listRes.error);
    assert.strictEqual(listRes.data.remotePath, AUTH_PATH);
    const names = listRes.data.backups.map((b) => b.name);
    assert.ok(names.includes(bkKeep));
    assert.ok(names.includes(bkEmpty));
    assert.ok(names.includes(bkOther));
    assert.ok(!names.some((n) => n.includes('/')), '列表里不能出现完整路径');
    const stamps = listRes.data.backups.map((b) => b.stamp);
    assert.deepStrictEqual(stamps, [...stamps].sort().reverse(), '应按时间倒序');
  });

  const diffRes = JSON.parse(
    (
      await request(
        'GET',
        `/api/backups/diff?sid=${encodeURIComponent(s1.sid)}&name=${encodeURIComponent(bkKeep)}`,
        null
      )
    ).raw
  );
  test('差异方向正确：备份比当前多出的钥匙标为恢复后新增', () => {
    assert.strictEqual(diffRes.ok, true, diffRes.error);
    const d = diffRes.data;
    assert.strictEqual(d.usedKeyInBackup, true);
    assert.strictEqual(d.backupKeyCount, 2);
    assert.strictEqual(d.currentKeyCount, 1);
    assert.deepStrictEqual(d.added.map((k) => k.fingerprint), [keyC.fingerprint]);
    assert.strictEqual(d.removed.length, 0);
    assert.strictEqual(d.emptyBackup, false);
  });

  const restored = await call('/api/backups/restore', { sid: s1.sid, name: bkKeep });
  test('恢复备份：整文件还原、生成新备份并回读校验', () => {
    assert.strictEqual(restored.changed, true);
    assert.ok(FS.get(AUTH_PATH).includes(keyC.publicKeyLine));
    assert.ok(FS.get(AUTH_PATH).includes('# 注释留在备份里'));
    assert.ok(restored.steps.some((s) => s.includes('已备份原文件')), '恢复前必须再备份当前文件');
    assert.ok(restored.session.items.some((i) => i.fingerprint === keyC.fingerprint));
  });

  const restoredAgain = await call('/api/backups/restore', { sid: s1.sid, name: bkKeep });
  test('恢复内容与当前完全一致时不重复写入', () => {
    assert.strictEqual(restoredAgain.changed, false);
    assert.ok(restoredAgain.message.includes('完全一致'));
  });

  const beforeEmpty = FS.get(AUTH_PATH);
  await assertRejects(
    () => call('/api/backups/restore', { sid: s1.sid, name: bkEmpty }),
    /没有任何可用的密钥|清空/,
    '空备份被硬拒绝（防清空 authorized_keys）'
  );
  test('空备份被拒后文件原样保留', () => assert.strictEqual(FS.get(AUTH_PATH), beforeEmpty));

  const forceGate = await call('/api/backups/restore', { sid: s1.sid, name: bkOther });
  test('备份不含当前登录钥匙时先拦下并要求 force', () => {
    assert.strictEqual(forceGate.changed, false);
    assert.strictEqual(forceGate.needsForce, true);
    assert.ok(FS.get(AUTH_PATH).includes(keyA.publicKeyLine), '未确认前不能改动文件');
  });

  const forced = await call('/api/backups/restore', { sid: s1.sid, name: bkOther, force: true });
  test('确认 force 后可以恢复（本次会话仍能操作）', () => {
    assert.strictEqual(forced.changed, true);
    assert.ok(FS.get(AUTH_PATH).includes(keyC.publicKeyLine.split(' ')[1]));
    assert.ok(!FS.get(AUTH_PATH).includes(keyA.publicKeyLine.split(' ')[1]));
  });

  const backToKeep = await call('/api/backups/restore', { sid: s1.sid, name: bkKeep, force: true });
  test('可以从备份恢复回包含登录钥匙的版本', () => {
    assert.strictEqual(backToKeep.changed, true);
    assert.ok(FS.get(AUTH_PATH).includes(keyA.publicKeyLine));
  });

  // 收尾：把恢复带回来的 keyC 再删掉，让文件回到只剩 keyA（后续终端测试依赖它）
  await call('/api/delete', { sid: s1.sid, fingerprints: [keyC.fingerprint] });

  const traversal = await request(
    'GET',
    `/api/backups/diff?sid=${encodeURIComponent(s1.sid)}&name=${encodeURIComponent('../../etc/passwd')}`,
    null
  );
  test('差异接口拒绝路径穿越', () => {
    assert.strictEqual(traversal.json.ok, false);
    assert.ok(/备份文件名不合法/.test(traversal.json.error));
  });

  await assertRejects(
    () => call('/api/backups/restore', { sid: s1.sid, name: `authorized_keys.bak.20260102000000/../../x` }),
    /备份文件名不合法/,
    '恢复接口拒绝带路径的备份名'
  );

  await assertRejects(
    () => call('/api/backups/restore', { sid: s1.sid, name: 'authorized_keys.bak.19990101000000' }),
    /备份文件不存在/,
    '不存在的备份会提示刷新列表'
  );

  console.log('\n[内置终端]');

  const sTerm = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
  });

  const termUrl = `ws://127.0.0.1:${process.env.PORT}/api/terminal?sid=${encodeURIComponent(
    sTerm.sid
  )}&cols=80&rows=24`;

  const termOut = [];
  const termWs = new WSClient(termUrl, { headers: { Cookie: COOKIE } });

  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('终端超时（5s）')), 5000);
    termWs.on('open', () => {
      if (process.env.KW_DEBUG) console.log('  [term] ws open');
      termWs.send(JSON.stringify({ t: 'in', d: Buffer.from('echo hi\n', 'utf8').toString('base64') }));
    });
    termWs.on('unexpected-response', (req, res) => {
      if (process.env.KW_DEBUG) console.log('  [term] 握手被拒:', res.statusCode);
    });
    termWs.on('message', (raw) => {
      if (process.env.KW_DEBUG) console.log('  [term] <-', raw.toString().slice(0, 90));
      const msg = JSON.parse(raw.toString());
      if (msg.t === 'out') {
        termOut.push(Buffer.from(msg.d, 'base64').toString('utf8'));
        const all = termOut.join('');
        if (all.includes('mock-shell ready') && all.includes('echo hi')) {
          clearTimeout(timer);
          resolve();
        }
      }
    });
    termWs.on('error', (e) => {
      clearTimeout(timer);
      reject(e);
    });
  });

  test('终端 WebSocket 能连上、收到 shell 提示符并回显输入', () => {
    const all = termOut.join('');
    assert.ok(all.includes('mock-shell ready'), `实际输出：${JSON.stringify(all)}`);
    assert.ok(all.includes('echo hi'), '输入应被回显');
  });

  // 无 Cookie 的握手必须被拒
  const badWs = new WSClient(`ws://127.0.0.1:${process.env.PORT}/api/terminal?sid=${sTerm.sid}`);
  await new Promise((resolve) => {
    const done = () => resolve();
    badWs.on('open', () => {
      badWs.close();
      done();
    });
    badWs.on('error', done);
    setTimeout(done, 1500);
  });

  test('没有会话 Cookie 的终端连接被拒绝', () => {
    assert.notStrictEqual(badWs.readyState, WSClient.OPEN, '未授权握手不应成功');
  });

  const shellExit = await new Promise((resolve) => {
    const timer = setTimeout(() => resolve(false), 3000);
    termWs.on('close', (code) => {
      clearTimeout(timer);
      resolve(code);
    });
    termWs.send(JSON.stringify({ t: 'in', d: Buffer.from('exit\n', 'utf8').toString('base64') }));
  });

  test('输入 exit 后 shell 正常结束并关闭连接', () => {
    assert.strictEqual(shellExit, 4000);
  });

  console.log('\n[会话被服务器断开]');

  const sLost = await call('/api/connect', {
    host: '127.0.0.1',
    port: mockPort,
    username: 'tester',
    identityFile: keyAPath,
  });

  // 让服务端把连接全部踢掉，模拟空闲超时 / 网络中断
  mockClients.forEach((c) => {
    try {
      c.end();
    } catch {}
  });
  await new Promise((r) => setTimeout(r, 400));

  const lostRes = await request('POST', '/api/delete', {
    sid: sLost.sid,
    fingerprints: [keyA.fingerprint],
  });

  test('连接断了之后再操作 → 409 + SESSION_LOST，提示重新连接', () => {
    assert.strictEqual(lostRes.status, 409);
    assert.strictEqual(lostRes.json.code, 'SESSION_LOST');
    assert.ok(lostRes.json.error.includes('连接已断开'), `实际提示：${lostRes.json.error}`);
    assert.ok(!/Unable to exec|No response from server/.test(lostRes.json.error), '不该把底层英文错误抛给用户');
  });

  console.log(`\n结果：${passed} 通过，${failed} 失败\n`);

  mockServer.close();
  await stopServer().catch(() => {});
  setTimeout(() => process.exit(failed ? 1 : 0), 200);
}

main().catch((e) => {
  console.error('\n端到端测试异常终止：', e.stack || e.message);
  setTimeout(() => process.exit(1), 100);
});
