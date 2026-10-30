/*
 * Tauri 桌面桥：只填 window.keywarden，形状与旧 Electron preload 完全一致，
 * 前端 js/*.js 一行不用改。注入在页面脚本之前，theme.initial / ui.initial 可同步读。
 */
(function () {
  'use strict';
  var T = window.__TAURI__;
  if (!T || !T.core || !T.window) return;

  var init = window.__KW_INIT || { theme: '', ui: { mode: 'keys', termHeight: 0 } };
  var invoke = T.core.invoke;

  /* 会话令牌由初始化脚本一次性带进来，落成当前 origin 的 Cookie 后立刻从 window 上删掉。
     服务端已设 KW_NO_AUTO_COOKIE=1，本机别的进程扫到端口也拿不到会话。
     这里用非 HttpOnly 是 WebView2 里唯一可行的通道；令牌本来就已在页面 JS 内存中，
     作用域限于 127.0.0.1:<随机端口>，且 SameSite=Strict。 */
  if (init.token) {
    try {
      document.cookie = 'kw=' + init.token + '; Path=/; SameSite=Strict';
    } catch (e) {
      /* 文档还没拿到 origin 时跳过，页面会以 401 明确失败，不静默降级成不限权 */
    }
    delete init.token;
  }

  function currentWindow() {
    return T.window.getCurrentWindow();
  }

  /** 解除监听：Tauri v2 返回 Promise<UnlistenFn>，也兼容同步返回函数的老写法 */
  function unlisten(handle) {
    if (!handle) return;
    if (typeof handle === 'function') handle();
    else if (typeof handle.then === 'function') {
      handle.then(function (fn) {
        if (typeof fn === 'function') fn();
      });
    }
  }

  var api = {
    isDesktop: true,
    pickFile: function (opts) {
      return invoke('pick_file', { opts: opts || {} });
    },
    pickDir: function (opts) {
      return invoke('pick_dir', { opts: opts || {} });
    },
    openDataDir: function () {
      return invoke('open_data_dir');
    },
    windowControls: {
      minimize: function () {
        currentWindow().minimize();
      },
      toggleMaximize: function () {
        invoke('toggle_maximize');
      },
      close: function () {
        currentWindow().close();
      },
      isMaximized: function () {
        return invoke('is_maximized');
      },
      onMaximizeChange: function (cb) {
        var apply = function () {
          invoke('is_maximized')
            .then(function (v) {
              cb(Boolean(v));
            })
            .catch(function () {});
        };
        var off = currentWindow().onResized(apply);
        apply();
        return function () {
          unlisten(off);
        };
      }
    },
    readClipboard: function () {
      return invoke('read_clipboard').then(function (t) {
        return typeof t === 'string' ? t : '';
      });
    },
    writeClipboard: function (text) {
      return invoke('write_clipboard', { text: text == null ? '' : String(text) });
    },
    systemTheme: {
      get: function () {
        return invoke('system_theme');
      },
      onChange: function (cb) {
        var off = currentWindow().onThemeChanged(function (e) {
          cb(Boolean(e && e.payload === 'dark'));
        });
        return function () {
          unlisten(off);
        };
      }
    },
    theme: {
      initial: init.theme || '',
      get: function () {
        return api.theme.initial;
      },
      set: function (choice) {
        return invoke('theme_set', { choice: String(choice || '') }).then(function (v) {
          api.theme.initial = typeof v === 'string' ? v : api.theme.initial;
          return v;
        });
      }
    },
    ui: {
      initial: init.ui || { mode: '', termHeight: 0 },
      get: function () {
        return api.ui.initial;
      },
      set: function (patch) {
        return invoke('ui_set', { patch: patch || {} }).then(function (v) {
          if (v) api.ui.initial = v;
          return v;
        });
      }
    }
  };

  window.keywarden = api;

  /* 无边框窗口的拖动区：Tauri 认 data-tauri-drag-region="deep"（子树内点击也算拖动、
     按钮/下拉等可点元素自动排除），CSS 的 app-region 只在认它的内核里生效，两套都标上。 */
  function markDragRegions() {
    var bar = document.querySelector('.topbar');
    if (!bar) return;
    bar.setAttribute('data-tauri-drag-region', 'deep');
  }

  function injectDragCss() {
    var style = document.createElement('style');
    style.id = 'kw-drag-css';
    style.textContent =
      'body.desktop .topbar{-webkit-app-region:drag;app-region:drag;user-select:none}' +
      'body.desktop .topbar button,body.desktop .topbar select,body.desktop .topbar input,' +
      'body.desktop .topbar a,body.desktop .topbar [role="button"]' +
      '{-webkit-app-region:no-drag;app-region:no-drag}';
    (document.head || document.documentElement).appendChild(style);
  }

  function boot() {
    injectDragCss();
    markDragRegions();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
