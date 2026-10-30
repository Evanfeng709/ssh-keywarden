//! 主机指纹（known_hosts 式 TOFU）。
//!
//! 首次连接由用户确认指纹后写入；之后密钥变化必须再次确认，防止中间人冒充。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use kw_keys::fingerprint_of_blob;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn host_id(host: &str, port: u16) -> String {
    format!("{}:{}", host.trim().to_lowercase(), port)
}

/// 从公钥 wire blob 里读第一位类型名；读不出来返回空串。
pub fn key_type_of_blob(blob: &[u8]) -> String {
    if blob.len() < 8 {
        return String::new();
    }
    let len = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    if !(3..=64).contains(&len) || blob.len() < 4 + len {
        return String::new();
    }
    let Ok(type_str) = std::str::from_utf8(&blob[4..4 + len]) else {
        return String::new();
    };
    let ok = type_str
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-'));
    if ok {
        type_str.to_string()
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostEntry {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    pub public_key: String,
    pub added_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostDescription {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
}

pub fn describe(host: &str, port: u16, blob: &[u8]) -> HostDescription {
    HostDescription {
        host: host.trim().to_string(),
        port,
        key_type: key_type_of_blob(blob),
        fingerprint: fingerprint_of_blob(blob),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyCode {
    Unknown,
    Changed,
}

impl HostKeyCode {
    pub fn as_str(self) -> &'static str {
        match self {
            HostKeyCode::Unknown => "HOSTKEY_UNKNOWN",
            HostKeyCode::Changed => "HOSTKEY_CHANGED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostKeyError {
    pub code: HostKeyCode,
    /// 给前端弹窗用的详情（host/port/keyType/fingerprint/previousFingerprint…）
    pub details: Value,
}

impl HostKeyError {
    pub fn message(&self) -> String {
        match self.code {
            HostKeyCode::Changed => format!(
                "主机密钥已变化：{}:{} 返回的指纹与本地记录不一致。若非你主动重装或更换过服务器，请取消连接并检查网络",
                self.details.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                self.details.get("port").and_then(|v| v.as_u64()).unwrap_or(22)
            ),
            HostKeyCode::Unknown => format!(
                "首次连接 {}:{}，请核对主机指纹后确认",
                self.details.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                self.details.get("port").and_then(|v| v.as_u64()).unwrap_or(22)
            ),
        }
    }
}

impl fmt::Display for HostKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for HostKeyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyStatus {
    Match,
    Trusted,
    Updated,
}

#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub status: VerifyStatus,
    pub current: Value,
    pub previous: Option<Value>,
}

pub struct KnownHosts {
    file: PathBuf,
}

impl KnownHosts {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into() }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    fn read_all(&self) -> BTreeMap<String, Value> {
        let Ok(text) = fs::read_to_string(&self.file) else {
            return BTreeMap::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    fn write_all(&self, all: &BTreeMap<String, Value>) {
        if let Some(parent) = self.file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = PathBuf::from(format!("{}.tmp", self.file.display()));
        let text = serde_json::to_string_pretty(all).unwrap_or_else(|_| "{}".to_string());
        if fs::write(&tmp, text).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        let _ = fs::rename(&tmp, &self.file);
    }

    pub fn get(&self, host: &str, port: u16) -> Option<HostEntry> {
        let all = self.read_all();
        all.get(&host_id(host, port))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn list(&self) -> Vec<HostEntry> {
        self.read_all()
            .values()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }

    pub fn save(&self, host: &str, port: u16, blob: &[u8]) -> HostEntry {
        let mut all = self.read_all();
        let id = host_id(host, port);
        let prev = all.get(&id).cloned();
        let added_at = prev
            .as_ref()
            .and_then(|p| p.get("addedAt"))
            .and_then(|v| v.as_u64())
            .unwrap_or_else(now_ms);
        let desc = describe(host, port, blob);
        let entry = HostEntry {
            host: desc.host,
            port: desc.port,
            key_type: desc.key_type,
            fingerprint: desc.fingerprint,
            public_key: base64_encode(blob),
            added_at,
            updated_at: now_ms(),
        };
        all.insert(id, serde_json::to_value(&entry).unwrap());
        self.write_all(&all);
        entry
    }

    /// 校验远端主机密钥。
    pub fn verify(
        &self,
        host: &str,
        port: u16,
        blob: &[u8],
        trust_fingerprint: Option<&str>,
    ) -> Result<VerifyResult, HostKeyError> {
        let current = describe(host, port, blob);
        let current_value = serde_json::to_value(&current).unwrap();
        let entry = self.get(host, port);
        let confirmed = trust_fingerprint == Some(current.fingerprint.as_str());

        match entry {
            None => {
                if confirmed {
                    let saved = self.save(host, port, blob);
                    Ok(VerifyResult {
                        status: VerifyStatus::Trusted,
                        current: serde_json::to_value(saved).unwrap(),
                        previous: None,
                    })
                } else {
                    Err(HostKeyError {
                        code: HostKeyCode::Unknown,
                        details: current_value,
                    })
                }
            }
            Some(prev) if prev.fingerprint == current.fingerprint => Ok(VerifyResult {
                status: VerifyStatus::Match,
                current: current_value,
                previous: Some(serde_json::to_value(prev).unwrap()),
            }),
            Some(prev) => {
                if confirmed {
                    self.save(host, port, blob);
                    Ok(VerifyResult {
                        status: VerifyStatus::Updated,
                        current: current_value,
                        previous: Some(serde_json::to_value(&prev).unwrap()),
                    })
                } else {
                    let mut details = current_value;
                    if let Value::Object(map) = &mut details {
                        map.insert(
                            "previousFingerprint".into(),
                            Value::String(prev.fingerprint),
                        );
                        map.insert("previousUpdatedAt".into(), Value::from(prev.updated_at));
                    }
                    Err(HostKeyError {
                        code: HostKeyCode::Changed,
                        details,
                    })
                }
            }
        }
    }
}

fn base64_encode(blob: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(blob)
}
