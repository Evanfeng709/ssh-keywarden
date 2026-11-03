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
