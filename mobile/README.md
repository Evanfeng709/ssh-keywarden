# kw_mobile（Flutter 手机端）

直连 `kw-ffi` 的薄客户端：**不开 HTTP、不依赖桌面壳**，会话与密钥操作都复用
`kw-server` 的同一套实现（`connect::open` / `keys_ops::*`），所以「防锁死」闸门
与桌面端走同一条代码路径。

## 目录

```
lib/
├── main.dart                 应用入口（主题选择 + 状态注入）
└── src/
    ├── app_paths.dart        应用可写目录（Android 走 MethodChannel，iOS/macOS 用沙盒 HOME）
    ├── kw_ffi.dart           C ABI 绑定：kw_init / kw_call / kw_free_string
    ├── models.dart           Host / LocalKey / KeyItem / SessionView / Bootstrap / MsStatus
    ├── app_state.dart        状态与所有业务调用
    ├── terminal_emulator.dart 纯 Dart 的 VT 子集仿真（网格、SGR、滚动区、回滚）
    ├── terminal_runner.dart  后台 isolate 里的 PTY 代理（长轮询 + 同步写入）
    └── ui/
        ├── shell.dart        三 Tab 外壳 + 主题入口 + 初始化失败页
        ├── keys_tab.dart     主机、连接、密钥卡片、增删换、备份、sshd
        ├── keyring_tab.dart  微软账号同步面板（配置/登录/同步/退出）+ 随身密钥包
        ├── terminal_tab.dart 纯终端页（PTY + 按键栏 + 粘贴）
        ├── common.dart       弹窗 / 提示 / 指纹缩写
        └── theme.dart        6 套色板（曜石/午夜/石墨/云白/亚麻/雾青）+ 主题弹层
```

## 本机（Windows）调试

Windows 只是开发宿主，手机上不会走这条路径。

```powershell
# 1) 编 Rust 动态库
cargo build -p kw-ffi --release

# 2) 放到 exe 旁边（或设 KW_FFI_LIB 指向它）
copy ..\target\release\kw_ffi.dll build\windows\x64\runner\Debug\

# 3) 跑
flutter run -d windows
```

`kw_mobile` 会按 `KW_APP_DIR` → Android MethodChannel → `%APPDATA%\ssh-keywarden-mobile`
的顺序找可写目录；本地调试建议显式设 `KW_APP_DIR` 到临时目录，避免污染真实配置。

## 测试

```powershell
$env:KW_FFI_LIB='D:\my-own-program\ssh-keywarden-rs\target\release\kw_ffi.dll'
# 本机代理若劫持了 localhost 的 WebSocket，需要放行：
$env:NO_PROXY='127.0.0.1,localhost'
flutter test
flutter analyze
```

`test/ffi_test.dart` 是真 FFI：初始化 → 生成 Ed25519 → 扫描密钥包 → 存主机 →
写备注 → 微软同步状态 / 客户端 ID 校验 / 记住与清除同步口令 → 删除主机 → 未知方法报错。

`test/terminal_emulator_test.dart` 只测网格仿真器（不连 SSH）：SGR / 256 色、
宽字符与续格、ED/EL、插删行列、滚动区、备用屏进出、括号粘贴、回滚上限。
终端本身不在 Dart 侧：`kw-server/tests/e2e.rs` 的
`pty_terminal_buffers_echo_tracks_size_and_closes_on_exit` 在内存版 russh 服务端上
验了 pty-req 的 TERM/行列、回显、stderr 合并、resize 和退出；`kw-ssh` 的单测只覆盖
输出缓冲。**没有接真实 Linux shell 跑全屏程序**。

## Android / iOS

Android 已用本机的 **NDK 28.2.13676358 + cargo-ndk 4.1.2** 编过三种 ABI：

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk --locked
export ANDROID_NDK_HOME="$HOME/AppData/Local/Android/sdk/ndk/28.2.13676358"

cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -P 24 \
  -o mobile/android/app/src/main/jniLibs \
  build -p kw-ffi --release

cd mobile
flutter build apk --release                    # 通用包
flutter build apk --release --split-per-abi    # 按 ABI 拆分
```

产物在 `mobile/build/app/outputs/flutter-apk/`，仓库另存了一份到 `dist/android/`：

| 文件 | 大小 | 说明 |
|---|---|---|
| `SSH-Keywarden-0.3.0-universal.apk` | 约 72 MB | 三种 ABI 的 `libkw_ffi.so` 都在里面 |
| `SSH-Keywarden-0.3.0-arm64-v8a.apk` | 约 25 MB | 只带 arm64 库，多数新机装这个 |

（0.3.0 体积比 0.2.0 大，是微软同步引入 reqwest + rustls 的代价。）

注意两点：

- **`AndroidManifest.xml` 必须显式写 `INTERNET` 权限**。Flutter 只在 debug/profile
  的 manifest 里加它，release 包不会自动带；漏了会在真机上「连接失败」而看不出原因。
- 当前 release 沿用 Flutter 模板的 **debug 签名**，可以直接侧载安装，但不能上应用商店，
  换签名后也不能覆盖安装。

iOS 需要 Xcode + `aarch64-apple-ios` 目标，把 `staticlib` 链进 Runner；
`kw_init` 的目录由 Swift 侧 `FileManager` 的 Application Support 提供（当前 Dart 侧
走 `HOME/Library/Application Support`，与沙盒一致）。

## 终端与微软账号同步

终端页走真 PTY，不是假壳：

- `termOpen`/`termRead`/`termWrite`/`termResize`/`termClose` 五个方法，底层是
  `kw-ssh` 的 `Terminal`：真正的 `request_pty` + `request_shell`，`TERM=xterm-256color`，
  远端拿到正常 tty（不是 `exec` 包一层）；
- 读是长轮询：`termRead` 带 `waitMs`，有输出立刻返回，没输出最多等到上限
  （默认 250 ms，封顶 1 s）再返回空；手机端按这个节奏循环，不需要常驻线程；
- 写入和 resize 共用同一条 channel 的写半边（`Mutex` 串行），`termResize` 只发
  `window-change`，不重开通道；
- 渲染在 Dart 侧：`terminal_emulator.dart` 是纯 Dart 的 VT 子集（宽字符、SGR /
  256 色 / 真彩、ED/EL、插删行列、滚动区、备用屏 `?1049`、括号粘贴 `?2004`、
  OSC 0/2 标题、回滚缓冲），`terminal_runner.dart` 把轮询和写入放进后台 isolate；
- **已知限制**：不是完整 xterm——鼠标上报、左右边距、超链接、同步输出（`?2026`）、
  iTerm 图片协议、IRM 插入模式没做，遇到安静忽略；OSC 52 剪贴板也不认，
  粘贴走界面上的按钮。没有真机点按验收，`flutter test` 只覆盖仿真器本身。

微软账号同步（「密钥包 · 同步」页）：

- 先在 [portal.azure.com](https://portal.azure.com) 的 Microsoft Entra ID → 应用注册里注册一个
  个人账户应用（允许公共客户端流；Graph 委托权限 `Files.ReadWrite.AppFolder`、`User.Read`、
  `offline_access`），把「应用程序(客户端) ID」填进同步页的配置弹窗；
  仓库不内置任何客户端 ID，也不借用别人的应用；
- 登录走设备码流：点「登录微软账号」后屏幕给出 8 位码和地址，
  在任意浏览器输一次即可；token 存在 `data/ms-auth.json`，界面上不显示；
- 同步包（主机 / 备注 / 主机指纹，不含私钥）在 `data/` 里用口令加密后上传到
  OneDrive 应用专属文件夹的 `ssh-keywarden.kwsync`；口令可记在本机（`ms-config.json`）；
- 主机指纹冲突时弹窗二选一：「保留本机」（其余变更照常拉取，本次不上传）或
  「全部覆盖后重传」；
- 「退出」只删本机 token，云端包保留；「清除同步口令」只忘掉本机记住的那份。
- **已知限制**：实网登录/上传尚未用真实微软账号验收（需要你自己的客户端 ID）；
  主题选择不落盘；Windows 上 `ms-auth.json` 只能靠用户目录 ACL。
