//! 公钥解析 / 指纹 / 本地密钥对生成。
//!
//! 与旧 Node 版 `src/keygen.js` 行为对齐：
//!  * 指纹 = `SHA256:` + base64(sha256(公钥 wire blob))，去掉 `=` 填充；
//!  * 一行公钥允许带 options 前缀，类型在中间靠后，后面都是 comment；
//!  * 解析失败不返回 None，而是 `valid=false` 并带上原因 —— 前端要把它显示出来。

use base64::Engine;
use rand::rngs::OsRng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use thiserror::Error;

pub const SUPPORTED_KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ssh-dss",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

pub fn is_supported_key_type(t: &str) -> bool {
    SUPPORTED_KEY_TYPES.contains(&t)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedPublicKey {
    #[serde(rename = "type")]
    pub key_type: String,
    pub b64: String,
    pub options: String,
    pub comment: String,
    pub fingerprint: String,
    pub valid: bool,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedKey {
    #[serde(rename = "type")]
    pub key_type: String,
    pub private_key_pem: String,
    pub public_key_line: String,
    pub fingerprint: String,
    pub comment: String,
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateKeyInfo {
    pub encrypted: bool,
    #[serde(rename = "type")]
    pub key_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Error)]
pub enum KeyGenError {
    #[error("生成密钥失败：{0}")]
    Generate(String),
    #[error("私钥解析失败：{0}")]
    Parse(String),
    #[error("生成的私钥与公钥不匹配（内部错误）")]
    Mismatch,
    #[error("加密私钥竟然能在无密码时解开（内部错误）")]
    EncryptionNotEffective,
}

pub fn fingerprint_of_blob(blob: &[u8]) -> String {
    let digest = Sha256::digest(blob);
    let b64 = base64::engine::general_purpose::STANDARD.encode(digest);
    format!("SHA256:{}", b64.trim_end_matches('='))
}

/// 宽松 base64 解码：Node 的 `Buffer.from(s, 'base64')` 会跳过非法字符、
/// 接受 base64url 字母表并容忍缺失的填充。严格引擎会直接报错，行为对不上。
fn decode_b64_lenient(s: &str) -> Vec<u8> {
    let mut cleaned = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => cleaned.push(ch),
            '-' => cleaned.push('+'),
            '_' => cleaned.push('/'),
            _ => {}
        }
    }
    // 长度 mod 4 == 1 时最后一个字符不构成完整字节，Node 会丢弃它
    if cleaned.len() % 4 == 1 {
        cleaned.pop();
    }
    while cleaned.len() % 4 != 0 {
        cleaned.push('=');
    }
    base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .unwrap_or_default()
}

/// 一行公钥 → 结构化条目；无法识别返回 None。
pub fn parse_public_key_line(line: &str) -> Option<ParsedPublicKey> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let idx = tokens.iter().position(|t| is_supported_key_type(t))?;
    if idx + 1 >= tokens.len() {
        return None;
    }

    let key_type = tokens[idx].to_string();
    let b64 = tokens[idx + 1].to_string();
    let options = tokens[..idx].join(" ");
    let comment = tokens[idx + 2..].join(" ");
    let blob = decode_b64_lenient(&b64);
    if blob.is_empty() {
        return None;
    }

    let mut parsed = ParsedPublicKey {
        key_type,
        b64,
        options,
        comment,
        fingerprint: fingerprint_of_blob(&blob),
        valid: true,
        error: String::new(),
    };

    match ssh_key::PublicKey::from_openssh(&format!("{} {}", parsed.key_type, parsed.b64)) {
        Ok(pk) => {
            parsed.key_type = pk.algorithm().as_str().to_string();
            if let Ok(wire) = pk.to_bytes() {
                parsed.fingerprint = fingerprint_of_blob(&wire);
            }
        }
        Err(e) => {
            parsed.valid = false;
            parsed.error = e.to_string();
        }
    }

    Some(parsed)
}

/// 多行公钥文本 → 条目数组（跳过空行与注释行）。
pub fn parse_public_keys(text: &str) -> Vec<ParsedPublicKey> {
    text.split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(parse_public_key_line)
        .collect()
}

/// 注释归一化：所有空白折叠成 `_`，最多 80 个字符。
pub fn normalize_comment(comment: &str) -> String {
    let joined = comment.split_whitespace().collect::<Vec<_>>().join("_");
    joined.chars().take(80).collect()
}

fn comment_or_default(comment: Option<&str>) -> String {
    let c = normalize_comment(comment.unwrap_or(""));
    if c.is_empty() {
        "ssh-keywarden".to_string()
    } else {
        c
    }
}

/// 生成 ed25519 密钥对（OpenSSH 容器格式；给了 passphrase 就加密）。
pub fn generate_ed25519(
    comment: Option<&str>,
    passphrase: Option<&str>,
) -> Result<GeneratedKey, KeyGenError> {
    let c = comment_or_default(comment);
    let mut rng = OsRng;
    let mut key = PrivateKey::random(&mut rng, Algorithm::Ed25519)
        .map_err(|e| KeyGenError::Generate(e.to_string()))?;
    key.set_comment(&c);
    finish_generated(key, c, passphrase, &mut rng)
}

/// 生成 RSA 密钥对。默认 3072 位，与旧版界面默认值一致。
pub fn generate_rsa(
    comment: Option<&str>,
    bits: usize,
    passphrase: Option<&str>,
) -> Result<GeneratedKey, KeyGenError> {
    let c = comment_or_default(comment);
    let mut rng = OsRng;
    let rsa_key = rsa::RsaPrivateKey::new(&mut rng, bits)
        .map_err(|e| KeyGenError::Generate(e.to_string()))?;
    let keypair = ssh_key::private::RsaKeypair::try_from(rsa_key)
        .map_err(|e| KeyGenError::Generate(e.to_string()))?;
    let mut key = PrivateKey::from(keypair);
    key.set_comment(&c);
    finish_generated(key, c, passphrase, &mut rng)
}

fn finish_generated(
    key: PrivateKey,
    comment: String,
    passphrase: Option<&str>,
    rng: &mut OsRng,
) -> Result<GeneratedKey, KeyGenError> {
    let pass = passphrase.filter(|p| !p.is_empty());

    let mut public = key.public_key().clone();
    public.set_comment(&comment);
    let expected_blob = public
        .to_bytes()
        .map_err(|e| KeyGenError::Generate(e.to_string()))?;
    let key_fingerprint = fingerprint_of_blob(&expected_blob);

    let key = match pass {
        Some(p) => key
            .encrypt(rng, p)
            .map_err(|e| KeyGenError::Generate(e.to_string()))?,
        None => key,
    };

    let private_key_pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| KeyGenError::Generate(e.to_string()))?
        .to_string();
    let public_key_line = public
        .to_openssh()
        .map_err(|e| KeyGenError::Generate(e.to_string()))?;

    assert_round_trip(&private_key_pem, &expected_blob, pass.is_some())?;

    Ok(GeneratedKey {
        key_type: public.algorithm().as_str().to_string(),
        private_key_pem,
        public_key_line,
        fingerprint: key_fingerprint,
        comment,
        encrypted: pass.is_some(),
    })
}

/// 生成后立刻校验：私钥能被回读，公钥与预期 blob 完全一致，
/// 设了密码就必须真的挡住无密码解析。
fn assert_round_trip(pem: &str, expected_blob: &[u8], encrypted: bool) -> Result<(), KeyGenError> {
    let parsed = PrivateKey::from_openssh(pem).map_err(|e| KeyGenError::Parse(e.to_string()))?;
    if parsed.is_encrypted() != encrypted {
        return Err(KeyGenError::Mismatch);
    }
    let wire = parsed
        .public_key()
        .to_bytes()
        .map_err(|e| KeyGenError::Parse(e.to_string()))?;
    if wire != expected_blob {
        return Err(KeyGenError::Mismatch);
    }
    if encrypted && parsed.decrypt("").is_ok() {
        return Err(KeyGenError::EncryptionNotEffective);
    }
    Ok(())
}

/// 读取私钥文本，返回 `{ encrypted, type, error }`。
///
/// 旧版生成的 RSA 私钥是 PKCS#1 PEM，ssh-key 只认 OpenSSH 容器，这里补一条兜底。
pub fn inspect_private_key(text: &str) -> PrivateKeyInfo {
    if let Ok(key) = PrivateKey::from_openssh(text) {
        if key.is_encrypted() {
            return PrivateKeyInfo {
                encrypted: true,
                key_type: String::new(),
                error: None,
            };
        }
        return PrivateKeyInfo {
            encrypted: false,
            key_type: key.algorithm().as_str().to_string(),
            error: None,
        };
    }

    if text.contains("ENCRYPTED") && text.contains("PRIVATE KEY") {
        return PrivateKeyInfo {
            encrypted: true,
            key_type: String::new(),
            error: None,
        };
    }

    use rsa::pkcs1::DecodeRsaPrivateKey;
    if let Ok(_) = rsa::RsaPrivateKey::from_pkcs1_pem(text) {
        return PrivateKeyInfo {
            encrypted: false,
            key_type: "ssh-rsa".to_string(),
            error: None,
        };
    }

    use rsa::pkcs8::DecodePrivateKey;
    if rsa::RsaPrivateKey::from_pkcs8_pem(text).is_ok() {
        return PrivateKeyInfo {
            encrypted: false,
            key_type: "ssh-rsa".to_string(),
            error: None,
        };
    }

    PrivateKeyInfo {
        encrypted: false,
        key_type: String::new(),
        error: Some("无法解析的私钥格式".to_string()),
    }
}

/// 从私钥文本反推公钥 wire blob（仅未加密时可用）。
pub fn public_key_blob_of_private(text: &str) -> Option<Vec<u8>> {
    if let Ok(key) = PrivateKey::from_openssh(text) {
        if key.is_encrypted() {
            return None;
        }
        return key.public_key().to_bytes().ok();
    }

    use rsa::pkcs1::DecodeRsaPrivateKey;
    if let Ok(rsa) = rsa::RsaPrivateKey::from_pkcs1_pem(text) {
        let keypair = ssh_key::private::RsaKeypair::try_from(rsa).ok()?;
        return PrivateKey::from(keypair).public_key().to_bytes().ok();
    }

    use rsa::pkcs8::DecodePrivateKey;
    if let Ok(rsa) = rsa::RsaPrivateKey::from_pkcs8_pem(text) {
        let keypair = ssh_key::private::RsaKeypair::try_from(rsa).ok()?;
        return PrivateKey::from(keypair).public_key().to_bytes().ok();
    }

    None
}

/// 认证前把私钥文本变成可用私钥。错误文案沿用旧版 `friendlyError` 的措辞。
pub fn load_private_key(text: &str, passphrase: Option<&str>) -> Result<PrivateKey, String> {
    const NEED_PASS: &str = "这把私钥已加密，需要填写 passphrase 才能使用";
    const BAD_PASS: &str = "私钥密码（passphrase）不正确，这把私钥解不开。请再确认一下";

    if let Ok(key) = PrivateKey::from_openssh(text) {
        if !key.is_encrypted() {
            return Ok(key);
        }
        let pass = passphrase.filter(|p| !p.is_empty()).ok_or(NEED_PASS)?;
        return key
            .decrypt(pass.as_bytes())
            .map_err(|_| BAD_PASS.to_string());
    }

    use rsa::pkcs1::DecodeRsaPrivateKey;
    if let Ok(rsa) = rsa::RsaPrivateKey::from_pkcs1_pem(text) {
        return rsa_to_ssh_key(rsa);
    }
    use rsa::pkcs8::DecodePrivateKey;
    if let Ok(rsa) = rsa::RsaPrivateKey::from_pkcs8_pem(text) {
        return rsa_to_ssh_key(rsa);
    }

    // 走到这里还带 ENCRYPTED 标记，说明是加密的老式 PEM：能识别但解不开
    if text.contains("ENCRYPTED") && text.contains("PRIVATE KEY") {
        return Err(if passphrase.is_some_and(|p| !p.is_empty()) {
            "私钥无法解析：暂不支持这种加密 PEM 格式，请转换成 OpenSSH 格式再试".to_string()
        } else {
            NEED_PASS.to_string()
        });
    }

    Err(format!(
        "私钥无法解析：{}",
        inspect_private_key(text)
            .error
            .unwrap_or_else(|| "格式无法识别".into())
    ))
}

fn rsa_to_ssh_key(rsa: rsa::RsaPrivateKey) -> Result<PrivateKey, String> {
    let pair =
        ssh_key::private::RsaKeypair::try_from(rsa).map_err(|e| format!("私钥无法解析：{e}"))?;
    Ok(PrivateKey::from(pair))
}

/// 把私钥解成未加密的 OpenSSH 文本。
///
/// russh 锁的是 ssh-key 0.7（本 crate 是 0.6），类型不能直接跨过去；
/// 解密与报错文案都留在本层，russh 那边只做一次解析。
pub fn private_key_openssh(text: &str, passphrase: Option<&str>) -> Result<String, String> {
    let key = load_private_key(text, passphrase)?;
    let pem = key
        .to_openssh(ssh_key::LineEnding::LF)
        .map_err(|e| format!("私钥无法序列化：{e}"))?;
    Ok(pem.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_base64_without_padding() {
        let fp = fingerprint_of_blob(b"hello");
        assert!(fp.starts_with("SHA256:"));
        assert_eq!(fp.len(), "SHA256:".len() + 43);
        assert!(!fp.ends_with('='));
    }

    #[test]
    fn lenient_base64_matches_node_semantics() {
        assert_eq!(decode_b64_lenient("aGVsbG8="), b"hello");
        assert_eq!(decode_b64_lenient("aGVsbG8"), b"hello");
        assert_eq!(decode_b64_lenient("aGVs bG8="), b"hello");
        assert_eq!(decode_b64_lenient("!!!"), b"");
    }

    #[test]
    fn comment_normalization() {
        assert_eq!(normalize_comment("  alice@laptop \n\t "), "alice@laptop");
        assert_eq!(normalize_comment("a  b\tc"), "a_b_c");
        assert_eq!(normalize_comment(&"x".repeat(100)).chars().count(), 80);
        assert_eq!(comment_or_default(None), "ssh-keywarden");
        assert_eq!(comment_or_default(Some("   ")), "ssh-keywarden");
    }

    #[test]
    fn load_private_key_handles_plain_and_encrypted() {
        let plain = generate_ed25519(Some("t"), None).unwrap();
        let key = load_private_key(&plain.private_key_pem, None).unwrap();
        assert!(!key.is_encrypted());

        let locked = generate_ed25519(Some("t"), Some("pass")).unwrap();
        let err = load_private_key(&locked.private_key_pem, None).unwrap_err();
        assert!(err.contains("需要填写 passphrase"), "{err}");
        let err = load_private_key(&locked.private_key_pem, Some("wrong")).unwrap_err();
        assert!(err.contains("passphrase）不正确"), "{err}");
        let key = load_private_key(&locked.private_key_pem, Some("pass")).unwrap();
        assert!(!key.is_encrypted());
        assert_eq!(
            key.public_key().to_bytes().unwrap(),
            PlainBlob::of(&locked.public_key_line)
        );
    }

    #[test]
    fn load_private_key_rejects_garbage() {
        let err = load_private_key("not a key", None).unwrap_err();
        assert!(err.starts_with("私钥无法解析"), "{err}");
    }

    struct PlainBlob;
    impl PlainBlob {
        fn of(public_line: &str) -> Vec<u8> {
            decode_b64_lenient(public_line.split_whitespace().nth(1).unwrap())
        }
    }
}
