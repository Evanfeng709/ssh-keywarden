//! 远端路径的小工具：POSIX basename 与「不能带换行/空字符」的安全校验。

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    #[error("远端路径为空")]
    Empty,
    #[error("远端路径包含非法字符")]
    IllegalChar,
}

/// 与 `path.posix.basename` 同义（只认 `/`，`\` 是普通字符）。
pub fn posix_basename(p: &str) -> &str {
    if p.is_empty() {
        return "";
    }
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

/// 与 `path.posix.dirname` 同义（只认 `/`）。
pub fn posix_dirname(p: &str) -> &str {
    if p.is_empty() {
        return ".";
    }
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    match trimmed.rfind('/') {
        Some(0) => "/",
        Some(i) => &trimmed[..i],
        None => ".",
    }
}

pub fn assert_safe_path(p: &str) -> Result<&str, PathError> {
    if p.is_empty() {
        return Err(PathError::Empty);
    }
    if p.contains(['\r', '\n', '\0']) {
        return Err(PathError::IllegalChar);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_matches_posix() {
        assert_eq!(posix_basename(""), "");
        assert_eq!(posix_basename("/"), "/");
        assert_eq!(posix_basename("///"), "/");
        assert_eq!(posix_basename("a/"), "a");
        assert_eq!(
            posix_basename("/home/alice/.ssh/authorized_keys"),
            "authorized_keys"
        );
        assert_eq!(
            posix_basename("C:\\x\\authorized_keys"),
            "C:\\x\\authorized_keys"
        );
    }

    #[test]
    fn safe_path_rejects_newlines() {
        assert!(assert_safe_path("").is_err());
        assert!(assert_safe_path("a\nb").is_err());
        assert!(assert_safe_path("/home/a\r").is_err());
        assert!(assert_safe_path("/home/a").is_ok());
    }
}
