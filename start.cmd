@echo off
chcp 65001 >nul
title SSH Keywarden
cd /d "%~dp0"

where node >nul 2>nul
if errorlevel 1 (
  echo.
  echo [错误] 没有找到 node。请先安装 Node.js 18 或更高版本：https://nodejs.org/
  echo.
  pause
  exit /b 1
)

if not exist "node_modules\ssh2" goto install
if not exist "node_modules\ws" goto install
if not exist "node_modules\@xterm\xterm" goto install
goto run

:install
  echo 正在安装/补齐依赖（首次约 1 分钟）...
  call npm install --no-audit --no-fund
  if errorlevel 1 (
    echo.
    echo [错误] 依赖安装失败，请检查网络后重试。
    echo.
    pause
    exit /b 1
  )

:run
echo 正在启动，浏览器会自动打开 http://127.0.0.1:7791
start "" cmd /c "timeout /t 2 /nobreak >nul & start http://127.0.0.1:7791"

node server.js

echo.
echo 服务已停止。
pause
