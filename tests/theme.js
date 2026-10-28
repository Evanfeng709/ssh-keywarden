'use strict';
/**
 * 主题与操作按钮的静态校验：令牌完整性、对比度、无硬编码颜色、注册表与 CSS 对得上。
 * 跑法：node tests/theme.js
 */

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const CSS = fs.readFileSync(path.join(ROOT, 'public', 'style.css'), 'utf8');
const THEME_JS = fs.readFileSync(path.join(ROOT, 'public', 'js', 'theme.js'), 'utf8');
const HTML = fs.readFileSync(path.join(ROOT, 'public', 'index.html'), 'utf8');
const MAIN = fs.readFileSync(path.join(ROOT, 'electron', 'main.js'), 'utf8');
const PRELOAD = fs.readFileSync(path.join(ROOT, 'electron', 'preload.js'), 'utf8');

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

/* ---------------------------- 解析 ---------------------------- */

function parseThemeBlocks(css) {
  const blocks = {};
  let cur = null;
  for (const line of css.split('\n')) {
    const head = line.match(/^(?::root,\s*\[data-theme="obsidian"\]|\[data-theme="([a-z-]+)"\])\s*\{/);
    if (head) {
      cur = head[1] || 'obsidian';
      blocks[cur] = {};
      continue;
    }
    if (cur && /^\}/.test(line)) {
      cur = null;
      continue;
    }
    if (cur) {
      const m = line.match(/^\s*(--[a-z0-9-]+):\s*([^;]+);/);
      if (m) blocks[cur][m[1]] = m[2].trim();
    }
  }
  return blocks;
}

function luminance(hex) {
  const h = hex.replace('#', '');
  const full = h.length === 3 ? h.split('').map((c) => c + c).join('') : h;
  const rgb = [0, 2, 4]
    .map((i) => parseInt(full.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)));
  return 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
}

function contrast(a, b) {
  const l1 = luminance(a);
  const l2 = luminance(b);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

const blocks = parseThemeBlocks(CSS);
const themeIds = Object.keys(blocks);
const base = blocks.obsidian;

/* 每套主题必须覆盖的令牌（mono / radius / win-close 可继承） */
const REQUIRED = Object.keys(base).filter(
  (k) => !['--mono', '--radius', '--win-close'].includes(k)
);

console.log('\n[主题令牌]');

test('注册表包含 6 套手动主题 + 2 套自动主题', () => {
  for (const id of ['obsidian', 'midnight', 'graphite', 'cloud', 'linen', 'mist']) {
    assert.ok(THEME_JS.includes(`id: '${id}'`), `theme.js 缺少主题 ${id}`);
  }
  for (const id of ['auto', 'auto-soft']) {
    assert.ok(THEME_JS.includes(`id: '${id}'`), `theme.js 缺少自动主题 ${id}`);
  }
  assert.deepStrictEqual(
    themeIds.sort(),
    ['cloud', 'graphite', 'linen', 'midnight', 'mist', 'obsidian'],
    `CSS 主题块：${themeIds.join(', ')}`
  );
});

test('每套主题都定义了全部颜色令牌', () => {
  for (const id of themeIds) {
    const missing = REQUIRED.filter((k) => !(k in blocks[id]));
    assert.strictEqual(missing.length, 0, `${id} 缺少：${missing.join(', ')}`);
  }
});

test('令牌值不引用自身或其它变量', () => {
  const skip = new Set(['--mono', '--radius']);
  for (const id of themeIds) {
    for (const [k, v] of Object.entries(blocks[id])) {
      if (skip.has(k)) continue;
      assert.ok(!v.includes('var(--'), `${id} ${k} 引用了变量：${v}`);
      assert.match(v, /^(#[0-9a-fA-F]{3,8}$|rgba\(|[\d-])/, `${id} ${k}=${v}`);
    }
  }
});

test('自动主题映射到存在的具体主题', () => {
  const pairMatches = [...THEME_JS.matchAll(/pair:\s*\[([^\]]+)\]/g)].map((m) =>
    m[1].split(',').map((s) => s.trim().replace(/'/g, ''))
  );
  assert.strictEqual(pairMatches.length, 2, '应有 2 套自动主题');
  const ids = new Set(themeIds);
  for (const pair of pairMatches) {
    assert.strictEqual(pair.length, 2);
    pair.forEach((id) => assert.ok(ids.has(id), `自动主题引用了不存在的 ${id}`));
  }
});

test('首帧脚本与注册表的自动主题一致', () => {
  const m = HTML.match(/var auto = \{([^}]+)\}/);
  assert.ok(m, 'index.html 缺少首帧主题脚本');
  const ids = new Set(themeIds);
  for (const id of themeIds) assert.ok(HTML.includes(`'${id}'`), `首帧脚本缺少 ${id}`);
  for (const auto of ['auto', 'auto-soft']) assert.ok(m[1].includes(`'${auto}'`), `首帧脚本缺少 ${auto}`);
  const pairs = [...m[1].matchAll(/'([a-z-]+)':\s*\['([a-z-]+)',\s*'([a-z-]+)'\]/g)];
  assert.strictEqual(pairs.length, 2);
  for (const p of pairs) {
    assert.ok(ids.has(p[2]) && ids.has(p[3]), `首帧映射指向未知主题：${p[0]}`);
  }
});

test('组件区不再有硬编码颜色', () => {
  const marker = CSS.indexOf('   组件');
  assert.ok(marker > 0, '找不到组件分隔注释');
  const component = CSS.slice(marker);
  const hex = component.match(/#[0-9a-fA-F]{3,8}\b/g) || [];
  assert.strictEqual(hex.length, 0, `组件区仍有硬编码颜色：${[...new Set(hex)].join(', ')}`);
});

console.log('\n[对比度]');

const PAIRS = [
  ['正文', '--text', '--panel', 4.5],
  ['正文 / 底色', '--text', '--bg', 4.5],
  ['次要文字', '--text-dim', '--panel', 4.5],
  ['弱化文字', '--text-faint', '--panel', 3],
  ['输入占位符', '--text-placeholder', '--input-bg', 3],
  ['输入框正文', '--text', '--input-bg', 4.5],
  ['主按钮文字', '--on-accent', '--accent', 3],
  ['强调按钮', '--accent-text', '--accent-soft', 4.5],
  ['选中行文字', '--accent-text-strong', '--accent-selected', 4.5],
  ['危险按钮', '--danger-text', '--danger-soft', 4.5],
  ['危险提示', '--danger-text-strong', '--danger-soft', 4.5],
  ['成功徽章', '--ok-text', '--ok-soft', 4.5],
  ['警告徽章', '--warn-text', '--warn-soft', 4.5],
  ['普通徽章', '--badge-text', '--badge-soft', 4.5],
  ['计数胶囊', '--accent-text-strong', '--pill-bg', 4.5],
  ['危险计数', '--danger-text-strong', '--danger-soft', 4.5],
];

for (const id of themeIds) {
  test(`${id}：文字与控件对比度达标`, () => {
    const t = { ...base, ...blocks[id] };
    const bad = [];
    for (const [label, a, b, min] of PAIRS) {
      const v = contrast(t[a], t[b]);
      if (v < min) bad.push(`${label} ${v.toFixed(2)} < ${min}`);
    }
    assert.strictEqual(bad.length, 0, bad.join('；'));
  });
}

console.log('\n[按钮与系统跟随]');

test('操作按钮规格：28px 高、13px 图标、7px 圆角', () => {
  const block = CSS.match(/\.act-btn \{([\s\S]*?)\n\}/);
  assert.ok(block, '缺少 .act-btn');
  assert.match(block[1], /height:\s*28px/);
  assert.match(block[1], /border-radius:\s*7px/);
  assert.match(CSS, /\.act-btn svg \{[^}]*width:\s*13px/);
  assert.match(CSS, /\.act-sep \{[^}]*height:\s*14px/);
});

test('删除按钮只在有选中时变红', () => {
  const keys = fs.readFileSync(path.join(ROOT, 'public', 'js', 'keys.js'), 'utf8');
  assert.match(keys, /classList\.toggle\('is-danger', n > 0\)/);
  assert.match(CSS, /\.act-btn\.is-danger \{[\s\S]*?--danger-soft/);
});

test('桌面版提供系统深浅色通道', () => {
  assert.match(MAIN, /nativeTheme\.shouldUseDarkColors/);
  assert.match(MAIN, /nativeTheme\.on\('updated'/);
  assert.match(PRELOAD, /systemTheme: \{/);
  assert.match(PRELOAD, /kw:system-theme-changed/);
});

test('桌面版主题写进数据目录而不是 localStorage', () => {
  assert.match(MAIN, /theme\.json/);
  assert.match(MAIN, /ipcMain\.on\('kw:theme-get'/);
  assert.match(MAIN, /ipcMain\.handle\('kw:theme-set'/);
  assert.match(PRELOAD, /initial: ipcRenderer\.sendSync\('kw:theme-get'\)/);
  assert.match(PRELOAD, /set: \(choice\) => ipcRenderer\.invoke\('kw:theme-set', choice\)/);
  assert.match(THEME_JS, /window\.keywarden\.theme/);
  assert.match(THEME_JS, /bridge\.set\(choice\)/);
  assert.match(HTML, /window\.keywarden\.theme\.initial/);
});

test('终端按主题取色并能热切换', () => {
  const term = fs.readFileSync(path.join(ROOT, 'public', 'js', 'terminal.js'), 'utf8');
  assert.match(term, /termPalette\(resolvedTheme\(\)\)/);
  assert.match(term, /onThemeChange\(/);
  assert.ok(!/#131920|#d7e0ea/.test(term), '终端仍残留写死的旧配色');
});

test('下拉只保存选择，不把 auto 当具体主题写进 DOM', () => {
  assert.match(THEME_JS, /localStorage\.setItem\(THEME_KEY, choice\)/);
  assert.match(THEME_JS, /document\.documentElement\.dataset\.theme = resolved/);
  assert.ok(!/dataset\.theme = choice/.test(THEME_JS));
});

console.log(`\n结果：${passed} 通过，${failed} 失败\n`);
process.exit(failed ? 1 : 0);
