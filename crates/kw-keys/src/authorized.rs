//! authorized_keys 解析 / 序列化。
//!
//! 设计原则与旧版一致：**不重排、不美化、不丢行**。
//! 解析只用于展示与定位，落盘时按原始文本行做增删。

use serde::ser::{Serialize, SerializeMap, Serializer};
use thiserror::Error;

use crate::keygen::{parse_public_key_line, ParsedPublicKey};

pub const MALFORMED_REASON: &str = "无法识别的行格式（已原样保留）";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Blank,
    Comment,
    Malformed,
    Key,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Blank => "blank",
            ItemKind::Comment => "comment",
            ItemKind::Malformed => "malformed",
            ItemKind::Key => "key",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ItemData {
    Blank,
    Comment {
        text: String,
    },
    Malformed {
        reason: String,
    },
    Key {
        key: ParsedPublicKey,
        duplicate_of: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub struct AuthorizedItem {
    pub index: usize,
    pub raw: String,
    pub data: ItemData,
}

impl AuthorizedItem {
    pub fn kind(&self) -> ItemKind {
        match self.data {
            ItemData::Blank => ItemKind::Blank,
            ItemData::Comment { .. } => ItemKind::Comment,
            ItemData::Malformed { .. } => ItemKind::Malformed,
            ItemData::Key { .. } => ItemKind::Key,
        }
    }

    pub fn key(&self) -> Option<&ParsedPublicKey> {
        match &self.data {
            ItemData::Key { key, .. } => Some(key),
            _ => None,
        }
    }
}

impl Serialize for AuthorizedItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("index", &self.index)?;
        map.serialize_entry("raw", &self.raw)?;
        map.serialize_entry("kind", self.kind().as_str())?;
        match &self.data {
            ItemData::Blank => {}
            ItemData::Comment { text } => {
                map.serialize_entry("text", text)?;
            }
            ItemData::Malformed { reason } => {
                map.serialize_entry("reason", reason)?;
            }
            ItemData::Key { key, duplicate_of } => {
                map.serialize_entry("type", &key.key_type)?;
                map.serialize_entry("b64", &key.b64)?;
                map.serialize_entry("options", &key.options)?;
                map.serialize_entry("comment", &key.comment)?;
                map.serialize_entry("fingerprint", &key.fingerprint)?;
                map.serialize_entry("valid", &key.valid)?;
                map.serialize_entry("error", &key.error)?;
                map.serialize_entry("duplicateOf", duplicate_of)?;
            }
        }
        map.end()
    }
}

#[derive(Debug, Clone)]
pub struct AuthorizedKeys {
    pub items: Vec<AuthorizedItem>,
    pub raw_lines: Vec<String>,
    pub ends_with_newline: bool,
    pub existed: bool,
}

impl AuthorizedKeys {
    pub fn key_items(&self) -> impl Iterator<Item = &AuthorizedItem> {
        self.items.iter().filter(|i| i.kind() == ItemKind::Key)
    }

    pub fn key_count(&self) -> usize {
        self.key_items().count()
    }

    pub fn malformed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.kind() == ItemKind::Malformed)
            .count()
    }

    pub fn comment_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.kind() == ItemKind::Comment)
            .count()
    }
}

pub fn parse_authorized_keys(text: &str) -> AuthorizedKeys {
    let mut raw_lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect()
    };
    let ends_with_newline = text.ends_with('\n');
    if ends_with_newline && !raw_lines.is_empty() {
        raw_lines.pop();
    }

    let mut items = Vec::with_capacity(raw_lines.len());
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (index, raw) in raw_lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            items.push(AuthorizedItem {
                index,
                raw: raw.clone(),
                data: ItemData::Blank,
            });
            continue;
        }
        if line.starts_with('#') {
            items.push(AuthorizedItem {
                index,
                raw: raw.clone(),
                data: ItemData::Comment {
                    text: line.trim_start_matches('#').trim_start().to_string(),
                },
            });
            continue;
        }
        match parse_public_key_line(line) {
            None => items.push(AuthorizedItem {
                index,
                raw: raw.clone(),
                data: ItemData::Malformed {
                    reason: MALFORMED_REASON.to_string(),
                },
            }),
            Some(key) => {
                let first = seen.get(&key.fingerprint).copied();
                if first.is_none() {
                    seen.insert(key.fingerprint.clone(), index);
                }
                let duplicate_of = match first {
                    Some(f) if f != index => Some(f),
                    _ => None,
                };
                items.push(AuthorizedItem {
                    index,
                    raw: raw.clone(),
                    data: ItemData::Key { key, duplicate_of },
                });
            }
        }
    }

    AuthorizedKeys {
        items,
        raw_lines,
        ends_with_newline,
        existed: !text.is_empty(),
    }
}

/// 按索引删除后重新拼装文本。
pub fn render_without(parsed: &AuthorizedKeys, drop_indexes: &[usize]) -> String {
    let drop: std::collections::HashSet<usize> = drop_indexes.iter().copied().collect();
    let kept: Vec<&str> = parsed
        .raw_lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, l)| l.as_str())
        .collect();
    if kept.is_empty() {
        return String::new();
    }
    format!("{}\n", kept.join("\n"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOutcome {
    pub text: Option<String>,
    pub appended: bool,
    pub reason: Option<String>,
    pub fingerprint: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AppendError {
    #[error("公钥格式无法识别，拒绝写入")]
    Unrecognized,
    #[error("公钥无效：{0}")]
    Invalid(String),
}

/// 追加一行公钥；若指纹已存在则返回 unchanged。
pub fn append_key(
    parsed: &AuthorizedKeys,
    public_key_line: &str,
) -> Result<AppendOutcome, AppendError> {
    let line = parse_public_key_line(public_key_line).ok_or(AppendError::Unrecognized)?;
    if !line.valid {
        let reason = if line.error.is_empty() {
            "结构校验失败".to_string()
        } else {
            line.error.clone()
        };
        return Err(AppendError::Invalid(reason));
    }

    if parsed
        .key_items()
        .any(|i| i.key().is_some_and(|k| k.fingerprint == line.fingerprint))
    {
        return Ok(AppendOutcome {
            text: None,
            appended: false,
            reason: Some("该公钥已在服务器上（指纹相同），未重复写入".to_string()),
            fingerprint: line.fingerprint,
        });
    }

    let body = parsed.raw_lines.join("\n");
    let text = if body.is_empty() {
        format!("{public_key_line}\n")
    } else {
        format!("{body}\n{public_key_line}\n")
    };

    Ok(AppendOutcome {
        text: Some(text),
        appended: true,
        reason: None,
        fingerprint: line.fingerprint,
    })
}
