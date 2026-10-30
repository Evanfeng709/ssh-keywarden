# SSH Keywarden（Rust 版）

本地 SSH 授权密钥管理台 —— 用来打理服务器上的 `~/.ssh/authorized_keys`：
看清每把钥匙是谁的、给新设备加一把、把旧钥匙换掉、把不用的删掉。

这是旧版 Electron + Node 实现的 Rust 重写：**前端 `assets/public` 与旧版 `public/`
逐字节一致**，后端与桌面壳整体替换为 Tauri v2 + axum + russh。同一套核心另外提供了
`kw-ffi`，供手机端（Flutter）直接调用，不需要在手机上开 HTTP 端口。

> 旧版仍在 `../ssh-keywarden`，两版可对照；改代码前先确认动的是哪一版。

## 功能

| 功能 | 说明 |
|---|---|
| **查看** | 拉取 `authorized_keys`，逐条解析类型 / SHA256 指纹 / 注释 / options，并与本地密钥库比对，直接标出「这条是本地哪个文件」 |
| **搜索 / 筛选** | 按注释、指纹、本地文件名 / 路径、备注实时搜索；一键筛「本地无匹配 / 重复 / 本次在用」 |
| **备注** | 给钥匙写本地归属备注，只存本机 `key-notes.json`，不写服务器 |
| **备份与恢复** | 列出同目录 `.bak.YYYYMMDDHHMMSS`，先看差异再恢复；恢复前自动再备份，空备份硬拒绝 |
| **新增** | 本机生成 Ed25519 / RSA3072（可设 passphrase）、从已有 `.pub` 选、直接粘贴公钥；写入后回读校验 |
| **更换** | 严格三步：先加新钥匙 → 用新私钥**真实登录一次** → 再删旧；验证不过保留旧钥匙 |
| **删除** | 多选删除；不允许删空，不允许删掉本次连接正在用的钥匙（需二次确认） |
| **仅密钥登录** | 检测服务器登录方式并直接改 `sshd_config`（含 `sshd_config.d` drop-in）；开启前要求当前确为密钥登录且该钥匙在文件里 |
| **终端** | 页面内 xterm + WebSocket 终端，支持颜色、滚回、窗口自适应与快捷键粘贴 / 复制 |
| **纯终端模式** | 只建 SSH 会话开终端，不读家目录、不读 `authorized_keys`；切回密钥模式才按需补读 |
| **外观** | 6 套手动主题 + 2 套跟随系统；桌面端主题 / 视图 / 窗口状态落盘，重启保持 |

## 快速开始

```bash
# 开发态直接开桌面窗口（debug 构建保留控制台，方便看日志）
cargo run -p kw-desktop

# 独立浏览器模式（自动打开临时登录文件）
cargo run -p kw-server
# 不自动打开时，手动打开控制台给出的登录文件
cargo run -p kw-server -- --no-open

# 发布构建
cargo build --release          # → target/release/kw-desktop.exe
cargo tauri build              # → target/release/bundle/nsis/SSH Keywarden_<ver>_x64-setup.exe

# 手机端动态库（Android .so / iOS .a 由各自工具链再交叉编译）
cargo build -p kw-ffi --release
```

首次启动会自动做两件事：

1. 读 `~/.ssh/config`，主机列表为空时把 Host 导入一次；
2. 从 `IdentityFile` 往上找「密钥文件最多」的那层目录，作为本地密钥库默认扫描目录。

浏览器模式支持 `--port` / `-p`（优先于 `PORT` 环境变量），
`KW_PUBLIC_DIR` 可指定含 `index.html` 的资源目录；默认查找程序旁 `public/`，
再查源码资源目录。打包单独分发时需要一并携带 `public/`。

浏览器入口不会输出令牌或监听地址，首页也不会自动发 Cookie。
打开本次运行的临时登录文件并点击登录后，浏览器获得会话 Cookie。
该文件含登录凭据，请勿分享；Ctrl+C（Windows 也支持 Ctrl+Break）正常退出会删除，
强制终止后需要自行清理。Unix 文件权限为 0600，Windows 依赖用户临时目录 ACL。
远程使用需自行建立 SSH 本地转发并安全取用登录文件，不直接暴露服务。

## 架构

```
ssh-keywarden-rs/
├── crates/
│   ├── kw-core/       Secret：敏感串封装（zeroize，不实现 Serialize）
│   ├── kw-keys/       纯文本规则层：authorized_keys 解析/渲染、keygen、本地扫描、
│   │                  备份差异、sshd 四态判定、POSIX 路径
│   ├── kw-store/      config.json / key-notes.json 持久化，~/.ssh/config 导入
│   ├── kw-ssh/        russh 客户端：连接/认证/exec/原子写/PTY/新钥验登，
│   │                  known_hosts TOFU，sshd 配置改写
│   ├── kw-server/     axum 本地 HTTP + WebSocket，桌面嵌入 / 独立浏览器入口
│   ├── kw-desktop/    Tauri v2 壳：无边框窗口、单实例、剪贴板/对话框、数据目录
│   └── kw-ffi/        手机端门面：MobileEngine + C ABI，复用 kw-server 的会话与密钥操作；
│                      微软账号同步（设备码 OAuth2 + Graph）
├── crates/kw-sync/    桌面同步 CLI：login / sync / status / logout
├── assets/public/     前端（原生 JS + xterm，与旧版完全一致）
├── mobile/            Flutter 手机端（直连 kw-ffi，不开 HTTP）
└── scripts/
    ├── desktop-smoke.mjs   WebView2 CDP 桌面冒烟
    └── server-smoke.mjs    Edge CDP 浏览器登录与退出冒烟
```

几个关键约束：

- `kw-keys` 不碰网络与 UI，桌面端与将来的手机端共用；
- `kw-server` 与 `kw-desktop` 之间只通过本地 HTTP 通信，桌面壳不承载业务；
- `ssh-key` 0.6 与 russh 内部的 0.7 类型不互通，私钥统一在 `kw-keys` 里解成
  未加密 OpenSSH 文本再交给 russh（`private_key_openssh`）。

## 手机端（kw-ffi）

`kw-ffi` 把 `kw-keys` / `kw-store` / `kw-ssh` 直接编进 App，**不开 HTTP、不依赖桌面壳**。
会话与密钥操作复用 `kw-server` 的实现（`connect::open` / `keys_ops::*`），
所以「防锁死」那套闸门与桌面端是同一条代码路径。

```text
kw_init(app_dir)                     // 一次；data/ 放配置，keys/ 放随身密钥包
kw_call("bootstrap", "{}")           // 主机 + 备注 + 本地密钥扫描
kw_call("connect", {"hostId":"…","passphrase":"…","withKeys":true})
kw_call("addKey" | "replaceKey" | "deleteKeys" | "restoreBackup", {"sid":"…", …})
kw_call("backups" | "backupDiff" | "sshd" | "setKeyOnly", {…})
kw_call("msStatus" | "msSaveConfig" | "msStartLogin" | "msCompleteLogin"
        | "msLogout" | "msSync", {…})
kw_call("termOpen" | "termRead" | "termWrite" | "termResize" | "termClose", {…})
kw_free_string(ptr)                  // 释放每次返回的字符串
```

- 返回统一是 `{"ok":true,"data":…}` 或 `{"ok":false,"error":…,"code":…,"details":…}`；
- 主机指纹错误仍带 `HOSTKEY_UNKNOWN` / `HOSTKEY_CHANGED` 与会话 `SESSION_LOST`；
- 同步包（v2）包含主机、钥匙备注、已知主机指纹，**不含私钥**；口令加密
  （Argon2id 派生 + AES-256-GCM）。合并规则见下文「微软账号同步」；
- 手机生成的私钥落在 `app_dir/keys`，0600（Unix）。

## 微软账号同步（只同步元数据，不上传私钥）

设计取向：**每台设备一把自己的钥匙**。服务器上的 `authorized_keys` 本身就是事实源，
不需要同步；要同步的只有本机数据——主机列表、钥匙归属备注、已信任的主机指纹。
撤销一台设备 = 从服务器删掉那把公钥，不涉及任何私钥搬运。

同步包是 OneDrive 应用专属文件夹里的单个 `ssh-keywarden.kwsync`（JSON，口令加密）。
合并规则：

- 主机按 `id`（缺失则按 `host:port:user`）合并，**以 `updated_at` 新者为准**；
  删除打墓碑（`deletedAt`）随包传播，对方不会「复活」已删主机；
- 本机的私钥路径（`identityFile`）是设备本地设置，**不会被同步包覆盖**；
- 备注按 `updatedAt` 合并，删除 = 空备注墓碑；
- `known_hosts` 指纹相同只补缺；**指纹不一致默认不覆盖**——本机保留、其余变更照常合并、
  本次不上传，直到你显式信任（手机端弹窗确认、CLI `--trust-changed host:port`）。

### 先注册一个 Azure 应用（一次性）

本仓不内置任何客户端 ID，用的是你自己的应用注册：

1. <https://portal.azure.com> → Microsoft Entra ID → 应用注册 → 新注册；
2. 受支持的账户类型选「仅个人 Microsoft 账户」（要同时支持工作账户就选带组织的项）；
3. 「身份验证」里允许**公共客户端流**（设备码登录不需要重定向 URI）；
4. 「API 权限」加 Microsoft Graph **委托**权限 `Files.ReadWrite.AppFolder`、`User.Read`、
   `offline_access`，并授予管理员/用户同意；
5. 把「应用程序(客户端) ID」填进桌面端顶栏「同步」的弹窗、手机端同步页的配置弹窗，
   或 CLI 的 `--client-id` / 环境变量 `KW_MS_CLIENT_ID`。

### 桌面客户端

顶栏「同步」按钮：配置 Azure 客户端 ID → 登录微软账号（设备码，浏览器里输一次
8 位码）→ 立即同步。同步口令可记住（存本机 `ms-config.json`，只在本机加密用）；
退出登录只清本机 token，云端包保留。指纹冲突在弹窗里就地二选一：
「保留本机」（其余变更照常拉取，本次不上传）或「全部覆盖后重传」。

### 桌面 CLI

```bash
cargo build -p kw-sync --release
export KW_MS_CLIENT_ID=<你的应用（客户端）ID>

./target/release/kw-sync login                 # 屏幕给出 8 位码，浏览器输一次
./target/release/kw-sync status --json         # 已配置/已登录/上次同步时间
KW_SYNC_PASSPHRASE=… ./target/release/kw-sync sync --remember-key
./target/release/kw-sync sync --trust-changed host:22 --json
./target/release/kw-sync logout                # 只删本机 token，云端包保留
```

口令来源顺序：`KW_SYNC_PASSPHRASE` → 本机记住的口令（`data/ms-config.json`）→ 交互询问；
`--remember-key` 决定本次同步后是否记住。

### 手机端

密钥包·同步 页：配置 Azure 客户端 ID（可顺手记住同步口令）→ 登录微软账号（设备码）→
立即同步。指纹冲突时弹窗二选一：「保留本机」（其余变更照常拉取，本次不上传）或
「全部覆盖后重传」。

云端只有密文：口令不出设备，微软也解不开；忘了口令只能删掉云端的
`ssh-keywarden.kwsync` 重新开始（旧的那个谁也解不开）。同步元数据与私钥无关，
所以通道是 OneDrive 也不影响安全取舍。

> 手机端 UI 已落在 `mobile/`（Flutter）：三个 Tab 分别是服务器密钥、纯终端、
> 密钥包·同步（微软账号）。6 套手动主题（曜石 / 午夜 / 石墨 / 云白 / 亚麻 / 雾青）。
> Windows 宿主已实跑；Android APK 继续用 NDK 28.2 + cargo-ndk 构建
> （`dist/android/`：通用包 + arm64 包，三种 ABI 的 `libkw_ffi.so` 都已打进去）。
> 纯终端是「Rust 侧缓冲 PTY（`termOpen/termRead/termWrite/termResize/termClose`）+
> Dart 侧 VT 子集仿真」：C ABI 没有回调，所以用长轮询拉输出，轮询跑在独立 isolate 里，
> 写字同步执行以保证按键顺序。仿真只覆盖常用子集（SGR/CUP/ED/EL/换行与延迟折行/
> 备用屏/滚动区），不是完整 xterm。
> 桌面端与手机端共用同一份安全闸门是刻意的，别在 Dart 侧重写一遍。

## 安全边界

这是能改服务器登录凭据的工具，边界卡得比较死：

- **只监听 `127.0.0.1`**，并严格校验 `Host` 头（挡 DNS rebinding）；
- 每次启动生成随机令牌；桌面端使用随机端口、浏览器端可配置端口。
  两个入口均设 `KW_NO_AUTO_COOKIE=1`，**访问首页不会下发 `Set-Cookie`**；
  桌面端由初始化脚本注入 Cookie，浏览器端需通过临时登录文件换取
  HttpOnly / SameSite=Strict Cookie，登录响应禁止缓存和 Referer 传播；
- 首次连接确认主机指纹，之后主机密钥变化会被拦下并要求确认（TOFU，存 `known_hosts.json`）；
- 密码 / passphrase 不写配置文件；认证配置使用 `Secret`，调试输出脱敏，
  自有缓冲区释放时清零，且认证后即释放、不跨远端探测持有。
  这不保证 HTTP 原始请求、第三方库副本和调用方 C 缓冲区全部清零；
- 每次写盘前自动备份，写入是原子的（同目录临时文件 → `mv` → `chmod 600` → 回读校验）；
- 备份名只认同目录 + 固定前缀 + 14 位时间戳，接口不会被当成任意远端文件读取器；
- 删空、删当前连接钥匙、新钥匙验证不过，全部拦下并说明原因；
- 纯终端模式不读 `authorized_keys`；未补读前，所有密钥类接口一律拒绝；
- 网络面只有 `127.0.0.1`；对外的网络请求只有微软同步：设备码与 Graph 都走 HTTPS，
  用内置 `webpki-roots` 校验（不读系统私有根证书），上传的包体本身是口令加密的；
  OAuth 令牌存本机 `ms-auth.json`（Unix 0600，Windows 依赖用户目录 ACL），不打印、不进仓库。

## 数据目录

| 项 | 位置 |
|---|---|
| 桌面数据目录 | `KW_DATA_DIR` 优先，否则 `%APPDATA%\ssh-keywarden\data` |
| 浏览器数据目录 | `KW_DATA_DIR` 优先，否则当前工作目录的 `data/` |
| 主机与设置 | `config.json` |
| 本地备注 | `key-notes.json`（0600，不写服务器） |
| 主机指纹 | `known_hosts.json`（0600） |
| 微软登录令牌 | `ms-auth.json`（0600，Unix；Windows 依赖用户目录 ACL） |
| 微软同步配置 | `ms-config.json`（客户端 ID、可选记住的同步口令，0600） |
| 桌面状态 | `theme.json` / `ui.json` / `window-state.json`（原子写） |

升级接管：目标目录还没有 `config.json` 时，会从 exe 旁 `data/` 复制三个核心数据文件。

## 兼容性

远端只用 `base64`、`mv`、`cp`、`chmod`、`mkdir`、`printf`、`uname` 和 sh 内建
`for` / `printf`；**不依赖 SFTP 子系统**。认证支持私钥（含 passphrase）与密码
（含 keyboard-interactive）。

## 测试

```bash
cargo test --workspace        # 规则层 / 存储 / HTTP / 真实 SSH / 桌面状态 / FFI
cargo build -p kw-server -p kw-desktop
node scripts/desktop-smoke.mjs --exe target/debug/kw-desktop.exe
node scripts/server-smoke.mjs # Windows + Edge；KW_TEST_BROWSER 可指定浏览器程序

cd mobile
flutter analyze
flutter test                  # 需 KW_FFI_LIB 指向 kw_ffi 动态库，详见 mobile/README.md
```

- `kw-keys/tests/parity.rs`（33 项）是移植验收标准：行为必须与旧 Node 版一致；
- `kw-server/tests/routes.rs` 覆盖令牌校验、Host 头、静态资源穿越、会话失效、主机往返；
- `kw-server/tests/e2e.rs` 通过真实 russh 环回服务端验证认证与安全操作顺序；
  远端命令/文件使用内存模型，不代替 Linux shell 和 sshd reload 实机测试；
- `server-smoke.mjs` 使用隔离数据和 Edge profile，验证登录、界面切换、刷新鉴权、
  退出信号和临时登录文件清理；不读取真实主机或密钥；
- `desktop-smoke.mjs` 通过 WebView2 CDP 验证桥接、主题/视图落盘、令牌注入、
  首页不自动发 Cookie、窗口最大化与状态落盘；
- `kw-ffi` 覆盖密钥落包与扫描、同步包（v2，含墓碑与指纹冲突）明文/加密往返、
  错误口令拒绝、微软同步的配置/令牌落盘与参数校验（不联网）、终端缓冲、
  C ABI 初始化与分发；
- `kw-sync` 覆盖参数解析（`login` / `sync` / `status` / `logout`、信任列表、JSON 输出）；
- `mobile/test/ffi_test.dart` 是真 FFI（不是 mock）：初始化、生成密钥、主机 CRUD、
  备注、加密同步包往返与错误口令拒绝。

## 版本号规则

- 语义化版本，当前 `0.3.1`；
- 单一来源：根 `Cargo.toml` 的 `[workspace.package].version`；
- 必须同步两处：`crates/kw-desktop/tauri.conf.json` 的 `version`、本仓 `CHANGELOG.md`
  的 `## v<version>` 标题；
- `kw-desktop` 的单测 `version_matches_tauri_conf_and_changelog` 会在 `cargo test` 时
  校验这三处一致，漏改直接红。

## 已知限制

- 只出 Windows x64（NSIS 安装包），未签名，首次运行可能弹 SmartScreen；无自动更新。
- Secret 仅保护其拥有的认证缓冲区；其他请求 JSON、第三方库副本与 C 调用方内存不在此保证内。
- SSH 端到端测试使用内存远端命令模型；尚未做真实 Linux sshd 配置改写/重载验收。
- 手机端 Android APK 已构建（通用包 + arm64 包），但**本机没有真机/模拟器，
  未做安装运行验证**；release 沿用 Flutter 模板的 debug 签名，只能侧载，不能上架。
- 微软同步需要你自己的 Azure 应用注册与客户端 ID（仓库不带任何应用 ID）；
  设备码登录与 Graph 上传下载尚未用真实账号实网验收。
- 同步只认内置 `webpki-roots`：企业自签/私有根 CA 环境会连不上；
  Windows 上 `ms-auth.json` 只能靠用户目录 ACL，没有 Unix 那样的 0600。
- 手机端主题选择不落盘，重启回到曜石（桌面端主题才写 `theme.json`）。
- 终端是常用 VT 子集，全屏 TUI（vim/htop 的部分特性）可能显示不全；iOS 需要 Xcode 链 `staticlib`。
- 同步墓碑只留在 `config.json` / `key-notes.json` 里，暂无自动清理。
- 只处理 `authorized_keys` 里的普通公钥行；`Match` 等 sshd 配置不管。
- 备份不会自动清理；备份名精确到秒，同一秒两次写盘会覆盖同一个 `.bak`。
- 不走跳板机 / ProxyJump。
- 密钥库位于 OneDrive 等按需同步目录时，首次扫描会触发占位符下载。

## 许可

MIT，见 [LICENSE](LICENSE)。
