//! 手机端（Flutter）直接调用的 Rust 核心门面。
//!
//! 不在手机上开 HTTP 端口：直接复用 `kw-server` 里已经过测试的会话与密钥操作
//! （`connect` / `keys_ops`），保证桌面与手机走同一条安全闸门。
//!
//! 两种调用方式：
//!  * Rust / `flutter_rust_bridge`：直接用 [`MobileEngine`]；
//!  * C ABI：`kw_init(app_dir)` 一次，之后 `kw_call(method, args_json)`，
//!    返回的字符串用 `kw_free_string` 释放。

use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use kw_keys::keygen::{generate_ed25519, generate_rsa};
use kw_keys::localkeys::scan_keys;
use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::session::known_hosts_file;
use kw_ssh::util::shanghai_stamp;
use kw_store::{HostPatch, Store};

use kw_server::connect::{self, ConnectBody};
use kw_server::error::ApiError;
use kw_server::fsbrowse::safe_name;
use kw_server::keys_ops;
use kw_server::sessions::Session;
use kw_server::state::AppState;

pub mod terminal;

/// 同步包与微软账号同步的实现都在 `kw-server` 里（桌面端也要用同一份）。
pub use kw_server::ms_sync;
pub use kw_server::sync;

pub use kw_server::sync::{
    export_bundle, import_bundle, ImportOptions, ImportReport, KnownHostConflict, SyncBundle,
};

pub struct MobileEngine {
    data_dir: PathBuf,
    keys_dir: PathBuf,
    state: Arc<AppState>,
    /// termId → 缓冲 PTY 终端；只在内存里，不落盘。
    terminals: terminal::Terminals,
}

impl MobileEngine {
    /// `app_dir` 传手机系统的 ApplicationSupportDirectory；
    /// 配置落在 `app_dir/data`，随身密钥包落在 `app_dir/keys`。
    pub fn new(app_dir: impl Into<PathBuf>) -> Result<Self, String> {
        let base = app_dir.into();
        let data_dir = base.join("data");
        let keys_dir = base.join("keys");
        fs::create_dir_all(&data_dir).map_err(|e| format!("创建数据目录失败：{e}"))?;
        fs::create_dir_all(&keys_dir).map_err(|e| format!("创建密钥目录失败：{e}"))?;

        let store = Arc::new(Store::new(&data_dir));
        let known_hosts = Arc::new(KnownHosts::new(known_hosts_file(&data_dir)));
        let state = Arc::new(AppState::new(
            String::new(),
            0,
            store,
            known_hosts,
            data_dir.clone(),
            env!("CARGO_PKG_VERSION"),
        ));

        Ok(Self {
            data_dir,
            keys_dir,
            state,
            terminals: terminal::Terminals::new(),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn keys_dir(&self) -> &Path {
        &self.keys_dir
    }

    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// 首页全量状态：主机、备注、随身密钥包。
    pub fn bootstrap(&self) -> Value {
        let scan = scan_keys(&self.keys_dir, 2);
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "hosts": self.state.store.list_hosts(),
            "keyNotes": self.state.store.list_key_notes(),
            "localKeys": {
                "dir": self.keys_dir.display().to_string(),
                "entries": scan.entries,
                "errors": scan.errors,
            },
        })
    }

    /// 在手机沙盒里生成密钥对；私钥 0600（Unix），公钥写同名 `.pub`。
    pub fn generate_key(
        &self,
        algorithm: &str,
        label: &str,
        comment: Option<&str>,
        passphrase: Option<&str>,
    ) -> Result<Value, String> {
        let gen = if algorithm.eq_ignore_ascii_case("rsa") {
            generate_rsa(comment, 3072, passphrase)
        } else {
            generate_ed25519(comment, passphrase)
        }
        .map_err(|e| format!("生成密钥失败：{e}"))?;

        let date: String = shanghai_stamp(SystemTime::now()).chars().take(8).collect();
        let stem = format!("{}_{date}", safe_name(label));
        let mut priv_path = self.keys_dir.join(format!("{stem}.key"));
        let mut n = 2;
        while priv_path.exists() {
            priv_path = self.keys_dir.join(format!("{stem}_{n}.key"));
            n += 1;
        }
        let pub_path = PathBuf::from(format!("{}.pub", priv_path.display()));

        fs::write(&priv_path, &gen.private_key_pem).map_err(|e| format!("写入私钥失败：{e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&priv_path, fs::Permissions::from_mode(0o600));
        }
        fs::write(&pub_path, format!("{}\n", gen.public_key_line))
            .map_err(|e| format!("写入公钥失败：{e}"))?;

        Ok(json!({
            "privateKeyPath": priv_path.display().to_string(),
            "publicKeyPath": pub_path.display().to_string(),
            "publicKeyLine": gen.public_key_line,
            "fingerprint": gen.fingerprint,
            "type": gen.key_type,
            "encrypted": gen.encrypted,
        }))
    }

    /* ------------------------------------------------------------------ */
    /* 会话                                                                */
    /* ------------------------------------------------------------------ */

    pub async fn connect(&self, body: ConnectBody) -> Result<Value, Value> {
        connect::open(&self.state, body).await.map_err(err_json)
    }

    /// 新增或修改主机（手机上没有桌面那种导入入口，只能手填或同步包导入）。
    pub fn save_host(&self, patch: HostPatch) -> Result<Value, Value> {
        self.state
            .store
            .upsert_host(patch)
            .map(|h| json!(h))
            .map_err(|e| json!({ "error": e.to_string() }))
    }

    pub fn delete_host(&self, id: &str) -> Result<Value, Value> {
        self.state
            .store
            .remove_host(id)
            .map(|_| json!(true))
            .map_err(|e| json!({ "error": e.to_string() }))
    }

    /// 钥匙归属备注只存本机，不写服务器。
    pub fn set_key_note(&self, fingerprint: &str, note: &str) -> Result<Value, Value> {
        self.state
            .store
            .set_key_note(fingerprint, note)
            .map(|notes| json!(notes))
            .map_err(|e| json!({ "error": e.to_string() }))
    }

    pub async fn refresh(&self, sid: &str) -> Result<Value, Value> {
        connect::refresh(&self.state, sid).await.map_err(err_json)
    }

    pub async fn disconnect(&self, sid: &str) -> Result<Value, Value> {
        self.terminals.close_for(sid).await;
        connect::disconnect(&self.state, sid)
            .await
            .map_err(err_json)
    }

    async fn session(&self, sid: &str) -> Result<Arc<Session>, Value> {
        connect::session_by_sid(&self.state, sid)
            .await
            .map_err(err_json)
    }

    /// 终端有动静就续命，免得空闲清理把正在打字的会话踢掉。
    async fn touch_session(&self, sid: &str) {
        if let Some(session) = self.state.sessions.get(sid).await {
            session.touch();
        }
    }

    /* ------------------------------------------------------------------ */
    /* 终端                                                                */
    /* ------------------------------------------------------------------ */

    pub async fn term_open(&self, body: Value) -> Result<Value, Value> {
        let sid = body_str(&body, "sid")?;
        let session = self.session(sid).await?;
        self.terminals
            .open(&session, num_field(&body, "cols"), num_field(&body, "rows"))
            .await
            .map_err(err_json)
    }

    pub async fn term_read(&self, term_id: &str, wait_ms: Option<u64>) -> Result<Value, Value> {
        let term = self.terminals.require(term_id).await.map_err(err_json)?;
        self.touch_session(term.sid()).await;
        Ok(terminal::read_json(&term, wait_ms).await)
    }

    pub async fn term_write(&self, body: Value) -> Result<Value, Value> {
        let term_id = body_str(&body, "termId")?;
        let data = arg(&body, "data").ok_or_else(|| json!({ "error": "缺少 data" }))?;
        let term = self.terminals.require(&term_id).await.map_err(err_json)?;
        self.touch_session(term.sid()).await;
        terminal::write(&term, data).await.map_err(err_json)
    }

    pub async fn term_resize(&self, body: Value) -> Result<Value, Value> {
        let term_id = body_str(&body, "termId")?;
        let term = self.terminals.require(&term_id).await.map_err(err_json)?;
        terminal::resize(&term, num_field(&body, "cols"), num_field(&body, "rows"))
            .await
            .map_err(err_json)
    }

    pub async fn term_close(&self, term_id: &str) -> Result<Value, Value> {
        Ok(json!(self.terminals.close(term_id).await))
    }

    /* ------------------------------------------------------------------ */
    /* 密钥增删换 / 备份 / sshd                                            */
    /* ------------------------------------------------------------------ */

    pub async fn add_key(&self, body: Value) -> Result<Value, Value> {
        let sid = body_str(&body, "sid")?;
        let session = self.session(sid).await?;
        let _write = session.write_lock.lock().await;
        // 手机上没有「桌面默认保存目录」，生成的新密钥直接进随身密钥包
        let mut body = body;
        if body.get("mode").and_then(Value::as_str) == Some("generate")
            && body.get("saveDir").is_none()
        {
            body["saveDir"] = json!(self.keys_dir.display().to_string());
        }
        keys_ops::add_key(&self.state, &session, &body)
            .await
            .map_err(err_json)
    }

    pub async fn replace_key(&self, body: Value) -> Result<Value, Value> {
        let sid = body_str(&body, "sid")?;
        let session = self.session(sid).await?;
        let _write = session.write_lock.lock().await;
        let mut body = body;
        if body.get("mode").and_then(Value::as_str) == Some("generate")
            && body.get("saveDir").is_none()
        {
            body["saveDir"] = json!(self.keys_dir.display().to_string());
        }
        keys_ops::replace_key(&self.state, &session, &body)
            .await
            .map_err(err_json)
    }

    pub async fn delete_keys(&self, body: Value) -> Result<Value, Value> {
        let sid = body_str(&body, "sid")?;
        let session = self.session(sid).await?;
        let _write = session.write_lock.lock().await;
        keys_ops::delete_keys(&session, &body)
            .await
            .map_err(err_json)
    }

    pub async fn restore_backup(&self, body: Value) -> Result<Value, Value> {
        let sid = body_str(&body, "sid")?;
        let session = self.session(sid).await?;
        let _write = session.write_lock.lock().await;
        keys_ops::restore_backup(&session, &body)
            .await
            .map_err(err_json)
    }

    pub async fn backups(&self, sid: &str) -> Result<Value, Value> {
        let session = self.session(sid).await?;
        keys_ops::list_backups(&session).await.map_err(err_json)
    }

    pub async fn backup_diff(&self, sid: &str, name: &str) -> Result<Value, Value> {
        let session = self.session(sid).await?;
        keys_ops::backup_diff(&session, name)
            .await
            .map_err(err_json)
    }

    pub async fn sshd_status(&self, sid: &str) -> Result<Value, Value> {
        let session = self.session(sid).await?;
        kw_ssh::sshd::inspect(&session.client)
            .await
            .map_err(|e| err_json(ApiError::from(e)))
    }

    pub async fn set_key_only(&self, sid: &str, enable: bool) -> Result<Value, Value> {
        let session = self.session(sid).await?;
        let _write = session.write_lock.lock().await;
        keys_ops::set_key_only(&session, enable)
            .await
            .map_err(err_json)
    }

    /* ------------------------------------------------------------------ */
    /* 微软账号（OneDrive AppFolder）同步                                   */
    /* ------------------------------------------------------------------ */

    pub fn ms_status(&self) -> Value {
        json!(ms_sync::status(&self.data_dir))
    }

    /// 存 Azure 客户端 ID / 同步口令。字段传 `None` 表示不动，传空串表示清掉。
    pub fn ms_save_config(
        &self,
        client_id: Option<&str>,
        sync_key: Option<&str>,
    ) -> Result<Value, String> {
        ms_sync::save_config(&self.data_dir, client_id, sync_key).map(|st| json!(st))
    }

    pub async fn ms_start_login(&self, client_id: Option<&str>) -> Result<Value, String> {
        ms_sync::start_device_login(&self.data_dir, client_id)
            .await
            .map(|p| json!(p))
    }

    pub async fn ms_complete_login(
        &self,
        device_code: &str,
        timeout_secs: u64,
    ) -> Result<Value, String> {
        let timeout = if timeout_secs == 0 {
            ms_sync::DEFAULT_LOGIN_TIMEOUT
        } else {
            Duration::from_secs(timeout_secs.clamp(5, 900))
        };
        ms_sync::complete_device_login(&self.data_dir, device_code, None, timeout)
            .await
            .map(|st| json!(st))
    }

    pub fn ms_logout(&self) -> Result<Value, String> {
        ms_sync::logout(&self.data_dir).map(|_| json!(true))
    }

    /// 云端 ↔ 本机双向同步。口令留空就用本机记住的那份。
    ///
    /// `force_upload` 为真时跳过拉取，直接用本机覆盖云端 —— 云端包解不开时的唯一出路。
    pub async fn ms_sync(
        &self,
        passphrase: &str,
        trust_changed: &[String],
        force_upload: bool,
    ) -> Result<Value, String> {
        ms_sync::sync_now(
            &self.state.store,
            &self.state.known_hosts,
            passphrase,
            trust_changed,
            force_upload,
        )
        .await
        .map(|out| json!(out))
    }

    /* ------------------------------------------------------------------ */
    /* JSON 方法分发（C ABI 用）                                            */
    /* ------------------------------------------------------------------ */

    pub fn call_json(&self, method: &str, raw: &str) -> Value {
        let raw = if raw.trim().is_empty() { "{}" } else { raw };
        if method == "connect" {
            return match serde_json::from_str::<ConnectBody>(raw) {
                Ok(body) => async_call(block_on(self.connect(body))),
                Err(_) => fail("连接参数格式错误"),
            };
        }
        match serde_json::from_str(raw) {
            Ok(args) => self.call(method, &args),
            Err(_) => fail("args 不是合法 JSON"),
        }
    }

    pub fn call(&self, method: &str, args: &Value) -> Value {
        match method {
            "bootstrap" => ok(self.bootstrap()),
            "generateKey" => match self.generate_key(
                arg(args, "algorithm").unwrap_or("ed25519"),
                arg(args, "label").unwrap_or("mobile"),
                arg(args, "comment"),
                args.get("passphrase")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty()),
            ) {
                Ok(v) => ok(v),
                Err(e) => fail(e),
            },
            "connect" => {
                let body: ConnectBody = match serde_json::from_value(args.clone()) {
                    Ok(b) => b,
                    Err(e) => return fail(format!("参数格式错误：{e}")),
                };
                match block_on(self.connect(body)) {
                    Ok(v) => ok(v),
                    Err(e) => fail_value(e),
                }
            }
            "saveHost" => match serde_json::from_value::<HostPatch>(args.clone()) {
                Ok(patch) => async_call(self.save_host(patch)),
                Err(e) => fail(format!("参数格式错误：{e}")),
            },
            "deleteHost" => match arg(args, "id") {
                Some(id) => async_call(self.delete_host(id)),
                None => fail("缺少 id"),
            },
            "setKeyNote" => match arg(args, "fingerprint") {
                Some(fp) => async_call(
                    self.set_key_note(fp, args.get("note").and_then(Value::as_str).unwrap_or("")),
                ),
                None => fail("缺少 fingerprint"),
            },
            "refresh" => match arg(args, "sid") {
                Some(sid) => match block_on(self.refresh(sid)) {
                    Ok(v) => ok(v),
                    Err(e) => fail_value(e),
                },
                None => fail("缺少 sid"),
            },
            "disconnect" => match arg(args, "sid") {
                Some(sid) => match block_on(self.disconnect(sid)) {
                    Ok(v) => ok(v),
                    Err(e) => fail_value(e),
                },
                None => fail("缺少 sid"),
            },
            "addKey" => async_call(block_on(self.add_key(args.clone()))),
            "replaceKey" => async_call(block_on(self.replace_key(args.clone()))),
            "deleteKeys" => async_call(block_on(self.delete_keys(args.clone()))),
            "restoreBackup" => async_call(block_on(self.restore_backup(args.clone()))),
            "termOpen" => async_call(block_on(self.term_open(args.clone()))),
            "termRead" => match arg(args, "termId") {
                Some(term_id) => async_call(block_on(
                    self.term_read(term_id, args.get("waitMs").and_then(Value::as_u64)),
                )),
                None => fail("缺少 termId"),
            },
            "termWrite" => async_call(block_on(self.term_write(args.clone()))),
            "termResize" => async_call(block_on(self.term_resize(args.clone()))),
            "termClose" => match arg(args, "termId") {
                Some(term_id) => async_call(block_on(self.term_close(term_id))),
                None => fail("缺少 termId"),
            },
            "backups" => match arg(args, "sid") {
                Some(sid) => async_call(block_on(self.backups(sid))),
                None => fail("缺少 sid"),
            },
            "backupDiff" => match (arg(args, "sid"), arg(args, "name")) {
                (Some(sid), Some(name)) => async_call(block_on(self.backup_diff(sid, name))),
                _ => fail("缺少 sid 或 name"),
            },
            "sshd" => match arg(args, "sid") {
                Some(sid) => async_call(block_on(self.sshd_status(sid))),
                None => fail("缺少 sid"),
            },
            "setKeyOnly" => match arg(args, "sid") {
                Some(sid) => async_call(block_on(self.set_key_only(
                    sid,
                    args.get("enable").and_then(Value::as_bool).unwrap_or(false),
                ))),
                None => fail("缺少 sid"),
            },
            "msStatus" => ok(self.ms_status()),
            "msSaveConfig" => {
                // 用 get 而不是 arg：空串代表「清掉这一项」
                let client_id = args.get("clientId").and_then(Value::as_str);
                let sync_key = args.get("syncKey").and_then(Value::as_str);
                match self.ms_save_config(client_id, sync_key) {
                    Ok(v) => ok(v),
                    Err(e) => fail(e),
                }
            }
            "msStartLogin" => match block_on(self.ms_start_login(arg(args, "clientId"))) {
                Ok(v) => ok(v),
                Err(e) => fail(e),
            },
            "msCompleteLogin" => match arg(args, "deviceCode") {
                Some(code) => {
                    let wait = args.get("timeoutSec").and_then(Value::as_u64).unwrap_or(0);
                    match block_on(self.ms_complete_login(code, wait)) {
                        Ok(v) => ok(v),
                        Err(e) => fail(e),
                    }
                }
                None => fail("缺少 deviceCode"),
            },
            "msLogout" => match self.ms_logout() {
                Ok(v) => ok(v),
                Err(e) => fail(e),
            },
            "msSync" => {
                let pass = args
                    .get("passphrase")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let trust_changed: Vec<String> = args
                    .get("trustChanged")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let force_upload = args
                    .get("forceUpload")
                    .or_else(|| args.get("force_upload"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                match block_on(self.ms_sync(pass, &trust_changed, force_upload)) {
                    Ok(v) => ok(v),
                    Err(e) => fail(e),
                }
            }
            other => fail(format!("未知方法：{other}")),
        }
    }
}

/* ------------------------------------------------------------------ */
/* C ABI                                                               */
/* ------------------------------------------------------------------ */

static ENGINE: OnceLock<Arc<MobileEngine>> = OnceLock::new();
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("无法创建 tokio runtime")
    })
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    runtime().block_on(f)
}

/// 初始化引擎。返回 JSON：`{"ok":true}` 或 `{"ok":false,"error":...}`。
/// 重复调用是幂等的，不会丢掉已有会话。
///
/// # Safety
/// `app_dir` 必须是以 NUL 结尾的合法 C 字符串。
#[no_mangle]
pub extern "C" fn kw_init(app_dir: *const c_char) -> *mut c_char {
    if app_dir.is_null() {
        return into_c(fail("app_dir 为空"));
    }
    let dir = match unsafe { CStr::from_ptr(app_dir) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return into_c(fail("app_dir 不是合法 UTF-8")),
    };
    match MobileEngine::new(&dir) {
        Ok(engine) => {
            let _ = ENGINE.set(Arc::new(engine));
            into_c(ok(json!(null)))
        }
        Err(e) => into_c(fail(e)),
    }
}

/// 通用调用入口：`method` + `args`（JSON 对象，UTF-8 C 字符串）。
/// 返回值必须用 [`kw_free_string`] 释放。
///
/// # Safety
/// 两个指针都必须是以 NUL 结尾的合法 C 字符串，或为空指针（按空参数处理）。
#[no_mangle]
pub extern "C" fn kw_call(method: *const c_char, args: *const c_char) -> *mut c_char {
    let Some(engine) = ENGINE.get() else {
        return into_c(fail("引擎尚未初始化，请先调用 kw_init"));
    };
    if method.is_null() {
        return into_c(fail("method 为空"));
    }
    let method = match unsafe { CStr::from_ptr(method) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return into_c(fail("method 不是合法 UTF-8")),
    };
    let args = if args.is_null() {
        "{}"
    } else {
        match unsafe { CStr::from_ptr(args) }.to_str() {
            Ok(s) => s,
            Err(_) => return into_c(fail("args 不是合法 UTF-8")),
        }
    };

    // C 入参仍归调用方所有；库内不复制整份含凭据的 JSON。
    into_c(engine.call_json(&method, args))
}

/// 释放 [`kw_init`] / [`kw_call`] 返回的字符串。
///
/// # Safety
/// 只能传本库返回过、且尚未释放的指针；传其他指针是未定义行为。
#[no_mangle]
pub extern "C" fn kw_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(ptr));
    }
}

fn into_c(value: Value) -> *mut c_char {
    match CString::new(value.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/* ------------------------------------------------------------------ */
/* 小工具                                                              */
/* ------------------------------------------------------------------ */

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 终端宽高：缺省或 0 交给 `terminal::clamp_*` 兜底，界面不用传满参数。
fn num_field(args: &Value, key: &str) -> u32 {
    args.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
}

fn body_str<'a>(body: &'a Value, key: &str) -> Result<&'a str, Value> {
    arg(body, key).ok_or_else(|| json!({ "error": format!("缺少 {key}") }))
}

fn err_json(e: ApiError) -> Value {
    let mut out = json!({ "error": e.message });
    if let Some(code) = e.code {
        out["code"] = json!(code);
    }
    if let Some(details) = e.details {
        out["details"] = details;
    }
    out
}

fn ok(data: Value) -> Value {
    json!({ "ok": true, "data": data })
}

fn fail(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}

fn fail_value(mut err: Value) -> Value {
    if let Value::Object(map) = &mut err {
        map.insert("ok".into(), Value::Bool(false));
    }
    if err.is_object() {
        err
    } else {
        fail(err.to_string())
    }
}

fn async_call(result: Result<Value, Value>) -> Value {
    match result {
        Ok(v) => ok(v),
        Err(e) => fail_value(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_connect_json_uses_typed_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MobileEngine::new(dir.path()).unwrap();
        let result = engine.call_json("connect", r#"{"password":"private-test-credential"}"#);
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"], "主机地址不能为空");
        assert!(!result.to_string().contains("private-test-credential"));
        let invalid = engine.call_json(
            "connect",
            r#"{"password":{"private-test-credential":true}}"#,
        );
        assert_eq!(invalid["error"], "连接参数格式错误");
        assert!(!invalid.to_string().contains("private-test-credential"));
        assert_eq!(engine.call_json("connect", "")["ok"], false);
        assert_eq!(engine.call_json("bootstrap", "{")["ok"], false);
    }

    #[test]
    fn generate_key_lands_in_keyring_and_is_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MobileEngine::new(dir.path()).unwrap();

        let out = engine
            .generate_key("ed25519", "测试 手机", Some("mobile@test"), None)
            .unwrap();
        let priv_path = out["privateKeyPath"].as_str().unwrap();
        assert!(Path::new(priv_path).is_file());
        assert!(Path::new(&format!("{priv_path}.pub")).is_file());
        assert_eq!(out["encrypted"], json!(false));

        let boot = engine.bootstrap();
        let entries = boot["localKeys"]["entries"].as_array().unwrap();
        assert!(entries.iter().any(|e| e["kind"] == "private"));
        assert!(entries.iter().any(|e| e["kind"] == "public"));
    }

    #[test]
    fn generated_key_preserves_whitespace_in_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MobileEngine::new(dir.path()).unwrap();
        for (index, passphrase) in [" spaced passphrase ", "   "].iter().enumerate() {
            let out = engine.call_json(
                "generateKey",
                &json!({
                    "algorithm": "ed25519",
                    "label": format!("whitespace-{index}"),
                    "passphrase": passphrase,
                })
                .to_string(),
            );
            assert!(out["ok"] == true && out["data"]["encrypted"] == true);
            let pem =
                std::fs::read_to_string(out["data"]["privateKeyPath"].as_str().unwrap()).unwrap();
            assert!(kw_keys::keygen::load_private_key(&pem, Some(passphrase)).is_ok());
            assert!(kw_keys::keygen::load_private_key(&pem, Some(passphrase.trim())).is_err());
            assert!(kw_keys::keygen::load_private_key(&pem, None).is_err());
        }
    }

    #[test]
    fn sync_bundle_roundtrip_plain_and_encrypted() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let a = MobileEngine::new(src.path()).unwrap();
        a.state
            .store
            .upsert_host(HostPatch {
                id: None,
                name: Some("prod".into()),
                host: Some("203.0.113.10".into()),
                port: Some(22),
                username: Some("root".into()),
                identity_file: Some("/不存在的路径".into()),
                identity_fingerprint: None,
                remote_path: None,
            })
            .unwrap();
        a.state
            .store
            .set_key_note(
                "SHA256:9xK2mP7vQ1nL8wR4tY6uI0oP3aS5dF7gH9jK1lZ3xCV",
                "台式机",
            )
            .unwrap();

        let plain = sync::export_bundle(
            &a.state.store,
            &a.state.known_hosts,
            &a.state.keys_dir,
            false,
            None,
        )
        .unwrap();
        let b = MobileEngine::new(dst.path()).unwrap();
        let counts = sync::import_bundle(
            &b.state.store,
            &b.state.known_hosts,
            &b.state.keys_dir,
            &plain,
            None,
            &sync::ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(counts.added_hosts, 1);
        assert_eq!(counts.synced_notes, 1);
        assert_eq!(b.state.store.list_hosts()[0].identity_file, "");

        // 重复导入不叠加
        let again = sync::import_bundle(
            &b.state.store,
            &b.state.known_hosts,
            &b.state.keys_dir,
            &plain,
            None,
            &sync::ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(again.added_hosts, 0);
        assert_eq!(again.synced_notes, 0);
    }

    #[test]
    fn encrypted_bundle_needs_right_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let a = MobileEngine::new(dir.path()).unwrap();
        a.state
            .store
            .upsert_host(HostPatch {
                id: None,
                name: Some("h".into()),
                host: Some("example.com".into()),
                port: Some(2222),
                username: Some("deploy".into()),
                identity_file: None,
                identity_fingerprint: None,
                remote_path: None,
            })
            .unwrap();

        let text = sync::export_bundle(
            &a.state.store,
            &a.state.known_hosts,
            &a.state.keys_dir,
            false,
            Some("correct horse"),
        )
        .unwrap();
        assert!(text.contains("\"encrypted\":true"));
        assert!(!text.contains("example.com"));

        let b = MobileEngine::new(other.path()).unwrap();
        let import = |pass: Option<&str>| {
            sync::import_bundle(
                &b.state.store,
                &b.state.known_hosts,
                &b.state.keys_dir,
                &text,
                pass,
                &sync::ImportOptions::default(),
            )
        };
        assert!(import(None).is_err());
        assert!(import(Some("wrong")).is_err());
        let counts = import(Some("correct horse")).unwrap();
        assert_eq!(counts.added_hosts, 1);
    }

    #[test]
    fn host_crud_through_engine() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MobileEngine::new(dir.path()).unwrap();

        let host = engine
            .save_host(HostPatch {
                id: None,
                name: Some("测试机".into()),
                host: Some("198.51.100.7".into()),
                port: Some(2222),
                username: Some("root".into()),
                identity_file: Some(String::new()),
                identity_fingerprint: None,
                remote_path: None,
            })
            .unwrap();
        let id = host["id"].as_str().unwrap().to_string();
        assert_eq!(host["port"], json!(2222));
        assert_eq!(engine.bootstrap()["hosts"].as_array().unwrap().len(), 1);

        engine.delete_host(&id).unwrap();
        assert_eq!(engine.bootstrap()["hosts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn call_dispatch_wraps_errors() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MobileEngine::new(dir.path()).unwrap();
        let out = engine.call("nope", &json!({}));
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().contains("未知方法"));

        let boot = engine.call("bootstrap", &json!({}));
        assert_eq!(boot["ok"], json!(true));
    }
}
