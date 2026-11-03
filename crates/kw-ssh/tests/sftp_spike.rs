//! SFTP 连通性 spike：验证 russh 0.65 的 `Channel::into_stream()` 能直接喂给
//! `russh-sftp` 3.x 的客户端，并跑通「列目录 → 建目录 → 上传 → 回读 → 改名 → 删除」。

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use russh_sftp::client::SftpSession;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn russh_sftp_client_works_over_channel_stream() {
    let server = common::TestServer::start().await;

    let mut handle = russh::client::connect(
        Arc::new(russh::client::Config::default()),
        SocketAddr::from(([127, 0, 0, 1], server.port)),
        common::ClientVerifier,
    )
    .await
    .unwrap();
    assert!(handle
        .authenticate_password("spike", "spike")
        .await
        .unwrap()
        .success());

    let channel = handle.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = SftpSession::new(channel.into_stream()).await.unwrap();

    assert!(sftp.read_dir("/").await.unwrap().next().is_none());

    sftp.create_dir("/upload").await.unwrap();
    let payload = b"hello over russh-sftp\n".to_vec();
    let mut file = sftp.create("/upload/probe.txt").await.unwrap();
    file.write_all(&payload).await.unwrap();
    file.close().await.unwrap();

    assert_eq!(sftp.read("/upload/probe.txt").await.unwrap(), payload);
    let meta = sftp.metadata("/upload/probe.txt").await.unwrap();
    assert_eq!(meta.size, Some(payload.len() as u64));
    assert!(meta.is_regular());

    let names: Vec<String> = sftp
        .read_dir("/upload")
        .await
        .unwrap()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(names, vec!["probe.txt".to_string()]);

    sftp.rename("/upload/probe.txt", "/upload/moved.txt")
        .await
        .unwrap();
    assert_eq!(sftp.read("/upload/moved.txt").await.unwrap(), payload);
    sftp.remove_file("/upload/moved.txt").await.unwrap();
    sftp.remove_dir("/upload").await.unwrap();
    assert!(sftp.read_dir("/").await.unwrap().next().is_none());

    sftp.close().await.unwrap();
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "")
        .await;

    server.stop().await;
}
