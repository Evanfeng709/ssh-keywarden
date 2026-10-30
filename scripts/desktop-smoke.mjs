// 桌面壳冒烟：启动 kw-desktop，用 WebView2 的 CDP 通道在页面里验证桌面桥，
// 然后正常关窗，确认窗口状态落盘。不连 SSH、不碰真实数据目录。
//
// 用法：node scripts/desktop-smoke.mjs [--exe target/debug/kw-desktop.exe]

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
const exeArg = args.indexOf('--exe');
const exe = path.resolve(exeArg >= 0 ? args[exeArg + 1] : 'target/debug/kw-desktop.exe');
const debugPort = 9333;

const checks = [];
function check(name, ok, detail = '') {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ' — ' + detail : ''}`);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForTarget(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json();
      const page = list.find((t) => t.type === 'page' && t.url.startsWith('http://127.0.0.1:'));
      if (page) return page;
    } catch {
      /* 端口还没起来 */
    }
    await sleep(200);
  }
  throw new Error('等待页面超时');
}

async function evaluate(wsUrl, expression) {
  const ws = new WebSocket(wsUrl);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('CDP 超时')), 20000);
    ws.addEventListener('open', () => {
      ws.send(
        JSON.stringify({
          id: 1,
          method: 'Runtime.evaluate',
          params: { expression, awaitPromise: true, returnByValue: true },
        })
      );
    });
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id !== 1) return;
      clearTimeout(timer);
      ws.close();
      if (msg.result?.exceptionDetails) {
        const d = msg.result.exceptionDetails;
        const desc = d.exception?.description || d.exception?.value || d.text;
        reject(new Error('页面脚本抛错：' + String(desc).split('\n')[0]));
      } else {
        resolve(msg.result?.result?.value);
      }
    });
    ws.addEventListener('error', () => {
      clearTimeout(timer);
      reject(new Error('CDP 连接失败'));
    });
  });
}

async function waitExit(child, timeoutMs) {
  return Promise.race([
    new Promise((r) => child.once('exit', () => r(true))),
    sleep(timeoutMs).then(() => false),
  ]);
}

async function main() {
  if (!fs.existsSync(exe)) throw new Error(`找不到可执行文件：${exe}`);

  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-desktop-smoke-'));
  const keys = path.join(dataDir, 'keys');
  fs.mkdirSync(keys);
  fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({
    settings: { localKeyDir: keys, saveDir: keys },
    hosts: [{ id: 'test-tombstone', deletedAt: 1 }],
  }));
  fs.writeFileSync(path.join(dataDir, 'theme.json'), JSON.stringify({ choice: 'graphite' }));

  const child = spawn(exe, [], {
    env: {
      ...process.env,
      KW_DATA_DIR: dataDir,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort}`,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.on('data', (b) => (stderr += b.toString()));
  child.stdout.on('data', () => {});

  try {
    const page = await waitForTarget(30000);
    const run = (expr) => evaluate(page.webSocketDebuggerUrl, expr);

    // 目标一出现页面脚本可能还没跑完，等桥接和首屏渲染就位再断言
    let ready = false;
    for (let i = 0; i < 100 && !ready; i++) {
      ready =
        (await run(
          `document.readyState === 'complete' && !!window.keywarden && document.getElementById('version').textContent !== 'v—'`
        )) === true;
      if (!ready) await sleep(200);
    }
    check('首屏脚本就位', ready);

    const boot = JSON.parse(
      await run(`(() => {
        const bar = document.querySelector('.topbar');
        return JSON.stringify({
          inner: [innerWidth, innerHeight],
          desktopClass: document.body.classList.contains('desktop'),
          bridge: !!window.keywarden && window.keywarden.isDesktop,
          theme: document.documentElement.dataset.theme,
          version: document.getElementById('version').textContent,
          overflow: bar.scrollWidth > bar.clientWidth,
          drag: document.querySelector('.topbar').getAttribute('data-tauri-drag-region'),
          winControls: document.querySelectorAll('.win-controls .win-btn').length,
        });
      })()`)
    );

    check('页面有尺寸', boot.inner[0] > 900 && boot.inner[1] > 600, `${boot.inner.join('x')}`);
    check('识别为桌面版', boot.desktopClass && boot.bridge);
    check('主题来自数据目录', boot.theme === 'graphite', boot.theme);
    check('版本号已渲染', /^\d+\.\d+\.\d+/.test(boot.version), boot.version);
    check('顶栏不溢出', !boot.overflow);
    check('顶栏可拖动', boot.drag === 'deep');
    check('窗口按钮齐全', boot.winControls === 3);

    // 会话令牌：初始化脚本注入 → 落 Cookie → 从 window 上消失；服务端不再自动发 Cookie
    const auth = JSON.parse(
      await run(`(async () => {
        const r = await fetch('/api/state', { credentials: 'same-origin' });
        const j = await r.json();
        return JSON.stringify({
          status: r.status,
          ok: j.ok === true,
          hasCookie: /(?:^|; )kw=/.test(document.cookie),
          tokenGone: !window.__KW_INIT || !('token' in window.__KW_INIT),
          version: j.data && j.data.version,
        });
      })()`)
    );
    check('会话令牌落 Cookie', auth.hasCookie);
    check('令牌不留在 window.__KW_INIT', auth.tokenGone);
    check('/api/state 可读', auth.status === 200 && auth.ok, String(auth.status));
    check('版本贯通到页面', typeof auth.version === 'string' && auth.version.length > 0, auth.version || '');

    const sysTheme = await run(`window.keywarden.systemTheme.get()`);
    check('系统深浅色返回布尔值', typeof sysTheme === 'boolean', String(sysTheme));

    // 不带 Cookie 直接请求首页：桌面端必须不再 Set-Cookie，否则本机扫端口的进程也能白拿会话
    const indexRes = await fetch(page.url, { redirect: 'manual' });
    const setCookies = indexRes.headers.getSetCookie ? indexRes.headers.getSetCookie() : [];
    check('首页不再自动下发 Cookie', setCookies.length === 0);

    const token = await run(`(async () => {
      const kw = window.keywarden;
      await kw.writeClipboard('kw-smoke-' + Date.now());
      return kw.readClipboard();
    })()`);
    check('剪贴板往返', /^kw-smoke-\d+$/.test(token));

    const themeSet = await run(`window.keywarden.theme.set('linen')`);
    check('主题写入', themeSet === 'linen');
    check(
      '主题落盘',
      JSON.parse(fs.readFileSync(path.join(dataDir, 'theme.json'), 'utf8')).choice === 'linen'
    );

    const uiRaw = await run(`window.keywarden.ui.set({ mode: 'term', termHeight: 400 })`);
    const ui = typeof uiRaw === 'string' ? JSON.parse(uiRaw) : uiRaw;
    check('视图写入', ui.mode === 'term' && ui.termHeight === 400);
    check(
      '视图落盘',
      JSON.parse(fs.readFileSync(path.join(dataDir, 'ui.json'), 'utf8')).mode === 'term'
    );

    const maxRaw = await run(`(async () => {
      const kw = window.keywarden;
      const wait = (ms) => new Promise((r) => setTimeout(r, ms));
      document.getElementById('winMax').click();
      await wait(600);
      const on = { max: await kw.windowControls.isMaximized(), cls: document.body.classList.contains('maximized') };
      document.getElementById('winMax').click();
      await wait(600);
      const off = { max: await kw.windowControls.isMaximized(), cls: document.body.classList.contains('maximized') };
      return JSON.stringify({ on, off });
    })()`);
    const max = typeof maxRaw === 'string' ? JSON.parse(maxRaw) : maxRaw;
    check(
      '最大化按钮双向生效',
      max.on.max && max.on.cls && !max.off.max && !max.off.cls
    );

    await run(`window.keywarden.windowControls.close()`);
    const exited = await waitExit(child, 15000);
    check('关窗后进程退出', exited);

    await sleep(500);
    const statePath = path.join(dataDir, 'window-state.json');
    const saved = fs.existsSync(statePath);
    check('窗口状态落盘', saved);
    if (saved) {
      const state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
      check(
        '窗口状态含缩放比',
        state.width > 0 && state.height > 0 && state.scale > 0,
        `${state.width}x${state.height}@${state.scale}`
      );
    }
  } finally {
    if (child.exitCode === null) child.kill();
    fs.rmSync(dataDir, { recursive: true, force: true });
  }

  const failed = checks.filter((c) => !c.ok);
  console.log(`\n${checks.length - failed.length}/${checks.length} 通过`);
  if (failed.length) {
    if (stderr.trim()) console.error(stderr.trim());
    process.exitCode = 1;
  }
}

main().catch((e) => {
  console.error('冒烟失败：' + e.message);
  process.exitCode = 1;
});
