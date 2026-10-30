//! russh 客户端：连接、认证、执行命令、读写远端文件、PTY 终端。
//!
//! 对应旧版 `src/ssh.js`。那边的注释仍然成立：不走系统 ssh.exe，
//! 全部在本进程里完成，桌面端与将来的手机端共用这一层。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine;
use russh::client::{self, Config, Handle, Msg};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, ChannelReadHalf, ChannelWriteHalf};
use thiserror::Error;
use tokio::sync::Mutex;

use kw_core::Secret;
use kw_keys::pathutil::assert_safe_path;
use kw_keys::private_key_openssh;

use crate::known_hosts::{HostKeyError, KnownHosts};
use crate::util::{friendly_error, quote, read_remote_text_command, shanghai_stamp};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(30);
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Default)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key: Option<Secret>,
    pub passphrase: Option<Secret>,
    pub password: Option<Secret>,
    /// 用户在指纹弹窗里确认过的指纹；给了才会放行未知/已变化的主机密钥。
    pub trust_fingerprint: Option<String>,
}

impl SshConfig {
    pub fn new(host: impl Into<String>, port: u16, username: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Error)]
pub enum SshError {
    #[error(transparent)]
    HostKey(#[from] HostKeyError),
    #[error(transparent)]
    Russh(#[from] russh::Error),
    #[error("{0}")]
    Msg(String),
}

impl SshError {
    pub fn msg(m: impl Into<String>) -> Self {
        SshError::Msg(m.into())
    }

    /// 主机指纹错误的机器码，给 HTTP 层生成 `{ code, details }` 用。
    pub fn host_key_code(&self) -> Option<&'static str> {
        match self {
            SshError::HostKey(e) => Some(e.code.as_str()),
            _ => None,
        }
    }

    pub fn host_key_details(&self) -> Option<&serde_json::Value> {
        match self {
            SshError::HostKey(e) => Some(&e.details),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerProbe {
    pub uname: String,
    pub user: String,
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ExecResult {
    /// stderr 优先，空则退回「退出码 N」——旧版报错拼法。
    pub fn failure_text(&self) -> String {
        let err = self.stderr.trim();
        if err.is_empty() {
            format!("退出码 {}", self.code)
        } else {
            err.to_string()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub readback: String,
    /// 备份文件名；原文件不存在时为 None
    pub backup: Option<String>,
}

struct ClientHandler {
    known_hosts: Arc<KnownHosts>,
    host: String,
    port: u16,
    trust: Option<String>,
}

impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let blob = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key
                .to_bytes()
                .map_err(|e| SshError::msg(format!("无法读取服务器主机公钥：{e}")))?,
            PublicKeyOrCertificate::Certificate(_) => {
                return Err(SshError::msg(
                    "服务器返回的是主机证书，当前版本只支持普通主机密钥",
                ))
            }
        };
        self.known_hosts
            .verify(&self.host, self.port, &blob, self.trust.as_deref())?;
        Ok(true)
    }
}

pub struct SshClient {
    handle: Mutex<Handle<ClientHandler>>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: &'static str,
}

impl SshClient {
    /// 打开一条 session 通道。只在这一步占着 handle 的锁，之后互不影响。
    async fn open_channel(&self) -> Result<Channel<Msg>, SshError> {
        let handle = self.handle.lock().await;
        if handle.is_closed() {
            return Err(SshError::msg("SSH 连接已断开，请重新连接"));
        }
        Ok(handle.channel_open_session().await?)
    }

    pub async fn exec(
        &self,
        command: &str,
        input: Option<Vec<u8>>,
    ) -> Result<ExecResult, SshError> {
        self.exec_with_timeout(command, input, EXEC_TIMEOUT).await
    }

    pub async fn exec_with_timeout(
        &self,
        command: &str,
        input: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<ExecResult, SshError> {
        let mut channel = self.open_channel().await?;
        let run = async {
            channel.exec(true, command.as_bytes().to_vec()).await?;
            if let Some(data) = input {
                channel.data_bytes(data).await?;
            }
            // 关掉写侧：命令不读 stdin 时相当于 EOF
            channel.eof().await?;

            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut code: Option<i32> = None;
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, ext } => {
                        if ext == 1 {
                            stderr.extend_from_slice(&data)
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
                    ChannelMsg::ExitSignal { signal_name, .. } => {
                        return Err(SshError::msg(format!(
                            "远端命令被信号中断（{signal_name:?}）：{}",
                            clip(command)
                        )))
                    }
                    ChannelMsg::Failure => {
                        return Err(SshError::msg(format!(
                            "服务器拒绝了执行请求：{}",
                            clip(command)
                        )))
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            Ok::<ExecResult, SshError>(ExecResult {
                code: code.unwrap_or(0),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        };

        match tokio::time::timeout(timeout, run).await {
            Ok(result) => result,
            Err(_) => Err(SshError::msg(format!(
                "命令执行超时（{}s）：服务器已接受登录，但执行「{}」无响应。常见原因：受限账号（只允许 sftp/单命令）、服务器负载过高、或网络不稳定",
                timeout.as_secs(),
                clip(command)
            ))),
        }
    }

    /// 读远端文本文件；文件不存在返回 None，空文件返回 Some("")。
    pub async fn read_text(&self, remote_path: &str) -> Result<Option<String>, SshError> {
        assert_safe_path(remote_path).map_err(|e| SshError::msg(e.to_string()))?;
        let res = self
            .exec(&read_remote_text_command(remote_path), None)
            .await?;
        let out: String = res.stdout.chars().filter(|c| !c.is_whitespace()).collect();
        if out == "__KW_MISSING__" {
            return Ok(None);
        }
        if res.code != 0 {
            return Err(SshError::msg(format!(
                "读取 {remote_path} 失败：{}",
                res.failure_text()
            )));
        }
        let bytes = decode_base64(&out)
            .map_err(|e| SshError::msg(format!("解析 {remote_path} 的内容失败：{e}")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// 原子写：临时文件 → mv 覆盖 → chmod 600。
    /// 避免写一半断线把 authorized_keys 写坏，导致自己再也进不去。
    pub async fn write_text_atomic(
        &self,
        remote_path: &str,
        content: &str,
    ) -> Result<(), SshError> {
        assert_safe_path(remote_path).map_err(|e| SshError::msg(e.to_string()))?;
        let tmp = format!("{remote_path}.tmp-{}", now_ms());
        let payload = base64::engine::general_purpose::STANDARD
            .encode(content.as_bytes())
            .into_bytes();

        let w = self
            .exec(&format!("base64 -d > {}", quote(&tmp)), Some(payload))
            .await?;
        if w.code != 0 {
            return Err(SshError::msg(format!(
                "写入临时文件失败（退出码 {}）：{}",
                w.code,
                if w.stderr.trim().is_empty() {
                    "无输出"
                } else {
                    w.stderr.trim()
                }
            )));
        }

        let res = self
            .exec(
                &format!(
                    "mv -f {tmp} {path} && chmod 600 {path}",
                    tmp = quote(&tmp),
                    path = quote(remote_path)
                ),
                None,
            )
            .await?;
        if res.code != 0 {
            let _ = self.exec(&format!("rm -f {}", quote(&tmp)), None).await;
            return Err(SshError::msg(format!(
                "替换文件失败（退出码 {}）：{}",
                res.code,
                if res.stderr.trim().is_empty() {
                    "无输出"
                } else {
                    res.stderr.trim()
                }
            )));
        }
        Ok(())
    }

    /// 备份磁盘上的文件，返回备份文件名；文件不存在返回 None。
    pub async fn backup_file(&self, remote_path: &str) -> Result<Option<String>, SshError> {
        assert_safe_path(remote_path).map_err(|e| SshError::msg(e.to_string()))?;
        let target = format!("{remote_path}.bak.{}", shanghai_stamp(SystemTime::now()));
        let res = self
            .exec(
                &format!(
                    "if [ -f {p} ]; then cp {p} {t} && chmod 600 {t} && echo \"__OK__\"; else echo \"__SKIP__\"; fi",
                    p = quote(remote_path),
                    t = quote(&target)
                ),
                None,
            )
            .await?;
        if res.stdout.contains("__OK__") {
            return Ok(Some(target));
        }
        if res.stdout.contains("__SKIP__") {
            return Ok(None);
        }
        let detail = if !res.stderr.trim().is_empty() {
            res.stderr.trim().to_string()
        } else if !res.stdout.trim().is_empty() {
            res.stdout.trim().to_string()
        } else {
            "未知原因".to_string()
        };
        Err(SshError::msg(format!("备份失败：{detail}")))
    }

    /// 列出同目录下的 `<remotePath>.bak.YYYYMMDDHHMMSS`，新的在前。
    pub async fn list_backup_names(
        &self,
        remote_path: &str,
    ) -> Result<Vec<kw_keys::backups::BackupName>, SshError> {
        assert_safe_path(remote_path).map_err(|e| SshError::msg(e.to_string()))?;
        // 只跑 sh 内建 for/printf，不依赖 ls/find/SFTP
        let res = self
            .exec(
                &format!(
                    "for f in {p}.bak.*; do printf '%s\\n' \"$f\"; done",
                    p = quote(remote_path)
                ),
                None,
            )
            .await?;
        if res.code != 0 {
            return Err(SshError::msg(format!(
                "读取备份列表失败：{}",
                if res.stderr.trim().is_empty() {
                    format!("退出码 {}", res.code)
                } else {
                    res.stderr.trim().to_string()
                }
            )));
        }

        let base = kw_keys::pathutil::posix_basename(remote_path);
        let prefix = format!("{remote_path}.bak.");
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for raw in res.stdout.split(['\n', '\r']) {
            if raw.is_empty() || !raw.starts_with(prefix.as_str()) {
                continue;
            }
            let name = format!("{base}.bak.{}", &raw[prefix.len()..]);
            let Some(parsed) = kw_keys::backups::parse_backup_name(remote_path, &name) else {
                continue;
            };
            if !seen.insert(name) {
                continue;
            }
            out.push(parsed);
        }
        out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
        Ok(out)
    }

    /// 「读 → 改 → 备份 → 写 → 回读」的公共落盘流程。
    pub async fn commit_text(
        &self,
        remote_path: &str,
        new_text: &str,
    ) -> Result<CommitOutcome, SshError> {
        assert_safe_path(remote_path).map_err(|e| SshError::msg(e.to_string()))?;
        let dir = kw_keys::pathutil::posix_dirname(remote_path);
        assert_safe_path(dir).map_err(|e| SshError::msg(e.to_string()))?;
        let mk = self
            .exec(
                &format!("mkdir -p {d} && chmod 700 {d}", d = quote(dir)),
                None,
            )
            .await?;
        if mk.code != 0 {
            let detail = if mk.stderr.trim().is_empty() {
                "权限不足"
            } else {
                mk.stderr.trim()
            };
            return Err(SshError::msg(format!("创建目录 {dir} 失败：{detail}")));
        }

        let backup = self.backup_file(remote_path).await?;
        self.write_text_atomic(remote_path, new_text).await?;

        let readback = self
            .read_text(remote_path)
            .await?
            .ok_or_else(|| SshError::msg("写入后回读失败：文件不存在（请检查目录权限）"))?;
        // 有些 sshd 环境会规整末尾换行，允许尾部空白差异，内容不一致必须报错
        if readback.trim_end() != new_text.trim_end() {
            return Err(SshError::msg(
                "写入后回读内容与预期不完全一致，请检查服务器",
            ));
        }
        Ok(CommitOutcome { readback, backup })
    }

    /// 拿家目录
    pub async fn resolve_home(&self) -> Result<String, SshError> {
        let res = self.exec("printf \"%s\" \"$HOME\"", None).await?;
        let mut home = res.stdout.trim().to_string();
        if home.is_empty() {
            let alt = self
                .exec("getent passwd \"$(id -u)\" | cut -d: -f6", None)
                .await?;
            home = alt.stdout.trim().to_string();
        }
        if home.is_empty() || home == "/" {
            return Err(SshError::msg("无法确定服务器上的用户家目录（$HOME 为空）"));
        }
        Ok(home)
    }

    /// 确保 ~/.ssh 存在且权限正确
    pub async fn ensure_ssh_dir(&self, home: &str) -> Result<String, SshError> {
        let dir = format!("{}/.ssh", home.trim_end_matches('/'));
        let res = self
            .exec(
                &format!(
                    "mkdir -p {d} && chmod 700 {d} && echo __OK__",
                    d = quote(&dir)
                ),
                None,
            )
            .await?;
        if !res.stdout.contains("__OK__") {
            let detail = if res.stderr.trim().is_empty() {
                "权限不足"
            } else {
                res.stderr.trim()
            };
            return Err(SshError::msg(format!("无法创建 {dir}：{detail}")));
        }
        Ok(dir)
    }

    /// 服务器基础信息。每一条失败都不致命，字段留空即可。
    pub async fn probe_server(&self) -> ServerProbe {
        let mut out = ServerProbe::default();
        let probes: [(&str, fn(&mut ServerProbe) -> &mut String); 3] = [
            ("uname -srm 2>/dev/null", |p| &mut p.uname),
            ("id -un 2>/dev/null", |p| &mut p.user),
            ("hostname 2>/dev/null", |p| &mut p.hostname),
        ];
        for (cmd, field) in probes {
            if let Ok(res) = self.exec_with_timeout(cmd, None, PROBE_TIMEOUT).await {
                if res.code == 0 && !res.stdout.trim().is_empty() {
                    *field(&mut out) = res.stdout.trim().to_string();
                }
            }
        }
        out
    }

    /// 传输层是否已关闭。服务器主动踢掉空闲连接后这里会先变成 true，
    /// 上层可以在进入任何业务（读文件、跑命令）之前就把死会话清掉。
    pub async fn is_closed(&self) -> bool {
        self.handle.lock().await.is_closed()
    }

    /// 主动断开（连接被服务器踢掉时也会自然失效）。
    pub async fn disconnect(&self) {
        let handle = self.handle.lock().await;
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
}

/// 连接 + 认证。失败信息已经翻译成人话（含 `hostkey` 错误码）。
pub async fn connect(cfg: &SshConfig, known_hosts: Arc<KnownHosts>) -> Result<SshClient, SshError> {
    let config = Arc::new(build_config());
    let handler = ClientHandler {
        known_hosts,
        host: cfg.host.clone(),
        port: cfg.port,
        trust: cfg.trust_fingerprint.clone(),
    };

    let addr = (cfg.host.as_str(), cfg.port);
    let mut handle = match tokio::time::timeout(
        CONNECT_TIMEOUT + Duration::from_secs(2),
        client::connect(config, addr, handler),
    )
    .await
    {
        Ok(Ok(handle)) => handle,
        Ok(Err(err)) => return Err(translate_connect_error(err, cfg)),
        Err(_) => {
            return Err(SshError::msg(format!(
                "连接超时（{}s）：{}:{} 无响应",
                CONNECT_TIMEOUT.as_secs(),
                cfg.host,
                cfg.port
            )))
        }
    };

    let auth_method = authenticate(&mut handle, cfg).await?;

    Ok(SshClient {
        handle: Mutex::new(handle),
        host: cfg.host.clone(),
        port: cfg.port,
        username: cfg.username.clone(),
        auth_method,
    })
}

/// 从配置目录拼出 known_hosts 文件路径（与旧版 `data/known_hosts.json` 同名）。
pub fn known_hosts_file(data_dir: impl Into<PathBuf>) -> PathBuf {
    data_dir.into().join("known_hosts.json")
}

fn build_config() -> Config {
    Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 6,
        inactivity_timeout: None,
        nodelay: true,
        ..Default::default()
    }
}

fn translate_connect_error(err: SshError, cfg: &SshConfig) -> SshError {
    match err {
        SshError::HostKey(_) | SshError::Msg(_) => err,
        SshError::Russh(e) => {
            let msg = e.to_string();
            let friendly = match &e {
                russh::Error::Version => {
                    "服务器没有返回 SSH 协议标识（这个端口可能不是 SSH 服务）".to_string()
                }
                russh::Error::NoCommonAlgo { .. } => {
                    "算法协商失败：服务器与客户端没有共同的加密算法（服务器可能过旧）".to_string()
                }
                _ => friendly_error(&msg, &cfg.host, cfg.port, passphrase_set(cfg)),
            };
            SshError::msg(friendly)
        }
    }
}

fn passphrase_set(cfg: &SshConfig) -> bool {
    secret_text(&cfg.passphrase).is_some()
}

/// 取出非空明文（不做 trim，passphrase / 密码里的空格是用户自己的事）。
fn secret_text(v: &Option<Secret>) -> Option<&str> {
    v.as_ref().map(Secret::expose).filter(|s| !s.is_empty())
}

async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    cfg: &SshConfig,
) -> Result<&'static str, SshError> {
    let mut attempted = false;

    if let Some(text) = cfg.private_key.as_ref().and_then(Secret::trimmed_non_empty) {
        attempted = true;
        let pem = Secret::new(
            private_key_openssh(text, secret_text(&cfg.passphrase)).map_err(SshError::msg)?,
        );
        let key = PrivateKey::from_openssh(pem.expose().trim())
            .map_err(|e| SshError::msg(format!("私钥无法解析：{e}")))?;
        let key = Arc::new(key);
        for hash_alg in rsa_hash_candidates(handle, &key).await {
            let with_alg = PrivateKeyWithHashAlg::new(Arc::clone(&key), hash_alg);
            match handle.authenticate_publickey(&cfg.username, with_alg).await {
                Ok(result) if result.success() => return Ok("key"),
                Ok(_) => {}
                Err(e) => {
                    return Err(SshError::msg(friendly_error(
                        &e.to_string(),
                        &cfg.host,
                        cfg.port,
                        passphrase_set(cfg),
                    )))
                }
            }
        }
    }

    if let Some(password) = secret_text(&cfg.password) {
        attempted = true;
        if handle
            .authenticate_password(&cfg.username, password)
            .await?
            .success()
        {
            return Ok("password");
        }
        if keyboard_interactive(handle, &cfg.username, password).await? {
            return Ok("password");
        }
    }

    if !attempted {
        return Err(SshError::msg(
            "没有可用的认证方式：这把主机没有配置私钥，也没有输入密码",
        ));
    }

    Err(SshError::msg(friendly_error(
        "All configured authentication methods failed",
        &cfg.host,
        cfg.port,
        passphrase_set(cfg),
    )))
}

/// RSA 要挑一个服务器认的签名哈希；不是 RSA 就返回唯一的 None。
async fn rsa_hash_candidates(
    handle: &mut Handle<ClientHandler>,
    key: &PrivateKey,
) -> Vec<Option<HashAlg>> {
    if !key.algorithm().is_rsa() {
        return vec![None];
    }
    match handle
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten()
    {
        Some(alg) => vec![Some(alg)],
        // 服务器没报 server-sig-algs：按从新到旧依次试
        None => vec![Some(HashAlg::Sha256), Some(HashAlg::Sha512), None],
    }
}

async fn keyboard_interactive(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    password: &str,
) -> Result<bool, SshError> {
    use client::KeyboardInteractiveAuthResponse as Ki;
    let mut resp = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await?;
    for _ in 0..4 {
        match resp {
            Ki::Success => return Ok(true),
            Ki::Failure { .. } => return Ok(false),
            Ki::InfoRequest { prompts, .. } => {
                resp = handle
                    .authenticate_keyboard_interactive_respond(vec![
                        password.to_string();
                        prompts.len()
                    ])
                    .await?;
            }
        }
    }
    Ok(false)
}

/// PTY 交互终端：`/api/terminal` 的 WebSocket 桥会在这上面收发。
pub struct PtyShell {
    read_half: ChannelReadHalf,
    write_half: ChannelWriteHalf<Msg>,
}

impl PtyShell {
    pub async fn open(
        client: &SshClient,
        term: &str,
        cols: u32,
        rows: u32,
    ) -> Result<Self, SshError> {
        let channel = client.open_channel().await?;
        channel
            .request_pty(true, term, cols, rows, 0, 0, &[])
            .await?;
        channel.request_shell(true).await?;
        let (read_half, write_half) = channel.split();
        Ok(Self {
            read_half,
            write_half,
        })
    }

    pub async fn next(&mut self) -> Option<ChannelMsg> {
        self.read_half.wait().await
    }

    /// 拆成读/写两半：读半边可以搬进独立任务，写半边留在原地接输入。
    pub fn into_parts(self) -> (ChannelReadHalf, ChannelWriteHalf<Msg>) {
        (self.read_half, self.write_half)
    }

    pub async fn input(&self, data: Vec<u8>) -> Result<(), SshError> {
        self.write_half.data_bytes(data).await?;
        Ok(())
    }

    pub async fn resize(&self, cols: u32, rows: u32) -> Result<(), SshError> {
        self.write_half.window_change(cols, rows, 0, 0).await?;
        Ok(())
    }

    pub async fn close(&self) {
        let _ = self.write_half.eof().await;
        let _ = self.write_half.close().await;
    }
}

fn decode_base64(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s))
}

fn clip(command: &str) -> String {
    command.chars().take(40).collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 另开一条连接，用「还没上场的私钥」真连一次 —— 切换登录密钥前必须过这一关。
pub async fn verify_login(cfg: &SshConfig, known_hosts: Arc<KnownHosts>) -> Result<(), SshError> {
    let client = connect(cfg, known_hosts).await?;
    let res = client.exec("printf __LOGIN_OK__", None).await;
    client.disconnect().await;
    let res = res?;
    if !res.stdout.contains("__LOGIN_OK__") {
        return Err(SshError::msg("新密钥登录测试未通过（命令回显异常）"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decode_tolerates_missing_padding() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello");
    }

    #[test]
    fn failure_text_prefers_stderr() {
        let r = ExecResult {
            code: 1,
            stdout: String::new(),
            stderr: "  boom\n".into(),
        };
        assert_eq!(r.failure_text(), "boom");
        let r = ExecResult {
            code: 3,
            stdout: "x".into(),
            stderr: String::new(),
        };
        assert_eq!(r.failure_text(), "退出码 3");
    }

    #[test]
    fn known_hosts_file_matches_old_layout() {
        assert_eq!(
            known_hosts_file("data"),
            PathBuf::from("data").join("known_hosts.json")
        );
    }
}
