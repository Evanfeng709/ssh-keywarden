//! SFTP 业务层：目录浏览、文件操作，以及带进度/取消的异步传输。
//!
//! 传输任务在后台跑，调用方拿 `id` 轮询 `transfer()`；取消只是置位标志，
//! 真正的清理（删临时文件）由 `kw-ssh` 在块边界完成。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use kw_ssh::sftp::{SftpEntry, SftpHandle, SftpStat, TransferError};

use crate::error::ApiError;
use crate::sessions::{now_ms, Session};
use crate::state::AppState;

/// 已完成任务最多留这么多条，避免常驻内存越跑越大。
const MAX_TRANSFERS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Upload => "upload",
            Direction::Download => "download",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Running,
    Done,
    Failed(String),
    Canceled,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed(_) => "failed",
            Status::Canceled => "canceled",
        }
    }
}

pub struct Transfer {
    pub id: String,
    pub sid: String,
    pub direction: Direction,
    pub remote_path: String,
    pub local_path: String,
    total: AtomicU64,
    done: AtomicU64,
    cancel: AtomicBool,
    finished: AtomicBool,
    status: Mutex<Status>,
    started_at: u64,
    finished_at: AtomicU64,
}

impl Transfer {
    pub async fn snapshot(&self) -> Value {
        let status = self.status.lock().await;
        json!({
            "id": self.id,
            "sid": self.sid,
            "direction": self.direction.as_str(),
            "remotePath": self.remote_path,
            "localPath": self.local_path,
            "total": self.total.load(Ordering::Relaxed),
            "done": self.done.load(Ordering::Relaxed),
            "status": status.as_str(),
            "error": match &*status {
                Status::Failed(msg) => msg.clone(),
                _ => String::new(),
            },
            "startedAt": self.started_at,
            "finishedAt": self.finished_at.load(Ordering::Relaxed),
        })
    }

    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
pub struct Transfers {
    inner: Mutex<Registry>,
    seq: AtomicU64,
}

#[derive(Default)]
struct Registry {
    items: HashMap<String, Arc<Transfer>>,
    order: VecDeque<String>,
}

impl Transfers {
    pub async fn start(
        &self,
        sid: &str,
        direction: Direction,
        remote: &str,
        local: &str,
        total: u64,
    ) -> Arc<Transfer> {
        let id = format!("t{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let transfer = Arc::new(Transfer {
            id: id.clone(),
            sid: sid.to_string(),
            direction,
            remote_path: remote.to_string(),
            local_path: local.to_string(),
            total: AtomicU64::new(total),
            done: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            status: Mutex::new(Status::Running),
            started_at: now_ms(),
            finished_at: AtomicU64::new(0),
        });
        let mut registry = self.inner.lock().await;
        registry.items.insert(id.clone(), Arc::clone(&transfer));
        registry.order.push_back(id);
        prune(&mut registry);
        transfer
    }

    pub async fn get(&self, id: &str) -> Option<Arc<Transfer>> {
        self.inner.lock().await.items.get(id).cloned()
    }

    /// 新的在前。
    pub async fn list(&self) -> Vec<Arc<Transfer>> {
        let registry = self.inner.lock().await;
        registry
            .order
            .iter()
            .rev()
            .filter_map(|id| registry.items.get(id).cloned())
            .collect()
    }
}

/// 从最老的开始丢，只丢已完成的任务；最老的还在跑就停手。
fn prune(registry: &mut Registry) {
    while registry.order.len() > MAX_TRANSFERS {
        let Some(id) = registry.order.front().cloned() else {
            break;
        };
        let finished = registry
            .items
            .get(&id)
            .map(|t| !t.is_running())
            .unwrap_or(true);
        if !finished {
            break;
        }
        registry.order.pop_front();
        registry.items.remove(&id);
    }
}

async fn finish(transfer: &Transfer, result: Result<u64, TransferError>) {
    let mut status = transfer.status.lock().await;
    match result {
        Ok(done) => {
            transfer.done.store(done, Ordering::Relaxed);
            if transfer.total.load(Ordering::Relaxed) == 0 {
                transfer.total.store(done, Ordering::Relaxed);
            }
            *status = Status::Done;
        }
        Err(TransferError::Canceled) => *status = Status::Canceled,
        Err(err) => *status = Status::Failed(err.to_string()),
    }
    transfer.finished.store(true, Ordering::Relaxed);
    transfer.finished_at.store(now_ms(), Ordering::Relaxed);
}

/* ------------------------------------------------------------------ */
/* 目录 / 文件操作                                                      */
/* ------------------------------------------------------------------ */

async fn handle(state: &AppState, sid: &str) -> Result<Arc<SftpHandle>, ApiError> {
    let session = crate::connect::session_by_sid(state, sid).await?;
    Ok(session.sftp().await?)
}

fn entry_json(entry: &SftpEntry) -> Value {
    json!({
        "name": entry.name,
        "path": entry.path,
        "kind": entry.kind.as_str(),
        "size": entry.size,
        "permissions": entry.permissions,
        "mtime": entry.mtime,
    })
}

fn stat_json(stat: &SftpStat) -> Value {
    json!({
        "kind": stat.kind.as_str(),
        "size": stat.size,
        "permissions": stat.permissions,
        "uid": stat.uid,
        "gid": stat.gid,
        "atime": stat.atime,
        "mtime": stat.mtime,
    })
}

/// 远端 home 的绝对路径（REALPATH "."），用作文件浏览的初始目录。
pub async fn home_dir(state: &AppState, sid: &str) -> Result<Value, ApiError> {
    let sftp = handle(state, sid).await?;
    Ok(json!({ "path": sftp.home_dir().await? }))
}

pub async fn list_dir(state: &AppState, sid: &str, path: &str) -> Result<Value, ApiError> {
    let sftp = handle(state, sid).await?;
    let entries = sftp.list_dir(path).await?;
    Ok(json!({
        "path": path,
        "entries": entries.iter().map(entry_json).collect::<Vec<_>>(),
    }))
}

pub async fn stat(state: &AppState, sid: &str, path: &str) -> Result<Value, ApiError> {
    let sftp = handle(state, sid).await?;
    Ok(stat_json(&sftp.stat(path).await?))
}

pub async fn mkdir(state: &AppState, sid: &str, path: &str) -> Result<Value, ApiError> {
    let sftp = handle(state, sid).await?;
    sftp.mkdir(path).await?;
    Ok(json!({ "path": path }))
}

pub async fn rename(state: &AppState, sid: &str, from: &str, to: &str) -> Result<Value, ApiError> {
    let sftp = handle(state, sid).await?;
    sftp.rename(from, to).await?;
    Ok(json!({ "from": from, "to": to }))
}

pub async fn remove(state: &AppState, sid: &str, path: &str) -> Result<Value, ApiError> {
    let sftp = handle(state, sid).await?;
    sftp.remove(path).await?;
    Ok(json!({ "path": path }))
}

pub async fn chmod(state: &AppState, sid: &str, path: &str, mode: u32) -> Result<Value, ApiError> {
    if mode > 0o7777 {
        return Err(ApiError::bad("权限值必须是 0–7777 的八进制"));
    }
    let sftp = handle(state, sid).await?;
    sftp.chmod(path, mode).await?;
    Ok(json!({ "path": path, "mode": format!("{mode:o}") }))
}

/* ------------------------------------------------------------------ */
/* 传输任务                                                            */
/* ------------------------------------------------------------------ */

async fn checked_session(
    state: &AppState,
    sid: &str,
) -> Result<(Arc<Session>, Arc<SftpHandle>), ApiError> {
    let session = crate::connect::session_by_sid(state, sid).await?;
    let sftp = session.sftp().await?;
    Ok((session, sftp))
}

fn require_text<'a>(value: &'a str, label: &str) -> Result<&'a str, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::bad(format!("{label}不能为空")));
    }
    Ok(value)
}

fn local_parent_missing(local: &Path) -> bool {
    match local.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => !parent.is_dir(),
        _ => false,
    }
}

pub async fn start_download(
    state: &Arc<AppState>,
    sid: &str,
    remote: &str,
    local: &str,
) -> Result<Value, ApiError> {
    let remote = require_text(remote, "远端路径")?;
    let local = require_text(local, "本地路径")?;
    let local_path = PathBuf::from(local);
    if local_path.is_dir() {
        return Err(ApiError::bad("本地路径是一个目录，请指定文件名"));
    }
    if local_parent_missing(&local_path) {
        return Err(ApiError::bad(format!(
            "本地目录不存在：{}",
            local_path.parent().unwrap_or(Path::new("")).display()
        )));
    }

    let (session, sftp) = checked_session(state, sid).await?;
    let stat = sftp.stat(remote).await?;
    if stat.kind.is_dir() {
        return Err(ApiError::bad("远端路径是目录，请指定文件名"));
    }

    let transfer = state
        .transfers
        .start(sid, Direction::Download, remote, local, stat.size)
        .await;
    let task_transfer = Arc::clone(&transfer);
    let progress_transfer = Arc::clone(&transfer);
    let remote = remote.to_string();
    tokio::spawn(async move {
        let result = sftp
            .download(
                &remote,
                &local_path,
                &task_transfer.cancel,
                move |done, total| {
                    progress_transfer.done.store(done, Ordering::Relaxed);
                    if total > 0 {
                        progress_transfer.total.store(total, Ordering::Relaxed);
                    }
                },
            )
            .await;
        finish(&task_transfer, result).await;
        drop(session);
    });
    Ok(transfer.snapshot().await)
}

pub async fn start_upload(
    state: &Arc<AppState>,
    sid: &str,
    local: &str,
    remote: &str,
) -> Result<Value, ApiError> {
    let local = require_text(local, "本地路径")?;
    let remote = require_text(remote, "远端路径")?;
    let local_path = PathBuf::from(local);
    let meta = std::fs::metadata(&local_path)
        .map_err(|e| ApiError::bad(format!("读取本地文件失败：{e}")))?;
    if !meta.is_file() {
        return Err(ApiError::bad("本地路径不是文件"));
    }

    let (session, sftp) = checked_session(state, sid).await?;
    let parent = kw_keys::pathutil::posix_dirname(remote);
    if parent != "." && parent != "/" && !sftp.exists(parent).await? {
        return Err(ApiError::bad(format!("远端目录不存在：{parent}")));
    }
    if let Ok(stat) = sftp.stat(remote).await {
        if stat.kind.is_dir() {
            return Err(ApiError::bad("远端同名路径是目录，请换一个文件名"));
        }
    }

    let transfer = state
        .transfers
        .start(sid, Direction::Upload, remote, local, meta.len())
        .await;
    let task_transfer = Arc::clone(&transfer);
    let progress_transfer = Arc::clone(&transfer);
    let remote = remote.to_string();
    tokio::spawn(async move {
        let result = sftp
            .upload(
                &local_path,
                &remote,
                &task_transfer.cancel,
                move |done, total| {
                    progress_transfer.done.store(done, Ordering::Relaxed);
                    if total > 0 {
                        progress_transfer.total.store(total, Ordering::Relaxed);
                    }
                },
            )
            .await;
        finish(&task_transfer, result).await;
        drop(session);
    });
    Ok(transfer.snapshot().await)
}

pub async fn list_transfers(state: &AppState) -> Value {
    let mut items = Vec::new();
    for transfer in state.transfers.list().await {
        items.push(transfer.snapshot().await);
    }
    json!({ "items": items })
}

pub async fn get_transfer(state: &AppState, id: &str) -> Result<Value, ApiError> {
    let transfer = state
        .transfers
        .get(id)
        .await
        .ok_or_else(|| ApiError::not_found("传输任务不存在或已过期"))?;
    Ok(transfer.snapshot().await)
}

pub async fn cancel_transfer(state: &AppState, id: &str) -> Result<Value, ApiError> {
    let transfer = state
        .transfers
        .get(id)
        .await
        .ok_or_else(|| ApiError::not_found("传输任务不存在或已过期"))?;
    if transfer.is_running() {
        transfer.cancel.store(true, Ordering::Relaxed);
    }
    Ok(transfer.snapshot().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transfer_lifecycle_and_cancel() {
        let transfers = Transfers::default();
        let transfer = transfers
            .start("sid-1", Direction::Upload, "/tmp/a", "C:/a", 100)
            .await;

        let snapshot = transfer.snapshot().await;
        assert_eq!(snapshot["status"], "running");
        assert_eq!(snapshot["direction"], "upload");
        assert_eq!(snapshot["total"], 100);
        assert_eq!(snapshot["done"], 0);

        transfer.done.store(40, Ordering::Relaxed);
        transfer.cancel.store(true, Ordering::Relaxed);
        finish(&transfer, Err(TransferError::Canceled)).await;

        let snapshot = transfers.get(&transfer.id).await.unwrap().snapshot().await;
        assert_eq!(snapshot["status"], "canceled");
        assert_eq!(snapshot["done"], 40);
        assert_eq!(snapshot["error"], "");
        assert!(snapshot["finishedAt"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn successful_finish_fills_unknown_total() {
        let transfers = Transfers::default();
        let transfer = transfers
            .start("sid-1", Direction::Download, "/a", "C:/a", 0)
            .await;
        finish(&transfer, Ok(1234)).await;
        let snapshot = transfer.snapshot().await;
        assert_eq!(snapshot["status"], "done");
        assert_eq!(snapshot["done"], 1234);
        assert_eq!(snapshot["total"], 1234);
    }

    #[tokio::test]
    async fn prune_keeps_recent_transfers_and_newest_first() {
        let transfers = Transfers::default();
        let mut first = None;
        for i in 0..(MAX_TRANSFERS + 5) {
            let transfer = transfers
                .start("sid", Direction::Upload, "/a", "C:/a", i as u64)
                .await;
            if i == 0 {
                first = Some(transfer.clone());
            }
            finish(&transfer, Ok(0)).await;
        }
        let list = transfers.list().await;
        assert_eq!(list.len(), MAX_TRANSFERS);
        assert!(first.is_some_and(|t| !list.iter().any(|item| item.id == t.id)));
        assert_eq!(list[0].id, format!("t{}", MAX_TRANSFERS + 5));
    }
}
