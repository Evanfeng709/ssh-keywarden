//! 敏感凭据（登录密码 / 私钥 passphrase）的内存包装。

use serde::{Deserialize, Deserializer};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// 拥有的字符串在释放时清零；不实现 Serialize，避免隐式导出凭据。
#[derive(Clone, Default, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出明文。调用这个函数本身就是一次「我确实要用它」的表态。
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 只去掉首尾空白后的非空明文；全空白视同没给。
    pub fn trimmed_non_empty(&self) -> Option<&str> {
        let s = self.0.trim();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl From<String> for Secret {
    fn from(v: String) -> Self {
        Self(v)
    }
}

impl From<&str> for Secret {
    fn from(v: &str) -> Self {
        Self(v.to_owned())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只区分空/非空，连长度都不给 —— 免得从长度反推密码
        f.write_str(if self.0.is_empty() {
            "Secret(<empty>)"
        } else {
            "Secret(***)"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_plaintext() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(format!("{:?}", Secret::default()), "Secret(<empty>)");
    }

    #[test]
    fn expose_returns_plaintext() {
        assert_eq!(Secret::new("abc").expose(), "abc");
    }

    #[test]
    fn empty_detection() {
        assert!(Secret::new("").is_empty());
        assert!(!Secret::new("x").is_empty());
        assert_eq!(Secret::new("  ").trimmed_non_empty(), None);
        assert_eq!(Secret::new(" pw ").trimmed_non_empty(), Some("pw"));
    }

    #[test]
    fn deserializes_from_json() {
        let s: Secret = serde_json::from_str(r#""hunter2""#).unwrap();
        assert_eq!(s.expose(), "hunter2");
        let spaced: Secret = serde_json::from_str(r#"" pw ""#).unwrap();
        assert_eq!(spaced.expose(), " pw ");
        assert!(serde_json::from_str::<Secret>("42").is_err());
        assert!(serde_json::from_str::<Secret>("null").is_err());
    }

    #[test]
    fn explicit_zeroize_clears_string() {
        let mut secret = Secret::new("sensitive");
        secret.zeroize();
        assert!(secret.is_empty());
    }
}
