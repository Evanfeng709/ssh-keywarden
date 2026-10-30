//! 与旧版 `tests/run.js` 的「本地钥匙备注」断言对齐。

use kw_store::{sanitize_note, HostPatch, SettingsPatch, Store, StoreError, NOTE_MAX};

const FP_A: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FP_B: &str = "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    (dir, store)
}

#[test]
fn note_normalizes_whitespace_and_persists() {
    let (_dir, store) = store();
    let notes = store
        .set_key_note(FP_A, "  张三的 MacBook\n\t备用  ")
        .unwrap();
    assert_eq!(notes[FP_A].note, "张三的 MacBook 备用");
    assert!(notes[FP_A].updated_at > 0);

    let persisted = store.list_key_notes();
    assert_eq!(persisted[FP_A].note, "张三的 MacBook 备用");
}

#[test]
fn note_max_length_and_empty_deletes() {
    let (_dir, store) = store();
    let too_long = "x".repeat(NOTE_MAX + 1);
    assert!(matches!(
        store.set_key_note(FP_A, &too_long),
        Err(StoreError::NoteTooLong)
    ));

    store.set_key_note(FP_A, "临时").unwrap();
    let notes = store.set_key_note(FP_A, "").unwrap();
    assert!(!notes.contains_key(FP_A), "空备注应删除条目");
    assert!(!store.list_key_notes().contains_key(FP_A), "删除也要落盘");
}

#[test]
fn note_rejects_bad_fingerprint() {
    let (_dir, store) = store();
    assert!(matches!(
        store.set_key_note("not-a-fingerprint", "x"),
        Err(StoreError::BadFingerprint)
    ));
    assert!(matches!(
        store.set_key_note("", "x"),
        Err(StoreError::BadFingerprint)
    ));
}

#[test]
fn note_file_filters_invalid_entries() {
    let (_dir, store) = store();
    std::fs::write(
        store.notes_path(),
        serde_json::json!({
            "version": 1,
            "notes": {
                "bad": { "note": "没有合法指纹" },
                FP_B: { "note": "  正常备注  " },
                FP_A: { "note": "   " }
            }
        })
        .to_string(),
    )
    .unwrap();

    let notes = store.list_key_notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[FP_B].note, "正常备注");
}

#[test]
fn note_corrupt_file_is_backed_up() {
    let (dir, store) = store();
    std::fs::write(store.notes_path(), "{ broken json").unwrap();
    let notes = store.list_key_notes();
    assert!(notes.is_empty());

    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("key-notes.json.bad-"))
        .collect();
    assert!(!backups.is_empty(), "应保留坏文件备份，方便排查");
}

#[test]
fn sanitize_note_edge_cases() {
    assert_eq!(sanitize_note(""), "");
    assert_eq!(sanitize_note("   "), "");
    assert_eq!(sanitize_note("a\u{0}b"), "ab");
    assert_eq!(sanitize_note("a  b\t\tc"), "a b c");
}

#[test]
fn host_upsert_and_remove() {
    let (_dir, store) = store();
    let host = store
        .upsert_host(HostPatch {
            host: Some("  example.test ".into()),
            username: Some("alice".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(host.name, "example.test", "没给名字时用地址兜底");
    assert_eq!(host.port, 22);
    assert!(!host.id.is_empty());
    assert_eq!(store.list_hosts().len(), 1);

    let updated = store
        .upsert_host(HostPatch {
            id: Some(host.id.clone()),
            host: Some("example.test".into()),
            username: Some("alice".into()),
            name: Some("生产机".into()),
            port: Some(2222),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(store.list_hosts().len(), 1, "同 id 应更新而不是新增");
    assert_eq!(updated.created_at, host.created_at);
    assert_eq!(updated.port, 2222);

    assert!(matches!(
        store.upsert_host(HostPatch {
            host: Some("".into()),
            username: Some("alice".into()),
            ..Default::default()
        }),
        Err(StoreError::EmptyHost)
    ));
    assert!(matches!(
        store.upsert_host(HostPatch {
            host: Some("h".into()),
            username: Some("  ".into()),
            ..Default::default()
        }),
        Err(StoreError::EmptyUsername)
    ));

    store.remove_host(&host.id).unwrap();
    assert!(store.list_hosts().is_empty());
    assert!(matches!(
        store.remove_host(&host.id),
        Err(StoreError::HostNotFound)
    ));
}

#[test]
fn settings_only_three_keys() {
    let (_dir, store) = store();
    assert_eq!(store.get_settings().local_key_dir, "");
    let s = store
        .update_settings(SettingsPatch {
            local_key_dir: Some("C:/keys".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(s.local_key_dir, "C:/keys");
    assert_eq!(store.get_settings().local_key_dir, "C:/keys");
    assert_eq!(store.get_settings().remote_path, "");
}

#[test]
fn config_defaults_when_missing_or_corrupt() {
    let (dir, store) = store();
    let cfg = store.read();
    assert!(cfg.hosts.is_empty());
    assert_eq!(cfg.settings.remote_path, "");

    std::fs::write(store.config_path(), "{ not json").unwrap();
    assert!(store.read().hosts.is_empty());
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("config.json.bad-"))
        .collect();
    assert!(!backups.is_empty());
}
