//! 桌面 ⇄ 手机的加密同步包。
//!
//! 同步「怎么连」与归属信息：主机、钥匙备注、已知主机指纹，
//! 以及**主机实际在用**的私钥（可选，且强制加密后才允许带上）。
//! 桌面 CLI（`kw-sync`）与手机 FFI 共用这一份实现，避免两端算法走样。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroize;

use kw_keys::find_private_key_by_fingerprint;
use kw_ssh::known_hosts::{HostEntry, KnownHosts};
use kw_store::{Host, MergeOutcome, NoteEntry, Store};

use crate::sessions::fingerprint_of_private_key;

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
    /// 主机实际在用的私钥。只有设置了同步口令才会带上（见 `export_bundle`）。
    #[serde(default)]
    pub private_keys: Vec<PrivateKeyEntry>,
}

/// 同步包里的一把私钥。
///
/// 只带指纹和原文，不带路径 —— 路径是各设备自己的事，落地时按下述规则重建。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateKeyEntry {
    pub fingerprint: String,
    /// OpenSSH / PEM 私钥原文；本身可能还带 passphrase。
    pub pem: String,
    /// 原文件名，仅作为落地时的命名参考。
    #[serde(default)]
    pub file_name: String,
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
    /// 落地的私钥数量（已存在且内容一致的不计）。
    pub imported_keys: usize,
    pub known_host_conflicts: Vec<KnownHostConflict>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 收集「主机实际在用」的私钥。
///
/// 不是把整个密钥库搬上去：只挑被某台存活主机引用的那些。
/// 加密私钥在不知道 passphrase 时算不出指纹，跳过并回报原因 ——
/// 这种跳过必须让用户看见，否则他会以为私钥已经同步好了。
fn collect_private_keys(store: &Store, keys_dir: &Path) -> (Vec<PrivateKeyEntry>, Vec<String>) {
    let mut out: Vec<PrivateKeyEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for host in store.list_hosts() {
        if host.deleted_at > 0 {
            continue;
        }
        let path = if !host.identity_file.trim().is_empty() {
            Some(PathBuf::from(host.identity_file.trim()))
        } else {
            find_private_key_by_fingerprint(keys_dir, &host.identity_fingerprint, 3)
                .map(|entry| PathBuf::from(entry.path))
        };
        let Some(path) = path else { continue };
        let Ok(text) = std::fs::read_to_string(&path) else {
            warnings.push(format!(
                "主机「{}」的私钥读不到，已跳过：{}",
                host.name,
                path.display()
            ));
            continue;
        };
        let fingerprint = fingerprint_of_private_key(&text, None);
        if fingerprint.is_empty() {
            warnings.push(format!(
                "主机「{}」的私钥已加密，没有 passphrase 无法识别，已跳过",
                host.name
            ));
            continue;
        }
        if !seen.insert(fingerprint.clone()) {
            continue;
        }
        out.push(PrivateKeyEntry {
            fingerprint,
            pem: text,
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
    }
    (out, warnings)
}

/// 把同步包里的私钥落到本机密钥库（0600）。
///
/// 已存在同名且内容一致的文件直接跳过 —— 反复同步不应该反复重写私钥文件。
fn write_private_key(keys_dir: &Path, entry: &PrivateKeyEntry) -> Result<PathBuf, String> {
    std::fs::create_dir_all(keys_dir).map_err(|e| format!("创建密钥目录失败：{e}"))?;
    // 指纹是 base64（含 `+` `/`），直接拿来当文件名会被 `/` 劈成子目录 ——
    // 只留字母数字，撞名概率依然极低，但路径一定合法
    let short: String = entry
        .fingerprint
        .trim_start_matches("SHA256:")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    let name = if short.is_empty() {
        entry.file_name.clone()
    } else {
        format!("synced_{short}.key")
    };
    let name = if name.trim().is_empty() {
        "synced.key".to_string()
    } else {
        name
    };
    let path = keys_dir.join(name);

    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == entry.pem {
            return Ok(path);
        }
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("写入私钥失败：{e}"))?;
        file.write_all(entry.pem.as_bytes())
            .map_err(|e| format!("写入私钥失败：{e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, entry.pem.as_bytes()).map_err(|e| format!("写入私钥失败：{e}"))?;
    }
    Ok(path)
}

/// 导出。给了口令就用 Argon2id + AES-256-GCM，否则是明文 JSON。
///
/// `include_keys` 为真但没给口令时**直接报错**：明文包里带私钥等于把它们
/// 摊在云盘上，宁可同步失败也不能悄悄降级。
pub fn export_bundle(
    store: &Store,
    known_hosts: &KnownHosts,
    keys_dir: &Path,
    include_keys: bool,
    passphrase: Option<&str>,
) -> Result<String, String> {
    let pass = passphrase.map(str::trim).filter(|p| !p.is_empty());
    if include_keys && pass.is_none() {
        return Err("要同步私钥必须先设置同步口令（不允许把私钥放进明文包）".to_string());
    }

    let private_keys = if include_keys {
        let (keys, warnings) = collect_private_keys(store, keys_dir);
        for w in &warnings {
            eprintln!("[sync] {w}");
        }
        keys
    } else {
        Vec::new()
    };

    let bundle = SyncBundle {
        version: SYNC_VERSION,
        exported_at: now_secs(),
        hosts: store.all_hosts(),
        key_notes: store.all_key_notes(),
        known_hosts: known_hosts.list(),
        private_keys,
    };
    match pass {
        Some(p) => {
            let plain = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
            encrypt_bundle(&plain, p)
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
    keys_dir: &Path,
    text: &str,
    passphrase: Option<&str>,
    opts: &ImportOptions,
) -> Result<ImportReport, String> {
    let bundle = decode_sync_bundle(text, passphrase)?;
    if !(MIN_VERSION..=SYNC_VERSION).contains(&bundle.version) {
        return Err(format!("不支持的同步包版本：{}", bundle.version));
    }

    let mut report = ImportReport::default();

    // 私钥先落地：后面的主机合并要靠指纹在本地密钥库里找到它们
    for key in &bundle.private_keys {
        match write_private_key(keys_dir, key) {
            Ok(_) => report.imported_keys += 1,
            Err(e) => eprintln!("[sync] 私钥落地失败（{}）：{e}", key.fingerprint),
        }
    }

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
                identity_fingerprint: None,
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
        let first = export_bundle(&a, &akh, &a.dir().join("keys"), false, None).unwrap();
        let report = import_bundle(&b, &bkh, &b.dir().join("keys"), &first, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.added_hosts, 1);
        assert_eq!(b.list_hosts().len(), 1);

        a.remove_host(&host.id).unwrap();
        let second = export_bundle(&a, &akh, &a.dir().join("keys"), false, None).unwrap();
        let report = import_bundle(&b, &bkh, &b.dir().join("keys"), &second, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.deleted_hosts, 1);
        assert!(b.list_hosts().is_empty(), "删除必须跟着墓碑同步过去");

        // 旧的（删除前）包不能把主机复活
        let report = import_bundle(&b, &bkh, &b.dir().join("keys"), &first, None, &ImportOptions::default()).unwrap();
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
        let bundle = export_bundle(&a, &akh, &a.dir().join("keys"), false, None).unwrap();
        import_bundle(&b, &bkh, &b.dir().join("keys"), &bundle, None, &ImportOptions::default()).unwrap();
        assert_eq!(b.list_key_notes()[fp].note, "台式机");

        a.set_key_note(fp, "").unwrap();
        let bundle = export_bundle(&a, &akh, &a.dir().join("keys"), false, None).unwrap();
        import_bundle(&b, &bkh, &b.dir().join("keys"), &bundle, None, &ImportOptions::default()).unwrap();
        assert!(!b.list_key_notes().contains_key(fp), "删除备注要同步");

        // 设备 B 又写了一条更新的备注，A 的旧删除不应覆盖它
        std::thread::sleep(std::time::Duration::from_millis(2));
        b.set_key_note(fp, "B 设备新备注").unwrap();
        let older = export_bundle(&a, &akh, &a.dir().join("keys"), false, None).unwrap();
        import_bundle(&b, &bkh, &b.dir().join("keys"), &older, None, &ImportOptions::default()).unwrap();
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
        let bundle = export_bundle(&a, &akh, &a.dir().join("keys"), false, None).unwrap();

        let report = import_bundle(&b, &bkh, &b.dir().join("keys"), &bundle, None, &ImportOptions::default()).unwrap();
        assert_eq!(report.known_host_conflicts.len(), 1);
        assert_eq!(
            bkh.get("example.test", 22).unwrap().fingerprint,
            kw_keys::fingerprint_of_blob(b"local-key")
        );

        let report = import_bundle(
            &b,
            &bkh,
            &b.dir().join("keys"),
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
        let text = export_bundle(&a, &ahk, &a.dir().join("keys"), false, None).unwrap();
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
            &b.dir().join("keys"),
            &value.to_string(),
            None,
            &ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(report.added_hosts, 1);
    }

    #[test]
    fn private_keys_need_passphrase_and_land_on_import() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, akh) = setup(a_dir.path());
        let (b, bkh) = setup(b_dir.path());

        // A 上生成一把钥匙放进密钥库，并把主机指向它
        let keys_a = a.dir().join("keys");
        std::fs::create_dir_all(&keys_a).unwrap();
        let gen = kw_keys::generate_ed25519(Some("sync@test"), None).unwrap();
        let key_path = keys_a.join("id_test");
        std::fs::write(&key_path, &gen.private_key_pem).unwrap();
        let host = sample_host(&a, "withkey");
        // upsert_host 是「传什么写什么」的全量语义，字段必须一条不落
        a.upsert_host(HostPatch {
            id: Some(host.id.clone()),
            name: Some(host.name.clone()),
            host: Some(host.host.clone()),
            port: Some(host.port),
            username: Some(host.username.clone()),
            identity_file: Some(key_path.display().to_string()),
            identity_fingerprint: Some(gen.fingerprint.clone()),
            remote_path: Some(host.remote_path.clone()),
        })
        .unwrap();

        // 要带私钥却不给口令 —— 必须拒绝，不能悄悄退化成明文包
        assert!(
            export_bundle(&a, &akh, &keys_a, true, None).is_err(),
            "没有口令时不允许把私钥放进包里"
        );

        let text = export_bundle(&a, &akh, &keys_a, true, Some("correct horse")).unwrap();
        let b_keys = b.dir().join("keys");
        let report = import_bundle(
            &b,
            &bkh,
            &b_keys,
            &text,
            Some("correct horse"),
            &ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(report.imported_keys, 1);

        // 落地后能按指纹在本地找回来 —— 这是「换设备不用改路径」的关键
        let found = kw_keys::find_private_key_by_fingerprint(&b_keys, &gen.fingerprint, 3)
            .expect("导入后应能按指纹找到私钥");
        assert_eq!(found.fingerprint, gen.fingerprint);

        // 指纹跟着同步了；路径是本机的，不该被覆盖成 A 的路径
        let imported = b
            .list_hosts()
            .into_iter()
            .find(|h| h.name == "withkey")
            .unwrap();
        assert_eq!(imported.identity_fingerprint, gen.fingerprint);
        assert_ne!(imported.identity_file, key_path.display().to_string());
    }
}
