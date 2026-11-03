//! SftpHandle：在真实 SSH 连接上跑一遍列目录 / 建目录 / 改名 / 权限 / 删除。

mod common;

use std::sync::Arc;

use kw_core::Secret;
use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::session::{connect, SshConfig};
use kw_ssh::sftp::{SftpHandle, SftpKind};

#[tokio::test]
async fn sftp_handle_runs_core_file_operations() {
    let server = common::TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = Arc::new(KnownHosts::new(dir.path().join("known_hosts.json")));
    server.trust(&known_hosts);

    let mut cfg = SshConfig::new("127.0.0.1", server.port, "spike");
    cfg.password = Some(Secret::new("spike"));
    let client = connect(&cfg, known_hosts).await.unwrap();

    let sftp = SftpHandle::open(&client).await.unwrap();

    assert_eq!(sftp.home_dir().await.unwrap(), "/");
    assert!(sftp.list_dir("/").await.unwrap().is_empty());

    sftp.mkdir("/upload").await.unwrap();
    sftp.mkdir("/upload/nested").await.unwrap();

    let root = sftp.list_dir("/").await.unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].name, "upload");
    assert_eq!(root[0].path, "/upload");
    assert_eq!(root[0].kind, SftpKind::Dir);

    let inside = sftp.list_dir("/upload").await.unwrap();
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].name, "nested");

    let stat = sftp.stat("/upload").await.unwrap();
    assert_eq!(stat.kind, SftpKind::Dir);
    assert_eq!(stat.uid, Some(1000));
    assert_eq!(stat.mtime, Some(1_700_000_000));

    sftp.chmod("/upload", 0o700).await.unwrap();
    let stat = sftp.lstat("/upload").await.unwrap();
    assert_eq!(stat.kind, SftpKind::Dir);
    assert_eq!(stat.permissions.unwrap() & 0o777, 0o700);

    sftp.rename("/upload/nested", "/upload/renamed")
        .await
        .unwrap();
    assert!(sftp.exists("/upload/renamed").await.unwrap());
    assert!(!sftp.exists("/upload/nested").await.unwrap());

    // 非空目录不能直接删
    assert!(sftp.remove("/upload").await.is_err());
    sftp.remove("/upload/renamed").await.unwrap();
    sftp.remove("/upload").await.unwrap();
    assert!(sftp.list_dir("/").await.unwrap().is_empty());

    assert!(sftp.list_dir("").await.is_err());

    sftp.close().await.unwrap();
    client.disconnect().await;
    server.stop().await;
}
