# CHANGELOG

> 版本号规则：语义化版本（semver），当前 `0.3.1`。
> 单一来源是根 `Cargo.toml` 的 `[workspace.package].version`；
> `crates/kw-desktop/tauri.conf.json` 的 `version` 与本文件标题必须同步，
> 由 `kw-desktop` 的单测 `version_matches_tauri_conf_and_changelog` 兜底。

## v0.4.0 — 2026-10-30

### 新增

- **私钥可以随同步包走了（必须设口令）**：以前只同步「怎么连」，手机要连新服务器只能重新造一把钥匙。
  现在同步包会带上**主机实际在用**的私钥（不是整个密钥库），落地到本机 `<data>/keys`（0600）。
  口令留空时**直接拒绝**带上私钥 —— 宁可同步失败，也不把私钥摊进明文包。
  加密沿用同步包那一套（Argon2id + AES-256-GCM），云端只有密文。
  加密私钥在没有 passphrase 时算不出指纹，会跳过并在 stderr 说明是哪台主机的，不假装同步成功。
- **主机记「首选私钥指纹」**：`Host` 新增 `identityFingerprint`（跨设备有效），
  `identityFile` 仍是本机路径。连接时的优先级为「请求显式指定 → 本机路径 → 按指纹去本地密钥库找」，
  所以换设备后不用再手动重指路径。切换登录密钥时会同步更新这个指纹，避免同步出去的是旧钥匙。
- **手机端「粘贴公钥」**：新增密钥原来只能现场生成，现在可切到「粘贴公钥」，
  把电脑上同步过来的那把直接追加到服务器。内容为空时按钮不响应。

### 变更

- `export_bundle` 增加 `keys_dir` 与 `include_keys`，`import_bundle` 增加 `keys_dir`；
  微软账号同步固定开启（没有口令根本走不到那一步），CLI 与 FFI 测试按需传 false。
- 导入报告新增 `importedKeys`。

### 测试

- 新增 `private_keys_need_passphrase_and_land_on_import`：验证「无口令必须拒绝」、
  「导入后能按指纹找回私钥」、「对方的主机路径不会覆盖本机」。
  这条测试上线就抓到一个真 bug —— 落地文件名取指纹前 12 位，而 base64 指纹里含 `/`，
  会把路径劈成子目录（`os error 3`），单跑时靠运气没踩上。现已只保留字母数字。
- `cargo test --workspace`：**149 项通过**；`flutter analyze` 无问题。

## v0.3.1 — 2026-10-30

### 新增

- **桌面客户端内置微软账号同步界面**：顶栏新增「同步」按钮（状态点：灰=未配置、
  黄=冲突/出错、绿=已登录），弹窗里完成配置 Azure 客户端 ID → 设备码登录（复制 8 位码 /
  打开登录页，最长等 5 分钟）→ 立即同步；同步口令可记住（存本机 `ms-config.json`），
  退出登录只清本机 token。主机指纹冲突直接在弹窗里选择「保留本机」或
  「全部覆盖后重传」。`kw-sync` CLI 保留不动。

### 变更

- 同步包与微软账号同步实现从 `kw-ffi` 上移到 `kw-server`，桌面端与手机端共用同一份实现；
  `kw-ffi` 只 `pub use` 转发，`kw-sync` CLI 改依赖 `kw-server`。
- 新增本地 HTTP 路由 `/api/sync/*`（status / config / login start+complete / now / logout）。
  设备码中间态只存内存、不落盘不进日志，重启即失效；`kw-server` 新增 6 个不联网的
  同步路由测试。

## v0.3.0 — 2026-10-30

### 新增

- **微软账号同步（OneDrive 应用文件夹）**：同步包走 OneDrive 的
  `special/approot`（`ssh-keywarden.kwsync`），桌面端与手机端不再需要倒腾文件或连同一个 Wi-Fi。
  认证用 OAuth2 设备码流（`login.microsoftonline.com/consumers`，范围
  `Files.ReadWrite.AppFolder User.Read offline_access`），电脑/手机各显示一个 8 位验证码，
  在浏览器输一次即可；token 存 `data/ms-auth.json`，不上传、不打印。
- **客户端 ID 可配置**：本仓不内置任何 Azure 应用 ID。首次使用需自行在
  「Microsoft Entra ID → 应用注册」注册一个个人账户应用（公共客户端流，Graph 委托权限
  `Files.ReadWrite.AppFolder` / `User.Read` / `offline_access`），然后填应用（客户端）ID：
  桌面 CLI 用 `--client-id` 或 `KW_MS_CLIENT_ID`，手机端在同步页的配置弹窗里填。
- **端到端加密沿用同步包口令**（Argon2id + AES-256-GCM），云端与传输链路都只有密文；
  私钥仍不上传。口令可记在本机（`data/ms-config.json`），也可每次输入；
  忘了口令只能删掉云端包重新开始。
- **主机指纹冲突不上传**：拉到与本机不一致的 `known_hosts` 指纹时，保留本机、其余变更照常合并，
  停止上传并列出冲突；确认可信后显式重跑（手机端「全部覆盖」、CLI `--trust-changed`）。
- **`kw-sync` CLI 改为 `login` / `sync` / `status` / `logout`**，全部支持 `--json`；
  `sync` 的口令顺序是 `KW_SYNC_PASSPHRASE` → 本机记住的口令 → 交互询问，
  `--remember-key` 决定同步后是否记住口令。
- **`kw-ffi` 新接口**：`msStatus` / `msSaveConfig` / `msStartLogin` / `msCompleteLogin` /
  `msLogout` / `msSync`；`msSync` 回传 `{pulled,pushed,lastSyncAt,report}`，
  冲突在 `report.knownHostConflicts` 里。
- **手机端 6 套手动主题**：曜石 / 午夜 / 石墨（深色）与云白 / 亚麻 / 雾青（浅色），
  顶栏调色板按钮弹出选择层；全部颜色改成运行时取值，切换后整屏（含终端、底部导航）即时生效。
- **手机端界面收紧**：紧凑 AppBar（50 px）、11 px 辅助文字、38 px 按钮、dense 列表，
  同屏信息量明显提高；「密钥包·同步」页改成微软账号面板（未配置 / 未登录 / 已登录三态，
  配置弹窗带 Azure 注册步骤、设备码登录弹窗、同步口令弹窗、退出与清除口令）。
- **TLS**：reqwest 走自带 rustls（ring）与内置 `webpki-roots`，不依赖 Android 平台验证器
  （平台验证器在部分机型上会 panic），也不读取系统私有根证书。

### 移除

- **局域网直传**：`kw-ffi` 的 `lan.rs`、`kw-sync serve / pull / push`、8 位配对码、
  UDP 47999 广播发现、手机端「局域网拉取 / 推送」，以及对应 5 项 FFI 测试。
- **手动文件 / 文本同步**：FFI 的 `exportSync` / `importSync`、桌面 CLI 的 `export` / `import`、
  手机端 Android SAF 存取与粘贴导入。同步统一走微软账号；离线仍可手工拷贝
  `ms-config.json` 之外的数据目录。

### 测试与边界

- `cargo test --workspace`：142 项全部通过（`kw-ffi` 新增微软同步 5 项——客户端 ID 形态、
  配置往返、退出保留配置、清空 ID、未配置提示，以及无口令时先拒绝联网；
  `kw-sync` 参数解析 2 项；删除 LAN 5 项）。
- `flutter analyze` 无告警；`flutter test` 28 项全部通过（真 FFI 用例需 `KW_FFI_LIB`，
  未设置时跳过）。
- 手机端在 Windows 宿主实测：同步面板未配置态、6 主题弹层（顺手修掉小窗时 26 px 溢出，
  弹层改为可滚动）、云白浅色主题整屏换色、Azure 配置弹窗。
- 未做：用真实微软账号 + 自建 Azure 应用的实网登录与上传下载验收（需用户自己的客户端 ID）；
  Android 真机 / 模拟器点击验收。
- 边界：`ms-auth.json` 在 Unix 是 0600，Windows 只能依赖用户目录 ACL；
  主题选择不落盘（重启回到曜石）；企业私有根 CA 不被信任（只认 `webpki-roots`）；
  桌面端 UI 未加同步入口，仍用 `kw-sync` 命令行。

## v0.2.0 — 2026-10-30

### 新增

- **手机端纯终端（真 PTY）**：`kw-ffi` 新增 `termOpen` / `termRead` / `termWrite` /
  `termResize` / `termClose`。C ABI 没有回调，所以 Rust 侧按会话缓冲输出
  （1 MiB，放不下整块丢弃并置 `dropped`），Dart 侧长轮询（默认 250 ms、最长 1000 ms）。
  轮询跑在独立 isolate 里，写字同步执行以保证按键顺序；切 Tab 只 pause 不杀 shell，
  断连或关闭时才回收句柄。
- **Dart 侧 VT 子集仿真**（纯 Dart，无新依赖）：SGR（16/256 色、粗体/暗色/下划线/反显）、
  CUP/CUU/CUD/CUF/CUB、ED/EL/ECH、插入与删除行、滚动区、延迟折行、备用屏、
  应用光标键、括号粘贴、标题与 2000 行回滚。
- **局域网直传**：`kw-sync serve / pull / push`，手机端「局域网拉取 / 推送到电脑」。
  8 位配对码（去掉 `I/O/0/1`），**配对码本身不上线**——服务端每次连接现发 32 字节随机盐，
  客户端用 `Argon2id(码, 盐)` 派生密钥发 AES-256-GCM 凭证；同步包同样用配对码加密。
  UDP 47999 广播发现（手机不用填 IP），5 分钟 TTL、输错三轮停止、成功一次即关监听。
- FFI 新增 `lanFetch` / `lanOffer`；`lanFetch` 回传整理后的配对码，手机端直接拿它当导入口令，
  指纹冲突确认沿用既有的 `importSync` 弹窗。
- `ImportReport` / `KnownHostConflict` 支持反序列化（推送方向要读对方的合并回执）。

### 测试与边界

- `cargo test --workspace`：141 项全部通过；新增 `kw-ffi` 局域网直传 5 项
  （回环拉取、回环推送、错码拒收、抓包断言线上无明文、配对码整理）。
- 直传已用两套数据目录在本机 TCP 回环上实跑 `serve → pull` 与 `serve → push`；
  另用 Dart 侧同一份 `kw_init` / `kw_call` 契约跑通 `lanFetch → importSync` 与 `lanOffer`，
  验证主机确实落到对方 `config.json`。
- 终端仿真纯 Dart，27 项单测；`flutter analyze` 无告警；手机端 Windows 宿主可编译运行。
- 未做：真机 / 模拟器上的手机端点击验收；真实 Linux sshd 的交互验收；
  跨网段与多网卡环境的 UDP 发现。

## v0.1.1 — 2026-10-29

### 新增

- **Secret 接入认证链路**：支持反序列化、不实现序列化、调试输出脱敏；
  `ConnectBody` / `SshConfig` 的密码、私钥及 passphrase 使用 `Secret`，
  连接配置在认证完成后、远端探测前释放。C ABI 的 connect 直接解析类型化请求，
  不再额外复制整份含凭据的 JSON。
- **独立浏览器模式**：`cargo run -p kw-server`；支持 CLI / 环境变量设置监听端口、
  资源目录覆盖与 `--no-open`。只监听本机，通过临时 HTML 登录文件进入，
  控制台不打印令牌或监听地址；登录发放 HttpOnly / SameSite=Strict Cookie，
  禁止缓存与 Referer 传播，随后跳转到无令牌的首页。
- **退出清理**：支持 Ctrl+C、Windows Ctrl+Break、Unix SIGTERM；
  正常退出删除临时登录文件、断开 SSH 会话，并限制 HTTP 服务退出等待时间。
- **SSH 端到端测试**：19 项，真实 russh 环回认证与路由调用，远端文件和命令在内存模拟；
  覆盖 TOFU、密码与加密私钥、新钥验登顺序、防删空/锁死、备份恢复、sshd 安全闸门、
  并发写、断连，以及真实 HTTP 的浏览器登录鉴权。

### 修复

- 认证方式以真正成功的认证结果为准，私钥失败后用密码登录不再被误标为密钥登录，
  避免绕过「仅密钥登录」安全闸门。
- 密码与 passphrase 保留首尾空格，FFI 生成密钥也不再裁剪空格；
  全空格 passphrase 不会误变成未加密私钥。
- 桌面冒烟测试使用隔离密钥目录和主机墓碑，避免读取真实 SSH 配置；
  Cookie 检查失败时也不打印令牌。

### 测试与边界

- `cargo test --workspace`：127 项全部通过；桌面 WebView2 冒烟：23/23 通过。
- 新增 Edge 浏览器冒烟 18 项，已验证登录、视图切换、设置弹窗、刷新后鉴权、
  Windows 退出信号、登录文件删除及监听关闭。
- Secret 清零仅覆盖其拥有的 Rust 缓冲区，不保证 HTTP 原始请求、第三方库副本或
  调用方 C 缓冲区全部清零；C 入参仍由调用方负责释放。
- 临时登录文件在 Unix 使用 0600，Windows 依赖用户临时目录 ACL；
  强制终止可能留下文件，需要手动清理。
- 内存 SSH 服务端不执行真实系统 shell 或 sshd reload，不替代实机脚本验收。
- 本轮未改同步协议和手机界面，未重打 APK；原有同秒备份覆盖限制仍存在。

## v0.1.0 — 2026-10-29

Rust 重写版的首个可运行版本。前端 `assets/public` 与旧 Electron 版 `public/`
逐字节一致，后端与桌面壳整体替换。

### 新增

- **七 crate 分层**：`kw-core`（凭据封装）、`kw-keys`（authorized_keys / keygen / 本地扫描 /
  备份差异 / sshd 文本判定）、`kw-store`（配置与备注持久化）、`kw-ssh`（russh 连接、
  TOFU 主机指纹、原子写、sshd 改写）、`kw-server`（axum 本地 HTTP/WS 兼容层）、
  `kw-desktop`（Tauri v2 无边框桌面壳）、`kw-ffi`（手机端 C ABI 门面）。
- **`/api/*` 与旧版同形**：连接 / 刷新 / 断开 / 主机 / 设置 / 备注 / 本地密钥 / 备份列表、
  差异、恢复 / 新增 / 更换 / 删除 / sshd / 终端 WebSocket / 静态资源，前端无需改动。
- **桌面壳能力**：无边框窗口与自绘窗口按钮、单实例、原生文件/目录选择、剪贴板、
  `theme.json` / `ui.json` / `window-state.json` 持久化、窗口坐标夹回最近显示器、
  页面加载 Started 才显示（防白屏）。
- **数据接管**：`KW_DATA_DIR` 优先，否则 `%APPDATA%\ssh-keywarden\data`（与旧 Electron 同址），
  exe 旁若存在 `data/` 自动迁移三个核心数据文件。
- **`kw-ffi`（手机端门面）**：`MobileEngine` + C ABI（`kw_init` / `kw_call` /
  `kw_free_string`），不开 HTTP 直接复用会话与密钥操作；随身密钥包落在
  `app_dir/keys`（0600），同步包支持明文或 Argon2id + AES-256-GCM 口令加密，
  导入按 `(host, port, user)` 去重、备注不覆盖、指纹只补缺。
- **`mobile/` Flutter 手机端**：三 Tab（服务器密钥 / 终端占位 / 密钥包与同步），
  Dart FFI 直连 `kw_ffi`，主机增删、连接（含主机指纹确认）、密钥卡片、新增 / 三步更换 /
  删除、备份差异与恢复、sshd 切换、随身密钥包生成、同步包导入导出全部走真实 FFI。
- **共用层重构**：`kw-server::connect` 与 `keys_ops::{list_backups, backup_diff, set_key_only}`
  从路由里抽出，桌面 HTTP 与手机 FFI 走同一条代码路径。
- **Android APK**：NDK 28.2.13676358 + cargo-ndk 4.1.2 交叉编译
  `arm64-v8a / armeabi-v7a / x86_64` 三份 `libkw_ffi.so`，`flutter build apk --release`
  出通用包（63.2 MB）与按 ABI 拆分包（arm64 21.8 MB）；产物另存 `dist/android/`。
  同时补上 **release 的 `INTERNET` 权限** —— Flutter 只在 debug/profile 的 manifest
  里带它，漏掉会让真机上的 SSH 连接直接失败。
- **桌面 ⇄ 手机同步（P0，只同步元数据）**：`kw-store` 为主机与备注加删除墓碑
  （`deletedAt` / 空备注）与 `merge_host` / `merge_note`（`updated_at` 新者胜，
  本机私钥路径不被覆盖）；`kw-ffi::sync` 升级为 bundle v2——导出含墓碑、
  导入报告新增/更新/删除计数与 `knownHostConflicts`，主机指纹不一致默认不覆盖、
  需 `trustChanged` 显式信任；新增 **`kw-sync` 桌面 CLI**（`export` / `import`，
  口令加密，支持 `--trust-changed` / `--json` / `KW_SYNC_PASSPHRASE`）；
  手机端同步页支持 **Android SAF 存/取 `.kwsync` 文件**与指纹冲突确认弹窗。
  私钥刻意不进同步包：每台设备一把自己的钥匙。
- `LICENSE`（MIT）、本 CHANGELOG 与 `README.md`。

### 修复（桌面端安全与行为）

- **桌面端不再依赖「访问首页即发 Cookie」**：`boot()` 现在设置 `KW_NO_AUTO_COOKIE=1`，
  会话令牌由初始化脚本写入 `window.__KW_INIT.token`，`bridge.js` 立刻落成当前 origin 的
  `kw` Cookie 并从 window 上删除。此前 `auto_cookie` 默认为真，本机任何进程扫到端口
  访问首页就能拿到 `Set-Cookie`，v2026.10.25.1 建立的桌面安全边界实际是失效的。
- **`system_theme` 改为返回布尔值**：原来返回 `"dark"` / `"light"`，前端
  `theme.js` 直接交给 `setSystemDark(dark)`，`Boolean("light") === true`，
  浅色系统会被永远判成深色。
- **主动探测已断开的 SSH 连接**：新增 `SshClient::is_closed()`（russh `Handle::is_closed`），
  `Sessions::must()` 命中死连接时立即从内存表移除并返回 `SESSION_LOST`，
  不再等到某次读写失败才报错。

### 测试

- `cargo test --workspace` 93 项全部通过（core 3 / desktop 5 / keys 7 + parity 33 /
  server 5 + routes 8 / ssh 7 + known_hosts 6 / store 4 + 9 / ffi 5 + C ABI 1）。
- `scripts/desktop-smoke.mjs` 扩到 23 项并全部通过，新增：会话令牌落 Cookie、
  令牌不留在 `window.__KW_INIT`、`/api/state` 可读、版本贯通、系统深浅色返回布尔值、
  首页不再自动下发 `Set-Cookie`。
- `mobile/`：`flutter analyze` 无告警；`flutter test` 1 项真 FFI 通过（Windows 宿主，
  覆盖初始化 / 生成密钥 / 主机 CRUD / 备注 / 加密同步包往返 / 错误口令拒绝）；
  `flutter build windows --debug` 通过，实跑 10s 无异常。
- Android：`aapt2` 确认包名 / 版本 / label / `INTERNET` 权限 / 三种 ABI 均正确，
  `apksigner verify` 通过（v2 签名，debug 证书）；APK 内 `libkw_ffi.so` 只依赖
  `libdl/libm/libc`，导出 `kw_init` / `kw_call` / `kw_free_string`。
  本机没有真机与模拟器，**未做安装运行验证**。

### 已知限制

- 只出 Windows x64（NSIS），未签名、无自动更新。
- `kw-server` 目前只有库，没有独立浏览器模式的二进制入口。
- `kw-core::Secret` 尚未接入认证链路，密码 / passphrase 仍以 `String` 传递（只保证不落盘）。
- 旧版 `tests/e2e.js`（内存 SSH 服务端跑完整流程）尚未移植，Rust 侧没有真实 SSH 集成测试。
- 手机端 Android APK 已构建（通用包 + arm64 包），但本机没有真机/模拟器，
  未做安装运行验证；release 沿用 Flutter 模板的 debug 签名，只能侧载，不能上架。
- 终端流式输出尚未通过 FFI 暴露；iOS 需要 Xcode 链 `staticlib`。
- 同步是手动文件交换；局域网直传与 NAS 自动同步未实现，墓碑暂无自动清理。
