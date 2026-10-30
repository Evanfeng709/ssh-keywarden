import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const root = path.resolve(import.meta.dirname, '..');
const exe = path.join(root, 'target/debug/kw-server.exe');
const edge = process.env.KW_TEST_BROWSER || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
let stage = '初始化';
const checks = [];
function check(name, value) {
  checks.push(!!value);
  console.log(`${value ? 'PASS' : 'FAIL'} ${name}`);
  if (!value) throw new Error('检查失败');
}

function bounded(promise, ms = 15000) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error('等待超时')), ms);
  })]).finally(() => clearTimeout(timer));
}

function exited(child) {
  if (child.exitCode !== null) return Promise.resolve(child.exitCode);
  return new Promise((resolve) => child.once('exit', resolve));
}

async function interruptServer(child) {
  if (process.platform !== 'win32') {
    child.kill('SIGTERM');
    return;
  }
  // 独立控制台使用 Ctrl+Break，避免子进程继承的 Ctrl+C 忽略状态。
  const code = `Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class KwConsole {
 [DllImport("kernel32.dll")] public static extern bool FreeConsole();
 [DllImport("kernel32.dll")] public static extern bool AttachConsole(uint pid);
 public delegate bool Handler(uint evt);
 static Handler handler = evt => true;
 [DllImport("kernel32.dll")] static extern bool SetConsoleCtrlHandler(Handler callback, bool add);
 public static void IgnoreSignals() { SetConsoleCtrlHandler(handler, true); }
 [DllImport("kernel32.dll")] public static extern bool GenerateConsoleCtrlEvent(uint evt, uint group);
}
'@
[KwConsole]::FreeConsole() | Out-Null
if (-not [KwConsole]::AttachConsole(${child.serverPid || child.pid})) { exit 2 }
[KwConsole]::IgnoreSignals()
if (-not [KwConsole]::GenerateConsoleCtrlEvent(1, 0)) { exit 3 }
Start-Sleep -Milliseconds 150
[KwConsole]::FreeConsole() | Out-Null
`;
  const helper = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(code, 'utf16le').toString('base64')], { stdio: 'ignore', windowsHide: true });
  const codeResult = await bounded(exited(helper));
  if (codeResult !== 0) {
    console.error(`退出信号辅助程序失败：${codeResult}`);
    throw new Error('发送退出信号失败');
  }
}

async function cdp(url) {
  const ws = new WebSocket(url);
  let id = 0;
  const pending = new Map();
  const errors = [];
  ws.addEventListener('message', ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === 'Runtime.exceptionThrown') errors.push(true);
    if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') errors.push(true);
    const task = pending.get(message.id);
    if (task) {
      pending.delete(message.id);
      if (message.error) task.reject(new Error('CDP 调用失败'));
      else task.resolve(message.result);
    }
  });
  await bounded(new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  }));
  const send = (method, params = {}) => bounded(new Promise((resolve, reject) => {
    const next = ++id;
    pending.set(next, { resolve, reject });
    ws.send(JSON.stringify({ id: next, method, params }));
  }));
  const run = async (expression) => {
    const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error('页面检查失败');
    return result.result?.value;
  };
  const wait = (condition) => run(`new Promise(resolve => {
    const start = Date.now();
    const check = () => { if (${condition}) resolve(true); else if (Date.now()-start > 10000) resolve(false); else setTimeout(check, 50); }; check();
  })`);
  const click = async (selector) => {
    const rect = await run(`(() => { const r = document.querySelector(${JSON.stringify(selector)})?.getBoundingClientRect(); return r && {x:r.x+r.width/2,y:r.y+r.height/2}; })()`);
    if (!rect) throw new Error('找不到按钮');
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', clickCount: 1, ...rect });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', clickCount: 1, ...rect });
  };
  await send('Runtime.enable');
  await send('Page.enable');
  return { send, run, wait, click, errors, close: () => ws.close() };
}

async function main() {
  if (!fs.existsSync(exe) || !fs.existsSync(edge)) throw new Error('缺少测试程序');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kw-server-smoke-'));
  const keys = path.join(dir, 'keys');
  fs.mkdirSync(keys);
  fs.writeFileSync(path.join(dir, 'config.json'), JSON.stringify({
    settings: { localKeyDir: keys, saveDir: keys },
    hosts: [{ id: 'test-tombstone', deletedAt: 1 }],
  }));
  let output = '';
  let loginPath;
  let browser;
  let tab;
  let logWatcher;
  const log = path.join(dir, 'server-stdout.log');
  const errorLog = path.join(dir, 'server-stderr.log');
  fs.writeFileSync(log, '');
  const psQuote = (value) => "'" + value.replaceAll("'", "''") + "'";
  const launch = `$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$p = Start-Process -FilePath ${psQuote(exe)} -ArgumentList '--port','0','--no-open' -WindowStyle Hidden -RedirectStandardOutput ${psQuote(log)} -RedirectStandardError ${psQuote(errorLog)} -PassThru
Write-Output $p.Id
$p.WaitForExit()
exit $p.ExitCode
`;
  // Node detached 使用 DETACHED_PROCESS，没有可接收 Ctrl+C 的控制台。
  const child = spawn(process.platform === 'win32' ? 'powershell.exe' : exe,
    process.platform === 'win32'
      ? ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(launch, 'utf16le').toString('base64')]
      : ['--port', '0', '--no-open'], {
      cwd: root,
      env: { ...process.env, KW_DATA_DIR: dir, KW_PUBLIC_DIR: path.join(root, 'assets/public') },
      windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    });
  const childExit = exited(child);
  child.stderr.on('data', () => {});
  try {
    stage = '服务启动';
    await bounded(new Promise((resolve, reject) => {
      const inspect = () => {
        if (process.platform === 'win32') output = fs.readFileSync(log, 'utf8');
        const match = output.match(/登录文件：([^\r\n]+)/);
        if (match) loginPath = match[1];
        if (loginPath && output.includes('按 Ctrl+C') && (process.platform !== 'win32' || child.serverPid)) resolve();
      };
      child.on('error', reject);
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', (chunk) => {
        if (process.platform === 'win32') child.serverPid = Number(chunk.trim());
        else output += chunk;
        inspect();
      });
      if (process.platform === 'win32') logWatcher = fs.watch(log, inspect);
      inspect();
      child.once('exit', () => reject(new Error('服务提前退出')));
    }));
    logWatcher?.close();
    const html = fs.readFileSync(loginPath, 'utf8');
    const loginUrl = html.match(/href="([^"]+)"/)[1];
    const url = new URL(loginUrl);
    const origin = url.origin;
    const token = url.searchParams.get('token');
    check('启动日志不含令牌或监听地址', !output.includes(token) && !output.includes(origin));
    check('未登录 API 拒绝访问', (await fetch(`${origin}/api/state`)).status === 401);
    check('首页不自动发放 Cookie', !(await fetch(origin)).headers.has('set-cookie'));
    check('错误令牌拒绝登录', (await fetch(`${origin}/login?token=invalid`)).status === 401);

    stage = 'Edge 启动';
    const profile = path.join(dir, 'browser-profile');
    fs.mkdirSync(profile);
    const activePort = path.join(profile, 'DevToolsActivePort');
    const ready = bounded(new Promise((resolve) => {
      const watcher = fs.watch(profile, () => {
        if (fs.existsSync(activePort) && fs.readFileSync(activePort, 'utf8').includes('\n')) {
          watcher.close();
          resolve();
        }
      });
      setTimeout(() => watcher.close(), 15000).unref();
    }));
    browser = spawn(edge, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: 'ignore', windowsHide: true });
    await ready;
    const debugPort = fs.readFileSync(activePort, 'utf8').split('\n')[0];
    const pages = await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json();
    tab = await cdp(pages.find(p => p.type === 'page').webSocketDebuggerUrl);
    await tab.send('Page.navigate', { url: pathToFileURL(loginPath).href });
    check('临时登录文件可打开', await tab.wait("!!document.querySelector('a')"));
    await tab.click('a');
    stage = '浏览器登录';
    check('点击登录后首屏可用', await tab.wait("document.getElementById('version')?.textContent !== 'v—' && !!document.getElementById('version')"));
    check('登录跳转不保留令牌', await tab.run("location.pathname === '/' && !location.search"));
    check('浏览器会话鉴权成功', await tab.run("fetch('/api/state').then(r => r.status === 200)"));
    check('浏览器模式不伪装桌面壳', await tab.run("!document.body.classList.contains('desktop')"));
    await tab.click('[data-mode="term"]');
    check('纯终端视图可切换', await tab.wait("document.querySelector('[data-mode=term]').classList.contains('on')"));
    await tab.click('[data-mode="keys"]');
    check('密钥视图可切换', await tab.wait("document.querySelector('[data-mode=keys]').classList.contains('on')"));
    await tab.click('#btnSettings');
    check('设置弹窗可打开', await tab.wait("document.getElementById('modalRoot').textContent.includes('设置')"));
    await tab.send('Page.reload');
    check('刷新后 Cookie 仍有效', await tab.wait("document.getElementById('version')?.textContent !== 'v—' && !!document.getElementById('version')"));
    check('隔离环境没有导入真实主机', await tab.run("fetch('/api/state').then(r=>r.json()).then(j=>j.data.hosts.length===0)"));
    check('浏览器无脚本异常', tab.errors.length === 0);
    stage = '发送退出信号';
    await interruptServer(child);
    stage = '等待正常退出';
    check('退出信号触发正常退出', await bounded(childExit) === 0);
    check('退出后登录文件已清除', !fs.existsSync(loginPath));
    const closed = await fetch(origin).then(() => false).catch(() => true);
    check('退出后监听已关闭', closed);
  } catch (error) {
    console.error(JSON.stringify({ stage, error: error.code || error.name, exitCode: child.exitCode, pidKnown: !!child.serverPid, outputBytes: output.length }));
    throw error;
  } finally {
    if (tab) {
      await tab.send('Browser.close').catch(() => {});
      tab.close();
    }
    if (browser && browser.exitCode === null) await bounded(exited(browser), 5000).catch(() => browser.kill());
    logWatcher?.close();
    if (child.exitCode === null) {
      await interruptServer(child).catch(() => {});
      await bounded(childExit, 5000).catch(async () => {
        if (process.platform === 'win32' && child.serverPid) {
          const stop = spawn('taskkill.exe', ['/PID', String(child.serverPid), '/F'], { stdio: 'ignore', windowsHide: true });
          await bounded(exited(stop)).catch(() => {});
        }
        child.kill();
        await bounded(childExit, 5000).catch(() => {});
      });
    }
    if (loginPath) fs.rmSync(loginPath, { force: true });
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
  console.log(`浏览器冒烟：${checks.filter(Boolean).length}/${checks.length} 通过`);
}

main().catch(() => {
  console.error(`FAIL 阶段：${stage}`);
  process.exitCode = 1;
});
