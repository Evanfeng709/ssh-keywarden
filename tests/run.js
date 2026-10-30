'use strict';
/**
 * 纯 JS 断言测试（不依赖外部命令、不起子进程）
 * 跑法：node tests/run.js
 */

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');

// 指纹库等模块在 require 时解析数据目录，必须先指到临时目录再加载
const TEST_DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-test-'));
process.env.KW_DATA_DIR = TEST_DATA_DIR;

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
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-test-'));
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

console.log('\n[主机指纹 known_hosts]');

const knownhosts = require('../src/knownhosts');

function catchErr(fn) {
  try {
    fn();
    return null;
  } catch (e) {
    return e;
  }
}
const blobOf = (k) => Buffer.from(k.publicKeyLine.split(/\s+/)[1], 'base64');

const hostK1 = keygen.generateEd25519('host-1');
const hostK2 = keygen.generateEd25519('host-2');

test('指纹格式与 keygen 一致（SHA256:base64，无 = 填充）', () => {
  const fp = knownhosts.fingerprintOfBlob(blobOf(hostK1));
  assert.strictEqual(fp, hostK1.fingerprint);
  assert.match(fp, /^SHA256:[A-Za-z0-9+/]{43}$/);
  assert.strictEqual(knownhosts.keyTypeOfBlob(blobOf(hostK1)), 'ssh-ed25519');
});

test('首次连接未确认 → HOSTKEY_UNKNOWN，带指纹详情', () => {
  const e = catchErr(() => knownhosts.verify({ host: 'unknown.example', port: 22, blob: blobOf(hostK1) }));
  assert.ok(e, '应抛错');
  assert.strictEqual(e.code, 'HOSTKEY_UNKNOWN');
  assert.strictEqual(e.details.fingerprint, hostK1.fingerprint);
  assert.strictEqual(knownhosts.get('unknown.example', 22), null, '未被信任时不能落盘');
});

test('确认指纹后才写入；再次连接为 match', () => {
  const blob = blobOf(hostK1);
  const first = knownhosts.verify({ host: 'trust.example', port: 2222, blob, trustFingerprint: hostK1.fingerprint });
  assert.strictEqual(first.status, 'trusted');
  assert.ok(knownhosts.get('trust.example', 2222), '确认后应写入指纹库');
  const second = knownhosts.verify({ host: 'trust.example', port: 2222, blob });
  assert.strictEqual(second.status, 'match');
});

test('主机密钥变化 → HOSTKEY_CHANGED，且带旧指纹', () => {
  const e = catchErr(() => knownhosts.verify({ host: 'trust.example', port: 2222, blob: blobOf(hostK2) }));
  assert.ok(e, '应抛错');
  assert.strictEqual(e.code, 'HOSTKEY_CHANGED');
  assert.strictEqual(e.details.previousFingerprint, hostK1.fingerprint);
  assert.strictEqual(e.details.fingerprint, hostK2.fingerprint);
});

test('确认变化后覆盖记录，旧指纹失效', () => {
  const blob = blobOf(hostK2);
  const res = knownhosts.verify({
    host: 'trust.example',
    port: 2222,
    blob,
    trustFingerprint: hostK2.fingerprint,
  });
  assert.strictEqual(res.status, 'updated');
  assert.strictEqual(res.previous.fingerprint, hostK1.fingerprint);
  assert.strictEqual(knownhosts.get('trust.example', 2222).fingerprint, hostK2.fingerprint);
});

test('主机名大小写与默认端口归一', () => {
  knownhosts.save('Case.Example', 22, blobOf(hostK1));
  assert.ok(knownhosts.get('case.example', 22));
  assert.ok(knownhosts.get('CASE.EXAMPLE'));
});

console.log('\n[本地钥匙备注]');

const store = require('../src/store');

test('备注保存：换行/制表符归一化为空格并落盘', () => {
  const notes = store.setKeyNote(ed1.fingerprint, '  张三的 MacBook\n\t备用  ');
  assert.strictEqual(notes[ed1.fingerprint].note, '张三的 MacBook 备用');
  assert.ok(notes[ed1.fingerprint].updatedAt > 0);
  const persisted = store.listKeyNotes();
  assert.strictEqual(persisted[ed1.fingerprint].note, '张三的 MacBook 备用');
});

test('备注最多 200 字，空备注等于删除', () => {
  assert.throws(() => store.setKeyNote(ed1.fingerprint, 'x'.repeat(201)), /最多 200/);
  const notes = store.setKeyNote(ed1.fingerprint, '');
  assert.ok(!(ed1.fingerprint in notes), '空备注应删除条目');
  assert.ok(!(ed1.fingerprint in store.listKeyNotes()), '删除也要落盘');
});

test('备注拒绝非法指纹', () => {
  assert.throws(() => store.setKeyNote('not-a-fingerprint', 'x'), /指纹格式/);
  assert.throws(() => store.setKeyNote('', 'x'), /指纹格式/);
});

test('备注文件里的非法条目会被过滤，不影响其余备注', () => {
  fs.writeFileSync(
    store.NOTES_PATH,
    JSON.stringify({
      version: 1,
      notes: {
        bad: { note: '没有合法指纹' },
        [ed2.fingerprint]: { note: '  正常备注  ' },
        [rsa1.fingerprint]: { note: '   ' },
      },
    })
  );
  const notes = store.listKeyNotes();
  assert.deepStrictEqual(Object.keys(notes), [ed2.fingerprint]);
  assert.strictEqual(notes[ed2.fingerprint].note, '正常备注');
});

test('备注文件损坏时备份坏文件并按空表继续', () => {
  fs.writeFileSync(store.NOTES_PATH, '{ broken json', 'utf8');
  const notes = store.listKeyNotes();
  assert.deepStrictEqual(notes, {});
  const badFiles = fs.readdirSync(TEST_DATA_DIR).filter((n) => n.startsWith('key-notes.json.bad-'));
  assert.ok(badFiles.length >= 1, '应保留坏文件备份，方便排查');
  fs.rmSync(store.NOTES_PATH, { force: true });
});

console.log('\n[备份列表与差异]');

const backupsMod = require('../src/backups');
const RP = '/home/alice/.ssh/authorized_keys';

test('备份名解析：合法时间戳按年月日时分秒展示', () => {
  const p = backupsMod.parseBackupName(RP, 'authorized_keys.bak.20261022093015');
  assert.strictEqual(p.stamp, '20261022093015');
  assert.strictEqual(p.displayTime, '2026-10-22 09:30:15');
  assert.strictEqual(backupsMod.displayTime('20261022093015'), '2026-10-22 09:30:15');
});

test('备份名拒绝路径穿越、错误前缀与非法时间戳', () => {
  const bads = [
    '../authorized_keys.bak.20261022093015',
    'other.bak.20261022093015',
    'authorized_keys.bak.2026102209301',
    'authorized_keys.bak.20261022093015/../x',
    'authorized_keys.bak.2026102209301a',
    'authorized_keys.bak.20261022093015.bak.20261022093015',
    'C:\\authorized_keys.bak.20261022093015',
    '',
  ];
  for (const bad of bads) {
    assert.strictEqual(backupsMod.parseBackupName(RP, bad), null, `应拒绝：${bad}`);
  }
});

test('backupPathFor 只按 remotePath + 合法后缀重新拼路径', () => {
  assert.strictEqual(
    backupsMod.backupPathFor(RP, 'authorized_keys.bak.20261022093015'),
    `${RP}.bak.20261022093015`
  );
  assert.throws(
    () => backupsMod.backupPathFor(RP, '../authorized_keys.bak.20261022093015'),
    /不合法/
  );
});

test('差异方向：added = 恢复后会出现的钥匙，removed = 会消失的钥匙', () => {
  const cur = [ed1.publicKeyLine, ed2.publicKeyLine, 'garbage-line', ed1.publicKeyLine].join('\n') + '\n';
  const bak = ['# 备份里的注释', ed2.publicKeyLine, rsa1.publicKeyLine, 'garbage-line'].join('\n') + '\n';
  const d = backupsMod.diffBackup(cur, bak, ed2.fingerprint);

  assert.strictEqual(d.currentKeyCount, 2, '当前重复的公钥只算一把');
  assert.strictEqual(d.backupKeyCount, 2);
  assert.strictEqual(d.sameCount, 1);
  assert.deepStrictEqual(d.added.map((k) => k.fingerprint), [rsa1.fingerprint]);
  assert.deepStrictEqual(d.removed.map((k) => k.fingerprint), [ed1.fingerprint]);
  assert.strictEqual(d.currentMalformed, 1);
  assert.strictEqual(d.backupMalformed, 1);
  assert.strictEqual(d.emptyBackup, false);
  assert.strictEqual(d.usedKeyInBackup, true);
});

test('差异识别空备份与登录钥匙缺失', () => {
  const cur = `${keygen.generateEd25519('lonely').publicKeyLine}\n`;
  const empty = backupsMod.diffBackup(cur, '# 只有注释\n', ed1.fingerprint);
  assert.strictEqual(empty.emptyBackup, true);
  assert.strictEqual(empty.usedKeyInBackup, false);

  const missing = backupsMod.diffBackup(cur, ed2.publicKeyLine + '\n', ed1.fingerprint);
  assert.strictEqual(missing.usedKeyInBackup, false);

  const noKeyAuth = backupsMod.diffBackup(cur, ed2.publicKeyLine + '\n', '');
  assert.strictEqual(noKeyAuth.usedKeyInBackup, null, '密码会话无法判定');
});

console.log('\n[桌面数据目录]');

const desktopData = require('../electron/data-dir');
const DDTMP = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-datadir-'));

test('KW_DATA_DIR 优先于 userData', () => {
  const custom = path.join(DDTMP, 'custom');
  assert.strictEqual(
    desktopData.resolveDataDir({ userData: path.join(DDTMP, 'u'), env: { KW_DATA_DIR: custom } }),
    path.resolve(custom)
  );
});

test('未设置 KW_DATA_DIR 时落到 userData/data', () => {
  assert.strictEqual(
    desktopData.resolveDataDir({ userData: path.join(DDTMP, 'u'), env: {} }),
    path.join(DDTMP, 'u', 'data')
  );
});

test('空目标目录会复制三个数据文件，跳过无关文件', () => {
  const src = path.join(DDTMP, 'old');
  fs.mkdirSync(src, { recursive: true });
  for (const f of ['config.json', 'known_hosts.json', 'key-notes.json', 'junk.txt']) {
    fs.writeFileSync(path.join(src, f), '{}');
  }
  const dst = path.join(DDTMP, 'new');
  const result = desktopData.migrateData(dst, [src]);
  assert.strictEqual(result.migrated, true);
  assert.strictEqual(result.from, src);
  assert.deepStrictEqual(result.copied.sort(), ['config.json', 'key-notes.json', 'known_hosts.json']);
  assert.ok(fs.existsSync(path.join(dst, 'config.json')));
  assert.ok(!fs.existsSync(path.join(dst, 'junk.txt')), '无关文件不应复制');
});

test('目标已有 config.json 时不覆盖', () => {
  const src = path.join(DDTMP, 'old2');
  const dst = path.join(DDTMP, 'existing');
  fs.mkdirSync(src, { recursive: true });
  fs.mkdirSync(dst, { recursive: true });
  fs.writeFileSync(path.join(src, 'config.json'), '{"from":"src"}');
  fs.writeFileSync(path.join(dst, 'config.json'), '{"from":"dst"}');
  const result = desktopData.migrateData(dst, [src]);
  assert.strictEqual(result.migrated, false);
  assert.strictEqual(fs.readFileSync(path.join(dst, 'config.json'), 'utf8'), '{"from":"dst"}');
});

test('候选目录没有 config.json 时跳过', () => {
  const empty = path.join(DDTMP, 'empty');
  fs.mkdirSync(empty, { recursive: true });
  fs.writeFileSync(path.join(empty, 'known_hosts.json'), '{}');
  const result = desktopData.migrateData(path.join(DDTMP, 'fresh'), [empty]);
  assert.strictEqual(result.migrated, false);
  assert.strictEqual(result.copied.length, 0);
});

console.log('\n[版本一致性]');

const version = require('../src/version');
const pkg = require('../package.json');
const lock = require('../package-lock.json');
const changelog = fs.readFileSync(path.join(__dirname, '..', 'CHANGELOG.md'), 'utf8');

test('APP_VERSION 符合 vYYYY.MM.DD.N', () => {
  assert.match(version.APP_VERSION, /^v\d{4}\.\d{2}\.\d{2}\.\d+$/);
});

test('package.json / package-lock 与 version.js 同步', () => {
  assert.strictEqual(pkg.version, version.APP_VERSION_SEMVER);
  assert.strictEqual(lock.version, version.APP_VERSION_SEMVER);
  assert.strictEqual(lock.packages[''].version, version.APP_VERSION_SEMVER);
  assert.strictEqual(pkg.name, 'ssh-keywarden');
  assert.strictEqual(lock.packages[''].name, pkg.name);
  // engines 容易只改 package.json 忘了 lock（lock 是 npm install 时从 package.json 抄的）
  assert.deepStrictEqual(lock.packages[''].engines, pkg.engines, 'package-lock 的 engines 必须与 package.json 一致');
});

test('CHANGELOG 有当前版本条目', () => {
  assert.ok(changelog.includes(`## ${version.APP_VERSION} `), 'CHANGELOG 缺少当前版本条目');
});

test('锁文件全部指向官方 npm registry', () => {
  const raw = fs.readFileSync(path.join(__dirname, '..', 'package-lock.json'), 'utf8');
  assert.ok(!/registry\.npmmirror\.com/.test(raw), '锁文件里残留镜像源地址');
});

console.log(`\n结果：${passed} 通过，${failed} 失败\n`);
try {
  fs.rmSync(TEST_DATA_DIR, { recursive: true, force: true });
  fs.rmSync(DDTMP, { recursive: true, force: true });
} catch {}
process.exit(failed ? 1 : 0);
