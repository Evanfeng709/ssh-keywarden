//! 桌面端「微软账号同步」的本地接口。
//!
//! 设备码登录分两步：`login/start` 只把 8 位用户码交给界面，`device_code`
//! 留在服务端内存里轮询换令牌——它不出本机，也不进日志。

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::ms_sync;
use crate::state::AppState;

const DEFAULT_TIMEOUT_SECS: u64 = 180;
const MAX_TIMEOUT_SECS: u64 = 300;
const MIN_TIMEOUT_SECS: u64 = 5;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/sync/status", get(api_status))
        .route("/api/sync/config", post(api_config))
        .route("/api/sync/login/start", post(api_login_start))
        .route("/api/sync/login/complete", post(api_login_complete))
        .route("/api/sync/logout", post(api_logout))
        .route("/api/sync/now", post(api_sync_now))
}

fn ok(data: Value) -> Json<Value> {
    Json(json!({ "ok": true, "data": data }))
}

async fn status_value(state: &AppState) -> Value {
    let dir = state.store.dir().to_path_buf();
    let mut value = json!(ms_sync::status(&dir));
    if let Value::Object(map) = &mut value {
        let pending = state.pending_login.lock().await.is_some();
        map.insert("loginPending".into(), Value::Bool(pending));
    }
    value
}

async fn api_status(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    Ok(ok(status_value(&state).await))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigBody {
    /// 缺省 = 不动；空串 = 清除。
    client_id: Option<String>,
    sync_key: Option<String>,
}

async fn api_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConfigBody>,
) -> Result<Json<Value>, ApiError> {
    let dir = state.store.dir().to_path_buf();
    ms_sync::save_config(&dir, body.client_id.as_deref(), body.sync_key.as_deref())
        .map_err(ApiError::bad)?;
    Ok(ok(status_value(&state).await))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginStartBody {
    client_id: Option<String>,
}

/// 只回用户码；`deviceCode` 留在服务端，避免它出现在 DOM 或剪贴板里。
async fn api_login_start(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginStartBody>,
) -> Result<Json<Value>, ApiError> {
    let dir = state.store.dir().to_path_buf();
    let prompt = ms_sync::start_device_login(&dir, body.client_id.as_deref())
        .await
        .map_err(ApiError::bad)?;
    *state.pending_login.lock().await = Some(prompt.device_code.clone());
    Ok(ok(json!({
        "userCode": prompt.user_code,
        "verificationUri": prompt.verification_uri,
        "expiresIn": prompt.expires_in,
        "interval": prompt.interval,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginCompleteBody {
    timeout_sec: Option<u64>,
}

async fn api_login_complete(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginCompleteBody>,
) -> Result<Json<Value>, ApiError> {
    let code = {
        let mut slot = state.pending_login.lock().await;
        slot.take()
    };
    let Some(device_code) = code else {
        return Err(ApiError::bad("没有待完成的登录，请先点「登录微软账号」"));
    };

    let secs = body
        .timeout_sec
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
    let dir = state.store.dir().to_path_buf();

    match ms_sync::complete_device_login(
        &dir,
        &device_code,
        None,
        Duration::from_secs(secs),
    )
    .await
    {
        Ok(_) => Ok(ok(status_value(&state).await)),
        Err(e) => {
            // 超时不算失败：设备码还有效，用户完成授权后再点一次即可。
            *state.pending_login.lock().await = Some(device_code);
            Err(ApiError::bad(e))
        }
    }
}

async fn api_logout(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let dir = state.store.dir().to_path_buf();
    ms_sync::logout(&dir).map_err(ApiError::bad)?;
    *state.pending_login.lock().await = None;
    Ok(ok(status_value(&state).await))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncBody {
    passphrase: Option<String>,
    remember_key: Option<bool>,
    #[serde(default)]
    trust_changed: Vec<String>,
}

async fn api_sync_now(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SyncBody>,
) -> Result<Json<Value>, ApiError> {
    let dir = state.store.dir().to_path_buf();
    let passphrase = body.passphrase.unwrap_or_default();

    if body.remember_key.unwrap_or(false) && !passphrase.trim().is_empty() {
        ms_sync::save_config(&dir, None, Some(&passphrase)).map_err(ApiError::bad)?;
    }

    let result = ms_sync::sync_now(
        &state.store,
        &state.known_hosts,
        &passphrase,
        &body.trust_changed,
    )
    .await
    .map_err(ApiError::bad)?;

    Ok(ok(json!(result)))
}
