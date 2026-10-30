//! 建立 / 刷新 / 断开 SSH 会话。
//!
//! 从 `routes.rs` 抽出来，桌面 HTTP 层与手机 FFI 共用同一套凭据兜底与状态语义，
//! 避免两端各写一遍「私钥解不开就用密码重试」这类容易走样的逻辑。

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};

use kw_core::Secret;
use kw_keys::find_private_key_by_fingerprint;
use kw_ssh::session::{connect, SshConfig};
use kw_store::Host;

use crate::error::ApiError;
use crate::sessions::{fingerprint_of_private_key, RemoteState, Session, Target};
use crate::state::{expand_path, AppState};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnectBody {
    pub host_id: Option<String>,
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub identity_file: Option<String>,
    pub remote_path: Option<String>,
    pub passphrase: Option<Secret>,
    pub password: Option<Secret>,
    pub trust_fingerprint: Option<String>,
    pub with_keys: Option<bool>,
}

pub(crate) fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// 决定这次连接用哪个私钥文件。
///
/// 优先级：请求里显式指定 → 主机在本机存的路径 → 按主机记的指纹去密钥库找。
/// 最后一条是跨设备同步的关键：同步包只带指纹（路径是各设备自己的），
/// 手机拿到电脑的主机配置后，靠指纹在本地密钥包里找到同一把钥匙。
fn resolve_identity_file(state: &AppState, host: &Host, requested: Option<&str>) -> String {
    if let Some(path) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        return path.to_string();
    }
    if !host.identity_file.trim().is_empty() {
        return host.identity_file.clone();
    }
    let fp = host.identity_fingerprint.trim();
    if fp.is_empty() {
        return String::new();
    }
    find_private_key_by_fingerprint(&state.keys_dir, fp, 3)
        .map(|entry| entry.path)
        .unwrap_or_default()
}

/// 保存过的主机按 id 取；否则用请求里的字段拼一个「临时连接」。
fn resolve_target(state: &AppState, body: &ConnectBody) -> Result<Target, ApiError> {
    let target = if let Some(id) = non_empty(&body.host_id) {
        let host = state
            .store
            .list_hosts()
            .into_iter()
            .find(|h| h.id == id)
            .ok_or_else(|| ApiError::bad("主机配置不存在，请刷新页面"))?;
        // 凭据弹窗是在已保存主机的基础上重连：必须保留 id，否则会话变成「临时连接」
        let identity_file = resolve_identity_file(state, &host, body.identity_file.as_deref());
        Target {
            id: host.id,
            name: host.name,
            host: host.host,
            port: host.port,
            username: host.username,
            identity_file,
            remote_path: host.remote_path,
        }
    } else {
        Target {
            id: String::new(),
            name: non_empty(&body.name)
                .or_else(|| non_empty(&body.host))
                .unwrap_or_default()
                .to_string(),
            host: non_empty(&body.host).unwrap_or_default().to_string(),
            port: body.port.unwrap_or(22),
            username: non_empty(&body.username).unwrap_or_default().to_string(),
            identity_file: body.identity_file.clone().unwrap_or_default(),
            remote_path: body.remote_path.clone().unwrap_or_default(),
        }
    };
    if target.host.is_empty() {
        return Err(ApiError::bad("主机地址不能为空"));
    }
    if target.username.is_empty() {
        return Err(ApiError::bad("用户名不能为空"));
    }
    Ok(target)
}

fn read_identity(path: &str) -> Result<Option<Secret>, ApiError> {
    if path.trim().is_empty() {
        return Ok(None);
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let bytes = zeroize::Zeroizing::new(bytes);
            Ok(Some(Secret::new(
                String::from_utf8_lossy(&bytes).into_owned(),
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError::bad(format!("私钥文件不存在：{path}")))
        }
        Err(e) => Err(ApiError::bad(format!("读取私钥文件失败：{path}（{e}）"))),
    }
}

pub async fn session_by_sid(state: &AppState, sid: &str) -> Result<Arc<Session>, ApiError> {
    state
        .sessions
        .must(sid)
        .await
        .ok_or_else(ApiError::session_lost)
}

/// 连接并读取远端状态；成功返回与旧版 `sessionPayload` 同形的 JSON。
pub async fn open(state: &Arc<AppState>, body: ConnectBody) -> Result<Value, ApiError> {
    let target = resolve_target(state, &body)?;
    let with_keys = body.with_keys.unwrap_or(true);
    let mut cfg = SshConfig {
        host: target.host.clone(),
        port: target.port,
        username: target.username.clone(),
        private_key: read_identity(&target.identity_file)?,
        passphrase: body.passphrase.filter(|s| !s.is_empty()),
        password: body.password.filter(|s| !s.is_empty()),
        trust_fingerprint: non_empty(&body.trust_fingerprint).map(str::to_string),
    };

    if cfg.private_key.is_none() && cfg.password.is_none() {
        return Err(ApiError::bad(
            "没有可用凭据：请选择私钥文件，或填写登录密码",
        ));
    }

    let known_hosts = Arc::clone(&state.known_hosts);
    let client = match connect(&cfg, Arc::clone(&known_hosts)).await {
        Ok(client) => client,
        Err(err) => {
            let msg = err.to_string();
            let key_broken = [
                "passphrase",
                "私钥密码",
                "私钥无法解析",
                "需要填写 passphrase",
            ]
            .iter()
            .any(|k| msg.contains(k));
            if key_broken && cfg.private_key.is_some() && cfg.password.is_some() {
                cfg.private_key = None;
                cfg.passphrase = None;
                connect(&cfg, known_hosts).await?
            } else {
                return Err(err.into());
            }
        }
    };

    let auth_method = client.auth_method;
    let used_key_fp = match (&cfg.private_key, auth_method) {
        (Some(text), "key") => {
            fingerprint_of_private_key(text.expose(), cfg.passphrase.as_ref().map(Secret::expose))
        }
        _ => String::new(),
    };
    drop(cfg);
    let server = client.probe_server().await;
    let (home, remote_path, raw) = if with_keys {
        let home = client.resolve_home().await?;
        let remote_path = expand_path(&target.remote_path, &home);
        let raw = client.read_text(&remote_path).await?;
        (home, remote_path, raw)
    } else {
        (String::new(), String::new(), None)
    };

    let remote = RemoteState {
        home,
        remote_path,
        file_exists: raw.is_some(),
        keys_loaded: with_keys,
    };

    let session = Arc::new(Session::new(
        uuid::Uuid::new_v4().to_string(),
        client,
        target,
        remote,
        server,
        auth_method,
        used_key_fp,
    ));
    let payload = session.payload(raw.as_deref().unwrap_or("")).await;
    state.sessions.insert(session).await;
    Ok(payload)
}

/// 重新读取授权文件；纯终端会话首次刷新时顺手补读家目录与路径。
pub async fn refresh(state: &AppState, sid: &str) -> Result<Value, ApiError> {
    let session = session_by_sid(state, sid).await?;
    let _write = session.write_lock.lock().await;

    let (home, remote_path, keys_loaded) = {
        let remote = session.remote.read().await;
        (
            remote.home.clone(),
            remote.remote_path.clone(),
            remote.keys_loaded,
        )
    };
    let (home, remote_path) = if keys_loaded {
        (home, remote_path)
    } else {
        let home = session.client.resolve_home().await?;
        let target = session.target.read().await.clone();
        let remote_path = expand_path(&target.remote_path, &home);
        (home, remote_path)
    };

    let raw = session.client.read_text(&remote_path).await?;
    let payload = {
        let mut remote = session.remote.write().await;
        remote.home = home;
        remote.remote_path = remote_path;
        remote.file_exists = raw.is_some();
        remote.keys_loaded = true;
        drop(remote);
        session.payload(raw.as_deref().unwrap_or("")).await
    };
    Ok(payload)
}

pub async fn disconnect(state: &AppState, sid: &str) -> Result<Value, ApiError> {
    if let Some(session) = state.sessions.remove(sid).await {
        session.client.disconnect().await;
    }
    Ok(json!(true))
}
