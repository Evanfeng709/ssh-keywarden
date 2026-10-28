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

// 函数在真实 renderer 中执行；结果只返回断言，不返回主机、会话或请求内容。
async function evaluate(send, fn, ...args) {
  const result = await send('Runtime.evaluate', {
    expression: `(${fn.toString()})(${args.map((arg) => JSON.stringify(arg)).join(',')})`,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) throw new Error('回归页面脚本执行失败');
  return result.result.value;
}

async function installUiFixture() {
  const { state } = await import('/js/core.js');
  const keys = await import('/js/keys.js');
  const term = await import('/js/terminal.js');
  const realFetch = window.fetch;
  const RealWS = window.WebSocket;
  const f = {
    state, keys, term, requests: [], sockets: [], sent: [], refreshReply: null,
    sshd: { available: true, state: 'key-only', keyOnly: true, pubkeyAuth: true, canWrite: false },
    button: (mode) => document.querySelector('#modeSwitch button[data-mode="' + mode + '"]'),
    visible: (selector) => {
      const el = document.querySelector(selector);
      return !!el && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
    },
    frames: async () => {
      await document.fonts.ready;
      for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);
    },
    until: async (predicate) => {
      const deadline = performance.now() + 4000;
      while (!predicate() && performance.now() < deadline) await new Promise((r) => setTimeout(r, 20));
      return !!predicate();
    },
    session: (count = 1) => ({
      sid: 'smoke-ui-session', keysLoaded: true, fileExists: true, authMethod: 'key',
      host: { id: 'smoke', host: 'smoke.invalid', username: 'smoke', port: 22, identityFile: '' },
      server: { uname: 'Smoke OS' }, remotePath: '/smoke/.ssh/authorized_keys',
      usedKeyFp: 'SHA256:' + '0'.repeat(4) + 'S'.repeat(39), stats: { total: count },
      items: Array.from({ length: count }, (_, i) => ({
        kind: 'key', type: 'ssh-ed25519', fingerprint: 'SHA256:' + String(i).padStart(4, '0') + 'S'.repeat(39),
        comment: 'smoke-row-' + String(i).padStart(3, '0'), options: '',
      })),
    }),
    holdRefresh: () => {
      f.refreshReply = new Promise((resolve) => { f.releaseRefresh = resolve; });
    },
    showRows: (count) => {
      state.session = f.session(count);
      state.sshd = f.sshd;
      state.selected.clear();
      keys.renderServer();
      term.openTerminal();
    },
    measure: () => {
      const panel = document.querySelector('.server-panel');
      const wrap = panel.querySelector('.table-wrap');
      const table = wrap.querySelector('table');
      const splitter = document.getElementById('termSplitter');
      const range = document.createRange();
      range.selectNodeContents(table.querySelector('th.col-status'));
      const text = range.getBoundingClientRect();
      const sr = splitter.getBoundingClientRect();
      // 量实际伪元素横条的中心，而不是复用应用的 --align-x 计算。
      const bar = getComputedStyle(splitter, '::after');
      const transform = bar.transform === 'none' ? 0 : new DOMMatrixReadOnly(bar.transform).m41;
      const center = sr.left + parseFloat(bar.left) + transform + parseFloat(bar.width) / 2;
      const tr = document.getElementById('termPanel').getBoundingClientRect();
      const outer = ['html', 'body', '#layout', '.server-panel', '.topbar', '.logbar'].every((selector) => {
        const el = document.querySelector(selector);
        const r = el.getBoundingClientRect();
        return el.scrollWidth <= el.clientWidth + 1 && el.scrollHeight <= el.clientHeight + 1 &&
          r.left >= -1 && r.top >= -1 && r.right <= innerWidth + 1 && r.bottom <= innerHeight + 1;
      });
      const ctl = term.getTermCtl();
      const proposed = ctl && ctl.fit.proposeDimensions();
      const resize = f.sent.filter((m) => m.t === 'resize').at(-1);
      return {
        rows: document.querySelectorAll('#keyBody input[data-fp]').length,
        compact: sr.top - table.getBoundingClientRect().bottom >= -0.5 &&
          sr.top - table.getBoundingClientRect().bottom <= 2 && wrap.clientHeight <= table.offsetHeight + 2,
        aligned: text.width > 0 && sr.width > 0 && Math.abs(center - (text.left + text.width / 2)) < 1,
        minHeight: tr.height >= 180,
        fills: Math.abs(tr.bottom - (panel.getBoundingClientRect().bottom - panel.clientTop)) <= 2,
        outer,
        scrollable: wrap.scrollHeight > wrap.clientHeight && ['auto', 'scroll'].includes(getComputedStyle(wrap).overflowY),
        fitted: !!proposed && ctl.term.cols === proposed.cols && ctl.term.rows === proposed.rows &&
          !!resize && resize.cols === proposed.cols && resize.rows === proposed.rows,
      };
    },
    cleanup: () => {
      term.destroyTerminal();
      state.session = null;
      state.connecting = false;
      state.selected.clear();
      keys.renderServer();
      window.fetch = realFetch;
      window.WebSocket = RealWS;
      delete window.__smokeUi;
    },
  };
  window.WebSocket = class {
    constructor() {
      this.readyState = 1;
      this.closed = false;
      f.sockets.push(this);
      setTimeout(() => this.onopen && this.onopen(), 0);
    }
    send(raw) { f.sent.push(JSON.parse(raw)); }
    close() { this.closed = true; this.readyState = 3; }
  };
  // 全拦截：即使回归误发 connect，也绝不接触真实 SSH 服务。
  window.fetch = async (input, options = {}) => {
    const url = new URL(typeof input === 'string' ? input : input.url, location.href);
    const request = { path: url.pathname, method: options.method || 'GET', body: options.body && JSON.parse(options.body) };
    f.requests.push(request);
    let reply;
    if (request.path === '/api/refresh' && f.refreshReply) {
      const pending = f.refreshReply;
      f.refreshReply = null;
      reply = await pending;
    } else if (request.path === '/api/sshd') {
      request.validSid = url.searchParams.get('sid') === state.session?.sid;
      request.modeAtRequest = state.mode;
      if (f.sshdReply) {
        const pending = f.sshdReply;
        f.sshdReply = null;
        reply = await pending;
      } else {
        reply = { data: f.sshd };
      }
    } else if (request.path === '/api/disconnect') {
      reply = { data: {} };
    } else {
      reply = { status: 500, error: 'Unexpected smoke request' };
    }
    return new Response(JSON.stringify({ ok: !reply.error, data: reply.data, error: reply.error }), {
      status: reply.status || 200, headers: { 'Content-Type': 'application/json' },
    });
  };
  window.__smokeUi = f;
}

async function runUiRegression(send) {
  const checks = [];
  await evaluate(send, installUiFixture);
  try {
    checks.push(...await evaluate(send, async () => {
      const f = window.__smokeUi;
      const { state, term, keys } = f;
      const results = [];
      const check = (name, ok) => results.push([name, !!ok]);
      const ui = window.keywarden.ui;
      check('UI 桥接及终端高度接口存在', !!ui && typeof ui.get === 'function' && typeof ui.set === 'function' &&
        ui.initial.mode === 'keys' && ui.initial.termHeight === 0 &&
        ['applyTermHeight', 'saveTermHeight', 'resetTermHeight', 'initTermHeight', 'getTermCtl'].every((k) => typeof term[k] === 'function'));
      f.button('term').click();
      await f.frames();
      check('无会话切纯终端显示空态且不启动连接', state.mode === 'term' && document.body.classList.contains('mode-term') &&
        f.visible('#termEmpty') && !f.visible('.table-wrap') && !f.visible('#termPanel') &&
        !term.getTermCtl() && f.sockets.length === 0 && f.requests.length === 0 &&
        f.button('term').getAttribute('aria-pressed') === 'true' && f.button('keys').getAttribute('aria-pressed') === 'false');
      state.connecting = true;
      // dispatchEvent 不受 disabled 影响，验证状态保护而非仅验证按钮外观。
      f.button('keys').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      await f.frames();
      check('connecting 时不能切回密钥', state.mode === 'term' && f.requests.length === 0);
      state.connecting = false;
      f.button('keys').click();
      state.connecting = true;
      f.button('term').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      await f.frames();
      check('connecting 时不能切入纯终端', state.mode === 'keys' && !document.body.classList.contains('mode-term'));
      state.connecting = false;
      f.showRows(1);
      await f.frames();
      const ctl = term.getTermCtl();
      const sid = state.session.sid;
      document.getElementById('btnLocal').click();
      await f.frames();
      check('模式切换前表格本地库和授权路径可见', f.visible('.table-wrap') && f.visible('#localPanel') && f.visible('#serverMeta .meta-path'));
      f.button('term').click();
      await f.frames();
      check('纯终端隐藏密钥工具本地库及授权路径',
        ['.table-wrap', '#keyTools', '#localPanel', '#btnLocal', '#termSplitter', '#btnAdd', '#btnDelete', '#btnRefresh', '#btnBackups'].every((s) => !f.visible(s)) &&
        !document.querySelector('#serverMeta .meta-path') && !document.getElementById('layout').classList.contains('local-open') &&
        !f.visible('#termEmpty') && f.visible('#termPanel'));
      f.button('keys').click();
      await f.frames();
      check('已有密钥切模式保留会话与终端且不补读不重连', state.mode === 'keys' && state.session.sid === sid &&
        term.getTermCtl() === ctl && f.sockets.length === 1 && !ctl.ws.closed && f.requests.length === 0 &&
        f.visible('.table-wrap') && f.visible('#serverMeta .meta-path'));

      f.button('term').click();
      const unloaded = { ...f.session(), keysLoaded: false, items: [], remotePath: '' };
      state.session = unloaded;
      state.sshd = null;
      keys.renderServer();
      f.holdRefresh();
      f.button('keys').click();
      await f.frames();
      check('按需补读使用 POST 且完成前留在纯终端', state.mode === 'term' && state.session === unloaded &&
        f.button('keys').disabled && f.button('term').disabled && f.requests.length === 1 &&
        f.requests[0].path === '/api/refresh' && f.requests[0].method === 'POST' && f.requests[0].body.sid === sid);
      f.button('keys').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      check('补读中重复切换不会重复请求', f.requests.length === 1);
      f.releaseRefresh({ status: 503, error: 'Synthetic refresh failure' });
      await f.until(() => !f.button('keys').disabled);
      check('补读失败留 term 并保留 sid 与 termCtl', state.mode === 'term' && document.body.classList.contains('mode-term') &&
        state.session === unloaded && state.session.sid === sid && term.getTermCtl() === ctl && !ctl.ws.closed &&
        !f.button('keys').disabled && f.requests.length === 1 && !!document.querySelector('.toast.err'));
      f.holdRefresh();
      f.button('keys').click();
      f.releaseRefresh({ data: f.session() });
      await f.until(() => state.mode === 'keys' && state.sshd?.available);
      await f.frames();
      check('再次补读成功才切 keys 并读取 sshd', state.mode === 'keys' && state.session.keysLoaded === true &&
        f.visible('.table-wrap') && f.visible('#serverMeta .meta-path') && f.requests.length === 3 &&
        f.requests[1].path === '/api/refresh' && f.requests[1].method === 'POST' && f.requests[1].body.sid === sid &&
        f.requests[2].path === '/api/sshd' && f.requests[2].method === 'GET' && f.requests[2].validSid &&
        f.requests[2].modeAtRequest === 'keys' && state.sshd?.state === 'key-only');
      check('补读成功不替换终端实例且不重连', state.session.sid === sid && term.getTermCtl() === ctl && f.sockets.length === 1 && !ctl.ws.closed);
      f.button('term').click();
      f.button('keys').click();
      await f.frames();
      check('已补读会话再次切换不重复补读或查询 sshd', f.requests.length === 3 && term.getTermCtl() === ctl);

      f.button('term').click();
      state.session = { ...f.session(), keysLoaded: false, items: [] };
      state.sshd = null;
      keys.renderServer();
      f.holdRefresh();
      f.button('keys').click();
      document.getElementById('btnDisconnect').click();
      await f.until(() => state.session === null);
      check('补读中断开会销毁原终端', !state.session && !term.getTermCtl() && ctl.ws.closed &&
        f.requests.at(-1).path === '/api/disconnect' && f.requests.at(-1).method === 'POST');
      f.releaseRefresh({ data: f.session() });
      await f.until(() => !f.button('keys').disabled);
      await f.frames();
      check('补读迟到响应不会复活旧会话', !state.session && !term.getTermCtl() && state.mode === 'term' &&
        f.visible('#termEmpty') && !f.visible('#termPanel') && f.sockets.length === 1 && f.requests.length === 5 &&
        !f.button('keys').disabled && !f.button('term').disabled);
      f.button('keys').click();
      state.session = f.session();
      state.sshd = null;
      keys.renderServer();
      const sshd = await import('/js/sshd.js');
      const connect = await import('/js/connect.js');
      let releaseSshd;
      f.sshdReply = new Promise((resolve) => { releaseSshd = resolve; });
      const probe = sshd.refreshSshdBadge();
      f.holdRefresh();
      const refresh = connect.refreshSession();
      f.releaseRefresh({ data: f.session() });
      await refresh;
      releaseSshd({ data: f.sshd });
      await probe;
      check('同一会话刷新密钥后仍接收迟到的登录状态', state.sshd?.available && !!document.querySelector('.sshd-badge'));
      f.sshdReply = new Promise((resolve) => { releaseSshd = resolve; });
      const staleProbe = sshd.refreshSshdBadge();
      state.session = { ...f.session(), sid: 'smoke-next-session' };
      state.sshd = null;
      releaseSshd({ data: f.sshd });
      await staleProbe;
      check('更换会话后丢弃旧登录状态', state.sshd === null);
      return results;
    }));

    // CDP 设置 CSS viewport，不受实机显示器大小限制，且不改应用样式。
    for (const [width, height] of [[1280, 820], [1440, 900], [1920, 1080], [1024, 680]]) {
      await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
      const results = await evaluate(send, async (width, height) => {
        const f = window.__smokeUi;
        const results = [];
        const check = (name, ok) => results.push([width + 'x' + height + ' ' + name, !!ok]);
        f.showRows(1);
        await f.frames();
        let m = f.measure();
        check('视口尺寸准确', innerWidth === width && innerHeight === height);
        check('单行表紧贴分隔条且终端填满剩余空间', m.rows === 1 && m.compact && m.minHeight && m.fills);
        check('状态文字与横条中心误差小于一像素', m.aligned);
        check('无外层溢出', m.outer);
        const ctl = f.term.getTermCtl();
        const sockets = f.sockets.length;
        document.getElementById('termClose').click();
        await f.frames();
        const hidden = !f.visible('#termPanel') && !f.visible('#termSplitter') && !document.querySelector('.server-panel').classList.contains('term-open');
        document.getElementById('btnTerm').click();
        await f.frames();
        m = f.measure();
        check('收起再打开保持会话与紧凑对齐', hidden && f.term.getTermCtl() === ctl && f.sockets.length === sockets &&
          !ctl.ws.closed && m.compact && m.aligned && m.outer);
        for (const action of ['展开', '收起']) {
          document.getElementById('btnLocal').click();
          await f.frames();
          m = f.measure();
          check(action + '侧栏后仍对齐且无溢出', f.visible('#localPanel') === (action === '展开') && m.aligned && m.compact && m.outer);
        }
        f.showRows(60);
        await f.frames();
        const wrap = document.querySelector('.server-panel .table-wrap');
        wrap.scrollTop = wrap.scrollHeight;
        await f.frames();
        m = f.measure();
        check('多行表可滚动且终端至少一百八十像素', m.rows === 60 && m.scrollable && wrap.scrollTop > 0 && m.minHeight && m.fills && m.outer);
        check('滚动后状态文字仍与横条对齐', m.aligned);
        check('终端 ResizeObserver 自动 fit 并发送尺寸', m.fitted);
        document.querySelector('#keyFilter [data-filter="used"]').click();
        await f.frames();
        m = f.measure();
        check('筛选成一行后无留白且保持对齐', m.rows === 1 && m.compact && m.aligned && m.minHeight && m.outer);
        document.querySelector('#keyFilter [data-filter="all"]').click();
        const search = document.getElementById('keySearch');
        search.value = 'smoke-row-000';
        search.dispatchEvent(new Event('input', { bubbles: true }));
        await f.frames();
        m = f.measure();
        check('搜索后保持紧凑对齐', m.rows === 1 && m.compact && m.aligned && m.outer);
        document.getElementById('keySearchClear').click();
        await f.frames();
        m = f.measure();
        check('清空筛选恢复多行滚动且无外层溢出', m.rows === 60 && m.scrollable && m.minHeight && m.aligned && m.outer);
        return results;
      }, width, height);
      checks.push(...results);
    }

    await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 820, deviceScaleFactor: 1, mobile: false });
    checks.push(...await evaluate(send, async () => {
      const f = window.__smokeUi;
      f.showRows(1);
      await f.frames();
      const panel = document.getElementById('termPanel');
      const splitter = document.getElementById('termSplitter');
      const y = splitter.getBoundingClientRect().top + 3;
      const height = panel.offsetHeight;
      splitter.dispatchEvent(new MouseEvent('mousedown', { clientY: y, bubbles: true, button: 0 }));
      document.dispatchEvent(new MouseEvent('mousemove', { clientY: y + height - 230, bubbles: true, buttons: 1 }));
      document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0 }));
      await f.until(() => window.keywarden.ui.get().termHeight === 230);
      await f.frames();
      const dragged = panel.offsetHeight === 230 && panel.parentElement.classList.contains('term-manual') &&
        window.keywarden.ui.get().mode === 'keys' && window.keywarden.ui.get().termHeight === 230;
      f.term.applyTermHeight(40);
      const minimum = panel.offsetHeight === 180;
      f.term.applyTermHeight();
      const restoredHeight = panel.offsetHeight === 230;
      splitter.dispatchEvent(new MouseEvent('mousedown', { clientY: y, bubbles: true, button: 0 }));
      document.dispatchEvent(new MouseEvent('mousemove', { clientY: y + 600, bubbles: true, buttons: 1 }));
      const overdrag = panel.offsetHeight === 180 && panel.parentElement.classList.contains('term-manual');
      document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0 }));
      f.term.saveTermHeight(230);
      await f.until(() => window.keywarden.ui.get().termHeight === 230);
      splitter.dispatchEvent(new MouseEvent('mousedown', { clientY: y, bubbles: true, button: 2 }));
      document.dispatchEvent(new MouseEvent('mousemove', { clientY: y + 40, bubbles: true, buttons: 2 }));
      const rightClickIgnored = panel.offsetHeight === 230;
      document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 2 }));
      f.term.saveTermHeight(230);
      await f.until(() => window.keywarden.ui.get().termHeight === 230);
      return [
        ['拖动终端保存手动高度且不覆盖模式', dragged],
        ['手动高度下限为一百八十像素', minimum],
        ['无参 applyTermHeight 恢复已保存高度', restoredHeight],
        ['拖过下边界仍保持最小手动高度', overdrag],
        ['右键不会启动分隔条拖动', rightClickIgnored],
      ];
    }));
    for (const [width, height] of [[1440, 900], [1920, 1080], [1024, 680], [1280, 820]]) {
      await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
      const result = await evaluate(send, async (boundary) => {
        const f = window.__smokeUi;
        await f.frames();
        const panel = document.getElementById('termPanel');
        const m = f.measure();
        return {
          // 边界窗口允许夹紧可视高度，但保存值不能丢；放大后必须回到 230。
          persisted: panel.parentElement.classList.contains('term-manual') && window.keywarden.ui.get().termHeight === 230 &&
            (boundary ? panel.offsetHeight >= 180 && panel.offsetHeight <= 230 : panel.offsetHeight === 230),
          fitted: m.outer && m.fitted,
        };
      }, width === 1024);
      checks.push([width + 'x' + height + ' resize 不重置手动高度', result.persisted]);
      checks.push([width + 'x' + height + ' resize 自动 fit 且无外层溢出', result.fitted]);
    }
    checks.push(...await evaluate(send, async () => {
      const f = window.__smokeUi;
      document.getElementById('termSplitter').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
      await f.until(() => window.keywarden.ui.get().termHeight === 0);
      await f.frames();
      const panel = document.getElementById('termPanel');
      const m = f.measure();
      const reset = window.keywarden.ui.get().termHeight === 0 && !panel.parentElement.classList.contains('term-manual') &&
        panel.style.height === '' && m.compact && m.minHeight && m.fills && m.aligned;
      f.term.saveTermHeight(230);
      await f.until(() => window.keywarden.ui.get().termHeight === 230);
      f.button('term').click();
      await f.until(() => window.keywarden.ui.get().mode === 'term');
      await f.frames();
      const ui = window.keywarden.ui.get();
      return [
        ['双击分隔条将高度存为零并恢复自适应', reset],
        ['重启前持久化纯终端模式与手动高度', ui.mode === 'term' && ui.termHeight === 230 && f.state.mode === 'term'],
        ['假会话未产生任何连接请求', f.requests.every((r) => ['/api/refresh', '/api/sshd', '/api/disconnect'].includes(r.path))],
      ];
    }));
    for (const [width, height] of [[1440, 900], [1024, 680], [1280, 820]]) {
      await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
      const result = await evaluate(send, async () => {
        const f = window.__smokeUi;
        await f.frames();
        const panel = document.getElementById('termPanel');
        const style = getComputedStyle(panel);
        const m = f.measure();
        return f.state.mode === 'term' && window.keywarden.ui.get().termHeight === 230 &&
          style.flexGrow === '1' && style.flexShrink === '1' && m.fills && m.outer && m.fitted;
      });
      checks.push([width + 'x' + height + ' 纯终端忽略手动高度并随窗口填满', result]);
    }
  } finally {
    await evaluate(send, () => window.__smokeUi.cleanup());
    await send('Emulation.clearDeviceMetricsOverride');
  }
  return checks;
}

async function main() {
  if (process.platform !== 'win32') {
    console.log('PASS 桌面冒烟仅支持 Windows，本机跳过');
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
      hosts: [{ id: 'smoke', name: 'smoke', host: 'smoke.invalid', port: 22, username: 'smoke', identityFile: '' }],
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
    // Electron 的启动日志包含调试地址；成功或失败均不转发。
    stdio: 'ignore',
  });

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
    const themeBeforeUi = fs.readFileSync(path.join(tmp, 'theme.json'), 'utf8');
    const uiChecks = await runUiRegression(send);
    ws.close();

    // 同一 dataDir、全新 origin：UI 与主题各从自己的文件恢复，不靠 localStorage。
    let themeFile = '';
    let uiFile = {};
    const themeAfterUi = fs.readFileSync(path.join(tmp, 'theme.json'), 'utf8');
    try {
      themeFile = JSON.parse(themeAfterUi).choice;
      uiFile = JSON.parse(fs.readFileSync(path.join(tmp, 'ui.json'), 'utf8'));
    } catch {}
    // 单实例锁按 userData 划分，必须先关掉第一个进程再启动第二个
    try {
      execFileSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
    let restartState = { initial: '', stored: '', theme: '' };
    let restartChecks = [];
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
        restarted = spawn(ELECTRON, restartArgs, { cwd: ROOT, env, stdio: 'ignore' });
        const page2 = await waitForPage(restartPort);
        const ws2 = new WebSocket(page2.webSocketDebuggerUrl);
        await new Promise((resolve, reject) => {
          ws2.once('open', resolve);
          ws2.once('error', reject);
        });
        const send2 = cdp(ws2);
        try {
          restartState = await evaluate(send2, async (previousOrigin) => {
            const version = () => document.getElementById('version');
            for (let i = 0; i < 100 && (!version() || version().textContent === 'v—'); i++) {
              await new Promise((res) => setTimeout(res, 100));
            }
            return {
              initial: window.keywarden.theme.initial,
              stored: await window.keywarden.theme.get(),
              theme: document.documentElement.dataset.theme,
              originChanged: location.origin !== previousOrigin,
              uiInitial: window.keywarden.ui.initial,
              uiStored: await window.keywarden.ui.get(),
            };
          }, new URL(page.url).origin);
          await send2('Emulation.setDeviceMetricsOverride', { width: 1280, height: 820, deviceScaleFactor: 1, mobile: false });
          await evaluate(send2, installUiFixture);
          restartChecks = await evaluate(send2, async () => {
            const f = window.__smokeUi;
            try {
              await f.frames();
              const empty = f.state.mode === 'term' && document.body.classList.contains('mode-term') &&
                !f.state.session && !f.term.getTermCtl() && f.visible('#termEmpty') && !f.visible('.table-wrap') &&
                !f.visible('#termPanel') && f.button('term').getAttribute('aria-pressed') === 'true';
              f.showRows(1);
              await f.frames();
              const ctl = f.term.getTermCtl();
              const sid = f.state.session.sid;
              const fillsPure = document.getElementById('termPanel').offsetHeight > 230;
              f.button('keys').click();
              await f.until(() => window.keywarden.ui.get().mode === 'keys');
              await f.frames();
              const restored = f.state.mode === 'keys' && f.state.session.sid === sid && f.term.getTermCtl() === ctl &&
                document.getElementById('termPanel').offsetHeight === 230 &&
                document.querySelector('.server-panel').classList.contains('term-manual') &&
                window.keywarden.ui.get().termHeight === 230 && f.measure().aligned && f.measure().outer;
              // 验证完仍保留 term/230，不能用测试清理覆盖重启前后的持久化结果。
              f.button('term').click();
              await f.until(() => window.keywarden.ui.get().mode === 'term');
              await f.frames();
              const ui = window.keywarden.ui.get();
              return [
                ['重启无会话直接显示纯终端空态', empty],
                ['重启纯终端仍自动填满空间', fillsPure],
                ['重启从 term 切 keys 恢复手动高度与原终端', restored],
                ['重启切模式没有重连或额外补读', f.sockets.length === 1 && !ctl.ws.closed && f.requests.length === 0],
                ['最终保留 term 与手动高度且主题未改变', ui.mode === 'term' && ui.termHeight === 230 &&
                  window.keywarden.theme.get() === 'mist' && document.documentElement.dataset.theme === 'mist'],
              ];
            } finally {
              f.cleanup();
            }
          });
        } finally {
          ws2.close();
        }
      } catch (e) {
        if (attempt === 2) throw e;
        restartState = { initial: '', stored: '', theme: '' };
        restartChecks = [];
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
      ...uiChecks,
      ['UI 单独写入数据目录 ui.json', uiFile.mode === 'term' && uiFile.termHeight === 230 &&
        Object.keys(uiFile).sort().join(',') === 'mode,termHeight'],
      ['UI 操作未迁移或改写 theme.json', themeBeforeUi === themeAfterUi &&
        fs.readFileSync(path.join(tmp, 'theme.json'), 'utf8') === themeBeforeUi],
      ['重启使用新 origin 验证持久化', restartState.originChanged === true],
      ['重启 UI initial 与 get 恢复模式及高度', [restartState.uiInitial, restartState.uiStored].every((ui) =>
        ui && ui.mode === 'term' && ui.termHeight === 230)],
      ...restartChecks,
      ['最终 ui.json 仍保留 term 与手动高度', (() => {
        const ui = JSON.parse(fs.readFileSync(path.join(tmp, 'ui.json'), 'utf8'));
        return ui.mode === 'term' && ui.termHeight === 230;
      })()],
      ['页面没有未处理异常或控制台错误', pageErrors.length === 0],
    ];

    let failed = 0;
    for (const [name, ok] of checks) {
      console.log(`${ok ? 'PASS' : 'FAIL'} ${name}`);
      if (!ok) failed++;
    }
    console.log(`${failed ? 'FAIL' : 'PASS'} 桌面冒烟：${checks.length - failed} 项通过，${failed} 项失败`);
    if (failed) process.exitCode = 1;
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
      console.warn('FAIL 临时测试目录清理未完成');
    }
  }
}

main().catch(() => {
  // 异常栈、页面错误与 Electron stderr 可能含地址或凭据，不能直接打印。
  console.error('FAIL 桌面冒烟执行异常（运行时详情已隐藏）');
  process.exit(1);
});
