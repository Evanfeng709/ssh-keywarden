//! 本地密钥库扫描：把目录里的私钥/公钥读出来，供 UI 展示与「从本地选择」使用。
//!
//! 目录里常混着文档、日志、截图，整份读进来只为确认「这不是密钥」很浪费；
//! 先读 4KB 嗅探，判定像密钥才整份读。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::keygen::{
    fingerprint_of_blob, inspect_private_key, parse_public_key_line, public_key_blob_of_private,
};

const MAX_FILE_SIZE: u64 = 128 * 1024;
const HEAD_BYTES: usize = 4096;
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "$RECYCLE.BIN",
    "System Volume Information",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalKeyKind {
    Private,
    Public,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalKeyEntry {
    pub path: String,
    pub name: String,
    pub dir: String,
    pub kind: LocalKeyKind,
    #[serde(rename = "type")]
    pub key_type: String,
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub size: u64,
    pub mtime: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_line: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanError {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanResult {
    pub entries: Vec<LocalKeyEntry>,
    pub errors: Vec<ScanError>,
}

/// 私钥头部：`-----BEGIN ...PRIVATE KEY-----`，标签只允许大写字母与空格。
fn has_private_header(text: &str) -> bool {
    let marker = "-----BEGIN ";
    let mut search_from = 0;
    while let Some(pos) = text[search_from..].find(marker) {
        let start = search_from + pos + marker.len();
        if let Some(end) = text[start..].find("PRIVATE KEY-----") {
            let label = &text[start..start + end];
            if label.chars().all(|c| c.is_ascii_uppercase() || c == ' ') {
                return true;
            }
        }
        search_from = start;
    }
    false
}

fn looks_like_public(text: &str) -> bool {
    text.split('\n')
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|line| parse_public_key_line(line).is_some())
        .unwrap_or(false)
}

fn read_head(path: &Path, bytes: u64) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buf = vec![0u8; bytes as usize];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_text(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn mtime_ms(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 扫描目录（递归，限深度）。
pub fn scan_keys(root: &Path, max_depth: usize) -> ScanResult {
    let mut result = ScanResult::default();

    let meta = match fs::metadata(root) {
        Ok(m) if m.is_dir() => m,
        Ok(_) => {
            result.errors.push(ScanError {
                path: root.display().to_string(),
                message: "不是目录".to_string(),
            });
            return result;
        }
        Err(e) => {
            result.errors.push(ScanError {
                path: root.display().to_string(),
                message: e.to_string(),
            });
            return result;
        }
    };
    let _ = meta;

    walk(root, 0, max_depth, &mut result);
    result
}

fn walk(dir: &Path, depth: usize, max_depth: usize, result: &mut ScanResult) {
    let list = match fs::read_dir(dir) {
        Ok(l) => l,
        Err(e) => {
            result.errors.push(ScanError {
                path: dir.display().to_string(),
                message: e.to_string(),
            });
            return;
        }
    };

    for item in list {
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                result.errors.push(ScanError {
                    path: dir.display().to_string(),
                    message: e.to_string(),
                });
                continue;
            }
        };

        let path = item.path();
        let file_type = match item.file_type() {
            Ok(t) => t,
            Err(e) => {
                result.errors.push(ScanError {
                    path: path.display().to_string(),
                    message: e.to_string(),
                });
                continue;
            }
        };

        if file_type.is_dir() {
            let name = item.file_name();
            let name = name.to_string_lossy();
            if depth < max_depth && !SKIP_DIRS.contains(&name.as_ref()) {
                walk(&path, depth + 1, max_depth, result);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let stat = match fs::metadata(&path) {
            Ok(s) => s,
            Err(e) => {
                result.errors.push(ScanError {
                    path: path.display().to_string(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        if stat.len() == 0 || stat.len() > MAX_FILE_SIZE {
            continue;
        }

        let head = match read_head(&path, stat.len().min(HEAD_BYTES as u64)) {
            Ok(h) => h,
            Err(e) => {
                result.errors.push(ScanError {
                    path: path.display().to_string(),
                    message: e.to_string(),
                });
                continue;
            }
        };

        let head_is_private = has_private_header(&head);
        let head_is_public = !head_is_private && looks_like_public(&head);
        // 只有注释/空行时无法判定 —— 可能是「注释写在前面」的公钥文件，退回整份读
        let head_only_comments = !head_is_private
            && !head_is_public
            && head.split('\n').all(|l| {
                let t = l.trim();
                t.is_empty() || t.starts_with('#')
            });
        if !head_is_private && !head_is_public && !head_only_comments {
            continue;
        }

        let text = match read_text(&path) {
            Ok(t) => t,
            Err(e) => {
                result.errors.push(ScanError {
                    path: path.display().to_string(),
                    message: e.to_string(),
                });
                continue;
            }
        };

        let is_private = has_private_header(&text);
        let is_public = !is_private && looks_like_public(&text);

        let name = item.file_name().to_string_lossy().into_owned();
        let dir_str = dir.display().to_string();
        let path_str = path.display().to_string();
        let size = stat.len();
        let mtime = mtime_ms(&stat);

        if is_private {
            let info = inspect_private_key(&text);
            result.entries.push(LocalKeyEntry {
                path: path_str,
                name,
                dir: dir_str,
                kind: LocalKeyKind::Private,
                key_type: info.key_type,
                fingerprint: match_public_sibling(&path, &text).unwrap_or_default(),
                comment: None,
                encrypted: Some(info.encrypted),
                error: Some(if info.encrypted {
                    String::new()
                } else {
                    info.error.unwrap_or_default()
                }),
                size,
                mtime,
                valid: None,
                public_key_line: None,
            });
        } else if is_public {
            let Some(line) = text
                .split('\n')
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with('#'))
            else {
                continue;
            };
            let Some(parsed) = parse_public_key_line(line) else {
                continue;
            };
            let public_key_line = if parsed.comment.is_empty() {
                format!("{} {}", parsed.key_type, parsed.b64)
            } else {
                format!("{} {} {}", parsed.key_type, parsed.b64, parsed.comment)
            };
            result.entries.push(LocalKeyEntry {
                path: path_str,
                name,
                dir: dir_str,
                kind: LocalKeyKind::Public,
                key_type: parsed.key_type,
                fingerprint: parsed.fingerprint,
                comment: Some(parsed.comment),
                encrypted: None,
                error: None,
                size,
                mtime,
                valid: Some(parsed.valid),
                public_key_line: Some(public_key_line),
            });
        }
    }
}

/// 私钥本身没有「指纹」字段，靠同目录同名 `.pub` 反推；
/// 拿不到再从私钥自己算（未加密时可用）。
fn match_public_sibling(priv_path: &Path, priv_text: &str) -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.push(PathBuf::from(format!("{}.pub", priv_path.display())));

    let path_str = priv_path.display().to_string();
    let lower = path_str.to_lowercase();
    for ext in [".rsa", ".ed25519", ".ecdsa", ".dsa"] {
        if lower.ends_with(ext) {
            candidates.push(PathBuf::from(format!(
                "{}.pub",
                &path_str[..path_str.len() - ext.len()]
            )));
            break;
        }
    }

    for pub_path in candidates {
        if let Ok(text) = fs::read_to_string(&pub_path) {
            if let Some(line) = text
                .split('\n')
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with('#'))
            {
                if let Some(parsed) = parse_public_key_line(line) {
                    return Some(parsed.fingerprint);
                }
            }
        }
    }

    public_key_blob_of_private(priv_text).map(|blob| fingerprint_of_blob(&blob))
}
