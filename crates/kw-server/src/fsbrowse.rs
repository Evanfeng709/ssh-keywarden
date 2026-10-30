//! 本机文件浏览：给「浏览…」按钮选公钥/私钥/保存目录用。
//!
//! 只列目录内容，不做任何写入；密钥识别只看文件头部 2 KB。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub fn home_dir() -> PathBuf {
    kw_store::sshconfig::home_dir()
}

pub fn expand_local_path(p: &str) -> PathBuf {
    let t = p.trim();
    if t.is_empty() || t == "~" {
        return home_dir();
    }
    if let Some(rest) = t.strip_prefix("~/").or_else(|| t.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(t)
}

/// Windows 下枚举存在的盘符；其它平台只有 `/`。
fn list_drives() -> Vec<String> {
    if !cfg!(windows) {
        return vec!["/".to_string()];
    }
    let mut out = Vec::new();
    for c in b'C'..=b'Z' {
        let d = format!("{}:\\", c as char);
        if Path::new(&d).exists() {
            out.push(d);
        }
    }
    out
}

/// 判断一个文件像不像 SSH 密钥（只看头部，不整文件读）。
fn sniff_key_file(path: &Path, size: u64) -> Option<&'static str> {
    if size == 0 || size > 128 * 1024 {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    for line in head.lines() {
        let line = line.trim_start();
        if line.starts_with("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            return Some("private");
        }
        if [
            "ssh-ed25519",
            "ssh-rsa",
            "ssh-dss",
            "ecdsa-sha2-",
            "sk-ssh-",
            "sk-ecdsa-",
        ]
        .iter()
        .any(|p| line.starts_with(p))
        {
            return Some("public");
        }
    }
    None
}

pub struct BrowseError(pub String);

/// 列目录：目录在前、同类按名称自然序，附带盘符与父目录。
pub fn list_local_dir(raw_dir: &str) -> Result<Value, BrowseError> {
    let dir = expand_local_path(raw_dir);

    let meta = fs::metadata(&dir)
        .map_err(|_| BrowseError(format!("目录不存在或无权访问：{}", dir.display())))?;
    if !meta.is_dir() {
        return Err(BrowseError(format!("这不是目录：{}", dir.display())));
    }

    let read = fs::read_dir(&dir).map_err(|e| BrowseError(format!("无法读取目录：{e}")))?;

    let mut entries: Vec<Value> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for ent in read {
        let Ok(ent) = ent else { continue };
        let name = ent.file_name().to_string_lossy().into_owned();
        let full = dir.join(&name);
        let Ok(s) = fs::metadata(&full) else {
            skipped.push(name);
            continue;
        };
        if s.is_dir() {
            entries
                .push(json!({ "name": name, "path": full.display().to_string(), "type": "dir" }));
        } else if s.is_file() {
            let mut item = json!({
                "name": name,
                "path": full.display().to_string(),
                "type": "file",
                "size": s.len(),
                "mtime": mtime_ms(&s),
            });
            if let Some(kind) = sniff_key_file(&full, s.len()) {
                item["keyKind"] = json!(kind);
            }
            entries.push(item);
        }
    }

    entries.sort_by(|a, b| {
        let ta = a["type"].as_str().unwrap_or("");
        let tb = b["type"].as_str().unwrap_or("");
        if ta != tb {
            return if ta == "dir" {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let na = a["name"].as_str().unwrap_or("");
        let nb = b["name"].as_str().unwrap_or("");
        natural_cmp(na, nb)
    });
    skipped.sort();

    let parent = dir.parent().map(|p| p.display().to_string());
    Ok(json!({
        "dir": dir.display().to_string(),
        "parent": parent,
        "entries": entries,
        "skipped": skipped,
        "home": home_dir().display().to_string(),
        "drives": list_drives(),
    }))
}

fn mtime_ms(s: &fs::Metadata) -> f64 {
    s.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// 近似 `localeCompare(..., {numeric: true})`：数字段按数值比，其余忽略大小写。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek(), bi.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let mut xn = String::new();
                    while ai.peek().is_some_and(|c| c.is_ascii_digit()) {
                        xn.push(ai.next().unwrap());
                    }
                    let mut yn = String::new();
                    while bi.peek().is_some_and(|c| c.is_ascii_digit()) {
                        yn.push(bi.next().unwrap());
                    }
                    let xv = xn.trim_start_matches('0');
                    let yv = yn.trim_start_matches('0');
                    let ord = xv.len().cmp(&yv.len()).then_with(|| xv.cmp(yv));
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                } else {
                    let xl = x.to_ascii_lowercase();
                    let yl = y.to_ascii_lowercase();
                    let ord = xl.cmp(&yl);
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

/// 文件名安全化：只替换 Windows 非法字符，中文/字母/数字/点/横线都保留。
pub fn safe_name(s: &str) -> String {
    let mut out = String::new();
    let mut last: Option<char> = None;
    for ch in s.chars() {
        let illegal = matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
        if illegal || ch.is_whitespace() {
            let kind = if illegal { 'i' } else { 's' };
            if last != Some(kind) {
                out.push('_');
                last = Some(kind);
            }
        } else {
            out.push(ch);
            last = None;
        }
    }
    let trimmed = out.trim_start_matches('.');
    let cut: String = trimmed.chars().take(40).collect();
    if cut.is_empty() {
        "key".to_string()
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_keeps_chinese_and_strips_illegal() {
        assert_eq!(safe_name("我的服务器 1"), "我的服务器_1");
        assert_eq!(safe_name("a/b:c"), "a_b_c");
        assert_eq!(safe_name("....x"), "x");
        assert_eq!(safe_name(""), "key");
        assert_eq!(safe_name(&"长".repeat(60)).chars().count(), 40);
    }

    #[test]
    fn natural_order_sorts_numbers_by_value() {
        let mut v = vec!["a10", "a2", "a1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a1", "a2", "a10"]);
    }
}
