//! 分块上传/下载：进度、临时文件、取消后清理。

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kw_core::Secret;
use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::session::{connect, SshClient};
use kw_ssh::sftp::{SftpHandle, TransferError};

const CHUNK: usize = 64 * 1024;

async fn setup() -> (common::TestServer, SshClient, SftpHandle, tempfile::TempDir) {
    let server = common::TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = Arc::new(KnownHosts::new(dir.path().join("known_hosts.json")));
    server.trust(&known_hosts);
    let mut cfg = kw_ssh::session::SshConfig::new("127.0.0.1", server.port, "spike");
    cfg.password = Some(Secret::new("spike"));
    let client = connect(&cfg, known_hosts).await.unwrap();
    let sftp = SftpHandle::open(&client).await.unwrap();
    (server, client, sftp, dir)
}

async fn teardown(server: common::TestServer, client: SshClient, sftp: SftpHandle) {
    let _ = sftp.close().await;
    client.disconnect().await;
    server.stop().await;
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn has_part_file(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().contains(".kwpart-"))
}

#[tokio::test]
async fn upload_then_download_roundtrip_with_progress() {
    let (server, client, sftp, dir) = setup().await;
    let data = payload(3 * CHUNK + 1234);
    let local = dir.path().join("upload.bin");
    std::fs::write(&local, &data).unwrap();

    let upload_progress = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let seen = Arc::clone(&upload_progress);
    let cancel = AtomicBool::new(false);
    let sent = sftp
        .upload(&local, "/roundtrip.bin", &cancel, move |done, total| {
            seen.lock().unwrap().push((done, total));
        })
        .await
        .unwrap();
    assert_eq!(sent, data.len() as u64);

    let seen = upload_progress.lock().unwrap().clone();
    assert!(seen.len() >= 3);
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "进度必须递增");
    assert_eq!(
        seen.last().unwrap(),
        &(data.len() as u64, data.len() as u64)
    );

    let names: Vec<String> = sftp
        .list_dir("/")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, vec!["roundtrip.bin".to_string()]);

    // 覆盖已有本地文件，验证临时文件改名路径
    let download = dir.path().join("download.bin");
    std::fs::write(&download, b"old contents").unwrap();
    let download_progress = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let seen = Arc::clone(&download_progress);
    let cancel = AtomicBool::new(false);
    let received = sftp
        .download("/roundtrip.bin", &download, &cancel, move |done, total| {
            seen.lock().unwrap().push((done, total));
        })
        .await
        .unwrap();
    assert_eq!(received, data.len() as u64);
    assert_eq!(std::fs::read(&download).unwrap(), data);
    let seen = download_progress.lock().unwrap().clone();
    assert_eq!(
        seen.last().unwrap(),
        &(data.len() as u64, data.len() as u64)
    );
    assert!(!has_part_file(dir.path()), "下载完成后不应留下临时文件");

    teardown(server, client, sftp).await;
}

#[tokio::test]
async fn cancel_upload_removes_remote_part_file() {
    let (server, client, sftp, dir) = setup().await;
    let data = payload(1024 * 1024);
    let local = dir.path().join("cancel.bin");
    std::fs::write(&local, &data).unwrap();

    let cancel = AtomicBool::new(false);
    let result = sftp
        .upload(&local, "/cancel.bin", &cancel, |done, _| {
            if done > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .await;
    assert!(matches!(result, Err(TransferError::Canceled)));

    assert!(!sftp.exists("/cancel.bin").await.unwrap());
    assert!(
        sftp.list_dir("/").await.unwrap().is_empty(),
        "取消后应删掉远端临时文件"
    );

    teardown(server, client, sftp).await;
}

#[tokio::test]
async fn cancel_download_leaves_no_local_partial() {
    let (server, client, sftp, dir) = setup().await;
    let data = payload(1024 * 1024);
    let seed = dir.path().join("seed.bin");
    std::fs::write(&seed, &data).unwrap();
    let no_cancel = AtomicBool::new(false);
    sftp.upload(&seed, "/big.bin", &no_cancel, |_, _| {})
        .await
        .unwrap();

    let target = dir.path().join("big.bin");
    let cancel = AtomicBool::new(false);
    let result = sftp
        .download("/big.bin", &target, &cancel, |done, _| {
            if done > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .await;
    assert!(matches!(result, Err(TransferError::Canceled)));
    assert!(!target.exists(), "取消后不应生成目标文件");
    assert!(!has_part_file(dir.path()), "取消后应删掉本地临时文件");

    teardown(server, client, sftp).await;
}
