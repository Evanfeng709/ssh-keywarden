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

  // 独立 user-data-dir：单实例锁按 userData 划分，避免和用户正在运行的已安装版互相踢
  const userDataDir = path.join(tmp, 'userdata');
  const args = [
    `--remote-debugging-port=${debugPort}`,
    `--user-data-dir=${userDataDir}`,
    '--disable-gpu',
  ];
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
        clipboardType: await window.keywarden.readClipboard().then((t) => typeof t),
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

    // 布局：单行顶栏、自绘窗口按钮、本地面板默认收起且可开关
    const layoutResult = await send('Runtime.evaluate', {
      expression: `(async () => {
        const nextFrame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
        // 版本号是 boot() 读完 /api/state 才写入的，等到它说明事件绑定已完成
        for (let i = 0; i < 100 && document.getElementById('version').textContent === 'v—'; i++) {
          await new Promise((r) => setTimeout(r, 100));
        }
        const panel = document.getElementById('localPanel');
        const btnLocal = document.getElementById('btnLocal');
        const layout = document.getElementById('layout');
        const hiddenByDefault = getComputedStyle(panel).display === 'none';
        btnLocal.click();
        await nextFrame();
        const shown = getComputedStyle(panel).display !== 'none' && layout.classList.contains('local-open');
        const pressed = btnLocal.getAttribute('aria-pressed') === 'true';
        btnLocal.click();
        await nextFrame();
        const hiddenAgain = getComputedStyle(panel).display === 'none';
        const wc = window.keywarden && window.keywarden.windowControls;
        return {
          bodyDesktop: document.body.classList.contains('desktop'),
          topbarHeight: Math.round(document.querySelector('.topbar').getBoundingClientRect().height),
          hasWinButtons: ['winMin', 'winMax', 'winClose'].every((id) => {
            const el = document.getElementById(id);
            return !!el && el.offsetParent !== null;
          }),
          hasWindowControls:
            !!wc &&
            typeof wc.minimize === 'function' &&
            typeof wc.toggleMaximize === 'function' &&
            typeof wc.close === 'function',
          hasClipboard:
            typeof window.keywarden.readClipboard === 'function' &&
            typeof window.keywarden.writeClipboard === 'function',
          hasTermClipboardButtons: !!document.getElementById('termCopy') && !!document.getElementById('termPaste'),
          hiddenByDefault,
          shown,
          pressed,
          hiddenAgain
        };
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    const layout = layoutResult.result.value;

    // 终端粘贴：用假 WebSocket 截获发往 PTY 的字节，验证 paste 事件真的送达。
    // 不读写系统剪贴板，避免动到用户正在用的内容。
    const pasteResult = await send('Runtime.evaluate', {
      expression: `(async () => {
        const core = await import('/js/core.js');
        const sent = [];
        const RealWS = window.WebSocket;
        window.WebSocket = class {
          constructor() {
            this.readyState = 1;
            setTimeout(() => this.onopen && this.onopen(), 0);
          }
          send(d) {
            sent.push(d);
          }
          close() {}
        };
        core.state.session = {
          sid: 'fake',
          host: { host: 'fake', username: 'fake' },
          server: {},
          remotePath: '',
          stats: { total: 0 }
        };
        const term = await import('/js/terminal.js');
        term.openTerminal();
        await new Promise((r) => setTimeout(r, 400));

        const body = document.getElementById('termBody');
        const dt = new DataTransfer();
        dt.setData('text/plain', 'event-paste');
        body.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
        await new Promise((r) => setTimeout(r, 200));

        const decoded = sent
          .map((raw) => {
            try {
              const m = JSON.parse(raw);
              return m.t === 'in' ? atob(m.d) : '';
            } catch {
              return '';
            }
          })
          .join('');
        term.destroyTerminal();
        core.state.session = null;
        window.WebSocket = RealWS;
        return { received: decoded };
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    const paste = pasteResult.result.value;

    // 自绘最大化按钮：连点两次应经历 最大化 → 还原，且 body 状态跟着变
    const waitMaxClass = async (want, tries = 30) => {
      for (let i = 0; i < tries; i++) {
        const r = await send('Runtime.evaluate', {
          expression: `document.body.classList.contains('maximized')`,
          returnByValue: true,
        });
        if (r.result.value === want) return true;
        await new Promise((res) => setTimeout(res, 100));
      }
      return false;
    };
    await send('Runtime.evaluate', { expression: `window.keywarden.windowControls.toggleMaximize()` });
    const maximized = await waitMaxClass(true);
    await send('Runtime.evaluate', { expression: `window.keywarden.windowControls.toggleMaximize()` });
    const restored = await waitMaxClass(false);


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
      ['顶栏标记为桌面版（无边框拖拽）', layout.bodyDesktop === true],
      ['顶栏保持单行高度', layout.topbarHeight > 0 && layout.topbarHeight <= 52],
      ['顶栏含最小化/最大化/关闭按钮', layout.hasWinButtons === true],
      ['窗口控制桥接存在', layout.hasWindowControls === true],
      ['剪贴板桥接存在', layout.hasClipboard === true],
      ['剪贴板读取 IPC 可用', value.clipboardType === 'string'],
      ['终端含复制/粘贴按钮', layout.hasTermClipboardButtons === true],
      ['本地密钥库默认收起', layout.hiddenByDefault === true],
      ['点本地按钮后从右侧展开', layout.shown === true && layout.pressed === true],
      ['再点一次收起', layout.hiddenAgain === true],
      ['终端粘贴事件能送达 PTY', paste.received === 'event-paste'],
      ['最大化/还原切换生效', maximized === true && restored === true],
    ];

    let failed = 0;
    for (const [name, ok] of checks) {
      console.log(`  ${ok ? '✓' : '✗'} ${name}`);
      if (!ok) failed++;
    }
    if (failed) {
      console.error('设置弹窗诊断：', JSON.stringify(settings));
      console.error('布局诊断：', JSON.stringify(layout));
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
    // Chromium 退出后还会短暂占着 user-data-dir，删不掉就多等几轮，别让清理失败冒充测试失败
    try {
      fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 });
    } catch {
      console.warn(`  ! 临时目录未能删除，可手动清理：${tmp}`);
    }
  }
}

main().catch((e) => {
  console.error('桌面冒烟测试失败：', e.stack || e.message);
  process.exit(1);
});
