'use strict';
/**
 * 把 build/icon.svg 渲染成 512×512 带透明通道的 PNG（electron 自带 Chromium，不额外装依赖）。
 * 用法：npx electron build/render-icon.cjs build/icon.svg build/icon.png
 *
 * 不能用 capturePage：窗口背景不透明，导出的 PNG 没有 alpha，圆角外会变成白底。
 */

const { app, BrowserWindow } = require('electron');
const fs = require('fs');
const path = require('path');

const [, , srcArg, outArg] = process.argv;
if (!srcArg || !outArg) {
  console.error('用法：electron build/render-icon.cjs <input.svg> <output.png>');
  app.exit(1);
}

const SIZE = 512;

app.whenReady().then(async () => {
  const svg = fs.readFileSync(path.resolve(srcArg), 'utf8');
  const win = new BrowserWindow({
    width: SIZE,
    height: SIZE,
    show: false,
    frame: false,
    webPreferences: { offscreen: true, contextIsolation: true, nodeIntegration: false },
  });

  const html = '<!doctype html><meta charset="utf-8"><style>html,body{margin:0;background:transparent}canvas{display:block}</style><canvas id="c" width="512" height="512"></canvas>';
  await win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));

  const dataUrl = await win.webContents.executeJavaScript(
    `new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => {
        const canvas = document.getElementById('c');
        const ctx = canvas.getContext('2d');
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
        resolve(canvas.toDataURL('image/png'));
      };
      img.onerror = () => reject(new Error('SVG 加载失败'));
      img.src = ${JSON.stringify('data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg))};
    })`,
    true
  );

  fs.writeFileSync(path.resolve(outArg), Buffer.from(dataUrl.split(',')[1], 'base64'));
  console.log(`已生成 ${outArg}（${SIZE}×${SIZE}，带透明通道）`);
  app.exit(0);
}).catch((e) => {
  console.error(e.stack || e.message);
  app.exit(1);
});
