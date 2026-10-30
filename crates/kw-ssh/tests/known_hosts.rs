//! 与旧版 `tests/run.js` 的「主机指纹 known_hosts」断言对齐。

use kw_keys::generate_ed25519;
use kw_ssh::known_hosts::{HostKeyCode, KnownHosts, VerifyStatus};

fn blob_of(public_key_line: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(public_key_line.split_whitespace().nth(1).unwrap())
        .unwrap()
}

fn setup() -> (tempfile::TempDir, KnownHosts) {
    let dir = tempfile::tempdir().unwrap();
    let kh = KnownHosts::new(dir.path().join("known_hosts.json"));
    (dir, kh)
}

#[test]
fn fingerprint_matches_keygen() {
    let k = generate_ed25519(Some("host-1"), None).unwrap();
    let blob = blob_of(&k.public_key_line);
    let fp = kw_keys::fingerprint_of_blob(&blob);
    assert_eq!(fp, k.fingerprint);
    assert_eq!(kw_ssh::known_hosts::key_type_of_blob(&blob), "ssh-ed25519");
}

#[test]
fn first_connection_is_unknown() {
    let (_d, kh) = setup();
    let k = generate_ed25519(Some("host-1"), None).unwrap();
    let blob = blob_of(&k.public_key_line);

    let err = kh
        .verify("unknown.example", 22, &blob, None)
        .expect_err("应报错");
    assert_eq!(err.code, HostKeyCode::Unknown);
    assert_eq!(err.details["fingerprint"], k.fingerprint.as_str());
    assert!(
        kh.get("unknown.example", 22).is_none(),
        "未被信任时不能落盘"
    );
}

#[test]
fn confirm_writes_then_match() {
    let (_d, kh) = setup();
    let k = generate_ed25519(Some("host-1"), None).unwrap();
    let blob = blob_of(&k.public_key_line);

    let first = kh
        .verify("trust.example", 2222, &blob, Some(&k.fingerprint))
        .unwrap();
    assert_eq!(first.status, VerifyStatus::Trusted);
    assert!(
        kh.get("trust.example", 2222).is_some(),
        "确认后应写入指纹库"
    );

    let second = kh.verify("trust.example", 2222, &blob, None).unwrap();
    assert_eq!(second.status, VerifyStatus::Match);
}

#[test]
fn changed_key_reports_previous_fingerprint() {
    let (_d, kh) = setup();
    let k1 = generate_ed25519(Some("host-1"), None).unwrap();
    let k2 = generate_ed25519(Some("host-2"), None).unwrap();
    let blob1 = blob_of(&k1.public_key_line);
    kh.verify("trust.example", 2222, &blob1, Some(&k1.fingerprint))
        .unwrap();

    let err = kh
        .verify("trust.example", 2222, &blob_of(&k2.public_key_line), None)
        .expect_err("应报错");
    assert_eq!(err.code, HostKeyCode::Changed);
    assert_eq!(err.details["previousFingerprint"], k1.fingerprint.as_str());
    assert_eq!(err.details["fingerprint"], k2.fingerprint.as_str());
}

#[test]
fn confirming_change_overwrites_record() {
    let (_d, kh) = setup();
    let k1 = generate_ed25519(Some("host-1"), None).unwrap();
    let k2 = generate_ed25519(Some("host-2"), None).unwrap();
    let blob1 = blob_of(&k1.public_key_line);
    kh.verify("trust.example", 2222, &blob1, Some(&k1.fingerprint))
        .unwrap();

    let res = kh
        .verify(
            "trust.example",
            2222,
            &blob_of(&k2.public_key_line),
            Some(&k2.fingerprint),
        )
        .unwrap();
    assert_eq!(res.status, VerifyStatus::Updated);
    assert_eq!(
        res.previous.unwrap()["fingerprint"],
        k1.fingerprint.as_str()
    );
    assert_eq!(
        kh.get("trust.example", 2222).unwrap().fingerprint,
        k2.fingerprint
    );
}

#[test]
fn host_case_and_default_port_normalized() {
    let (_d, kh) = setup();
    let k = generate_ed25519(Some("host-1"), None).unwrap();
    kh.save("Case.Example", 22, &blob_of(&k.public_key_line));
    assert!(kh.get("case.example", 22).is_some());
    assert!(kh.get("CASE.EXAMPLE", 22).is_some());
}
