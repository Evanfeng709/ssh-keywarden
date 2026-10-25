'use strict';
/**
 * 桌面模式的服务端行为：随机端口、不自动发 Cookie、Host 校验、扫描缓存。
 * 跑法：node tests/desktop-server.js
 */

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const keygen = require('../src/keygen');

const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-desktop-'));
const DATA = path.join(TMP, 'data');
const KEYS = path.join(TMP, 'OneDrive 密钥');
fs.mkdirSync(DATA, { recursive: true });
fs.mkdirSync(KEYS, { recursive: true });

process.env.KW_DATA_DIR = DATA;
fs.writeFileSync(
  path.join(DATA, 'config.json'),
  JSON.stringify({
    settings: { localKeyDir: KEYS, saveDir: KEYS, remotePath: '' },
    hosts: [{ id: 'desk', name: 'desk', host: '127.0.0.1', port: 22, username: 'desk', identityFile: '' }],
  })
);

const { startServer } = require('../server.js');

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

function request(port, method, urlPath, opts = {}) {
  return new Promise((resolve, reject) => {
    const headers = {};
    if (opts.cookie) headers.Cookie = opts.cookie;
    if (opts.host) headers.Host = opts.host;
    const req = http.request({ host: '127.0.0.1', port, path: urlPath, method, headers }, (res) => {
      let raw = '';
      res.on('data', (d) => (raw += d));
      res.on('end', () => {
        let json = null;
        try {
          json = JSON.parse(raw);
        } catch {
          /* 首页是 HTML */
        }
        resolve({ status: res.statusCode, headers: res.headers, json, raw });
      });
    });
    req.on('error', reject);
    req.end();
  });
}

async function main() {
  const info = await startServer({ port: 0, autoCookie: false, scanCacheMs: 60_000 });

  test('PORT=0 时拿到系统分配的可用端口', () => {
    assert.ok(Number.isInteger(info.port) && info.port > 0, `实际端口：${info.port}`);
  });

  const index = await request(info.port, 'GET', '/');
  test('桌面模式访问首页不自动下发 Cookie', () => {
    assert.strictEqual(index.status, 200);
    assert.strictEqual(index.headers['set-cookie'], undefined);
  });

  const unauth = await request(info.port, 'GET', '/api/state');
  test('没有注入 Cookie 时 API 返回 401', () => {
    assert.strictEqual(unauth.status, 401);
  });

  const cookie = `kw=${info.token}`;
  const authorized = await request(info.port, 'GET', '/api/state', { cookie });
  test('主进程注入令牌后 API 正常', () => {
    assert.strictEqual(authorized.status, 200);
    assert.strictEqual(authorized.json.ok, true);
    assert.strictEqual(authorized.json.data.settings.localKeyDir, KEYS);
  });

  const badHost = await request(info.port, 'GET', '/api/state', { cookie, host: 'evil.example' });
  test('Host 头不合法时拒绝（防 DNS rebinding）', () => {
    assert.strictEqual(badHost.status, 421);
  });

  const k1 = keygen.generateEd25519('one');
  fs.writeFileSync(path.join(KEYS, 'one.pub'), `${k1.publicKeyLine}\n`);
  const cachedEmpty = await request(info.port, 'GET', '/api/state', { cookie });
  test('60 秒缓存内新增文件不会立即出现', () => {
    assert.strictEqual(cachedEmpty.json.data.localKeys.entries.length, 0);
  });

  const forced1 = await request(info.port, 'GET', '/api/local-keys?force=1', { cookie });
  test('force=1 强制重扫能扫到新增公钥', () => {
    assert.strictEqual(forced1.json.data.entries.length, 1);
  });

  const afterForce = await request(info.port, 'GET', '/api/state', { cookie });
  test('强制重扫后缓存同步更新', () => {
    assert.strictEqual(afterForce.json.data.localKeys.entries.length, 1);
  });

  const k2 = keygen.generateEd25519('two');
  fs.writeFileSync(path.join(KEYS, 'two.pub'), `${k2.publicKeyLine}\n`);
  const cached2 = await request(info.port, 'GET', '/api/state', { cookie });
  test('缓存内第二次新增仍不可见（仍 1 条）', () => {
    assert.strictEqual(cached2.json.data.localKeys.entries.length, 1);
  });

  const forced2 = await request(info.port, 'GET', '/api/local-keys?force=1', { cookie });
  test('再次强制重扫返回 2 条', () => {
    assert.strictEqual(forced2.json.data.entries.length, 2);
  });

  await info.close();

  console.log(`\n结果：${passed} 通过，${failed} 失败\n`);
  fs.rmSync(TMP, { recursive: true, force: true });
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error('桌面服务端测试异常终止：', e.stack || e.message);
  fs.rmSync(TMP, { recursive: true, force: true });
  process.exit(1);
});
