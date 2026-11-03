//! SFTP 文件操作：在已有 SSH 连接上开一条 sftp 子系统通道。
//!
//! `SftpHandle` 不建立连接，只从 `SshClient` 借一条 session 通道；
//! 同一句柄内的操作串行执行，避免改名/删除这类写操作互相插队。

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, FileType};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::session::{SshClient, SshError};

/// 握手超时：服务器接受子系统后迟迟不回 SSH_FXP_VERSION 时别把界面卡死。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// 每次读写的块大小。取消和进度都以它为粒度。
pub const TRANSFER_CHUNK: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum TransferError {
    #[error(transparent)]
    Ssh(#[from] SshError),
    #[error("SFTP 操作失败：{0}")]
    Sftp(#[from] russh_sftp::client::error::Error),
    #[error("本地文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("传输已取消")]
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpKind {
    Dir,
    File,
    Symlink,
    Other,
}

impl SftpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SftpKind::Dir => "dir",
            SftpKind::File => "file",
            SftpKind::Symlink => "symlink",
            SftpKind::Other => "other",
        }
    }

    pub fn is_dir(self) -> bool {
        self == SftpKind::Dir
    }
}

impl From<FileType> for SftpKind {
    fn from(t: FileType) -> Self {
        match t {
            FileType::Dir => SftpKind::Dir,
            FileType::File => SftpKind::File,
            FileType::Symlink => SftpKind::Symlink,
            FileType::Other => SftpKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpEntry {
    pub name: String,
    pub path: String,
    pub kind: SftpKind,
    pub size: u64,
    pub permissions: Option<u32>,
    /// Unix 秒；服务器没给就是 None
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpStat {
    pub kind: SftpKind,
    pub size: u64,
    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<u64>,
    pub mtime: Option<u64>,
}

impl SftpStat {
    fn from_attrs(attrs: FileAttributes) -> Self {
        Self {
            kind: attrs.file_type().into(),
            size: attrs.size.unwrap_or(0),
            permissions: attrs.permissions,
            uid: attrs.uid,
            gid: attrs.gid,
            atime: attrs.atime.map(u64::from),
            mtime: attrs.mtime.map(u64::from),
        }
    }
}

pub struct SftpHandle {
    session: Arc<SftpSession>,
    gate: Mutex<()>,
}

impl SftpHandle {
    /// 在已认证的 SSH 连接上打开 sftp 子系统。
    pub async fn open(client: &SshClient) -> Result<Self, SshError> {
        let channel = client.open_channel().await?;
        channel.request_subsystem(true, "sftp").await?;
        let session =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, SftpSession::new(channel.into_stream()))
                .await
                .map_err(|_| SshError::msg("SFTP 握手超时，服务器可能不支持 SFTP"))??;
        Ok(Self {
            session: Arc::new(session),
            gate: Mutex::new(()),
        })
    }

    /// 列出目录内容：目录在前，其余按名称忽略大小写排序。
    pub async fn list_dir(&self, path: &str) -> Result<Vec<SftpEntry>, SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        let mut entries: Vec<SftpEntry> = self
            .session
            .read_dir(path)
            .await?
            .map(|entry| {
                let attrs = entry.metadata();
                SftpEntry {
                    name: entry.file_name(),
                    path: entry.path(),
                    kind: attrs.file_type().into(),
                    size: attrs.size.unwrap_or(0),
                    permissions: attrs.permissions,
                    mtime: attrs.mtime.map(u64::from),
                }
            })
            .collect();
        entries.sort_by(|a, b| {
            b.kind
                .is_dir()
                .cmp(&a.kind.is_dir())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    /// 跟随符号链接取属性（STAT）。
    pub async fn stat(&self, path: &str) -> Result<SftpStat, SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        Ok(SftpStat::from_attrs(self.session.metadata(path).await?))
    }

    /// 不跟随符号链接（LSTAT）。
    pub async fn lstat(&self, path: &str) -> Result<SftpStat, SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        Ok(SftpStat::from_attrs(
            self.session.symlink_metadata(path).await?,
        ))
    }

    pub async fn exists(&self, path: &str) -> Result<bool, SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        Ok(self.session.try_exists(path).await?)
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        self.session.create_dir(path).await?;
        Ok(())
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        let from = checked(from)?;
        let to = checked(to)?;
        let _guard = self.gate.lock().await;
        self.session.rename(from, to).await?;
        Ok(())
    }

    /// 删除文件或空目录：先 LSTAT 判断类型，符号链接按文件处理。
    pub async fn remove(&self, path: &str) -> Result<(), SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        let attrs = self.session.symlink_metadata(path).await?;
        if attrs.is_dir() {
            self.session.remove_dir(path).await?;
        } else {
            self.session.remove_file(path).await?;
        }
        Ok(())
    }

    /// 只改权限位（0o000–0o7777）；其它属性保持不动。
    pub async fn chmod(&self, path: &str, mode: u32) -> Result<(), SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        let attrs = FileAttributes {
            permissions: Some(mode & 0o7777),
            ..Default::default()
        };
        self.session.set_metadata(path, attrs).await?;
        Ok(())
    }

    pub async fn read_link(&self, path: &str) -> Result<String, SshError> {
        let path = checked(path)?;
        let _guard = self.gate.lock().await;
        Ok(self.session.read_link(path).await?)
    }

    /// 远端 home 目录的绝对路径（REALPATH "."）。
    pub async fn home_dir(&self) -> Result<String, SshError> {
        let _guard = self.gate.lock().await;
        Ok(self.session.canonicalize(".").await?)
    }

    /// 分块下载到本地。先写同目录临时文件，完成后改名覆盖，
    /// 中途取消/失败不会留下半截目标文件。`progress(done, total)` 每块调用一次。
    pub async fn download<P>(
        &self,
        remote: &str,
        local: &Path,
        cancel: &AtomicBool,
        mut progress: P,
    ) -> Result<u64, TransferError>
    where
        P: FnMut(u64, u64) + Send,
    {
        let remote = checked(remote)?;
        if local.as_os_str().is_empty() {
            return Err(SshError::msg("本地保存路径为空").into());
        }
        let total = self.stat(remote).await.map(|stat| stat.size).unwrap_or(0);
        let mut file = {
            let _guard = self.gate.lock().await;
            self.session.open(remote).await?
        };

        let tmp = local_part_path(local);
        let result = async {
            let mut out = std::fs::File::create(&tmp)?;
            let mut buf = vec![0u8; TRANSFER_CHUNK];
            let mut done = 0u64;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(TransferError::Canceled);
                }
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                done += n as u64;
                progress(done, total);
            }
            out.flush()?;
            Ok::<u64, TransferError>(done)
        }
        .await;

        let _ = file.close().await;
        match result {
            Ok(done) => {
                replace_file(&tmp, local)?;
                Ok(done)
            }
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                Err(err)
            }
        }
    }

    /// 分块上传：先写远端 `<path>.kwpart-*`，完成后再改名，覆盖旧文件前不会写坏它。
    /// 取消或失败会尽力删掉临时文件。`progress(done, total)` 每块调用一次。
    pub async fn upload<P>(
        &self,
        local: &Path,
        remote: &str,
        cancel: &AtomicBool,
        mut progress: P,
    ) -> Result<u64, TransferError>
    where
        P: FnMut(u64, u64) + Send,
    {
        let remote = checked(remote)?;
        let mut input = std::fs::File::open(local)?;
        let total = input.metadata()?.len();
        let part = format!("{remote}.kwpart-{}", part_suffix());
        let mut file = {
            let _guard = self.gate.lock().await;
            self.session.create(part.as_str()).await?
        };

        let result = async {
            let mut buf = vec![0u8; TRANSFER_CHUNK];
            let mut done = 0u64;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(TransferError::Canceled);
                }
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).await?;
                done += n as u64;
                progress(done, total);
            }
            file.flush().await?;
            Ok::<u64, TransferError>(done)
        }
        .await;

        let closed = file.close().await;
        match result {
            Ok(done) => {
                if let Err(err) = closed {
                    self.remove_quietly(&part).await;
                    return Err(TransferError::Io(err));
                }
                let renamed = {
                    let _guard = self.gate.lock().await;
                    self.session.rename(part.as_str(), remote).await
                };
                match renamed {
                    Ok(()) => Ok(done),
                    Err(err) => {
                        self.remove_quietly(&part).await;
                        Err(err.into())
                    }
                }
            }
            Err(err) => {
                self.remove_quietly(&part).await;
                Err(err)
            }
        }
    }

    async fn remove_quietly(&self, path: &str) {
        let _guard = self.gate.lock().await;
        let _ = self.session.remove_file(path).await;
    }

    pub async fn close(self) -> Result<(), SshError> {
        let _guard = self.gate.lock().await;
        self.session.close().await?;
        Ok(())
    }
}

fn checked(path: &str) -> Result<&str, SshError> {
    kw_keys::pathutil::assert_safe_path(path).map_err(|e| SshError::msg(e.to_string()))
}

/// 同一进程内时间 + 序号，足够让并发传输的临时文件名互不相同。
fn part_suffix() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{millis:x}{seq:x}")
}

fn local_part_path(local: &Path) -> PathBuf {
    let name = local
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    local.with_file_name(format!(".{name}.kwpart-{}", part_suffix()))
}

/// Windows 上 rename 不能覆盖已存在的目标；Unix 上 rename 本身就是原子替换。
fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    std::fs::rename(tmp, dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_maps_to_wire_names() {
        assert_eq!(SftpKind::Dir.as_str(), "dir");
        assert_eq!(SftpKind::File.as_str(), "file");
        assert!(SftpKind::Symlink.as_str() == "symlink");
        assert!(!SftpKind::File.is_dir());
    }

    #[test]
    fn rejects_empty_or_newline_paths() {
        assert!(checked("").is_err());
        assert!(checked("/a\nb").is_err());
        assert!(checked("/home/a").is_ok());
    }

    #[test]
    fn stat_converts_attributes() {
        let attrs = FileAttributes {
            size: Some(12),
            permissions: Some(0o100640),
            mtime: Some(1_700_000_000),
            ..Default::default()
        };
        let stat = SftpStat::from_attrs(attrs);
        assert_eq!(stat.kind, SftpKind::File);
        assert_eq!(stat.size, 12);
        assert_eq!(stat.permissions, Some(0o100640));
        assert_eq!(stat.mtime, Some(1_700_000_000));
    }
}
