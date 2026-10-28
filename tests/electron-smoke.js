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

  let restarted = null;

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
        hasThemeBridge: !!(window.keywarden && window.keywarden.theme && typeof window.keywarden.theme.get === 'function'),
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

    // 主题：手动切换立即生效并记住；自动主题按系统深浅色落到具体一套
    const themeResult = await send('Runtime.evaluate', {
      expression: `(async () => {
        const theme = await import('/js/theme.js');
        const store = () => { try { return localStorage.getItem('kw.theme'); } catch { return null; } };
        const sysDark = await window.keywarden.systemTheme.get();
        theme.applyTheme('linen', { persist: true });
        const linen = {
          applied: document.documentElement.dataset.theme,
          stored: store(),
          bg: getComputedStyle(document.body).backgroundColor
        };
        theme.applyTheme('auto', { persist: true });
        const auto = {
          applied: document.documentElement.dataset.theme,
          stored: store(),
          expected: sysDark ? 'obsidian' : 'cloud'
        };
        const light = theme.termPalette('linen');
        return { sysDarkType: typeof sysDark, linen, auto, termLight: light.background, termDark: theme.termPalette('obsidian').background };
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    const themeState = themeResult.result.value;

    // 删除按钮：没选中时中性禁用，选中后变红并显示计数
    const deleteResult = await send('Runtime.evaluate', {
      expression: `(async () => {
        const core = await import('/js/core.js');
        const keys = await import('/js/keys.js');
        const savedSession = core.state.session;
        core.state.session = { sid: 'smoke', host: { host: 'h', username: 'u' }, server: {}, remotePath: '', stats: { total: 0 } };
        core.state.selected = new Set();
        keys.setConnected(true);
        keys.updateSelectionUi();
        const del = document.getElementById('btnDelete');
        const neutral = !del.classList.contains('is-danger') && del.disabled;
        core.state.selected = new Set(['SHA256:smoke']);
        keys.updateSelectionUi();
        const danger =
          del.classList.contains('is-danger') &&
          !del.disabled &&
          document.getElementById('selCount').textContent === '1';
        core.state.session = savedSession;
        core.state.selected = new Set();
        keys.setConnected(false);
        keys.updateSelectionUi();
        return { neutral, danger };
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    const deleteState = deleteResult.result.value;

    // 打开设置弹窗，确认桌面专属入口已渲染（不点，避免真的弹出资源管理器）。
    // 页面初始化是异步的，按钮监听可能晚于首次点击，这里重试到弹窗出现。
    let settings = { hasKeydir: false, hasOpenData: false, themeCards: 0, autoSelected: false };
    for (let i = 0; i < 8; i++) {
      await send('Runtime.evaluate', {
        expression: `document.querySelector('#btnSettings').click()`,
      });
      await new Promise((r) => setTimeout(r, 250));
      const settingsResult = await send('Runtime.evaluate', {
        expression: `({
          hasKeydir: !!document.querySelector('#s_keydir'),
          hasOpenData: !!document.querySelector('#s_open_data'),
          themeCards: document.querySelectorAll('#themeGrid .theme-card').length,
          autoSelected: !!document.querySelector('.theme-card[data-choice="auto"].on'),
          masks: document.querySelectorAll('.modal-mask').length
        })`,
        returnByValue: true,
      });
      settings = settingsResult.result.value;
      if (settings.hasKeydir) break;
    }

    // 点一张卡片即时预览但不落盘，取消后回到原来的主题
    const previewResult = await send('Runtime.evaluate', {
      expression: `(async () => {
        const before = document.documentElement.dataset.theme;
        document.querySelector('#themeGrid .theme-card[data-choice="linen"]').click();
        await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
        const live = document.documentElement.dataset.theme;
        const storedLive = localStorage.getItem('kw.theme');
        document.querySelector('.modal-mask [data-close]').click();
        await new Promise((r) => setTimeout(r, 80));
        return {
          before,
          live,
          storedLive,
          after: document.documentElement.dataset.theme,
          masks: document.querySelectorAll('.modal-mask').length
        };
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    const preview = previewResult.result.value;

    // 重新打开 → 选雾灰 → 保存，选择应写入 localStorage
    let saved = { applied: '', stored: '', closed: false };
    for (let i = 0; i < 8; i++) {
      await send('Runtime.evaluate', { expression: `document.querySelector('#btnSettings').click()` });
      await new Promise((r) => setTimeout(r, 250));
      const ready = await send('Runtime.evaluate', {
        expression: `!!document.querySelector('#themeGrid .theme-card[data-choice="mist"]')`,
        returnByValue: true,
      });
      if (!ready.result.value) continue;
      const saveResult = await send('Runtime.evaluate', {
        expression: `(async () => {
          document.querySelector('#themeGrid .theme-card[data-choice="mist"]').click();
          await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
          document.querySelector('#s_save').click();
          await new Promise((r) => setTimeout(r, 500));
          return {
            applied: document.documentElement.dataset.theme,
            stored: localStorage.getItem('kw.theme'),
            persisted: await window.keywarden.theme.get(),
            closed: document.querySelectorAll('.modal-mask').length === 0
          };
        })()`,
        awaitPromise: true,
        returnByValue: true,
      });
      saved = saveResult.result.value;
      break;
    }
    ws.close();

    // 重启验证：同一数据目录再启动一次，端口变了（origin 变、localStorage 空），主题应从主进程记录恢复
    let themeFile = '';
    try {
      themeFile = JSON.parse(fs.readFileSync(path.join(tmp, 'theme.json'), 'utf8')).choice;
    } catch {}
    // 单实例锁按 userData 划分，必须先关掉第一个进程再启动第二个
    try {
      execFileSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
    let restartState = { initial: '', stored: '', theme: '', port: '' };
    for (let attempt = 0; attempt < 3 && !restartState.theme; attempt++) {
      try {
        if (restarted) {
          try {
            execFileSync('taskkill', ['/PID', String(restarted.pid), '/T', '/F'], { stdio: 'ignore' });
          } catch {}
        }
        const restartPort = await freePort();
        const restartArgs = [`--remote-debugging-port=${restartPort}`, `--user-data-dir=${userDataDir}`, '--disable-gpu'];
        if (!APP_EXE) restartArgs.unshift('.');
        restarted = spawn(ELECTRON, restartArgs, { cwd: ROOT, env, stdio: ['ignore', 'pipe', 'pipe'] });
        restarted.stderr.on('data', () => {});
        const page2 = await waitForPage(restartPort);
        const ws2 = new WebSocket(page2.webSocketDebuggerUrl);
        await new Promise((resolve, reject) => {
          ws2.once('open', resolve);
          ws2.once('error', reject);
        });
        const send2 = cdp(ws2);
        const restartResult = await send2('Runtime.evaluate', {
          expression: `(async () => {
            const version = () => document.getElementById('version');
            for (let i = 0; i < 100 && (!version() || version().textContent === 'v—'); i++) {
              await new Promise((res) => setTimeout(res, 100));
            }
            return {
              initial: window.keywarden.theme.initial,
              stored: await window.keywarden.theme.get(),
              theme: document.documentElement.dataset.theme,
              port: location.port
            };
          })()`,
          awaitPromise: true,
          returnByValue: true,
        });
        ws2.close();
        restartState = restartResult.result.value;
      } catch (e) {
        if (attempt === 2) throw e;
        await new Promise((r) => setTimeout(r, 700));
      }
    }

    const checks = [
      ['窗口标题正确', value.title === 'SSH Keywarden'],
      ['桌面桥接已注入', value.desktop === true],
      ['主题桥接已注入', value.hasThemeBridge === true],
      ['原生选文件接口存在', value.hasPickFile === true],
      ['原生选目录接口存在', value.hasPickDir === true],
      ['页面主界面已渲染', value.topbar === true],
      ['Cookie 注入后 /api/state 返回 200', value.apiStatus === 200],
      ['接口返回 ok', value.apiOk === true],
      ['设置弹窗含密钥库目录', settings.hasKeydir === true],
      ['桌面版设置含「打开数据目录」', settings.hasOpenData === true],
      ['设置里有 8 套主题可选', settings.themeCards === 8],
      ['当前自动主题被标记为选中', settings.autoSelected === true],
      ['主题卡片即时预览', preview.live === 'linen' && preview.before !== 'linen'],
      ['预览不写 localStorage', preview.storedLive === 'auto'],
      ['取消后主题回滚', preview.after === preview.before && preview.masks === 0],
      ['保存后主题被记住', saved.applied === 'mist' && saved.stored === 'mist' && saved.closed === true],
      ['主题选择写入主进程存储', saved.persisted === 'mist' && themeFile === 'mist'],
      ['重启后主题仍然生效', restartState.initial === 'mist' && restartState.stored === 'mist' && restartState.theme === 'mist'],
      ['手动主题立即生效', themeState.linen.applied === 'linen' && themeState.linen.stored === 'linen'],
      ['浅色主题背景即时换色', themeState.linen.bg === 'rgb(246, 242, 234)'],
      ['自动主题按系统解析', themeState.auto.applied === themeState.auto.expected && themeState.auto.stored === 'auto'],
      ['桌面端可读系统深浅色', themeState.sysDarkType === 'boolean'],
      ['终端调色板区分深浅主题', themeState.termLight === '#fffdf8' && themeState.termDark === '#131920'],
      ['删除按钮未选中时中性禁用', deleteState.neutral === true],
      ['删除按钮有选中时变红带计数', deleteState.danger === true],
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
      console.error('重启诊断：', JSON.stringify({ themeFile, restartState }));
      if (pageErrors.length) console.error('页面错误：\n' + pageErrors.join('\n'));
      throw new Error(`${failed} 项桌面冒烟检查失败`);
    }
    console.log('\n桌面冒烟测试通过\n');
  } catch (e) {
    if (stderr.trim()) console.error(stderr.slice(-2000));
    throw e;
  } finally {
    for (const proc of [child, restarted]) {
      if (!proc) continue;
      try {
        execFileSync('taskkill', ['/PID', String(proc.pid), '/T', '/F'], { stdio: 'ignore' });
      } catch {
        try {
          proc.kill();
        } catch {}
      }
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
