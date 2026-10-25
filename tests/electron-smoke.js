'use strict';
/**
 * 桌面版冒烟测试（Windows）：启动真实 Electron 窗口，用 CDP 检查页面与 Cookie。
 * 用临时数据目录，不碰真实的 data/。
 */

const { spawn, execFileSync } = require('child_process');
const fs = require('fs');
const net = require('net');
const os = require('os');
const path = require('path');
const WebSocket = require('ws');

const ROOT = path.join(__dirname, '..');
const APP_EXE = process.env.KW_APP_EXE ? path.resolve(process.env.KW_APP_EXE) : '';
const ELECTRON = APP_EXE || path.join(ROOT, 'node_modules', 'electron', 'dist', 'electron.exe');

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

async function waitForPage(debugPort, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${debugPort}/json/list`);
      const list = await res.json();
      const page = list.find((t) => t.type === 'page' && /^http:\/\/127\.0\.0\.1/.test(t.url));
      if (page) return page;
    } catch {
      /* 调试端口还没起来 */
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error('Electron 窗口在 20 秒内没有加载本地页面');
}

function cdp(ws) {
  let nextId = 1;
  const pending = new Map();
  ws.on('message', (raw) => {
    let msg;
    try {
      msg = JSON.parse(raw.toString());
    } catch {
      return;
    }
    const waiter = pending.get(msg.id);
    if (!waiter) return;
    pending.delete(msg.id);
    if (msg.error) waiter.reject(new Error(msg.error.message));
    else waiter.resolve(msg.result);
  });
  return (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
}

async function main() {
  if (process.platform !== 'win32') {
    console.log('桌面冒烟测试只在 Windows 上运行，跳过');
    return;
  }
  if (!fs.existsSync(ELECTRON)) {
    throw new Error(`找不到 Electron：${ELECTRON}（先运行 npm install 或设置 KW_APP_EXE）`);
  }

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-electron-'));
  const keysDir = path.join(tmp, 'keys');
  fs.mkdirSync(keysDir, { recursive: true });
  fs.writeFileSync(
    path.join(tmp, 'config.json'),
    JSON.stringify({
      settings: { localKeyDir: keysDir, saveDir: keysDir, remotePath: '' },
      hosts: [{ id: 'smoke', name: 'smoke', host: '127.0.0.1', port: 22, username: 'smoke', identityFile: '' }],
    })
  );

  const debugPort = await freePort();
  const env = { ...process.env, KW_DATA_DIR: tmp, ELECTRON_RUN_AS_NODE: '' };
  delete env.ELECTRON_RUN_AS_NODE;

  const args = [`--remote-debugging-port=${debugPort}`, '--disable-gpu'];
  if (!APP_EXE) args.unshift('.');
  const child = spawn(ELECTRON, args, {
    cwd: ROOT,
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  let stderr = '';
  child.stderr.on('data', (d) => (stderr += d.toString()));

  try {
    const page = await waitForPage(debugPort);
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.once('open', resolve);
      ws.once('error', reject);
    });

    const send = cdp(ws);
    const pageErrors = [];
    ws.on('message', (raw) => {
      try {
        const msg = JSON.parse(raw.toString());
        if (msg.method === 'Runtime.exceptionThrown') {
          const d = msg.params.exceptionDetails;
          pageErrors.push(`${d.text} ${(d.exception && d.exception.description) || ''}`.trim());
        }
        if (msg.method === 'Log.entryAdded' && msg.params.entry.level === 'error') {
          pageErrors.push(msg.params.entry.text);
        }
      } catch {
        /* 非 JSON 消息忽略 */
      }
    });
    await send('Runtime.enable');
    await send('Log.enable');
    const result = await send('Runtime.evaluate', {
      expression: `(async () => ({
        title: document.title,
        desktop: !!(window.keywarden && window.keywarden.isDesktop),
        hasPickFile: typeof (window.keywarden && window.keywarden.pickFile) === 'function',
        hasPickDir: typeof (window.keywarden && window.keywarden.pickDir) === 'function',
        topbar: !!document.querySelector('.topbar'),
        apiStatus: await fetch('/api/state').then((r) => r.status),
        apiOk: await fetch('/api/state').then((r) => r.json()).then((j) => j.ok === true)
      }))()`,
      awaitPromise: true,
      returnByValue: true,
    });

    if (result.exceptionDetails) {
      throw new Error(`页面脚本异常：${result.exceptionDetails.text}`);
    }
    const value = result.result.value;

    // 打开设置弹窗，确认桌面专属入口已渲染（不点，避免真的弹出资源管理器）。
    // 页面初始化是异步的，按钮监听可能晚于首次点击，这里重试到弹窗出现。
    let settings = { hasKeydir: false, hasOpenData: false };
    for (let i = 0; i < 8; i++) {
      await send('Runtime.evaluate', {
        expression: `document.querySelector('#btnSettings').click()`,
      });
      await new Promise((r) => setTimeout(r, 250));
      const settingsResult = await send('Runtime.evaluate', {
        expression: `({
          hasKeydir: !!document.querySelector('#s_keydir'),
          hasOpenData: !!document.querySelector('#s_open_data'),
          masks: document.querySelectorAll('.modal-mask').length
        })`,
        returnByValue: true,
      });
      settings = settingsResult.result.value;
      if (settings.hasKeydir) break;
    }
    ws.close();

    const checks = [
      ['窗口标题正确', value.title === 'SSH Keywarden'],
      ['桌面桥接已注入', value.desktop === true],
      ['原生选文件接口存在', value.hasPickFile === true],
      ['原生选目录接口存在', value.hasPickDir === true],
      ['页面主界面已渲染', value.topbar === true],
      ['Cookie 注入后 /api/state 返回 200', value.apiStatus === 200],
      ['接口返回 ok', value.apiOk === true],
      ['设置弹窗含密钥库目录', settings.hasKeydir === true],
      ['桌面版设置含「打开数据目录」', settings.hasOpenData === true],
    ];

    let failed = 0;
    for (const [name, ok] of checks) {
      console.log(`  ${ok ? '✓' : '✗'} ${name}`);
      if (!ok) failed++;
    }
    if (failed) {
      console.error('设置弹窗诊断：', JSON.stringify(settings));
      if (pageErrors.length) console.error('页面错误：\n' + pageErrors.join('\n'));
      throw new Error(`${failed} 项桌面冒烟检查失败`);
    }
    console.log('\n桌面冒烟测试通过\n');
  } catch (e) {
    if (stderr.trim()) console.error(stderr.slice(-2000));
    throw e;
  } finally {
    try {
      execFileSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
    } catch {
      try {
        child.kill();
      } catch {}
    }
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('桌面冒烟测试失败：', e.stack || e.message);
  process.exit(1);
});
