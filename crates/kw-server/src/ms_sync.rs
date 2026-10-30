//! 微软账号（OneDrive 应用专属文件夹）端到端加密同步。
//!
//! 只碰主机、钥匙备注、已知主机指纹：同步包在上传前先用同步口令
//! （Argon2id + AES-256-GCM）加密，云端只有密文。文件落在 Graph 的
//! `special/approot`（应用专属文件夹），权限只要 `Files.ReadWrite.AppFolder`，
//! 看不到用户网盘里的其它东西。
//!
//! 登录走 OAuth 2.0 设备代码流：桌面和手机都不需要回调地址或签名哈希，
//! 屏幕上出 8 位码，用户在浏览器里输一次就行。
//!
//! 客户端 ID 不内置 —— 用的是用户自己在 Azure 注册的应用（见 [`CLIENT_ID_HELP`]），
//! 填一次后存 `data/ms-config.json`；环境变量 `KW_MS_CLIENT_ID` 可以临时覆盖。
//! 令牌存 `data/ms-auth.json`（Unix 0600；Windows 没有等价的文件权限收紧手段）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use kw_ssh::known_hosts::KnownHosts;
use kw_store::Store;

use crate::sync::{export_bundle, import_bundle, ImportOptions, ImportReport};

const LOGIN_BASE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const SCOPES: &str = "Files.ReadWrite.AppFolder User.Read offline_access";
const SYNC_FILE: &str = "ssh-keywarden.kwsync";
const AUTH_FILE: &str = "ms-auth.json";
const CONFIG_FILE: &str = "ms-config.json";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 设备码登录默认最多等这么久；超时后同一个设备码还能继续轮询。
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
pub const CLIENT_ID_ENV: &str = "KW_MS_CLIENT_ID";

pub const CLIENT_ID_HELP: &str = "还没有配置 Azure 客户端 ID。\
去 https://portal.azure.com → Microsoft Entra ID → 应用注册 → 新注册：\
受支持的账户类型选「仅个人 Microsoft 账户」，\
在「身份验证」里启用「允许公共客户端流」，\
在「API 权限」里添加 Microsoft Graph 的委托权限 \
Files.ReadWrite.AppFolder、User.Read、offline_access，\
然后把「应用程序(客户端) ID」填进来（桌面端也可以设环境变量 KW_MS_CLIENT_ID）。";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/* ------------------------------------------------------------------ */
/* 本机状态文件                                                        */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct MsConfig {
    client_id: String,
    /// 端到端加密口令。手机沙盒里的应用私有目录，不进云端、不进日志。
    sync_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct MsAccountState {
    client_id: String,
    account_name: String,
    account_email: String,
    access_token: String,
    refresh_token: String,
    /// epoch 毫秒。
    expires_at: u64,
    last_sync_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsSyncStatus {
    /// 有没有可用的 Azure 客户端 ID。
    pub configured: bool,
    pub client_id: String,
    pub logged_in: bool,
    pub account_name: String,
    pub account_email: String,
    /// epoch 毫秒；0 表示还没同步过。
    pub last_sync_at: u64,
    /// 本机记住了同步口令（不会把口令本身回传）。
    pub has_sync_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodePrompt {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsSyncResult {
    pub pulled: bool,
    pub pushed: bool,
    pub last_sync_at: u64,
    pub report: ImportReport,
}

fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE)
}

fn auth_path(data_dir: &Path) -> PathBuf {
    data_dir.join(AUTH_FILE)
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&tmp, text).map_err(|e| format!("写入 {} 失败：{e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).map_err(|e| format!("保存 {} 失败：{e}", path.display()))
}

fn read_config(data_dir: &Path) -> MsConfig {
    read_json(&config_path(data_dir))
}

fn write_config(data_dir: &Path, cfg: &MsConfig) -> Result<(), String> {
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    write_private(&config_path(data_dir), &text)
}

fn read_auth(data_dir: &Path) -> Option<MsAccountState> {
    let st: MsAccountState = read_json(&auth_path(data_dir));
    if st.refresh_token.is_empty() {
        None
    } else {
        Some(st)
    }
}

fn write_auth(data_dir: &Path, st: &MsAccountState) -> Result<(), String> {
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    write_private(&auth_path(data_dir), &text)
}

/// 客户端 ID 是公开信息（不是密钥），这里只做形状校验，免得把别的东西粘进来。
fn normalize_client_id(raw: &str) -> Result<String, String> {
    let id = raw.trim().to_ascii_lowercase();
    let shaped = id.len() == 36
        && id.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if !shaped {
        return Err("客户端 ID 应该是 xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx 这样的 GUID".to_string());
    }
    Ok(id)
}

fn env_client_id() -> Option<String> {
    std::env::var(CLIENT_ID_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn resolve_client_id(data_dir: &Path, explicit: Option<&str>) -> Result<String, String> {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_client_id(raw);
    }
    if let Some(env) = env_client_id() {
        return normalize_client_id(&env);
    }
    let cfg = read_config(data_dir);
    if !cfg.client_id.trim().is_empty() {
        return normalize_client_id(&cfg.client_id);
    }
    Err(CLIENT_ID_HELP.to_string())
}

/* ------------------------------------------------------------------ */
/* 账号状态 / 配置                                                     */
/* ------------------------------------------------------------------ */

pub fn status(data_dir: &Path) -> MsSyncStatus {
    let cfg = read_config(data_dir);
    let auth = read_auth(data_dir);
    let client_id = if !cfg.client_id.is_empty() {
        cfg.client_id.clone()
    } else {
        env_client_id().unwrap_or_default()
    };
    MsSyncStatus {
        configured: !client_id.is_empty(),
        client_id,
        logged_in: auth.is_some(),
        account_name: auth.as_ref().map(|a| a.account_name.clone()).unwrap_or_default(),
        account_email: auth.as_ref().map(|a| a.account_email.clone()).unwrap_or_default(),
        last_sync_at: auth.as_ref().map(|a| a.last_sync_at).unwrap_or(0),
        has_sync_key: !cfg.sync_key.is_empty(),
    }
}

/// 记下 Azure 客户端 ID 和（可选的）同步口令。传空字符串表示清掉该项。
pub fn save_config(
    data_dir: &Path,
    client_id: Option<&str>,
    sync_key: Option<&str>,
) -> Result<MsSyncStatus, String> {
    let mut cfg = read_config(data_dir);
    if let Some(raw) = client_id {
        cfg.client_id = if raw.trim().is_empty() {
            String::new()
        } else {
            normalize_client_id(raw)?
        };
    }
    if let Some(key) = sync_key {
        cfg.sync_key = key.to_string();
    }
    write_config(data_dir, &cfg)?;
    Ok(status(data_dir))
}

/// 本机记住的同步口令（桌面 CLI 拿它免交互同步）。
pub fn stored_sync_key(data_dir: &Path) -> Option<String> {
    let key = read_config(data_dir).sync_key;
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

/// 退出登录：删令牌，保留客户端 ID 和同步口令配置。
pub fn logout(data_dir: &Path) -> Result<(), String> {
    let file = auth_path(data_dir);
    if file.exists() {
        fs::remove_file(&file).map_err(|e| format!("退出微软账号失败：{e}"))?;
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* 设备码登录                                                          */
/* ------------------------------------------------------------------ */

/// 自己给 rustls 配根证书：reqwest 默认走 `rustls-platform-verifier`，它在 Android 上
/// 要求 Kotlin 侧先 `init_with_env`，Flutter FFI 没有那段初始化。这里用随包内置的
/// Mozilla 根证书，三个平台行为一致（代价是不认系统里后装的私有根证书）。
fn http_client() -> Result<reqwest::Client, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("初始化 TLS 失败：{e}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();

    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|e| format!("初始化网络客户端失败：{e}"))
}

async fn post_form(
    client: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let resp = client
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| format!("连不上微软登录服务：{e}"))?;
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("微软登录服务返回的内容看不懂：{e}"))?;
    Ok(val)
}

fn error_text(val: &serde_json::Value) -> Option<String> {
    let code = val.get("error").and_then(|v| v.as_str())?;
    let desc = val
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or(code);
    Some(desc.to_string())
}

/// 第一步：拿设备码。用户在浏览器里输的是 `user_code`。
pub async fn start_device_login(
    data_dir: &Path,
    client_id: Option<&str>,
) -> Result<DeviceCodePrompt, String> {
    let client_id = resolve_client_id(data_dir, client_id)?;
    let client = http_client()?;
    let val = post_form(
        &client,
        &format!("{LOGIN_BASE}/devicecode"),
        &[("client_id", client_id.as_str()), ("scope", SCOPES)],
    )
    .await?;

    if let Some(err) = error_text(&val) {
        return Err(format!("微软拒绝了登录请求：{err}"));
    }
    let device_code = val["device_code"].as_str().unwrap_or_default().to_string();
    if device_code.is_empty() {
        return Err("微软没有返回设备码".to_string());
    }
    let prompt = DeviceCodePrompt {
        device_code,
        user_code: val["user_code"].as_str().unwrap_or_default().to_string(),
        verification_uri: val["verification_uri"]
            .as_str()
            .unwrap_or("https://microsoft.com/devicelogin")
            .to_string(),
        expires_in: val["expires_in"].as_u64().unwrap_or(900),
        interval: val["interval"].as_u64().unwrap_or(5).max(1),
    };

    // 先把客户端 ID 落盘：轮询换令牌时必须用同一个 ID，重启应用也不丢。
    let mut cfg = read_config(data_dir);
    if cfg.client_id != client_id {
        cfg.client_id = client_id;
        write_config(data_dir, &cfg)?;
    }
    Ok(prompt)
}

/// 第二步：轮询授权结果。超时返回错误，但设备码没作废，可以再调一次。
pub async fn complete_device_login(
    data_dir: &Path,
    device_code: &str,
    client_id: Option<&str>,
    timeout: Duration,
) -> Result<MsSyncStatus, String> {
    let client_id = resolve_client_id(data_dir, client_id)?;
    let client = http_client()?;
    let url = format!("{LOGIN_BASE}/token");
    let started = Instant::now();
    let mut interval = 5u64;

    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if started.elapsed() >= timeout {
            return Err("还没等到授权。浏览器里完成登录后，再点一次「完成登录」".to_string());
        }

        let val = post_form(
            &client,
            &url,
            &[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code",
                ),
                ("client_id", client_id.as_str()),
                ("device_code", device_code),
            ],
        )
        .await?;

        if let Some(token) = val.get("access_token").and_then(|v| v.as_str()) {
            let refresh_token = val["refresh_token"].as_str().unwrap_or_default().to_string();
            if refresh_token.is_empty() {
                return Err("微软没有返回刷新令牌，请重新登录".to_string());
            }
            let expires_in = val["expires_in"].as_u64().unwrap_or(3600);
            let (name, email) = fetch_profile(&client, token).await;
            let state = MsAccountState {
                client_id: client_id.clone(),
                account_name: name,
                account_email: email,
                access_token: token.to_string(),
                refresh_token,
                expires_at: now_ms() + expires_in.saturating_mul(1000).saturating_sub(60_000),
                last_sync_at: 0,
            };
            write_auth(data_dir, &state)?;
            return Ok(status(data_dir));
        }

        match val.get("error").and_then(|v| v.as_str()) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("expired_token") => {
                return Err("验证码已过期，请重新发起登录".to_string());
            }
            Some("authorization_declined") => {
                return Err("授权被拒绝；如果选错了账号，重新发起登录再试".to_string());
            }
            Some("bad_verification_code") => {
                return Err("设备码无效（可能换了客户端 ID），请重新发起登录".to_string());
            }
            Some(_) => {
                return Err(format!(
                    "微软授权未完成：{}",
                    error_text(&val).unwrap_or_default()
                ));
            }
            None => return Err("微软返回了无法识别的授权状态".to_string()),
        }
    }
}

async fn fetch_profile(client: &reqwest::Client, access_token: &str) -> (String, String) {
    let fallback = ("Microsoft 账户".to_string(), String::new());
    let Ok(resp) = client
        .get(format!("{GRAPH_BASE}/me"))
        .bearer_auth(access_token)
        .send()
        .await
    else {
        return fallback;
    };
    let Ok(val) = resp.json::<serde_json::Value>().await else {
        return fallback;
    };
    let name = val["displayName"]
        .as_str()
        .unwrap_or("Microsoft 账户")
        .to_string();
    let email = val["userPrincipalName"]
        .as_str()
        .or_else(|| val["mail"].as_str())
        .unwrap_or_default()
        .to_string();
    (name, email)
}

async fn ensure_valid_token(
    client: &reqwest::Client,
    data_dir: &Path,
) -> Result<MsAccountState, String> {
    let mut st = read_auth(data_dir).ok_or_else(|| "还没有登录微软账号".to_string())?;
    if !st.access_token.is_empty() && now_ms() + 60_000 < st.expires_at {
        return Ok(st);
    }

    let client_id = resolve_client_id(data_dir, Some(&st.client_id))?;
    let val = post_form(
        client,
        &format!("{LOGIN_BASE}/token"),
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("refresh_token", st.refresh_token.as_str()),
            ("scope", SCOPES),
        ],
    )
    .await?;

    let Some(token) = val.get("access_token").and_then(|v| v.as_str()) else {
        let err = val.get("error").and_then(|v| v.as_str()).unwrap_or("");
        return Err(match err {
            "invalid_grant" => "微软账号授权已失效（改过密码或被撤销），请重新登录".to_string(),
            _ => format!(
                "刷新微软令牌失败：{}",
                error_text(&val).unwrap_or_else(|| "未知原因".to_string())
            ),
        });
    };

    st.client_id = client_id;
    st.access_token = token.to_string();
    if let Some(rt) = val.get("refresh_token").and_then(|v| v.as_str()) {
        if !rt.is_empty() {
            st.refresh_token = rt.to_string();
        }
    }
    let expires_in = val["expires_in"].as_u64().unwrap_or(3600);
    st.expires_at = now_ms() + expires_in.saturating_mul(1000).saturating_sub(60_000);
    write_auth(data_dir, &st)?;
    Ok(st)
}

/* ------------------------------------------------------------------ */
/* 双向同步                                                            */
/* ------------------------------------------------------------------ */

/// 拉云端 → 合并进本地 → 把合并后的结果加密传回去。
///
/// 已知主机指纹有冲突时**不上传**，把冲突交给用户确认：确认后带
/// `trust_changed` 重跑一遍即可，免得用本机还带着旧指纹的状态覆盖云端。
pub async fn sync_now(
    store: &Store,
    known_hosts: &KnownHosts,
    passphrase: &str,
    trust_changed: &[String],
) -> Result<MsSyncResult, String> {
    let data_dir = store.dir().to_path_buf();
    let cfg = read_config(&data_dir);
    let pass = passphrase.trim();
    let pass = if pass.is_empty() {
        cfg.sync_key.trim()
    } else {
        pass
    };
    if pass.is_empty() {
        return Err("请先设置同步口令：云端同步包用它做端到端加密".to_string());
    }

    let client = http_client()?;
    let mut auth = ensure_valid_token(&client, &data_dir).await?;
    let url = format!("{GRAPH_BASE}/me/drive/special/approot:/{SYNC_FILE}:/content");

    let resp = client
        .get(&url)
        .bearer_auth(&auth.access_token)
        .send()
        .await
        .map_err(|e| format!("读取 OneDrive 同步文件失败：{e}"))?;

    let keys_dir = store.dir().join("keys");
    let mut report = ImportReport::default();
    let pulled = match resp.status() {
        StatusCode::OK => {
            let text = resp
                .text()
                .await
                .map_err(|e| format!("读取云端内容失败：{e}"))?;
            if text.trim().is_empty() {
                false
            } else {
                report = import_bundle(
                    store,
                    known_hosts,
                    &keys_dir,
                    &text,
                    Some(pass),
                    &ImportOptions {
                        trust_changed: trust_changed.to_vec(),
                        trust_all_changed: false,
                    },
                )?;
                true
            }
        }
        StatusCode::NOT_FOUND => false,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err("微软账号授权已失效，请重新登录".to_string());
        }
        other => return Err(format!("OneDrive 读取失败（HTTP {}）", other.as_u16())),
    };

    if !report.known_host_conflicts.is_empty() {
        return Ok(MsSyncResult {
            pulled,
            pushed: false,
            last_sync_at: auth.last_sync_at,
            report,
        });
    }

    // 微软账号同步一定带口令（没有口令根本走不到这里），所以私钥随包一起走
    let bundle = export_bundle(store, known_hosts, &keys_dir, true, Some(pass))?;
    let put = client
        .put(&url)
        .bearer_auth(&auth.access_token)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(bundle)
        .send()
        .await
        .map_err(|e| format!("上传同步包到 OneDrive 失败：{e}"))?;

    let status = put.status();
    if !status.is_success() {
        return Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                "微软账号授权已失效，请重新登录".to_string()
            }
            other => format!("OneDrive 写入失败（HTTP {}）", other.as_u16()),
        });
    }

    auth.last_sync_at = now_ms();
    write_auth(&data_dir, &auth)?;

    Ok(MsSyncResult {
        pulled,
        pushed: true,
        last_sync_at: auth.last_sync_at,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn client_id_must_look_like_a_guid() {
        let ok = normalize_client_id("0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9").unwrap();
        assert_eq!(ok, "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9");
        assert!(normalize_client_id("not-a-client-id").is_err());
        assert!(normalize_client_id("0a1b2c3d4e5f60718293a4b5c6d7e8f9").is_err());
    }

    #[test]
    fn config_round_trip_and_logout_keeps_config() {
        let d = dir();
        let st = save_config(d.path(), Some("0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"), Some("pw"))
            .unwrap();
        assert!(st.configured);
        assert!(st.has_sync_key);
        assert!(!st.logged_in);

        // 退出登录只删令牌，客户端 ID 与口令配置要留着
        let auth = MsAccountState {
            client_id: st.client_id.clone(),
            refresh_token: "refresh".into(),
            ..Default::default()
        };
        write_auth(d.path(), &auth).unwrap();
        assert!(status(d.path()).logged_in);
        logout(d.path()).unwrap();
        let after = status(d.path());
        assert!(!after.logged_in);
        assert!(after.configured);
        assert!(after.has_sync_key);
    }

    #[test]
    fn empty_client_id_clears_config() {
        let d = dir();
        save_config(d.path(), Some("0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"), None).unwrap();
        assert!(status(d.path()).configured);
        save_config(d.path(), Some(""), None).unwrap();
        assert!(!status(d.path()).configured);
    }

    #[test]
    fn unconfigured_login_says_how_to_register() {
        let d = dir();
        // 环境变量在测试进程里可能是用户设的，清掉再看；设了就不再断言。
        let had_env = std::env::var(CLIENT_ID_ENV).is_ok();
        if !had_env {
            let err = resolve_client_id(d.path(), None).unwrap_err();
            assert!(err.contains("应用注册"), "错误里要给出注册指引：{err}");
        }
    }

    #[test]
    fn sync_without_key_is_rejected_before_any_network() {
        let d = dir();
        let store = Store::new(d.path());
        let kh = KnownHosts::new(d.path().join("known_hosts.json"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(sync_now(&store, &kh, "  ", &[])).unwrap_err();
        assert!(err.contains("同步口令"), "{err}");
    }
}
