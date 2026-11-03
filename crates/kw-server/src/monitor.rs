//! 状态监控：从当前 SSH 会话抓一份只读快照。
//!
//! 采集逻辑在 `kw-ssh::metrics`，这里只负责会话校验与错误映射。

use serde_json::Value;

use crate::error::ApiError;
use crate::state::AppState;

pub async fn snapshot(state: &AppState, sid: &str) -> Result<Value, ApiError> {
    let session = state
        .sessions
        .must(sid)
        .await
        .ok_or_else(ApiError::session_lost)?;
    let snap = kw_ssh::metrics::collect(&session.client).await?;
    serde_json::to_value(snap).map_err(|e| ApiError::bad(format!("监控数据序列化失败：{e}")))
}

/// 量一次 SSH 往返：`exec("true")` 走过去再回来所花的时间。
///
/// 这是信息条上那个「31ms」的来源。刻意选了 `true` 这种几乎零开销的命令 ——
/// 量的是链路往返，不是远端执行耗时。
pub async fn ping(state: &AppState, sid: &str) -> Result<Value, ApiError> {
    let session = state
        .sessions
        .must(sid)
        .await
        .ok_or_else(ApiError::session_lost)?;
    let started = std::time::Instant::now();
    session
        .client
        .exec("true", None)
        .await
        .map_err(|e| ApiError::bad(format!("延迟探测失败：{e}")))?;
    Ok(serde_json::json!({
        "ms": started.elapsed().as_millis() as u64,
    }))
}
