//! 内存会话表：sid → SSH 连接 + 远端状态。
//!
//! 旧版把会话放 `Map`，同一个会话的写操作用 Promise 链串行。
//! 这里换成 `tokio::sync::Mutex<()>`：写流程 `lock().await` 拿到的守卫干完活才放行下一个。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};

use kw_keys::{fingerprint_of_blob, load_private_key, parse_authorized_keys};
use kw_ssh::session::{ServerProbe, SshClient};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub identity_file: String,
    pub remote_path: String,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub home: String,
    pub remote_path: String,
    pub file_exists: bool,
    pub keys_loaded: bool,
}

pub struct Session {
    pub sid: String,
    pub client: SshClient,
    pub target: RwLock<Target>,
    pub remote: RwLock<RemoteState>,
    pub server: ServerProbe,
    pub auth_method: &'static str,
    /// 更换密钥后要跟着改，所以放在锁里
    pub used_key_fp: RwLock<String>,
    pub created_at: u64,
    last_used: AtomicU64,
    /// 同一会话的写操作排队：add/replace/delete/restore 都是读-改-写多步流程
    pub write_lock: Mutex<()>,
}

impl Session {
    pub fn new(
        sid: String,
        client: SshClient,
        target: Target,
        remote: RemoteState,
        server: ServerProbe,
        auth_method: &'static str,
        used_key_fp: String,
    ) -> Self {
        let now = now_ms();
        Self {
            sid,
            client,
            target: RwLock::new(target),
            remote: RwLock::new(remote),
            server,
            auth_method,
            used_key_fp: RwLock::new(used_key_fp),
            created_at: now,
            last_used: AtomicU64::new(now),
            write_lock: Mutex::new(()),
        }
    }

    pub fn touch(&self) {
        self.last_used.store(now_ms(), Ordering::Relaxed);
    }

    pub fn idle_for(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.last_used.load(Ordering::Relaxed)))
    }

    pub async fn remote_path(&self) -> String {
        self.remote.read().await.remote_path.clone()
    }

    pub async fn is_key_session(&self) -> bool {
        self.remote.read().await.keys_loaded
    }

    /// 前端要的会话视图；字段名与旧版 `sessionPayload` 一一对应。
    pub async fn payload(&self, raw: &str) -> Value {
        let target = self.target.read().await.clone();
        let remote = self.remote.read().await.clone();
        let parsed = parse_authorized_keys(raw);

        let items: Vec<Value> = parsed
            .items
            .iter()
            .filter(|i| i.kind() != kw_keys::ItemKind::Blank)
            .map(|i| {
                let key = i.key();
                json!({
                    "index": i.index,
                    "kind": i.kind().as_str(),
                    "type": key.map(|k| k.key_type.clone()).unwrap_or_default(),
                    "fingerprint": key.map(|k| k.fingerprint.clone()).unwrap_or_default(),
                    "comment": key.map(|k| k.comment.clone()).unwrap_or_default(),
                    "options": key.map(|k| k.options.clone()).unwrap_or_default(),
                    "b64": key.map(|k| k.b64.clone()).unwrap_or_default(),
                    "duplicateOf": match &i.data {
                        kw_keys::ItemData::Key { duplicate_of, .. } => duplicate_of
                            .as_ref()
                            .copied()
                            .map(Value::from)
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    },
                    "reason": match &i.data {
                        kw_keys::ItemData::Malformed { reason } => reason.clone(),
                        _ => String::new(),
                    },
                    "text": match &i.data {
                        kw_keys::ItemData::Comment { text } => text.clone(),
                        _ => String::new(),
                    },
                })
            })
            .collect();

        json!({
            "sid": self.sid,
            "host": {
                "id": target.id,
                "name": if target.name.is_empty() { target.host.clone() } else { target.name.clone() },
                "host": target.host,
                "port": target.port,
                "username": target.username,
                "identityFile": target.identity_file,
            },
            "server": {
                "uname": self.server.uname,
                "user": self.server.user,
                "hostname": self.server.hostname,
            },
            "home": remote.home,
            "remotePath": remote.remote_path,
            "fileExists": remote.file_exists,
            "keysLoaded": remote.keys_loaded,
            "authMethod": self.auth_method,
            "usedKeyFp": *self.used_key_fp.read().await,
            "items": items,
            "stats": {
                "total": parsed.key_count(),
                "malformed": parsed.malformed_count(),
                "comments": parsed.comment_count(),
            },
        })
    }
}

/// 私钥文本 → 公钥指纹。解不开（加密但没给 passphrase）返回空串，
/// 与旧版 `fingerprintOfPrivateKey` 一致：宁可空，也不能误报成「在用这把」。
pub fn fingerprint_of_private_key(text: &str, passphrase: Option<&str>) -> String {
    let Ok(key) = load_private_key(text, passphrase) else {
        return String::new();
    };
    match key.public_key().to_bytes() {
        Ok(blob) => fingerprint_of_blob(&blob),
        Err(_) => String::new(),
    }
}

#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, Arc<Session>>>,
}

impl Sessions {
    pub async fn insert(&self, session: Arc<Session>) {
        self.inner.lock().await.insert(session.sid.clone(), session);
    }

    pub async fn get(&self, sid: &str) -> Option<Arc<Session>> {
        self.inner.lock().await.get(sid).cloned()
    }

    /// 取会话并刷新 lastUsed；不存在或连接已断返回 None（上层转成 SESSION_LOST）。
    pub async fn must(&self, sid: &str) -> Option<Arc<Session>> {
        let session = self.get(sid).await?;
        // 服务器踢掉空闲连接后，内存表里还留着一条「假活」记录；
        // 不主动探测的话，用户要等到某次读写失败才会看到 SESSION_LOST。
        if session.client.is_closed().await {
            self.remove(sid).await;
            return None;
        }
        session.touch();
        Some(session)
    }

    pub async fn remove(&self, sid: &str) -> Option<Arc<Session>> {
        self.inner.lock().await.remove(sid)
    }

    pub async fn list(&self) -> Vec<Arc<Session>> {
        self.inner.lock().await.values().cloned().collect()
    }

    /// 清理超时空闲会话，返回被清掉的 sid。
    pub async fn sweep(&self, idle: Duration) -> Vec<String> {
        let mut inner = self.inner.lock().await;
        let dead: Vec<String> = inner
            .values()
            .filter(|s| s.idle_for() > idle)
            .map(|s| s.sid.clone())
            .collect();
        for sid in &dead {
            inner.remove(sid);
        }
        dead
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_of_locked_key_is_empty_without_passphrase() {
        let gen = kw_keys::generate_ed25519(Some("t"), Some("pw")).unwrap();
        assert_eq!(fingerprint_of_private_key(&gen.private_key_pem, None), "");
        assert_eq!(
            fingerprint_of_private_key(&gen.private_key_pem, Some("pw")),
            gen.fingerprint
        );
        assert_eq!(
            fingerprint_of_private_key(&gen.private_key_pem, Some("bad")),
            ""
        );
    }
}
