'use strict';
/**
 * 纯 JS 断言测试（不依赖外部命令、不起子进程）
 * 跑法：node tests/run.js
 */

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');

const keygen = require('../src/keygen');
const { parseAuthorizedKeys, keyItems, renderWithout, appendKey } = require('../src/authorized');
const { scanKeys } = require('../src/localkeys');

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    passed++;
    console.log(`  ✓ ${name}`);
  } catch (e) {
    failed++;
    console.log(`  ✗ ${name}`);
    console.log(`      ${e.message}`);
  }
}

console.log('\n[keygen]');

const ed1 = keygen.generateEd25519('alice@laptop');
const ed2 = keygen.generateEd25519('bob@nas');
const rsa1 = keygen.generateRsa('legacy@server', 2048);

test('ed25519 生成：公钥行可解析且指纹一致', () => {
  const p = keygen.parsePublicKeyLine(ed1.publicKeyLine);
  assert.ok(p, '公钥行应能解析');
  assert.strictEqual(p.type, 'ssh-ed25519');
  assert.strictEqual(p.fingerprint, ed1.fingerprint);
  assert.strictEqual(p.comment, 'alice@laptop');
});

test('ed25519 私钥可被 ssh2 解析（OpenSSH 容器格式正确）', () => {
  const { utils } = require('ssh2');
  const k = utils.parseKey(ed1.privateKeyPem);
  assert.ok(!(k instanceof Error), `解析失败：${k.message}`);
  const key = Array.isArray(k) ? k[0] : k;
  assert.strictEqual(key.type, 'ssh-ed25519');
});

test('指纹格式为 SHA256: 且无 = 填充', () => {
  assert.match(ed1.fingerprint, /^SHA256:[A-Za-z0-9+/]{43}$/);
});

test('两次生成的密钥指纹不同', () => {
  assert.notStrictEqual(ed1.fingerprint, ed2.fingerprint);
});

test('RSA 生成：类型与指纹正确', () => {
  const p = keygen.parsePublicKeyLine(rsa1.publicKeyLine);
  assert.strictEqual(p.type, 'ssh-rsa');
  assert.strictEqual(p.fingerprint, rsa1.fingerprint);
  assert.strictEqual(p.valid, true);
});

console.log('\n[authorized_keys 解析]');

const mixed = [
  '# 这是注释，应当原样保留',
  '',
  ed1.publicKeyLine,
  `command="echo hi",no-pty ${ed2.publicKeyLine}`,
  'garbage-not-a-key',
  ed1.publicKeyLine, // 重复
].join('\n') + '\n';

const parsed = parseAuthorizedKeys(mixed);

test('行分类正确：注释/空行/密钥/无法解析', () => {
  const kinds = parsed.items.map((i) => i.kind);
  assert.deepStrictEqual(kinds, ['comment', 'blank', 'key', 'key', 'malformed', 'key']);
});

test('options 前缀能被摘出来', () => {
  const withOpt = parsed.items[3];
  assert.strictEqual(withOpt.options, 'command="echo hi",no-pty');
  assert.strictEqual(withOpt.fingerprint, ed2.fingerprint);
});

test('重复密钥被标 duplicateOf', () => {
  const keys = keyItems(parsed.items);
  assert.strictEqual(keys.length, 3);
  assert.strictEqual(keys[0].duplicateOf, null);
  assert.strictEqual(keys[2].duplicateOf, keys[0].index);
});

test('无法解析的行保留原因', () => {
  const bad = parsed.items.find((i) => i.kind === 'malformed');
  assert.ok(bad.reason.length > 0);
});

test('renderWithout 只删指定行，其余原样保留', () => {
  const text = renderWithout(parsed, [2, 5]);
  assert.ok(!text.includes('alice@laptop'), '被删的行不应出现');
  assert.ok(text.includes('# 这是注释'), '注释要保留');
  assert.ok(text.includes('garbage-not-a-key'), '无法解析的行要保留');
  assert.ok(text.includes('bob@nas'), '未选中的密钥要保留');
  assert.ok(text.endsWith('\n'), '文件应以换行结尾');
});

test('appendKey：新钥匙追加成功', () => {
  const base = parseAuthorizedKeys('# head\n');
  const r = appendKey(base, ed1.publicKeyLine);
  assert.strictEqual(r.appended, true);
  assert.ok(r.text.includes(ed1.publicKeyLine));
  assert.ok(r.text.startsWith('# head\n'));
});

test('appendKey：同一把钥匙不重复写入（幂等）', () => {
  const base = parseAuthorizedKeys(`${ed1.publicKeyLine}\n`);
  const r = appendKey(base, ed1.publicKeyLine);
  assert.strictEqual(r.appended, false);
  assert.ok(r.reason.includes('已在服务器上'));
});

test('appendKey：格式错误的公钥直接拒绝', () => {
  const base = parseAuthorizedKeys('');
  assert.throws(() => appendKey(base, 'not-a-key'), /无法识别|格式/);
});

test('空文件解析不报错', () => {
  const p = parseAuthorizedKeys('');
  assert.strictEqual(p.items.length, 0);
  assert.strictEqual(p.existed, false);
});

console.log('\n[本地密钥库扫描]');

test('扫描临时目录能找到私钥与公钥并配对指纹', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kk-test-'));
  fs.mkdirSync(path.join(dir, 'sub'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'id_test'), ed1.privateKeyPem);
  fs.writeFileSync(path.join(dir, 'id_test.pub'), `${ed1.publicKeyLine}\n`);
  fs.writeFileSync(path.join(dir, 'sub', 'only.pub'), `${ed2.publicKeyLine}\n`);
  fs.writeFileSync(path.join(dir, 'notes.txt'), 'hello world');

  const { entries, errors } = scanKeys(dir);
  const priv = entries.find((e) => e.name === 'id_test');
  const pub = entries.find((e) => e.name === 'id_test.pub');
  const onlyPub = entries.find((e) => e.name === 'only.pub');

  assert.ok(priv && priv.kind === 'private', '应识别私钥');
  assert.strictEqual(priv.encrypted, false);
  assert.strictEqual(priv.fingerprint, ed1.fingerprint, '私钥指纹应通过同名 .pub 匹配');
  assert.ok(pub && pub.kind === 'public');
  assert.strictEqual(pub.fingerprint, ed1.fingerprint);
  assert.ok(onlyPub && onlyPub.kind === 'public', '子目录要递归到');
  assert.strictEqual(onlyPub.fingerprint, ed2.fingerprint);
  assert.ok(!entries.some((e) => e.name === 'notes.txt'), '普通文本不应被当成密钥');
  assert.strictEqual(errors.length, 0);
});

test('扫描不存在的目录要报错而不是静默返回空', () => {
  const r = scanKeys(path.join(os.tmpdir(), 'definitely-not-exist-' + Date.now()));
  assert.strictEqual(r.entries.length, 0);
  assert.strictEqual(r.errors.length, 1);
});

console.log('\n[sshd 登录方式判定]');

const sshdMod = require('../src/sshd');

test('未配置时按 OpenSSH 默认值：密码登录开启', () => {
  const s = sshdMod.summarize(sshdMod.parseConfigText(''));
  assert.strictEqual(s.passwordAuth, true);
  assert.strictEqual(s.pubkeyAuth, true);
  assert.strictEqual(s.keyOnly, false);
});

test('PasswordAuthentication no 但键盘交互仍开着 → 不算仅密钥登录', () => {
  const s = sshdMod.summarize(sshdMod.parseConfigText('PasswordAuthentication no\n'));
  assert.strictEqual(s.passwordAuth, false);
  assert.strictEqual(s.kbdInteractive, true);
  assert.strictEqual(s.keyOnly, false, 'PAM 键盘交互仍可能用密码登录');
});

test('三条密码通道全关才算仅密钥登录', () => {
  const text = [
    'PasswordAuthentication no',
    'KbdInteractiveAuthentication no',
    'ChallengeResponseAuthentication no',
  ].join('\n');
  const s = sshdMod.summarize(sshdMod.parseConfigText(text));
  assert.strictEqual(s.keyOnly, true);
  assert.strictEqual(s.pubkeyAuth, true);
});

test('注释行不生效', () => {
  const s = sshdMod.summarize(sshdMod.parseConfigText('#PasswordAuthentication no\n'));
  assert.strictEqual(s.passwordAuth, true);
});

test('大小写与多余空格都能识别', () => {
  const s = sshdMod.summarize(sshdMod.parseConfigText('  PASSWORDauthentication    NO  \n'));
  assert.strictEqual(s.passwordAuth, false);
});

test('同一文件内首次出现的值生效', () => {
  const s = sshdMod.summarize(
    sshdMod.parseConfigText('PasswordAuthentication no\nPasswordAuthentication yes\n')
  );
  assert.strictEqual(s.passwordAuth, false);
});

test('关掉公钥登录时不会误判为仅密钥登录', () => {
  const s = sshdMod.summarize(
    sshdMod.parseConfigText(
      'PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication no\n'
    )
  );
  assert.strictEqual(s.pubkeyAuth, false);
  assert.strictEqual(s.keyOnly, false);
});

test('四态判定：both / key-only / password-only / none', () => {
  const st = (text) => sshdMod.summarize(sshdMod.parseConfigText(text)).state;

  assert.strictEqual(st(''), 'both', '默认两种都能登');
  assert.strictEqual(
    st('PasswordAuthentication no\nKbdInteractiveAuthentication no\n'),
    'key-only',
    '只留公钥 = 仅密钥'
  );
  assert.strictEqual(
    st('PubkeyAuthentication no\n'),
    'password-only',
    '关掉公钥但密码还在 = 仅密码'
  );
  assert.strictEqual(
    st('PubkeyAuthentication no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n'),
    'none',
    '两个都关 ≠ 仅密钥'
  );
});

console.log('\n[加密私钥（passphrase）]');

test('ed25519 带 passphrase：能用密码解析、无密码解析失败', () => {
  const { utils } = require('ssh2');
  const k = keygen.generateEd25519('enc@test', 'test-pass-123');
  assert.strictEqual(k.encrypted, true);

  const withPass = utils.parseKey(k.privateKeyPem, 'test-pass-123');
  const key = Array.isArray(withPass) ? withPass[0] : withPass;
  assert.ok(!(key instanceof Error), `解析失败：${key.message}`);
  assert.strictEqual(key.type, 'ssh-ed25519');
  assert.strictEqual(keygen.fingerprintOfBlob(key.getPublicSSH()), k.fingerprint);

  const noPass = utils.parseKey(k.privateKeyPem);
  const bad = Array.isArray(noPass) ? noPass[0] : noPass;
  assert.ok(bad instanceof Error, '没给 passphrase 时必须解析失败');
});

test('加密私钥的容器声明为 aes256-ctr + bcrypt', () => {
  const k = keygen.generateEd25519('enc2@test', 'p');
  const body = k.privateKeyPem.split('\n').slice(1, -2).join('');
  const buf = Buffer.from(body, 'base64');
  assert.ok(buf.subarray(0, 15).toString() === 'openssh-key-v1\0');
  assert.ok(buf.includes(Buffer.from('aes256-ctr')), '应声明 aes256-ctr');
  assert.ok(buf.includes(Buffer.from('bcrypt')), '应声明 bcrypt KDF');
});

test('RSA 带 passphrase 也能加密并通过 ssh2 回读', () => {
  const { utils } = require('ssh2');
  const r = keygen.generateRsa('enc-rsa@test', 2048, 'rsa-pass');
  assert.strictEqual(r.encrypted, true);
  const parsed = utils.parseKey(r.privateKeyPem, 'rsa-pass');
  const key = Array.isArray(parsed) ? parsed[0] : parsed;
  assert.ok(!(key instanceof Error), `解析失败：${key.message}`);
  assert.strictEqual(key.type, 'ssh-rsa');
});

console.log(`\n结果：${passed} 通过，${failed} 失败\n`);
process.exit(failed ? 1 : 0);
