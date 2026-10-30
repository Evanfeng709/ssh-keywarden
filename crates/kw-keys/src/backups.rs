//! authorized_keys 备份：名字校验、差异计算。
//!
//! 只认同目录下的 `<remotePath>.bak.YYYYMMDDHHMMSS`。
//! 用户传的名字永远只当前缀校验用，实际读取路径由 remotePath 重新拼出来，
//! 避免把接口变成任意远端文件读取器。

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

use crate::authorized::parse_authorized_keys;
use crate::pathutil::{assert_safe_path, posix_basename};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupName {
    pub name: String,
    pub stamp: String,
    pub display_time: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupError {
    #[error("备份文件名不合法")]
    InvalidName,
    #[error("远端路径为空")]
    EmptyPath,
    #[error("远端路径包含非法字符")]
    UnsafePath,
}

pub fn display_time(stamp: &str) -> String {
    let part = |a: usize, b: usize| stamp.get(a..b).unwrap_or("");
    format!(
        "{}-{}-{} {}:{}:{}",
        part(0, 4),
        part(4, 6),
        part(6, 8),
        part(8, 10),
        part(10, 12),
        part(12, 14)
    )
}

fn is_stamp(s: &str) -> bool {
    s.len() == 14 && s.bytes().all(|b| b.is_ascii_digit())
}

/// 校验「basename.bak.14位时间戳」，合法返回展示信息。
pub fn parse_backup_name(remote_path: &str, name: &str) -> Option<BackupName> {
    let base = posix_basename(remote_path);
    if base.is_empty() || name.is_empty() {
        return None;
    }
    if name.contains('/') || name.contains('\\') {
        return None;
    }
    let prefix = format!("{base}.bak.");
    let stamp = name.strip_prefix(prefix.as_str())?;
    if !is_stamp(stamp) {
        return None;
    }
    Some(BackupName {
        name: name.to_string(),
        stamp: stamp.to_string(),
        display_time: display_time(stamp),
    })
}

/// 只根据 remotePath + 合法名字拼路径；不接受任何完整路径参数。
pub fn backup_path_for(remote_path: &str, name: &str) -> Result<String, BackupError> {
    let parsed = parse_backup_name(remote_path, name).ok_or(BackupError::InvalidName)?;
    assert_safe_path(remote_path).map_err(|_| BackupError::UnsafePath)?;
    Ok(format!("{remote_path}.bak.{}", parsed.stamp))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffKey {
    pub fingerprint: String,
    #[serde(rename = "type")]
    pub key_type: String,
    pub comment: String,
    pub options: String,
}

impl DiffKey {
    fn sort_key(&self) -> &str {
        if self.comment.is_empty() {
            &self.fingerprint
        } else {
            &self.comment
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupDiff {
    pub backup_key_count: usize,
    pub current_key_count: usize,
    pub same_count: usize,
    pub added: Vec<DiffKey>,
    pub removed: Vec<DiffKey>,
    pub backup_malformed: usize,
    pub current_malformed: usize,
    pub empty_backup: bool,
    pub used_key_in_backup: Option<bool>,
    pub current_file_exists: bool,
}

/// 按首次出现顺序收集去重后的钥匙。
fn key_map(text: &str) -> (Vec<DiffKey>, usize) {
    let parsed = parse_authorized_keys(text);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in parsed.key_items() {
        let Some(key) = item.key() else { continue };
        if !seen.insert(key.fingerprint.clone()) {
            continue;
        }
        out.push(DiffKey {
            fingerprint: key.fingerprint.clone(),
            key_type: key.key_type.clone(),
            comment: key.comment.clone(),
            options: key.options.clone(),
        });
    }
    (out, parsed.malformed_count())
}

/// 与 Node 的 `localeCompare` 近似：先忽略大小写，再按原串兜底。
fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// added = 备份里有、当前没有（恢复后会新增）
/// removed = 当前有、备份没有（恢复后会移除）
pub fn diff_backup(
    current_text: &str,
    backup_text: &str,
    used_key_fp: Option<&str>,
    current_file_exists: bool,
) -> BackupDiff {
    let (current, current_malformed) = key_map(current_text);
    let (backup, backup_malformed) = key_map(backup_text);

    let current_index: HashMap<&str, &DiffKey> = current
        .iter()
        .map(|k| (k.fingerprint.as_str(), k))
        .collect();
    let backup_index: HashMap<&str, &DiffKey> =
        backup.iter().map(|k| (k.fingerprint.as_str(), k)).collect();

    let mut added: Vec<DiffKey> = backup
        .iter()
        .filter(|k| !current_index.contains_key(k.fingerprint.as_str()))
        .cloned()
        .collect();
    let mut removed: Vec<DiffKey> = current
        .iter()
        .filter(|k| !backup_index.contains_key(k.fingerprint.as_str()))
        .cloned()
        .collect();
    added.sort_by(|a, b| locale_cmp(a.sort_key(), b.sort_key()));
    removed.sort_by(|a, b| locale_cmp(a.sort_key(), b.sort_key()));

    let same_count = backup
        .iter()
        .filter(|k| current_index.contains_key(k.fingerprint.as_str()))
        .count();

    let used_key_in_backup = match used_key_fp {
        Some(fp) if !fp.is_empty() => Some(backup_index.contains_key(fp)),
        _ => None,
    };

    BackupDiff {
        backup_key_count: backup.len(),
        current_key_count: current.len(),
        same_count,
        added,
        removed,
        backup_malformed,
        current_malformed,
        empty_backup: backup.is_empty(),
        used_key_in_backup,
        current_file_exists,
    }
}
