//! 配置持久化：主机列表 + 全局设置 + 本地钥匙备注。
//!
//! 只存「怎么连」（地址/端口/用户名/私钥路径），**绝不存密码**。
//! 写文件一律「临时文件 + rename」，避免写一半停电把配置写坏。

pub mod sshconfig;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const NOTE_MAX: usize = 200;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_fingerprint(fp: &str) -> bool {
    let Some(rest) = fp.strip_prefix("SHA256:") else {
        return false;
    };
    rest.len() == 43
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub local_key_dir: String,
    pub save_dir: String,
    /// 留空 = ~/.ssh/authorized_keys
    pub remote_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            local_key_dir: String::new(),
            save_dir: String::new(),
            remote_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    /// 本机私钥路径。只对这台设备有意义，同步时不会覆盖对方的值。
    pub identity_file: String,
    /// 首选私钥的指纹（`SHA256:...`）。跨设备有效：本机 `identity_file`
    /// 不可用时按它去本地密钥库里找同一把钥匙。
    #[serde(default)]
    pub identity_fingerprint: String,
    pub remote_path: String,
    pub created_at: u64,
    pub updated_at: u64,
    /// 删除墓碑：非 0 表示已删除，仍保留在 config.json 里供跨设备同步。
    #[serde(default)]
    pub deleted_at: u64,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "未命名".to_string(),
            host: String::new(),
            port: 22,
            username: String::new(),
            identity_file: String::new(),
            identity_fingerprint: String::new(),
            remote_path: String::new(),
            created_at: 0,
            updated_at: 0,
            deleted_at: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub settings: Settings,
    pub hosts: Vec<Host>,
}

/// 跨设备合并结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    Added,
    Updated,
    Tombstoned,
    Skipped,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostPatch {
    pub id: Option<String>,
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub identity_file: Option<String>,
    /// 不传 = 保留原值（前端表单不感知这个字段，别让一次编辑把它清掉）
    pub identity_fingerprint: Option<String>,
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    pub local_key_dir: Option<String>,
    pub save_dir: Option<String>,
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NoteEntry {
    pub note: String,
    pub updated_at: u64,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("主机地址不能为空")]
    EmptyHost,
    #[error("用户名不能为空")]
    EmptyUsername,
    #[error("主机不存在")]
    HostNotFound,
    #[error("指纹格式不正确，备注未保存")]
    BadFingerprint,
    #[error("备注最多 {NOTE_MAX} 个字符")]
    NoteTooLong,
    #[error("写入 {0} 失败：{1}")]
    Io(String, String),
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `KW_DATA_DIR` 优先，否则用相对目录 `data`。
    pub fn from_env() -> Self {
        let dir = std::env::var_os("KW_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data"));
        Self::new(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.json")
    }

    pub fn notes_path(&self) -> PathBuf {
        self.dir.join("key-notes.json")
    }

    fn ensure_dir(&self) -> io::Result<()> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir)?;
        }
        Ok(())
    }

    fn write_json_atomic(&self, file: &Path, value: &impl Serialize) -> io::Result<()> {
        self.ensure_dir()?;
        let tmp = PathBuf::from(format!("{}.tmp", file.display()));
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
        fs::write(&tmp, text)?;
        // write 的权限只在新建时生效：上次崩溃留下的 .tmp 会被直接覆盖，
        // 这里补一刀确保 0600（Windows 上是空操作）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp, file)
    }

    /// 配置坏了不能静默吞掉：备份坏文件，再用默认值继续。
    fn backup_bad(&self, file: &Path, tag: &str) {
        let bad = format!("{}.bad-{}", file.display(), now_ms());
        let _ = fs::copy(file, &bad);
        eprintln!("[{tag}] 配置文件解析失败，已备份到 {bad}，使用默认值");
    }

    pub fn read(&self) -> Config {
        let path = self.config_path();
        if !path.exists() {
            return Config::default();
        }
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    self.backup_bad(&path, "config");
                    let _ = e;
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn write(&self, cfg: &Config) -> Result<(), StoreError> {
        self.write_json_atomic(&self.config_path(), cfg)
            .map_err(|e| StoreError::Io(self.config_path().display().to_string(), e.to_string()))
    }

    /// 界面上能看到的主机（已过滤删除墓碑）。
    pub fn list_hosts(&self) -> Vec<Host> {
        self.read()
            .hosts
            .into_iter()
            .filter(|h| h.deleted_at == 0)
            .collect()
    }

    /// 含墓碑的全量主机，供同步导出使用。
    pub fn all_hosts(&self) -> Vec<Host> {
        self.read().hosts
    }

    pub fn upsert_host(&self, patch: HostPatch) -> Result<Host, StoreError> {
        let mut cfg = self.read();
        let host_name = patch
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| patch.host.clone().filter(|h| !h.trim().is_empty()))
            .unwrap_or_else(|| "未命名".to_string());

        let mut host = Host {
            id: patch
                .id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name: host_name.trim().to_string(),
            host: patch.host.clone().unwrap_or_default().trim().to_string(),
            port: patch.port.unwrap_or(22),
            username: patch
                .username
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string(),
            identity_file: patch
                .identity_file
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string(),
            identity_fingerprint: patch
                .identity_fingerprint
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string(),
            remote_path: patch
                .remote_path
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string(),
            created_at: 0,
            updated_at: now_ms(),
            deleted_at: 0,
        };
        if host.host.is_empty() {
            return Err(StoreError::EmptyHost);
        }
        if host.username.is_empty() {
            return Err(StoreError::EmptyUsername);
        }

        match cfg.hosts.iter().position(|h| h.id == host.id) {
            Some(idx) => {
                host.created_at = if cfg.hosts[idx].created_at > 0 {
                    cfg.hosts[idx].created_at
                } else {
                    now_ms()
                };
                // 指纹由同步包带过来，前端表单并不感知它 —— 传空即「保持原值」，
                // 否则用户在桌面改一次主机名就会把跨设备的私钥关联抹掉
                if host.identity_fingerprint.is_empty() {
                    host.identity_fingerprint = cfg.hosts[idx].identity_fingerprint.clone();
                }
                cfg.hosts[idx] = host.clone();
            }
            None => {
                host.created_at = now_ms();
                cfg.hosts.push(host.clone());
            }
        }
        self.write(&cfg)?;
        Ok(host)
    }

    /// 删除 = 打墓碑。真正移除留给未来的清理任务，否则跨设备同步时
    /// 对方看不到「这条被删了」，下次导入又会把它复活。
    pub fn remove_host(&self, id: &str) -> Result<(), StoreError> {
        let mut cfg = self.read();
        let Some(host) = cfg.hosts.iter_mut().find(|h| h.id == id) else {
            return Err(StoreError::HostNotFound);
        };
        if host.deleted_at > 0 {
            return Err(StoreError::HostNotFound);
        }
        let now = now_ms();
        host.deleted_at = now;
        host.updated_at = now;
        self.write(&cfg)
    }

    /// 按 id（缺失则按 host/port/user）合并一条远端主机记录。
    /// 以 `updated_at` 为准，旧记录不覆盖新记录；墓碑同样参与比较。
    pub fn merge_host(&self, incoming: &Host) -> Result<MergeOutcome, StoreError> {
        if incoming.deleted_at == 0 {
            if incoming.host.trim().is_empty() {
                return Err(StoreError::EmptyHost);
            }
            if incoming.username.trim().is_empty() {
                return Err(StoreError::EmptyUsername);
            }
        }

        let mut cfg = self.read();
        let idx = if incoming.id.trim().is_empty() {
            None
        } else {
            cfg.hosts.iter().position(|h| h.id == incoming.id)
        }
        .or_else(|| {
            cfg.hosts.iter().position(|h| {
                h.host == incoming.host
                    && h.port == incoming.port
                    && h.username == incoming.username
            })
        });

        let incoming_updated = if incoming.updated_at == 0 {
            now_ms()
        } else {
            incoming.updated_at
        };

        match idx {
            None => {
                let mut host = incoming.clone();
                if host.id.trim().is_empty() {
                    host.id = uuid::Uuid::new_v4().to_string();
                }
                host.name = if host.name.trim().is_empty() {
                    host.host.clone()
                } else {
                    host.name.trim().to_string()
                };
                host.created_at = if incoming.created_at > 0 {
                    incoming.created_at
                } else {
                    now_ms()
                };
                host.updated_at = incoming_updated;
                cfg.hosts.push(host);
                self.write(&cfg)?;
                Ok(MergeOutcome::Added)
            }
            Some(i) => {
                if incoming_updated <= cfg.hosts[i].updated_at {
                    return Ok(MergeOutcome::Skipped);
                }
                let created = cfg.hosts[i].created_at;
                if incoming.deleted_at > 0 {
                    cfg.hosts[i].deleted_at = incoming.deleted_at;
                    cfg.hosts[i].updated_at = incoming_updated;
                    self.write(&cfg)?;
                    return Ok(MergeOutcome::Tombstoned);
                }
                let mut host = incoming.clone();
                host.id = cfg.hosts[i].id.clone();
                host.created_at = created;
                host.updated_at = incoming_updated;
                host.deleted_at = 0;
                // 私钥路径是「这台设备」的本地设置，不能跟着同步包跨设备覆盖
                host.identity_file = cfg.hosts[i].identity_file.clone();
                cfg.hosts[i] = host;
                self.write(&cfg)?;
                Ok(MergeOutcome::Updated)
            }
        }
    }

    pub fn get_settings(&self) -> Settings {
        self.read().settings
    }

    pub fn update_settings(&self, patch: SettingsPatch) -> Result<Settings, StoreError> {
        let mut cfg = self.read();
        if let Some(v) = patch.local_key_dir {
            cfg.settings.local_key_dir = v;
        }
        if let Some(v) = patch.save_dir {
            cfg.settings.save_dir = v;
        }
        if let Some(v) = patch.remote_path {
            cfg.settings.remote_path = v;
        }
        self.write(&cfg)?;
        Ok(cfg.settings)
    }

    /// 读取本地备注；文件损坏时备份后按空表继续，不能拖垮整个配置。
    /// `include_empty=true` 时连墓碑（空备注）一起返回，供同步导出。
    fn read_notes(&self, include_empty: bool) -> BTreeMap<String, NoteEntry> {
        let path = self.notes_path();
        if !path.exists() {
            return BTreeMap::new();
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return BTreeMap::new(),
        };
        let raw: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                self.backup_bad(&path, "notes");
                return BTreeMap::new();
            }
        };
        let Some(notes) = raw.get("notes").and_then(|n| n.as_object()) else {
            return BTreeMap::new();
        };

        let mut out = BTreeMap::new();
        for (fp, entry) in notes {
            if !is_fingerprint(fp) {
                continue;
            }
            let Some(note) = entry.get("note").and_then(|n| n.as_str()) else {
                continue;
            };
            let note = sanitize_note(note);
            if note.is_empty() && !include_empty {
                continue;
            }
            out.insert(
                fp.clone(),
                NoteEntry {
                    note,
                    updated_at: entry.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0),
                },
            );
        }
        out
    }

    /// 界面用：只返回有内容的备注。
    pub fn list_key_notes(&self) -> BTreeMap<String, NoteEntry> {
        self.read_notes(false)
    }

    /// 同步用：连空备注墓碑一起导出。
    pub fn all_key_notes(&self) -> BTreeMap<String, NoteEntry> {
        self.read_notes(true)
    }

    fn write_notes(&self, notes: &BTreeMap<String, NoteEntry>) -> Result<(), StoreError> {
        #[derive(Serialize)]
        struct NotesFile<'a> {
            version: u32,
            notes: &'a BTreeMap<String, NoteEntry>,
        }
        self.write_json_atomic(&self.notes_path(), &NotesFile { version: 1, notes })
            .map_err(|e| StoreError::Io(self.notes_path().display().to_string(), e.to_string()))
    }

    /// 保存/删除一条备注；空备注写成墓碑（不物理删除），否则同步时对方无法感知删除。
    /// 返回界面可见的备注表（不含墓碑）。
    pub fn set_key_note(
        &self,
        fingerprint: &str,
        note: &str,
    ) -> Result<BTreeMap<String, NoteEntry>, StoreError> {
        let fp = fingerprint.trim();
        if !is_fingerprint(fp) {
            return Err(StoreError::BadFingerprint);
        }
        let text = sanitize_note(note);
        if text.chars().count() > NOTE_MAX {
            return Err(StoreError::NoteTooLong);
        }

        let mut notes = self.read_notes(true);
        notes.insert(
            fp.to_string(),
            NoteEntry {
                note: text,
                updated_at: now_ms(),
            },
        );
        self.write_notes(&notes)?;
        Ok(self.read_notes(false))
    }

    /// 按 `updated_at` 合并一条远端备注；旧记录不覆盖新记录。
    pub fn merge_note(
        &self,
        fingerprint: &str,
        incoming: &NoteEntry,
    ) -> Result<MergeOutcome, StoreError> {
        let fp = fingerprint.trim();
        if !is_fingerprint(fp) {
            return Err(StoreError::BadFingerprint);
        }
        let text = sanitize_note(&incoming.note);
        if text.chars().count() > NOTE_MAX {
            return Err(StoreError::NoteTooLong);
        }

        let mut notes = self.read_notes(true);
        let exists = notes.contains_key(fp);
        if let Some(current) = notes.get(fp) {
            if incoming.updated_at <= current.updated_at {
                return Ok(MergeOutcome::Skipped);
            }
        }
        notes.insert(
            fp.to_string(),
            NoteEntry {
                note: text.clone(),
                updated_at: if incoming.updated_at == 0 {
                    now_ms()
                } else {
                    incoming.updated_at
                },
            },
        );
        self.write_notes(&notes)?;
        Ok(if text.is_empty() {
            MergeOutcome::Tombstoned
        } else if exists {
            MergeOutcome::Updated
        } else {
            MergeOutcome::Added
        })
    }
}

/// 换行/制表符 → 空格，控制字符剔除，连续空白折叠，首尾去空。
pub fn sanitize_note(note: &str) -> String {
    let mut out = String::with_capacity(note.len());
    let mut last_space = false;
    for ch in note.chars() {
        let mapped = if ch == '\r' || ch == '\n' || ch == '\t' {
            ' '
        } else {
            ch
        };
        if (mapped as u32) < 0x20 || mapped == '\u{7f}' {
            continue;
        }
        if mapped == ' ' {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(mapped);
            last_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}
