//! HTTP 路由。M1 先落「连接 / 刷新 / 主机 / 断开 / 终端」这几条，
//! 其余接口（密钥增删换、备份、sshd、文件浏览）在 M2 按同一套错误约定补齐。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use kw_ssh::session::PtyShell;
use kw_ssh::ChannelMsg;
use kw_store::{HostPatch, SettingsPatch};

use crate::connect::ConnectBody;
use crate::error::ApiError;
use crate::sessions::Session;
use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/state", get(api_state))
        .route(
            "/api/hosts",
            post(api_hosts_upsert).delete(api_hosts_delete),
        )
        .route("/api/settings", post(api_settings))
        .route("/api/key-notes", post(api_key_notes))
        .route("/api/local-keys", get(api_local_keys))
        .route("/api/connect", post(api_connect))
        .route("/api/refresh", post(api_refresh))
        .route("/api/disconnect", post(api_disconnect))
        .route("/api/fs/list", get(api_fs_list))
        .route("/api/sshd", get(api_sshd))
        .route("/api/sshd/password-auth", post(api_sshd_password_auth))
        .route("/api/backups", get(api_backups))
        .route("/api/backups/diff", get(api_backups_diff))
        .route("/api/backups/restore", post(api_backups_restore))
        .route("/api/add", post(api_add))
        .route("/api/replace", post(api_replace))
        .route("/api/delete", post(api_delete))
        .route("/api/terminal", get(ws_terminal))
        .route("/login", get(login))
        .merge(crate::sync_api::router())
        .fallback(static_or_404)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// 每次启动自动导入 ~/.ssh/config 里的主机（只在主机列表为空时）。
pub fn bootstrap_hosts(state: &AppState) {
    // 用 all_hosts：用户把主机全删了（留下墓碑）时，不能下次启动又从 ~/.ssh/config 导回来
    if !state.store.all_hosts().is_empty() {
        return;
    }
    let home = kw_store::sshconfig::home_dir();
    let file = home.join(".ssh").join("config");
    for host in kw_store::sshconfig::import_from_ssh_config(&file) {
        let _ = state.store.upsert_host(HostPatch {
            id: None,
            name: Some(host.name),
            host: Some(host.host),
            port: Some(host.port),
            username: Some(host.username),
            identity_file: Some(host.identity_file),
            identity_fingerprint: None,
            remote_path: None,
        });
    }
}

/* ------------------------------------------------------------------ */
/* 鉴权 / Host 校验                                                     */
/* ------------------------------------------------------------------ */

async fn guard(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !host_allowed(req.headers(), state.port) {
        return Err(ApiError {
            status: StatusCode::MISDIRECTED_REQUEST,
            message: "拒绝访问：Host 头不合法（本工具只绑定 127.0.0.1）".into(),
            code: None,
            details: None,
        });
    }
    if req.uri().path().starts_with("/api/") && !token_valid(req.headers(), &state.token) {
        return Err(ApiError::unauthorized());
    }
    let is_login = req.uri().path() == "/login";
    let mut response = next.run(req).await;
    if is_login {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response.headers_mut().insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
    }
    Ok(response)
}

fn host_allowed(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true; // HTTP/1.0 或无 Host：本地环回直接放行
    };
    let host = host.to_ascii_lowercase();
    host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == "127.0.0.1"
        || host == "localhost"
}

fn token_valid(headers: &HeaderMap, token: &str) -> bool {
    let header_token = headers
        .get("x-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| constant_eq(v.as_bytes(), token.as_bytes()));
    if header_token {
        return true;
    }
    let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    cookie.split(';').any(|pair| {
        let Some((name, value)) = pair.split_once('=') else {
            return false;
        };
        name.trim() == "kw" && constant_eq(value.trim().as_bytes(), token.as_bytes())
    })
}

/// 长度先比、再逐字节异或，避免按字节提前返回
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/* ------------------------------------------------------------------ */
/* 连接 / 刷新 / 断开                                                   */
/* ------------------------------------------------------------------ */

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidBody {
    sid: String,
}

async fn api_connect(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConnectBody>,
) -> Result<Json<Value>, ApiError> {
    let data = crate::connect::open(&state, body).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_refresh(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SidBody>,
) -> Result<Json<Value>, ApiError> {
    let data = crate::connect::refresh(&state, &body.sid).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_disconnect(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SidBody>,
) -> Result<Json<Value>, ApiError> {
    let data = crate::connect::disconnect(&state, &body.sid).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

/* ------------------------------------------------------------------ */
/* 主机 / 设置 / 备注                                                   */
/* ------------------------------------------------------------------ */

async fn api_hosts_upsert(
    State(state): State<Arc<AppState>>,
    Json(body): Json<HostPatch>,
) -> Result<Json<Value>, ApiError> {
    let host = state.store.upsert_host(body)?;
    Ok(Json(json!({ "ok": true, "data": host })))
}

#[derive(Debug, Deserialize)]
struct IdQuery {
    id: String,
}

#[derive(Debug, Deserialize, Default)]
struct DirQuery {
    dir: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SidQuery {
    sid: Option<String>,
}

async fn api_hosts_delete(
    State(state): State<Arc<AppState>>,
    Query(q): Query<IdQuery>,
) -> Result<Json<Value>, ApiError> {
    state.store.remove_host(&q.id)?;
    Ok(Json(json!({ "ok": true, "data": true })))
}

async fn api_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 只接受字符串字段并 trim，与旧版一致
    let pick = |key: &str| {
        body.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
    };
    let settings = state.store.update_settings(SettingsPatch {
        local_key_dir: pick("localKeyDir"),
        save_dir: pick("saveDir"),
        remote_path: pick("remotePath"),
    })?;
    Ok(Json(json!({ "ok": true, "data": settings })))
}

async fn api_key_notes(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let fingerprint = body
        .get("fingerprint")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let note = body
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let notes = state.store.set_key_note(fingerprint, note)?;
    Ok(Json(json!({ "ok": true, "data": { "keyNotes": notes } })))
}

#[derive(Debug, Deserialize, Default)]
struct LocalKeysQuery {
    dir: Option<String>,
    force: Option<String>,
}

async fn api_local_keys(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LocalKeysQuery>,
) -> Result<Json<Value>, ApiError> {
    let dir = q
        .dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| state.current_settings().local_key_dir);
    if dir.trim().is_empty() {
        return Ok(Json(json!({
            "ok": true,
            "data": { "dir": "", "entries": [], "errors": [] }
        })));
    }
    let scan = state
        .scan_local(&dir, q.force.as_deref() == Some("1"))
        .await;
    Ok(Json(json!({
        "ok": true,
        "data": { "dir": dir, "entries": scan.entries, "errors": scan.errors }
    })))
}

async fn api_state(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let settings = state.current_settings();
    let scan = if settings.local_key_dir.is_empty() {
        kw_keys::localkeys::ScanResult::default()
    } else {
        state.scan_local(&settings.local_key_dir, false).await
    };

    let mut sessions = Vec::new();
    for s in state.sessions.list().await {
        let target = s.target.read().await.clone();
        let remote_path = s.remote.read().await.remote_path.clone();
        sessions.push(json!({ "sid": s.sid, "host": target.host, "remotePath": remote_path }));
    }

    Ok(Json(json!({
        "ok": true,
        "data": {
            "version": state.version,
            "hosts": state.store.list_hosts(),
            "settings": settings,
            "keyNotes": state.store.list_key_notes(),
            "localKeys": {
                "dir": settings.local_key_dir,
                "entries": scan.entries,
                "errors": scan.errors,
            },
            "sessions": sessions,
        }
    })))
}

/* ------------------------------------------------------------------ */
/* 本地文件浏览 / sshd / 备份 / 密钥增删换                               */
/* ------------------------------------------------------------------ */

async fn key_session(state: &AppState, sid: &str) -> Result<Arc<Session>, ApiError> {
    let session = crate::connect::session_by_sid(state, sid).await?;
    if !session.is_key_session().await {
        return Err(ApiError::bad("请先切换到密钥管理并读取服务器密钥"));
    }
    Ok(session)
}

async fn api_fs_list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DirQuery>,
) -> Result<Json<Value>, ApiError> {
    let dir = q.dir.unwrap_or_default();
    let mut data = crate::fsbrowse::list_local_dir(&dir).map_err(|e| ApiError::bad(e.0))?;

    let settings = state.current_settings();
    let home = crate::fsbrowse::home_dir();
    let mut shortcuts = vec![
        json!({ "label": "主目录", "path": home.display().to_string() }),
        json!({ "label": ".ssh", "path": home.join(".ssh").display().to_string() }),
    ];
    if !settings.local_key_dir.is_empty() {
        shortcuts.push(json!({ "label": "密钥库", "path": settings.local_key_dir }));
    }
    if !settings.save_dir.is_empty() {
        shortcuts.push(json!({ "label": "保存目录", "path": settings.save_dir }));
    }
    shortcuts.retain(|s| {
        s["path"]
            .as_str()
            .is_some_and(|p| std::path::Path::new(p).exists())
    });
    data["shortcuts"] = json!(shortcuts);
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_sshd(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SidQuery>,
) -> Result<Json<Value>, ApiError> {
    let session = key_session(&state, &q.sid.unwrap_or_default()).await?;
    let status = kw_ssh::sshd::inspect(&session.client).await?;
    Ok(Json(json!({ "ok": true, "data": status })))
}

async fn api_sshd_password_auth(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let sid = body.get("sid").and_then(|v| v.as_str()).unwrap_or_default();
    let session = key_session(&state, sid).await?;
    // 和密钥写入共用同一把锁：改 sshd 期间不允许 add/delete 穿插
    let _write = session.write_lock.lock().await;
    let enable = body
        .get("enable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let data = crate::keys_ops::set_key_only(&session, enable).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_backups(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SidQuery>,
) -> Result<Json<Value>, ApiError> {
    let session = key_session(&state, &q.sid.unwrap_or_default()).await?;
    let data = crate::keys_ops::list_backups(&session).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

#[derive(Debug, Deserialize, Default)]
struct BackupDiffQuery {
    sid: Option<String>,
    name: Option<String>,
}

async fn api_backups_diff(
    State(state): State<Arc<AppState>>,
    Query(q): Query<BackupDiffQuery>,
) -> Result<Json<Value>, ApiError> {
    let session = key_session(&state, q.sid.as_deref().unwrap_or_default()).await?;
    let data = crate::keys_ops::backup_diff(&session, &q.name.unwrap_or_default()).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_backups_restore(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let sid = body.get("sid").and_then(|v| v.as_str()).unwrap_or_default();
    let session = key_session(&state, sid).await?;
    let _write = session.write_lock.lock().await;
    let data = crate::keys_ops::restore_backup(&session, &body).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_add(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let sid = body.get("sid").and_then(|v| v.as_str()).unwrap_or_default();
    let session = key_session(&state, sid).await?;
    let _write = session.write_lock.lock().await;
    let data = crate::keys_ops::add_key(&state, &session, &body).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_replace(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let sid = body.get("sid").and_then(|v| v.as_str()).unwrap_or_default();
    let session = key_session(&state, sid).await?;
    let _write = session.write_lock.lock().await;
    let data = crate::keys_ops::replace_key(&state, &session, &body).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

async fn api_delete(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let sid = body.get("sid").and_then(|v| v.as_str()).unwrap_or_default();
    let session = key_session(&state, sid).await?;
    let _write = session.write_lock.lock().await;
    let data = crate::keys_ops::delete_keys(&session, &body).await?;
    Ok(Json(json!({ "ok": true, "data": data })))
}

/* ------------------------------------------------------------------ */
/* 终端（WebSocket ⇄ PTY）                                              */
/* ------------------------------------------------------------------ */

#[derive(Debug, Deserialize, Default)]
struct TerminalQuery {
    sid: Option<String>,
    cols: Option<String>,
    rows: Option<String>,
}

fn clamp_dim(raw: Option<&String>, min: u32, max: u32, dft: u32) -> u32 {
    raw.and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(dft)
        .clamp(min, max)
}

async fn ws_terminal(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let sid = q.sid.unwrap_or_default();
    let cols = clamp_dim(q.cols.as_ref(), 20, 400, 100);
    let rows = clamp_dim(q.rows.as_ref(), 5, 200, 30);
    ws.on_upgrade(move |socket| handle_terminal(socket, state, sid, cols, rows))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn handle_terminal(
    socket: WebSocket,
    state: Arc<AppState>,
    sid: String,
    cols: u32,
    rows: u32,
) {
    let Some(session) = state.sessions.must(&sid).await else {
        let mut socket = socket;
        let _ = socket
            .send(Message::Text(
                json!({ "t": "error", "message": "会话已失效，请重新连接服务器" })
                    .to_string()
                    .into(),
            ))
            .await;
        let _ = socket.close().await;
        return;
    };

    let shell = match PtyShell::open(&session.client, "xterm-256color", cols, rows).await {
        Ok(shell) => shell,
        Err(err) => {
            let mut socket = socket;
            let _ = socket
                .send(Message::Text(
                    json!({
                        "t": "error",
                        "message": kw_ssh::util::friendly_error(
                            &err.to_string(),
                            &session.target.read().await.host,
                            session.client.port,
                            false
                        )
                    })
                    .to_string()
                    .into(),
                ))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    let (mut read_half, write_half) = shell.into_parts();
    let (mut sink, mut stream) = socket.split();
    let host_label = session.target.read().await.host.clone();

    // 输出方向：PTY → WebSocket。统一 base64，避免多字节 UTF-8 被分帧切坏。
    let out = tokio::spawn(async move {
        loop {
            match read_half.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    let msg = json!({ "t": "out", "d": b64(&data) }).to_string();
                    if sink.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::ExtendedData { data, ext }) if ext == 1 => {
                    let msg = json!({ "t": "out", "d": b64(&data) }).to_string();
                    if sink.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::ExitStatus { .. }) => {}
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    let _ = sink
                        .send(Message::Text(json!({ "t": "exit" }).to_string().into()))
                        .await;
                    break;
                }
                Some(_) => {}
            }
        }
        let _ = sink.close().await;
    });

    // 输入方向：WebSocket → PTY
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        session.touch();
        match value.get("t").and_then(|v| v.as_str()) {
            Some("in") => {
                if let Some(data) = value.get("d").and_then(|v| v.as_str()) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                        let _ = write_half.data_bytes(bytes).await;
                    }
                }
            }
            Some("resize") => {
                let cols = value.get("cols").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let rows = value.get("rows").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if cols > 0 && rows > 0 {
                    let _ = write_half.window_change(cols, rows, 0, 0).await;
                }
            }
            _ => {}
        }
    }

    let _ = write_half.eof().await;
    let _ = write_half.close().await;
    out.abort();
    eprintln!("[终端] 已关闭：{host_label}");
}

/* ------------------------------------------------------------------ */
/* 静态资源                                                             */
/* ------------------------------------------------------------------ */

const MIME_HTML: &str = "text/html; charset=utf-8";
const MIME_JS: &str = "text/javascript; charset=utf-8";
const MIME_CSS: &str = "text/css; charset=utf-8";
const MIME_SVG: &str = "image/svg+xml";
const MIME_ICO: &str = "image/x-icon";
const MIME_BIN: &str = "application/octet-stream";

fn static_allowed(path: &str) -> bool {
    if path == "/index.html" || path == "/style.css" {
        return true;
    }
    if let Some(name) = path.strip_prefix("/js/") {
        return is_plain_name(name) && name.ends_with(".js");
    }
    if let Some(name) = path.strip_prefix("/vendor/") {
        return is_plain_name(name) && (name.ends_with(".js") || name.ends_with(".css"));
    }
    false
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn mime_of(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("html") => MIME_HTML,
        Some("js") => MIME_JS,
        Some("css") => MIME_CSS,
        Some("svg") => MIME_SVG,
        Some("ico") => MIME_ICO,
        _ => MIME_BIN,
    }
}

/* ------------------------------------------------------------------ */
/* 未匹配：/api 走 JSON，其余走静态资源 404                              */
/* ------------------------------------------------------------------ */

#[derive(Deserialize)]
struct LoginQuery {
    token: String,
}

async fn login(State(state): State<Arc<AppState>>, Query(q): Query<LoginQuery>) -> Response {
    if !constant_eq(q.token.as_bytes(), state.token.as_bytes()) {
        return (
            StatusCode::UNAUTHORIZED,
            "登录令牌无效，请使用本次启动生成的登录文件",
        )
            .into_response();
    }
    let Ok(cookie) = HeaderValue::from_str(&format!(
        "kw={}; Path=/; HttpOnly; SameSite=Strict",
        state.token
    )) else {
        return (StatusCode::UNAUTHORIZED, "登录令牌无效").into_response();
    };
    (
        StatusCode::SEE_OTHER,
        [
            (header::SET_COOKIE, cookie),
            (header::LOCATION, HeaderValue::from_static("/")),
        ],
    )
        .into_response()
}

async fn static_or_404(
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Response {
    let mut path = uri.path().to_string();
    if path.starts_with("/api/") {
        return ApiError::not_found(format!("未知接口：{method} {path}")).into_response();
    }
    if path == "/" {
        path = "/index.html".to_string();
    }
    if !static_allowed(&path) {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    let file = state.public_dir.join(path.trim_start_matches('/'));
    let Ok(bytes) = std::fs::read(&file) else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    let mut res = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime_of(&path)),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response();
    if path == "/index.html" && state.auto_cookie {
        if let Ok(cookie) = HeaderValue::from_str(&format!(
            "kw={}; Path=/; HttpOnly; SameSite=Strict",
            state.token
        )) {
            res.headers_mut().insert(header::SET_COOKIE, cookie);
        }
    }
    res
}
