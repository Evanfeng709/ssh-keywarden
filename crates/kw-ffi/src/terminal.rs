//! 手机端的终端句柄表：`termId` → `kw-ssh` 的缓冲终端。
//!
//! C ABI 没有推送通道，所以终端走「打开拿 id → 轮询取输出」；
//! 界面旋转/分屏要改行列就调 resize。句柄只在本进程内存活，
//! 会话断开或手机侧主动关闭都要把读任务收掉，不留悬挂的 tokio 任务。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use kw_server::error::ApiError;
use kw_server::sessions::Session;
use kw_ssh::terminal::Terminal;

/// 单次 `term_read` 最多等待多久，避免界面每个来回都新建 isolate。
const MAX_WAIT_MS: u64 = 1_000;
const DEFAULT_WAIT_MS: u64 = 250;
const POLL_MS: u64 = 25;

pub struct Terminals {
    inner: Mutex<HashMap<String, Arc<Terminal>>>,
    next_serial: AtomicUsize,
}

impl Default for Terminals {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminals {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next_serial: AtomicUsize::new(1),
        }
    }

    /// 打开 PTY 并登记；宽高走和桌面同一套夹取范围，返回给界面真实生效的行列。
    pub async fn open(
        &self,
        session: &Arc<Session>,
        cols: u32,
        rows: u32,
    ) -> Result<Value, ApiError> {
        let (cols, rows) = (clamp_cols(cols), clamp_rows(rows));
        let terminal = Terminal::open(&session.client, &session.sid, cols, rows)
            .await
            .map_err(ApiError::from)?;
        let id = self.next_id();
        self.inner
            .lock()
            .await
            .insert(id.clone(), Arc::clone(&terminal));
        Ok(json!({
            "termId": id,
            "sid": session.sid,
            "cols": cols,
            "rows": rows,
        }))
    }

    pub async fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        self.inner.lock().await.get(id).cloned()
    }

    /// 按 `termId` 取句柄；取不到说明界面拿的是上一个会话的 id。
    pub async fn require(&self, id: &str) -> Result<Arc<Terminal>, ApiError> {
        self.get(id)
            .await
            .ok_or_else(|| ApiError::not_found("终端已关闭，请重新打开").with_code("SESSION_LOST"))
    }

    pub async fn close(&self, id: &str) -> bool {
        match self.inner.lock().await.remove(id) {
            Some(terminal) => {
                terminal.close().await;
                true
            }
            None => false,
        }
    }

    /// 会话断开时把它名下的终端一起收掉，返回收掉几个。
    pub async fn close_for(&self, sid: &str) -> usize {
        let dead: Vec<Arc<Terminal>> = {
            let mut inner = self.inner.lock().await;
            let ids: Vec<String> = inner
                .iter()
                .filter(|(_, t)| t.sid() == sid)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| inner.remove(id)).collect()
        };
        for terminal in &dead {
            terminal.close().await;
        }
        dead.len()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// 句柄 id 只用于界面回传，与会话凭据无关。
    fn next_id(&self) -> String {
        format!("t{}", self.next_serial.fetch_add(1, Ordering::Relaxed))
    }
}

/// 取输出：有数据立刻返回，否则最多等 `wait_ms`，让 Dart 一次轮询省几个来回。
pub async fn read_json(terminal: &Arc<Terminal>, wait_ms: Option<u64>) -> Value {
    let wait = wait_ms
        .map(|ms| ms.min(MAX_WAIT_MS))
        .unwrap_or(DEFAULT_WAIT_MS);
    let deadline = Instant::now() + Duration::from_millis(wait);
    loop {
        let (bytes, dropped) = terminal.drain().await;
        let closed = terminal.is_closed();
        if !bytes.is_empty() || closed || Instant::now() >= deadline {
            return json!({
                "data": b64(&bytes),
                "dropped": dropped,
                "closed": closed,
            });
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
}

/// 键盘输入 / 粘贴：一次一条，`data` 是 base64。
pub async fn write(terminal: &Arc<Terminal>, data: &str) -> Result<Value, ApiError> {
    terminal
        .input(&from_b64(data)?)
        .await
        .map_err(ApiError::from)?;
    Ok(json!({ "closed": terminal.is_closed() }))
}

pub async fn resize(terminal: &Arc<Terminal>, cols: u32, rows: u32) -> Result<Value, ApiError> {
    let (cols, rows) = (clamp_cols(cols), clamp_rows(rows));
    terminal.resize(cols, rows).await.map_err(ApiError::from)?;
    Ok(json!({ "cols": cols, "rows": rows }))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn from_b64(text: &str) -> Result<Vec<u8>, ApiError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| ApiError::bad("data 不是合法 base64"))
}

pub fn clamp_cols(value: u32) -> u32 {
    clamp(value, 20, 400, 100)
}

pub fn clamp_rows(value: u32) -> u32 {
    clamp(value, 5, 200, 30)
}

fn clamp(value: u32, min: u32, max: u32, fallback: u32) -> u32 {
    if value == 0 {
        fallback
    } else {
        value.clamp(min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_fall_back_then_clamp_like_desktop() {
        assert_eq!(clamp_cols(0), 100);
        assert_eq!(clamp_cols(4), 20);
        assert_eq!(clamp_cols(900), 400);
        assert_eq!(clamp_rows(0), 30);
        assert_eq!(clamp_rows(1), 5);
        assert_eq!(clamp_rows(9999), 200);
    }

    #[test]
    fn base64_roundtrip_rejects_junk() {
        let bytes = from_b64(&b64(b"\x1b[31mred\x1b[0m")).unwrap();
        assert_eq!(bytes, b"\x1b[31mred\x1b[0m");
        assert!(from_b64("!!!not base64!!!").is_err());
    }

    #[tokio::test]
    async fn registry_lookups_are_empty_until_a_channel_exists() {
        let terminals = Terminals::new();
        assert_eq!(terminals.len().await, 0);
        assert!(terminals.get("missing").await.is_none());
        let err = terminals.require("missing").await.err().unwrap();
        assert_eq!(err.code, Some("SESSION_LOST"));
        assert!(!terminals.close("missing").await);
        assert_eq!(terminals.close_for("sid-that-never-existed").await, 0);
    }

    #[tokio::test]
    async fn ids_do_not_collide() {
        let terminals = Terminals::new();
        assert_ne!(terminals.next_id(), terminals.next_id());
    }
}
