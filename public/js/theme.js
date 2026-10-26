'use strict';

/* ------------------------------------------------------------------ */
/* 主题：只改 <html data-theme>，配色全部交给 style.css 里的变量        */
/* ------------------------------------------------------------------ */

export const THEME_KEY = 'kw.theme';

export const THEMES = [
  { id: 'obsidian', name: '曜石', desc: '深色 · 蓝', scheme: 'dark' },
  { id: 'midnight', name: '深海', desc: '深色 · 青', scheme: 'dark' },
  { id: 'graphite', name: '石墨', desc: '深色 · 青绿', scheme: 'dark' },
  { id: 'cloud', name: '云白', desc: '浅色 · 蓝', scheme: 'light' },
  { id: 'linen', name: '米白', desc: '浅色 · 赭', scheme: 'light' },
  { id: 'mist', name: '雾灰', desc: '浅色 · 靛', scheme: 'light' },
];

/* id 只存「跟随系统」这个选择，真正落到哪个主题由系统深浅色决定 */
export const AUTO_THEMES = [
  { id: 'auto', name: '跟随系统', desc: '深 曜石 / 浅 云白', pair: ['obsidian', 'cloud'] },
  { id: 'auto-soft', name: '系统·柔和', desc: '深 石墨 / 浅 米白', pair: ['graphite', 'linen'] },
];

export const THEME_CHOICES = [...THEMES, ...AUTO_THEMES];

const IDS = new Set(THEMES.map((t) => t.id));
const AUTOS = new Map(AUTO_THEMES.map((t) => [t.id, t.pair]));

const listeners = new Set();

const mq =
  typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-color-scheme: dark)')
    : null;

let systemDark = mq ? mq.matches : true;
let choice = readChoice();
let resolved = resolve(choice);

function readChoice() {
  try {
    const v = localStorage.getItem(THEME_KEY);
    if (v && (IDS.has(v) || AUTOS.has(v))) return v;
  } catch {}
  return 'obsidian';
}

function resolve(id) {
  const pair = AUTOS.get(id);
  if (pair) return pair[systemDark ? 0 : 1];
  return IDS.has(id) ? id : 'obsidian';
}

export function currentChoice() {
  return choice;
}

export function resolvedTheme() {
  return resolved;
}

export function isSystemDark() {
  return systemDark;
}

export function onThemeChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function emit() {
  document.documentElement.dataset.theme = resolved;
  for (const fn of listeners) {
    try {
      fn(resolved, choice);
    } catch {
      /* 订阅方自己报错不影响主题切换 */
    }
  }
}

/** persist=false 用于设置弹窗里的即时预览，「取消」时再切回去 */
export function applyTheme(next, { persist = true } = {}) {
  choice = IDS.has(next) || AUTOS.has(next) ? next : 'obsidian';
  if (persist) {
    try {
      localStorage.setItem(THEME_KEY, choice);
    } catch {}
  }
  resolved = resolve(choice);
  emit();
  return resolved;
}

function setSystemDark(dark) {
  dark = Boolean(dark);
  if (dark === systemDark) return;
  systemDark = dark;
  // 手动主题不受系统影响，但要记住状态，之后切回「跟随系统」才能立刻对上
  if (!AUTOS.has(choice)) return;
  resolved = resolve(choice);
  emit();
}

let systemBound = false;

/** 桌面版用主进程的 nativeTheme（和资源管理器同一时间点变色），网页版退 matchMedia */
export function initSystemTheme() {
  if (systemBound || typeof window === 'undefined') return;
  systemBound = true;

  const sys = window.keywarden && window.keywarden.systemTheme;
  if (sys) {
    sys.get().then(setSystemDark).catch(() => {});
    sys.onChange(setSystemDark);
  }

  if (mq) {
    const onChange = (e) => setSystemDark(e.matches);
    if (mq.addEventListener) mq.addEventListener('change', onChange);
    else if (mq.addListener) mq.addListener(onChange);
  }
}

/* 终端配色：xterm 不认 CSS 变量，按主题给一套 ANSI（浅色主题要压暗，不然白底看不清） */
const TERM = {
  obsidian: {
    background: '#131920',
    foreground: '#d7e0ea',
    cursor: '#4c8dff',
    selectionBackground: '#26436b',
    black: '#1a1f26',
    red: '#f85149',
    green: '#3fb950',
    yellow: '#d6a034',
    blue: '#4c8dff',
    magenta: '#bc8cff',
    cyan: '#39c5cf',
    white: '#c9d1d9',
    brightBlack: '#5c6773',
    brightRed: '#ff7b72',
    brightGreen: '#56d364',
    brightYellow: '#e3b341',
    brightBlue: '#79b8ff',
    brightMagenta: '#d2a8ff',
    brightCyan: '#56d4dd',
    brightWhite: '#f0f6fc',
  },
  midnight: {
    background: '#0d1420',
    foreground: '#d6e2f0',
    cursor: '#38bdf8',
    selectionBackground: '#14455e',
    black: '#111a26',
    red: '#fb7185',
    green: '#34d399',
    yellow: '#fbbf24',
    blue: '#38bdf8',
    magenta: '#c4a2ff',
    cyan: '#67e8f9',
    white: '#c9d6e4',
    brightBlack: '#5f7088',
    brightRed: '#fda4af',
    brightGreen: '#6ee7b7',
    brightYellow: '#fcd34d',
    brightBlue: '#7dd3fc',
    brightMagenta: '#ddc4ff',
    brightCyan: '#a5f3fc',
    brightWhite: '#f0f6fc',
  },
  graphite: {
    background: '#191c1f',
    foreground: '#d9dddf',
    cursor: '#2dd4bf',
    selectionBackground: '#1f4b46',
    black: '#1b2024',
    red: '#f87171',
    green: '#4ade80',
    yellow: '#e5b567',
    blue: '#6cb6e8',
    magenta: '#c084fc',
    cyan: '#2dd4bf',
    white: '#d4d8da',
    brightBlack: '#6d7579',
    brightRed: '#fca5a5',
    brightGreen: '#86efac',
    brightYellow: '#f0cf94',
    brightBlue: '#93cdf0',
    brightMagenta: '#d8b4fe',
    brightCyan: '#5eead4',
    brightWhite: '#f2f4f5',
  },
  cloud: {
    background: '#ffffff',
    foreground: '#1f2937',
    cursor: '#2563eb',
    selectionBackground: '#cfe0fb',
    black: '#1f2937',
    red: '#b42318',
    green: '#0f7a37',
    yellow: '#8a5a0f',
    blue: '#1d4ed8',
    magenta: '#7b2ea8',
    cyan: '#0e7490',
    white: '#64748b',
    brightBlack: '#6b7280',
    brightRed: '#d92d20',
    brightGreen: '#14883f',
    brightYellow: '#a86a12',
    brightBlue: '#2563eb',
    brightMagenta: '#9333ea',
    brightCyan: '#0891b2',
    brightWhite: '#111827',
  },
  linen: {
    background: '#fffdf8',
    foreground: '#2a2620',
    cursor: '#a05a35',
    selectionBackground: '#f0dccd',
    black: '#2a2620',
    red: '#9d2d22',
    green: '#256b33',
    yellow: '#8a5710',
    blue: '#315d8c',
    magenta: '#7a3f6b',
    cyan: '#1f6f74',
    white: '#6a6152',
    brightBlack: '#8a8175',
    brightRed: '#c0392b',
    brightGreen: '#2f7d3f',
    brightYellow: '#a86b16',
    brightBlue: '#3f6f9e',
    brightMagenta: '#8f4c7f',
    brightCyan: '#2a8288',
    brightWhite: '#221e18',
  },
  mist: {
    background: '#fbfcfe',
    foreground: '#1e2430',
    cursor: '#4f46e5',
    selectionBackground: '#dcdcfb',
    black: '#1e2430',
    red: '#a92333',
    green: '#0f6e4e',
    yellow: '#8b5f12',
    blue: '#4338ca',
    magenta: '#6d28d9',
    cyan: '#0e7490',
    white: '#586377',
    brightBlack: '#7f8898',
    brightRed: '#cc2b3d',
    brightGreen: '#12805a',
    brightYellow: '#a4711a',
    brightBlue: '#4f46e5',
    brightMagenta: '#8b5cf6',
    brightCyan: '#0891b2',
    brightWhite: '#151a23',
  },
};

export function termPalette(id) {
  return { ...(TERM[id] || TERM.obsidian) };
}
