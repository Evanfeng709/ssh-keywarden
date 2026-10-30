//! 桌面 ⇄ 手机的加密同步包。
//!
//! 只同步「怎么连」与归属信息：主机、钥匙备注、已知主机指纹，**不含私钥**。
//! 桌面 CLI（`kw-sync`）与手机 FFI 共用这一份实现，避免两端算法走样。

use std::collections::BTreeMap;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroize;

use kw_ssh::known_hosts::{HostEntry, KnownHosts};
use kw_store::{Host, MergeOutcome, NoteEntry, Store};

pub const SYNC_FORMAT: &str = "kw-sync";
pub const SYNC_VERSION: u32 = 2;
const MIN_VERSION: u32 = 1;
const SYNC_AAD: &[u8] = b"kw-sync-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncBundle {
    pub version: u32,
    pub exported_at: u64,
    pub hosts: Vec<Host>,
    pub key_notes: BTreeMap<String, NoteEntry>,
    pub known_hosts: Vec<HostEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// 用户已确认要覆盖的主机指纹，格式 `host:port`。
    pub trust_changed: Vec<String>,
    /// 一次性信任所有冲突（手机端「全部覆盖」按钮）。
    pub trust_all_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownHostConflict {
    /// `host:port`，用于回传给 trust_changed。
    pub key: String,
    pub host: String,
    pub port: u16,
    pub local_fingerprint: String,
    pub remote_fingerprint: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportReport {
    pub added_hosts: usize,
    pub updated_hosts: usize,
    pub deleted_hosts: usize,
    pub skipped_hosts: usize,
    /// 实际写入/更新/删除的备注条数。
    pub synced_notes: usize,
    pub skipped_notes: usize,
    pub added_known_hosts: usize,
    pub updated_known_hosts: usize,
    pub known_host_conflicts: Vec<KnownHostConflict>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 导出。给了口令就用 Argon2id + AES-256-GCM，否则是明文 JSON。
pub fn export_bundle(
    store: &Store,
    known_hosts: &KnownHosts,
    passphrase: Option<&str>,
) -> Result<String, String> {
    let bundle = SyncBundle {
        version: SYNC_VERSION,
        exported_at: now_secs(),
        hosts: store.all_hosts(),
        key_notes: store.all_key_notes(),
        known_hosts: known_hosts.list(),
    };
    match passphrase.filter(|p| !p.is_empty()) {
        Some(pass) => {
            let plain = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
            encrypt_bundle(&plain, pass)
        }
        None => Ok(json!({
            "format": SYNC_FORMAT,
            "version": SYNC_VERSION,
            "encrypted": false,
            "bundle": bundle,
        })
        .to_string()),
    }
}

/// 导入并合并。旧版本（v1）的包也能读，缺失的墓碑字段按 0 处理。
pub fn import_bundle(
    store: &Store,
    known_hosts: &KnownHosts,
    text: &str,
    passphrase: Option<&str>,
    opts: &ImportOptions,
) -> Result<ImportReport, String> {
    let bundle = decode_sync_bundle(text, passphrase)?;
    if !(MIN_VERSION..=SYNC_VERSION).contains(&bundle.version) {
        return Err(format!("不支持的同步包版本：{}", bundle.version));
    }

    let mut report = ImportReport::default();

    for host in &bundle.hosts {
        // 私钥路径不跨设备同步；本机已有配置由 merge_host 保留
        let mut host = host.clone();
        host.identity_file = String::new();
        match store.merge_host(&host) {
            Ok(MergeOutcome::Added) => report.added_hosts += 1,
            Ok(MergeOutcome::Updated) => report.updated_hosts += 1,
            Ok(MergeOutcome::Tombstoned) => report.deleted_hosts += 1,
            Ok(MergeOutcome::Skipped) => report.skipped_hosts += 1,
            Err(_) => report.skipped_hosts += 1,
        }
    }

    for (fp, entry) in &bundle.key_notes {
        match store.merge_note(fp, entry) {
            Ok(MergeOutcome::Skipped) => report.skipped_notes += 1,
            Ok(_) => report.synced_notes += 1,
            Err(_) => report.skipped_notes += 1,
        }
    }

    for kh in &bundle.known_hosts {
        let key = format!("{}:{}", kh.host, kh.port);
        match known_hosts.get(&kh.host, kh.port) {
            None => {
                if let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(&kh.public_key) {
                    known_hosts.save(&kh.host, kh.port, &blob);
                    report.added_known_hosts += 1;
                }
            }
            Some(local) if local.fingerprint == kh.fingerprint => {}
            Some(local) => {
                let trusted =
                    opts.trust_all_changed || opts.trust_changed.iter().any(|k| k == &key);
                if trusted {
                    if let Ok(blob) =
                        base64::engine::general_purpose::STANDARD.decode(&kh.public_key)
                    {
                        known_hosts.save(&kh.host, kh.port, &blob);
                        report.updated_known_hosts += 1;
                    }
                } else {
                    report.known_host_conflicts.push(KnownHostConflict {
                        key,
                        host: kh.host.clone(),
                        port: kh.port,
                        local_fingerprint: local.fingerprint,
                        remote_fingerprint: kh.fingerprint.clone(),
                    });
                }
            }
        }
    }

    Ok(report)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedEnvelope {
    format: String,
    version: u32,
    encrypted: bool,
    kdf: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

fn encrypt_bundle(plain: &[u8], pass: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);

    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(pass.as_bytes(), &salt, &mut key)
        .map_err(|e| format!("派生密钥失败：{e}"))?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("初始化加密器失败：{e}"))?;
    let ciphertext = cipher.encrypt(
        Nonce::from_slice(&nonce),
        Payload {
            msg: plain,
            aad: SYNC_AAD,
        },
    );
    key.zeroize();
    let ciphertext = ciphertext.map_err(|_| "加密同步包失败".to_string())?;

    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    Ok(json!(EncryptedEnvelope {
        format: SYNC_FORMAT.to_string(),
        version: SYNC_VERSION,
        encrypted: true,
        kdf: "argon2id".to_string(),
        salt: b64(&salt),
        nonce: b64(&nonce),
        ciphertext: b64(&ciphertext),
    })
    .to_string())
}

fn decode_sync_bundle(text: &str, passphrase: Option<&str>) -> Result<SyncBundle, String> {
    let env: Value = serde_json::from_str(text).map_err(|_| "同步包不是合法 JSON".to_string())?;
    if env.get("format").and_then(Value::as_str) != Some(SYNC_FORMAT) {
        return Err("不是 SSH Keywarden 同步包".to_string());
    }
    let version = env
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "同步包缺少版本号".to_string())?;
    if !(MIN_VERSION as u64..=SYNC_VERSION as u64).contains(&version) {
        return Err(format!("不支持的同步包版本：{version}"));
    }

    if env.get("encrypted").and_then(Value::as_bool) == Some(true) {
        let pass = passphrase
            .filter(|p| !p.is_empty())
            .ok_or_else(|| "这个同步包已加密，需要口令".to_string())?;
        let plain = decrypt_envelope(&env, pass)?;
        return serde_json::from_slice(&plain).map_err(|e| format!("同步包内容损坏：{e}"));
    }

    let bundle = env
        .get("bundle")
        .ok_or_else(|| "同步包缺少 bundle 字段".to_string())?;
    serde_json::from_value(bundle.clone()).map_err(|e| format!("同步包内容损坏：{e}"))
}

fn decrypt_envelope(env: &Value, pass: &str) -> Result<Vec<u8>, String> {
    let unb64 = |key: &str| -> Result<Vec<u8>, String> {
        let raw = env.get(key).and_then(Value::as_str).unwrap_or("");
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| format!("同步包的 {key} 字段损坏"))
    };
    let salt = unb64("salt")?;
    let nonce = unb64("nonce")?;
    let ciphertext = unb64("ciphertext")?;
    if salt.len() < 8 || nonce.len() != 12 {
        return Err("同步包的加密参数损坏".to_string());
    }

    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(pass.as_bytes(), &salt, &mut key)
        .map_err(|e| format!("派生密钥失败：{e}"))?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("初始化解密器失败：{e}"))?;
    let plain = cipher.decrypt(
        Nonce::from_slice(&nonce),
        Payload {
            msg: &ciphertext,
            aad: SYNC_AAD,
        },
    );
    key.zeroize();
    plain.map_err(|_| "解密失败：口令不正确或同步包已损坏".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kw_store::HostPatch;

    fn setup(dir: &std::path::Path) -> (Store, KnownHosts) {
        (
            Store::new(dir),
            KnownHosts::new(dir.join("known_hosts.json")),
        )
    }

    fn sample_host(store: &Store, name: &str) -> Host {
        store
            .upsert_host(HostPatch {
                id: None,
                name: Some(name.into()),
                host: Some("203.0.113.10".into()),
                port: Some(22),
                username: Some("root".into()),
                identity_file: None,
                remote_path: None,
            })
            .unwrap()
    }

    #[test]
    fn tombstone_survives_export_import() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, akh) = setup(a_dir.path());
        let (b, bkh) = setup(b_dir.path());

        let host = sample_host(&a, "prod");
        let first = export_bundle(&a, &akh, None).unwrap();
        let report = import_bundle(&b, &bkh, &first, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.added_hosts, 1);
        assert_eq!(b.list_hosts().len(), 1);

        a.remove_host(&host.id).unwrap();
        let second = export_bundle(&a, &akh, None).unwrap();
        let report = import_bundle(&b, &bkh, &second, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.deleted_hosts, 1);
        assert!(b.list_hosts().is_empty(), "删除必须跟着墓碑同步过去");

        // 旧的（删除前）包不能把主机复活
        let report = import_bundle(&b, &bkh, &first, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.added_hosts, 0);
        assert!(b.list_hosts().is_empty());
    }

    #[test]
    fn note_tombstone_and_newer_wins() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, akh) = setup(a_dir.path());
        let (b, bkh) = setup(b_dir.path());
        let fp = "SHA256:9xK2mP7vQ1nL8wR4tY6uI0oP3aS5dF7gH9jK1lZ3xCV";

        a.set_key_note(fp, "台式机").unwrap();
        let bundle = export_bundle(&a, &akh, None).unwrap();
        import_bundle(&b, &bkh, &bundle, None, &ImportOptions::default()).unwrap();
        assert_eq!(b.list_key_notes()[fp].note, "台式机");

        a.set_key_note(fp, "").unwrap();
        let bundle = export_bundle(&a, &akh, None).unwrap();
        import_bundle(&b, &bkh, &bundle, None, &ImportOptions::default()).unwrap();
        assert!(!b.list_key_notes().contains_key(fp), "删除备注要同步");

        // 设备 B 又写了一条更新的备注，A 的旧删除不应覆盖它
        std::thread::sleep(std::time::Duration::from_millis(2));
        b.set_key_note(fp, "B 设备新备注").unwrap();
        let older = export_bundle(&a, &akh, None).unwrap();
        import_bundle(&b, &bkh, &older, None, &ImportOptions::default()).unwrap();
        assert_eq!(b.list_key_notes()[fp].note, "B 设备新备注");
    }

    #[test]
    fn known_host_conflict_requires_opt_in() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, akh) = setup(a_dir.path());
        let (b, bkh) = setup(b_dir.path());

        akh.save("example.test", 22, b"remote-key");
        bkh.save("example.test", 22, b"local-key");
        let bundle = export_bundle(&a, &akh, None).unwrap();

        let report = import_bundle(&b, &bkh, &bundle, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.known_host_conflicts.len(), 1);
        assert_eq!(
            bkh.get("example.test", 22).unwrap().fingerprint,
            kw_keys::fingerprint_of_blob(b"local-key")
        );

        let report = import_bundle(
            &b,
            &bkh,
            &bundle,
            None,
            &ImportOptions {
                trust_all_changed: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.updated_known_hosts, 1);
        assert_eq!(
            bkh.get("example.test", 22).unwrap().fingerprint,
            kw_keys::fingerprint_of_blob(b"remote-key")
        );
    }

    #[test]
    fn v1_bundle_without_tombstones_still_imports() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, ahk) = setup(a_dir.path());
        let (b, bkh) = setup(b_dir.path());
        sample_host(&a, "old");
        let text = export_bundle(&a, &ahk, None).unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["version"] = json!(1);
        // 模拟旧版本：删掉 deletedAt 字段
        if let Some(hosts) = value["bundle"]["hosts"].as_array_mut() {
            for h in hosts {
                if let Some(obj) = h.as_object_mut() {
                    obj.remove("deletedAt");
                }
            }
        }
        let report = import_bundle(
            &b,
            &bkh,
            &value.to_string(),
            None,
            &ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(report.added_hosts, 1);
    }
}
