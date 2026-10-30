//! 进程内共享状态：配置、known_hosts、会话表、静态资源目录、扫描缓存。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use kw_keys::localkeys::{scan_keys, ScanResult};
use kw_ssh::known_hosts::KnownHosts;
use kw_store::sshconfig::import_from_ssh_config;
use kw_store::{Settings, Store};

use crate::sessions::Sessions;

pub struct AppState {
    /// 每次启动随机，等价旧版 `crypto.randomBytes(18).toString('hex')`
    pub token: String,
    /// 实际监听端口，Host 头校验要用
    pub port: u16,
    pub store: Arc<Store>,
    pub known_hosts: Arc<KnownHosts>,
    pub sessions: Sessions,
    pub public_dir: PathBuf,
    pub version: String,
    pub auto_cookie: bool,
    pub scan_cache_ms: u64,
    /// 本地密钥库目录。同步包只带指纹不带路径，连接时按指纹在这里找私钥。
    /// 桌面端与手机端都用 `<data_dir>/keys`，正好和 FFI 侧一致。
    pub keys_dir: PathBuf,
    /// 设备码登录的中间态：只有 device_code，不落盘、不进日志，重启即失效。
    pub pending_login: Mutex<Option<String>>,
    scan_cache: Mutex<HashMap<String, (Instant, ScanResult)>>,
}

impl AppState {
    pub fn new(
        token: String,
        port: u16,
        store: Arc<Store>,
        known_hosts: Arc<KnownHosts>,
        public_dir: impl Into<PathBuf>,
        version: impl Into<String>,
    ) -> Self {
        let scan_cache_ms = std::env::var("KW_SCAN_CACHE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let auto_cookie = std::env::var("KW_NO_AUTO_COOKIE").ok().as_deref() != Some("1");
        let keys_dir = store.dir().join("keys");
        Self {
            token,
            port,
            store,
            known_hosts,
            sessions: Sessions::default(),
            public_dir: public_dir.into(),
            version: version.into(),
            auto_cookie,
            scan_cache_ms,
            keys_dir,
            pending_login: Mutex::new(None),
            scan_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 带缓存的密钥库扫描；force=true 忽略缓存（手动「重新扫描」用）。
    pub async fn scan_local(&self, dir: &str, force: bool) -> ScanResult {
        let root = std::path::Path::new(dir).to_path_buf();
        if !force && self.scan_cache_ms > 0 {
            let cache = self.scan_cache.lock().await;
            if let Some((at, result)) = cache.get(dir) {
                if at.elapsed().as_millis() < self.scan_cache_ms as u128 {
                    return result.clone();
                }
            }
        }
        let result = scan_keys(&root, 3);
        if self.scan_cache_ms > 0 {
            self.scan_cache
                .lock()
                .await
                .insert(dir.to_string(), (Instant::now(), result.clone()));
        }
        result
    }

    pub async fn invalidate_scan(&self, dir: &str) {
        self.scan_cache.lock().await.remove(dir);
    }

    /// 旧版 `currentSettings`：localKeyDir 为空时按 ~/.ssh/config 猜一个并落盘。
    pub fn current_settings(&self) -> Settings {
        let mut settings = self.store.get_settings();
        if settings.local_key_dir.is_empty() {
            if let Some(guess) = detect_key_dir() {
                match self.store.update_settings(kw_store::SettingsPatch {
                    local_key_dir: Some(guess.clone()),
                    save_dir: None,
                    remote_path: None,
                }) {
                    Ok(updated) => settings = updated,
                    Err(_) => settings.local_key_dir = guess,
                }
            }
        }
        settings
    }
}

/// `~` 展开 + 相对路径拼到家目录 + 默认 authorized_keys。
pub fn expand_path(p: &str, home: &str) -> String {
    let t = p.trim();
    if t.is_empty() {
        return format!("{home}/.ssh/authorized_keys");
    }
    if t == "~" {
        return home.to_string();
    }
    if let Some(rest) = t.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    if t.starts_with('/') {
        return t.to_string();
    }
    format!("{home}/{t}")
}

/// 从 ~/.ssh/config 猜一个默认私钥目录（找不到就空着）。
pub fn detect_key_dir() -> Option<String> {
    let home = kw_store::sshconfig::home_dir();
    let file = home.join(".ssh").join("config");
    let hosts = import_from_ssh_config(&file);
    let dirs: Vec<PathBuf> = hosts
        .iter()
        .filter(|h| !h.identity_file.is_empty())
        .filter_map(|h| {
            Path::new(&h.identity_file)
                .parent()
                .map(|p| p.to_path_buf())
        })
        .filter(|d| d.exists())
        .collect();
    if dirs.is_empty() {
        return None;
    }

    // 私钥目录常有多层（如 密钥/1o&通用/id_rsa），向上浮一两级取「密钥文件最多」的那层
    let mut best = dirs[0].clone();
    let mut best_count = count_keys(&best);
    let mut cur = best.clone();
    for _ in 0..2 {
        let Some(parent) = cur.parent() else { break };
        if parent == cur.as_path() || parent == home.as_path() {
            break;
        }
        let count = count_keys(parent);
        if count > best_count {
            best = parent.to_path_buf();
            best_count = count;
        }
        cur = parent.to_path_buf();
    }
    Some(best.display().to_string())
}

fn count_keys(dir: &Path) -> usize {
    scan_keys(dir, 1).entries.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_path_matches_old_semantics() {
        assert_eq!(expand_path("", "/home/a"), "/home/a/.ssh/authorized_keys");
        assert_eq!(expand_path("~", "/home/a"), "/home/a");
        assert_eq!(expand_path("~/.ssh/x", "/home/a"), "/home/a/.ssh/x");
        assert_eq!(expand_path("/etc/x", "/home/a"), "/etc/x");
        assert_eq!(expand_path("rel/x", "/home/a"), "/home/a/rel/x");
    }
}
