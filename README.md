# SSH Keywarden

[![test](https://github.com/Evanfeng709/ssh-keywarden/actions/workflows/test.yml/badge.svg)](https://github.com/Evanfeng709/ssh-keywarden/actions/workflows/test.yml)

本地 SSH 授权密钥管理台 —— 桌面版双击即用，也保留浏览器模式，用来打理服务器上的 `~/.ssh/authorized_keys`。

解决的就是这几件烦心事：服务器上堆了一堆不知道是谁的钥匙、换机器后想把登录钥匙换掉、想给新设备加一把、想把早就不用的删掉。

## 功能

| 功能 | 说明 |
|---|---|
| **查看** | 拉取 `authorized_keys`，逐条解析出类型 / SHA256 指纹 / 注释 / 选项，并和本地密钥库比对，直接告诉你「这条是本地哪个文件的钥匙」 |
| **搜索 / 筛选** | 按注释、指纹、本地文件名 / 路径、备注实时搜索；一键筛出「本地无匹配 / 重复 / 本次在用」。刷新后保留条件，切换服务器时重置 |
| **备注** | 给钥匙写本地归属备注（如「谁的电脑」），服务器表与本地密钥库侧栏同步显示；只存本机 `data/key-notes.json` |
| **备份与恢复** | 列出同目录历史备份，先看与当前文件的钥匙差异再恢复；恢复前自动再备份当前文件，空备份直接拒绝 |
| **新增** | 三种来源：本机生成新密钥对（Ed25519 / RSA3072，**可设置 passphrase 加密私钥**）、从已有 `.pub` 选、直接粘贴公钥。写入后回读校验，可选「顺便把本机登录密钥切成新的」 |
| **更换** | 严格的两步式：先加新钥匙 → 用新私钥**真实登录一次**验证 → 再删旧钥匙。验证不过就保留旧钥匙，不会把你锁在门外 |
| **删除** | 勾选删除，支持多选。硬性拦截：不允许删空、不允许删掉本次连接正在用的钥匙（需二次确认） |
| **仅密钥登录** | 新增弹窗顶部显示服务器当前登录方式（密码 / 公钥 / 是否已仅密钥），并给出开关直接改 `sshd_config`。开启前有硬闸门：必须是密钥登录、当前钥匙必须在 `authorized_keys` 里、文件不能为空 |
| **终端** | 连接后点顶栏「终端」，在页面里直接开一个 SSH 终端敲命令（xterm + WebSocket，支持颜色、滚回、窗口自适应）；支持 `Ctrl+V` / `Ctrl+Shift+V` / `Shift+Insert` 粘贴与 `Ctrl+Shift+C` 复制，也可用标题栏的「粘贴 / 复制」按钮 |

## 快速开始

**桌面版**：

```bash
npm install
npm run app        # 开发态直接开桌面窗口
npm run dist       # 打 Windows 安装包 + 免安装 zip（输出到 dist/）
```

**浏览器模式**：

**方式一**：双击 `start.cmd`（首次会自动装依赖并打开浏览器）。

**方式二**：

```bash
npm install
npm start        # → http://127.0.0.1:7791
```

换端口：`set PORT=7792 && node server.js`

> 依赖锁定在 npm 官方源；国内网络慢时可加镜像安装：`npm install --registry=https://registry.npmmirror.com`

首次启动会自动做两件事，省得手抄配置：

1. 读 `~/.ssh/config`，把里面的 Host 导入成主机列表（仅当主机列表为空时导入一次）；
2. 从 `IdentityFile` 往上找「密钥文件最多」的那层目录，作为本地密钥库的默认扫描目录。

## 桌面版

- **窗口**：无边框单行顶栏，最小化 / 最大化 / 关闭自绘在右上角；空白处拖动窗口，双击最大化。
- **布局**：本地密钥库默认收起，点顶栏按钮从右侧展开（窄窗口为浮层抽屉），不再长期占用主界面宽度。
- **数据目录**：`%APPDATA%\ssh-keywarden\data`（`config.json` / `known_hosts.json` / `key-notes.json`）。首次启动若安装目录旁存在旧的 `data/`，会自动迁移；也可用 `KW_DATA_DIR` 指定别处。
- **会话边界**：服务仍只监听 `127.0.0.1`，但改用随机端口，并**关闭「访问首页即发 Cookie」**；令牌由主进程直接写进 Electron 会话，本机其他进程扫到端口也拿不到会话。
- **渲染进程**：关闭 Node、启用沙箱；「浏览…」走系统文件对话框，支持 OneDrive 占位符路径与带空格的目录。
- **密钥库**：扫描结果缓存 60 秒，点「重新扫描」强制刷新。OneDrive 目录建议设为「始终保留在此设备上」，否则首次扫描会触发占位符下载。
- **打包**：只出 Windows x64（NSIS 安装包 + zip），未签名，首次运行可能弹 SmartScreen；不含自动更新。

## 关于安全

这是能改服务器登录凭据的工具，所以边界卡得比较死：

- **只监听 `127.0.0.1`**，并严格校验 `Host` 头（挡 DNS rebinding）；
- **首次连接确认主机指纹**：指纹保存在本机 `data/known_hosts.json`，之后主机密钥变化会被拦下并要求确认，防止中间人冒充服务器；
- 会话用 `SameSite=Strict` 的 HttpOnly Cookie（挡浏览器里的其他网页伪造请求）；桌面版由主进程注入令牌，不在首页自动下发；
- **密码只在内存里**，不写配置文件；配置只存「怎么连」（地址/端口/用户名/私钥路径）；
- 每次写盘前**自动备份**为 `authorized_keys.bak.YYYYMMDDHHMMSS`；
- **备份恢复**同样先备份当前文件、原子写入并回读校验；空备份硬拒绝，备份不含当前登录钥匙时必须显式强制；
- 备份名只允许同目录 + 固定前缀 + 14 位时间戳，避免把接口变成任意远端文件读取器；
- 写入是**原子**的：先写同目录临时文件再 `mv` 覆盖，中途断线也不会写坏文件；
- 删空 / 删当前连接钥匙 / 新钥匙验证不过，全部拦下并说明原因；
- **备注只存在本机** `data/key-notes.json`（0600），不写入服务器，也不混进连接配置。

## 兼容性

远端只用这几样东西：`base64`、`mv`、`cp`、`chmod`、`mkdir`、`printf`、`uname`。
列备份只用 sh 内建的 `for` / `printf`，不额外依赖 `ls` / `find`。
**不依赖 SFTP 子系统** —— 有些 sshd 或受限账号会把 sftp 关掉。

认证支持私钥（含 passphrase）与密码。

## 目录结构

```
ssh-keywarden/
├── server.js              HTTP 服务 + 业务编排（可被 CLI / Electron 启动）
├── electron/
│   ├── main.js            桌面主进程：内置服务、注入 Cookie、窗口与 IPC
│   ├── preload.js         只暴露选文件 / 选目录 / 打开数据目录
│   └── data-dir.js        桌面数据目录解析与迁移
├── electron-builder.yml   安装包配置（NSIS + zip）
├── build/                 应用图标（icon.svg / icon.png）与渲染脚本
├── src/
│   ├── version.js         版本号单一来源
│   ├── keygen.js          公钥解析、SHA256 指纹、密钥对生成
│   ├── authorized.js      authorized_keys 解析/序列化（保留原格式，不重排）
│   ├── backups.js         备份名解析、差异计算、恢复路径校验
│   ├── localkeys.js       本地密钥库扫描（递归 + 私钥/公钥配对）
│   ├── ssh.js             SSH 连接、命令执行、原子写、备份
│   ├── knownhosts.js      主机指纹（known_hosts 式 TOFU）
│   ├── sshconfig.js       从 ~/.ssh/config 导入主机
│   └── store.js           配置与本地备注持久化（不含密码）
├── public/                前端（原生 JS，无框架）
│   ├── js/                页面逻辑（ES modules：core/hosts/keys/actions/backups/connect/terminal…）
│   └── vendor/            xterm.js 及其样式（UMD，直接从 node_modules 复制，运行时无需构建）
├── tests/
│   ├── run.js             单元测试（解析/生成/扫描/备注/备份差异/数据目录）
│   ├── e2e.js             端到端（内置内存 SSH 服务端，跑完四条主流程 + 备注 / 备份恢复）
│   ├── desktop-server.js  桌面模式（随机端口 / 不发 Cookie / Host 校验 / 扫描缓存）
│   └── electron-smoke.js  真实 Electron 窗口冒烟（Windows）
└── data/config.json       运行时生成的主机与设置（同目录：known_hosts.json、key-notes.json）
```

## 测试

```bash
npm test
```

- 单元测试验证解析规则、指纹格式、密钥生成能被 `ssh2` 回读，以及备注存储、备份名校验、差异方向与桌面数据目录迁移；
- 端到端测试在本机起一个真实 SSH 服务端（内存文件系统），把「查看 → 新增 → 重复新增 → 粘贴垃圾 → 更换 → 删除 → 防锁死 → 备注 → 备份列表 / 差异 / 恢复」全跑一遍，断言文件内容与备份；
- 桌面模式测试覆盖随机端口、关闭自动发 Cookie、Host 校验与扫描缓存；
- `npm run test:app` 启动真实 Electron 窗口做冒烟（Windows），检查桌面桥接、页面渲染、Cookie 注入、单行顶栏、窗口按钮最大化/还原、本地面板开关与终端粘贴；打包后可加 `KW_APP_EXE=dist/win-unpacked/"SSH Keywarden.exe"` 复跑同一套检查。

指纹算法已与 `ssh-keygen -lf` 逐一比对一致；生成的私钥可直接被 `ssh-keygen -y` 解析。

## 版本号规则

格式 `v<日期>.<当天第几次更新>`，例如 **v2026.10.19.4** = 2026-10-19 当天第 4 次更新。

- 单一来源：`src/version.js`，页面徽标从 `/api/state` 读取，不在 HTML 里硬编码；
- `package.json` 的 `version` 因为 npm 只认 semver（3 段），写成 `2026.10.19-4`，指同一个版本；
- 改完代码要同步三处：`src/version.js`、`CHANGELOG.md`、`package.json`。

## 已知限制

- 只处理 `authorized_keys` 里的普通公钥行；`Match` 之类的 sshd 配置不管。
- 无法自动验证「新钥匙能否登录」时（例如粘贴的公钥在本地没有对应私钥），默认**不删旧钥匙**，需要手动勾选强制。
- 备份不会自动清理；备份名精确到秒，同一秒内连续两次写盘可能落在同一个备份文件（后一次覆盖前一次）。
- 不走跳板机 / ProxyJump。
- 桌面安装包只做 Windows x64，未签名、无自动更新；密钥库位于 OneDrive 等按需同步目录时，首次扫描会触发下载。

## 许可

MIT，见 [LICENSE](LICENSE)。
