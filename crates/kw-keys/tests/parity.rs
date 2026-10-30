//! 与旧版 `tests/run.js` 的规则层断言逐条对齐。
//!
//! 这些测试是移植的验收标准：行为只能与 Node 版一致，不能「顺手改进」。

use kw_keys::authorized::{append_key, parse_authorized_keys, render_without, AppendError};
use kw_keys::backups::{backup_path_for, diff_backup, display_time, parse_backup_name};
use kw_keys::keygen::{
    self, generate_ed25519, generate_rsa, inspect_private_key, parse_public_key_line, GeneratedKey,
};
use kw_keys::localkeys::{scan_keys, LocalKeyKind};
use kw_keys::sshd::{parse_config_text, summarize, SshdState};
use ssh_key::PrivateKey;
use std::fs;

fn fingerprint_re_ok(fp: &str) -> bool {
    let Some(rest) = fp.strip_prefix("SHA256:") else {
        return false;
    };
    rest.len() == 43
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

fn assert_encrypted_key(key: &GeneratedKey, passphrase: &str) {
    assert!(key.encrypted, "应标记为加密");
    let parsed = PrivateKey::from_openssh(&key.private_key_pem).expect("容器应能解析");
    assert!(parsed.is_encrypted(), "容器应处于加密状态");
    let unlocked = parsed.decrypt(passphrase).expect("正确密码应能解开");
    let blob = unlocked.public_key().to_bytes().unwrap();
    assert_eq!(keygen::fingerprint_of_blob(&blob), key.fingerprint);
    assert!(parsed.decrypt("wrong-pass").is_err(), "错误密码必须失败");
    assert!(parsed.decrypt("").is_err(), "空密码必须失败");
}

#[test]
fn ed25519_generate_public_line_and_fingerprint() {
    let k = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let p = parse_public_key_line(&k.public_key_line).expect("公钥行应能解析");
    assert_eq!(p.key_type, "ssh-ed25519");
    assert_eq!(p.fingerprint, k.fingerprint);
    assert_eq!(p.comment, "alice@laptop");
    assert!(p.valid);
}

#[test]
fn ed25519_private_key_is_openssh_container() {
    let k = generate_ed25519(Some("alice@laptop"), None).unwrap();
    assert!(k
        .private_key_pem
        .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    let parsed = PrivateKey::from_openssh(&k.private_key_pem).unwrap();
    assert_eq!(parsed.algorithm().as_str(), "ssh-ed25519");
    assert!(!parsed.is_encrypted());
}

#[test]
fn fingerprint_format_is_sha256_without_padding() {
    let k = generate_ed25519(Some("alice@laptop"), None).unwrap();
    assert!(
        fingerprint_re_ok(&k.fingerprint),
        "指纹格式不对：{}",
        k.fingerprint
    );
}

#[test]
fn two_generated_keys_differ() {
    let a = generate_ed25519(Some("a"), None).unwrap();
    let b = generate_ed25519(Some("b"), None).unwrap();
    assert_ne!(a.fingerprint, b.fingerprint);
}

#[test]
fn rsa_generate_type_and_fingerprint() {
    let k = generate_rsa(Some("legacy@server"), 2048, None).unwrap();
    let p = parse_public_key_line(&k.public_key_line).unwrap();
    assert_eq!(p.key_type, "ssh-rsa");
    assert_eq!(p.fingerprint, k.fingerprint);
    assert!(p.valid);
    assert!(k
        .private_key_pem
        .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    let parsed = PrivateKey::from_openssh(&k.private_key_pem).unwrap();
    assert_eq!(parsed.algorithm().as_str(), "ssh-rsa");
}

#[test]
fn authorized_line_classification() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();
    let mixed = [
        "# 这是注释，应当原样保留",
        "",
        ed1.public_key_line.as_str(),
        &format!("command=\"echo hi\",no-pty {}", ed2.public_key_line),
        "garbage-not-a-key",
        ed1.public_key_line.as_str(),
    ]
    .join("\n")
        + "\n";

    let parsed = parse_authorized_keys(&mixed);
    let kinds: Vec<&str> = parsed.items.iter().map(|i| i.kind().as_str()).collect();
    assert_eq!(
        kinds,
        vec!["comment", "blank", "key", "key", "malformed", "key"]
    );
}

#[test]
fn authorized_options_prefix_extracted() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();
    let text = format!("command=\"echo hi\",no-pty {}\n", ed2.public_key_line);
    let parsed = parse_authorized_keys(&text);
    let item = &parsed.items[0];
    let key = item.key().unwrap();
    assert_eq!(key.options, "command=\"echo hi\",no-pty");
    assert_eq!(key.fingerprint, ed2.fingerprint);
    let _ = ed1;
}

#[test]
fn authorized_duplicate_marked() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();
    let text = format!(
        "{}\n{}\n{}\n",
        ed1.public_key_line, ed2.public_key_line, ed1.public_key_line
    );
    let parsed = parse_authorized_keys(&text);
    let keys: Vec<_> = parsed.key_items().collect();
    assert_eq!(keys.len(), 3);
    match &keys[0].data {
        kw_keys::ItemData::Key { duplicate_of, .. } => assert_eq!(*duplicate_of, None),
        _ => panic!("应为 key"),
    }
    match &keys[2].data {
        kw_keys::ItemData::Key { duplicate_of, .. } => {
            assert_eq!(*duplicate_of, Some(keys[0].index))
        }
        _ => panic!("应为 key"),
    }
}

#[test]
fn authorized_malformed_keeps_reason() {
    let parsed = parse_authorized_keys("garbage-not-a-key\n");
    let item = parsed
        .items
        .iter()
        .find(|i| i.kind().as_str() == "malformed")
        .unwrap();
    match &item.data {
        kw_keys::ItemData::Malformed { reason } => assert!(!reason.is_empty()),
        _ => panic!("应为 malformed"),
    }
}

#[test]
fn render_without_only_drops_selected_lines() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();
    let text = format!(
        "# 这是注释\n\n{}\n{}\ngarbage-not-a-key\n{}\n",
        ed1.public_key_line, ed2.public_key_line, ed1.public_key_line
    );
    let parsed = parse_authorized_keys(&text);
    let out = render_without(&parsed, &[2, 5]);
    assert!(!out.contains("alice@laptop"), "被删的行不应出现");
    assert!(out.contains("# 这是注释"), "注释要保留");
    assert!(out.contains("garbage-not-a-key"), "无法解析的行要保留");
    assert!(out.contains("bob@nas"), "未选中的密钥要保留");
    assert!(out.ends_with('\n'), "文件应以换行结尾");
}

#[test]
fn append_key_appends_new() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let base = parse_authorized_keys("# head\n");
    let r = append_key(&base, &ed1.public_key_line).unwrap();
    assert!(r.appended);
    let text = r.text.unwrap();
    assert!(text.contains(&ed1.public_key_line));
    assert!(text.starts_with("# head\n"));
}

#[test]
fn append_key_is_idempotent() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let base = parse_authorized_keys(&format!("{}\n", ed1.public_key_line));
    let r = append_key(&base, &ed1.public_key_line).unwrap();
    assert!(!r.appended);
    assert!(r.reason.unwrap().contains("已在服务器上"));
}

#[test]
fn append_key_rejects_garbage() {
    let base = parse_authorized_keys("");
    let err = append_key(&base, "not-a-key").unwrap_err();
    assert_eq!(err, AppendError::Unrecognized);
    assert!(err.to_string().contains("无法识别"));
}

#[test]
fn empty_file_parses_without_error() {
    let parsed = parse_authorized_keys("");
    assert!(parsed.items.is_empty());
    assert!(!parsed.existed);
}

#[test]
fn scan_finds_private_and_public_keys() {
    let dir = tempfile::tempdir().unwrap();
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();

    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("id_test"), &ed1.private_key_pem).unwrap();
    fs::write(
        dir.path().join("id_test.pub"),
        format!("{}\n", ed1.public_key_line),
    )
    .unwrap();
    fs::write(
        dir.path().join("sub").join("only.pub"),
        format!("{}\n", ed2.public_key_line),
    )
    .unwrap();
    fs::write(dir.path().join("notes.txt"), "hello world").unwrap();

    let r = scan_keys(dir.path(), 3);
    assert_eq!(r.errors.len(), 0);

    let priv_entry = r.entries.iter().find(|e| e.name == "id_test").unwrap();
    assert_eq!(priv_entry.kind, LocalKeyKind::Private);
    assert_eq!(priv_entry.encrypted, Some(false));
    assert_eq!(
        priv_entry.fingerprint, ed1.fingerprint,
        "应通过同名 .pub 配对"
    );

    let pub_entry = r.entries.iter().find(|e| e.name == "id_test.pub").unwrap();
    assert_eq!(pub_entry.kind, LocalKeyKind::Public);
    assert_eq!(pub_entry.fingerprint, ed1.fingerprint);

    let only = r.entries.iter().find(|e| e.name == "only.pub").unwrap();
    assert_eq!(only.kind, LocalKeyKind::Public);
    assert_eq!(only.fingerprint, ed2.fingerprint);

    assert!(!r.entries.iter().any(|e| e.name == "notes.txt"));
}

#[test]
fn scan_missing_dir_reports_error() {
    let missing = std::env::temp_dir().join(format!("kw-not-exist-{}", std::process::id()));
    let r = scan_keys(&missing, 3);
    assert!(r.entries.is_empty());
    assert_eq!(r.errors.len(), 1);
}

#[test]
fn sshd_defaults_allow_password() {
    let s = summarize(&parse_config_text(""));
    assert!(s.password_auth);
    assert!(s.pubkey_auth);
    assert!(!s.key_only);
    assert_eq!(s.state, SshdState::Both);
}

#[test]
fn sshd_password_no_but_kbd_on_is_not_key_only() {
    let s = summarize(&parse_config_text("PasswordAuthentication no\n"));
    assert!(!s.password_auth);
    assert!(s.kbd_interactive);
    assert!(!s.key_only, "PAM 键盘交互仍可能用密码登录");
}

#[test]
fn sshd_all_three_password_channels_off_is_key_only() {
    let text = [
        "PasswordAuthentication no",
        "KbdInteractiveAuthentication no",
        "ChallengeResponseAuthentication no",
    ]
    .join("\n");
    let s = summarize(&parse_config_text(&text));
    assert!(s.key_only);
    assert!(s.pubkey_auth);
}

#[test]
fn sshd_comments_ignored() {
    let s = summarize(&parse_config_text("#PasswordAuthentication no\n"));
    assert!(s.password_auth);
}

#[test]
fn sshd_case_and_spaces() {
    let s = summarize(&parse_config_text("  PASSWORDauthentication    NO  \n"));
    assert!(!s.password_auth);
}

#[test]
fn sshd_first_occurrence_wins() {
    let s = summarize(&parse_config_text(
        "PasswordAuthentication no\nPasswordAuthentication yes\n",
    ));
    assert!(!s.password_auth);
}

#[test]
fn sshd_pubkey_off_is_not_key_only() {
    let s = summarize(&parse_config_text(
        "PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication no\n",
    ));
    assert!(!s.pubkey_auth);
    assert!(!s.key_only);
}

#[test]
fn sshd_four_states() {
    let st = |text: &str| summarize(&parse_config_text(text)).state;
    assert_eq!(st(""), SshdState::Both);
    assert_eq!(
        st("PasswordAuthentication no\nKbdInteractiveAuthentication no\n"),
        SshdState::KeyOnly
    );
    assert_eq!(st("PubkeyAuthentication no\n"), SshdState::PasswordOnly);
    assert_eq!(
        st("PubkeyAuthentication no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n"),
        SshdState::None
    );
}

#[test]
fn encrypted_ed25519_round_trip() {
    let k = generate_ed25519(Some("enc@test"), Some("test-pass-123")).unwrap();
    assert_encrypted_key(&k, "test-pass-123");
}

#[test]
fn encrypted_container_declares_aes256_ctr_and_bcrypt() {
    let k = generate_ed25519(Some("enc2@test"), Some("p")).unwrap();
    let body: String = k
        .private_key_pem
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with("-----END"))
        .collect();
    use base64::Engine;
    let buf = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .unwrap();
    assert_eq!(&buf[..15], b"openssh-key-v1\0");
    assert!(
        buf.windows(10).any(|w| w == b"aes256-ctr"),
        "应声明 aes256-ctr"
    );
    assert!(buf.windows(6).any(|w| w == b"bcrypt"), "应声明 bcrypt KDF");
}

#[test]
fn encrypted_rsa_round_trip() {
    let k = generate_rsa(Some("enc-rsa@test"), 2048, Some("rsa-pass")).unwrap();
    assert_encrypted_key(&k, "rsa-pass");
}

#[test]
fn inspect_private_key_detects_encryption() {
    let plain = generate_ed25519(Some("p"), None).unwrap();
    let info = inspect_private_key(&plain.private_key_pem);
    assert!(!info.encrypted);
    assert_eq!(info.key_type, "ssh-ed25519");

    let enc = generate_ed25519(Some("p"), Some("secret")).unwrap();
    let info = inspect_private_key(&enc.private_key_pem);
    assert!(info.encrypted);
    assert_eq!(info.key_type, "");
}

#[test]
fn backup_name_parsing() {
    let rp = "/home/alice/.ssh/authorized_keys";
    let p = parse_backup_name(rp, "authorized_keys.bak.20261022093015").unwrap();
    assert_eq!(p.stamp, "20261022093015");
    assert_eq!(p.display_time, "2026-10-22 09:30:15");
    assert_eq!(display_time("20261022093015"), "2026-10-22 09:30:15");

    for bad in [
        "../authorized_keys.bak.20261022093015",
        "other.bak.20261022093015",
        "authorized_keys.bak.2026102209301",
        "authorized_keys.bak.20261022093015/../x",
        "authorized_keys.bak.2026102209301a",
        "authorized_keys.bak.20261022093015.bak.20261022093015",
        "C:\\authorized_keys.bak.20261022093015",
        "",
    ] {
        assert!(parse_backup_name(rp, bad).is_none(), "应拒绝：{bad}");
    }
}

#[test]
fn backup_path_rebuilt_from_remote_path() {
    let rp = "/home/alice/.ssh/authorized_keys";
    assert_eq!(
        backup_path_for(rp, "authorized_keys.bak.20261022093015").unwrap(),
        format!("{rp}.bak.20261022093015")
    );
    assert!(backup_path_for(rp, "../authorized_keys.bak.20261022093015").is_err());
}

#[test]
fn backup_diff_directions() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();
    let rsa1 = generate_rsa(Some("legacy@server"), 2048, None).unwrap();

    let cur = format!(
        "{}\n{}\ngarbage-line\n{}\n",
        ed1.public_key_line, ed2.public_key_line, ed1.public_key_line
    );
    let bak = format!(
        "# 备份里的注释\n{}\n{}\ngarbage-line\n",
        ed2.public_key_line, rsa1.public_key_line
    );

    let d = diff_backup(&cur, &bak, Some(&ed2.fingerprint), true);
    assert_eq!(d.current_key_count, 2, "当前重复的公钥只算一把");
    assert_eq!(d.backup_key_count, 2);
    assert_eq!(d.same_count, 1);
    assert_eq!(
        d.added
            .iter()
            .map(|k| k.fingerprint.as_str())
            .collect::<Vec<_>>(),
        vec![rsa1.fingerprint.as_str()]
    );
    assert_eq!(
        d.removed
            .iter()
            .map(|k| k.fingerprint.as_str())
            .collect::<Vec<_>>(),
        vec![ed1.fingerprint.as_str()]
    );
    assert_eq!(d.current_malformed, 1);
    assert_eq!(d.backup_malformed, 1);
    assert!(!d.empty_backup);
    assert_eq!(d.used_key_in_backup, Some(true));
}

#[test]
fn backup_diff_empty_and_missing_login_key() {
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let ed2 = generate_ed25519(Some("bob@nas"), None).unwrap();
    let lonely = generate_ed25519(Some("lonely"), None).unwrap();

    let cur = format!("{}\n", lonely.public_key_line);
    let empty = diff_backup(&cur, "# 只有注释\n", Some(&ed1.fingerprint), true);
    assert!(empty.empty_backup);
    assert_eq!(empty.used_key_in_backup, Some(false));

    let missing = diff_backup(
        &cur,
        &format!("{}\n", ed2.public_key_line),
        Some(&ed1.fingerprint),
        true,
    );
    assert_eq!(missing.used_key_in_backup, Some(false));

    let no_key_auth = diff_backup(&cur, &format!("{}\n", ed2.public_key_line), Some(""), true);
    assert_eq!(no_key_auth.used_key_in_backup, None, "密码会话无法判定");
}

#[test]
fn json_contract_matches_old_frontend() {
    // 前端直接消费这些字段名，改名就等于改协议
    let ed1 = generate_ed25519(Some("alice@laptop"), None).unwrap();
    let rsa1 = generate_rsa(Some("legacy@server"), 2048, None).unwrap();
    let text = format!(
        "# 注释\n\n{}\n{}\ngarbage\n",
        ed1.public_key_line, rsa1.public_key_line
    );
    let parsed = parse_authorized_keys(&text);
    let json = serde_json::to_value(&parsed.items).unwrap();
    let arr = json.as_array().unwrap();

    let comment = arr[0].as_object().unwrap();
    assert_eq!(comment["kind"], "comment");
    assert_eq!(comment["text"], "注释");
    assert_eq!(comment["index"], 0);

    let blank = arr[1].as_object().unwrap();
    assert_eq!(blank["kind"], "blank");

    let key = arr[2].as_object().unwrap();
    assert_eq!(key["kind"], "key");
    for field in [
        "index",
        "raw",
        "kind",
        "type",
        "b64",
        "options",
        "comment",
        "fingerprint",
        "valid",
        "error",
        "duplicateOf",
    ] {
        assert!(key.contains_key(field), "key 条目缺少字段 {field}");
    }
    assert_eq!(key["duplicateOf"], serde_json::Value::Null);

    let malformed = arr[4].as_object().unwrap();
    assert_eq!(malformed["kind"], "malformed");
    assert!(malformed["reason"].as_str().unwrap().len() > 0);

    let diff = diff_backup(&text, &text, None, true);
    let diff_json = serde_json::to_value(&diff).unwrap();
    for field in [
        "backupKeyCount",
        "currentKeyCount",
        "sameCount",
        "added",
        "removed",
        "backupMalformed",
        "currentMalformed",
        "emptyBackup",
        "usedKeyInBackup",
        "currentFileExists",
    ] {
        assert!(diff_json.get(field).is_some(), "差异结果缺少字段 {field}");
    }
    assert_eq!(diff_json["usedKeyInBackup"], serde_json::Value::Null);

    let summary = serde_json::to_value(summarize(&parse_config_text(""))).unwrap();
    for field in [
        "passwordAuth",
        "pubkeyAuth",
        "kbdInteractive",
        "challengeResponse",
        "pwAllowed",
        "state",
        "keyOnly",
        "rootLogin",
        "explicit",
    ] {
        assert!(summary.get(field).is_some(), "sshd 摘要缺少字段 {field}");
    }
    assert_eq!(summary["state"], "both");
    assert_eq!(summary["rootLogin"], "prohibit-password");
    assert_eq!(summary["explicit"]["passwordAuth"], serde_json::Value::Null);
}
