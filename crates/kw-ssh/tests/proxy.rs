//! 本地假代理：打通 SOCKS5 / HTTP CONNECT 的正负例，不需要真实 SSH 服务。

use std::time::Duration;

use kw_core::Secret;
use kw_ssh::proxy::{connect_through, connect_through_timeout, ProxyConfig, ProxyError, ProxyKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn read_http_head(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tail = [0u8; 4];
    loop {
        let b = stream.read_u8().await.unwrap();
        buf.push(b);
        tail.rotate_left(1);
        tail[3] = b;
        if &tail == b"\r\n\r\n" {
            return String::from_utf8(buf).unwrap();
        }
    }
}

async fn socks5_greeting(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], 5);
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods).await.unwrap();
    methods
}

async fn socks5_request(stream: &mut TcpStream) -> (u8, String, u16) {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], 5);
    assert_eq!(head[1], 1);
    let atyp = head[3];
    let host = match atyp {
        1 => {
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.unwrap();
            std::net::Ipv4Addr::from(buf).to_string()
        }
        3 => {
            let len = stream.read_u8().await.unwrap() as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await.unwrap();
            String::from_utf8(buf).unwrap()
        }
        other => panic!("unexpected atyp {other}"),
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await.unwrap();
    (atyp, host, u16::from_be_bytes(port))
}

fn socks_ok_reply() -> [u8; 10] {
    [5, 0, 0, 1, 127, 0, 0, 1, 0, 0]
}

fn proxy_cfg(kind: ProxyKind, port: u16) -> ProxyConfig {
    ProxyConfig {
        kind,
        host: "127.0.0.1".into(),
        port,
        username: None,
        password: None,
    }
}

async fn echo_target() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let _ = stream.write_all(b"SSH-2.0-target\r\n").await;
                let mut buf = [0u8; 4];
                if stream.read_exact(&mut buf).await.is_ok() {
                    let _ = stream.write_all(&buf).await;
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn socks5_connects_and_pipes_bytes() {
    let target_port = echo_target().await;
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        let methods = socks5_greeting(&mut client).await;
        assert_eq!(methods, vec![0]);
        client.write_all(&[5, 0]).await.unwrap();
        let (atyp, host, port) = socks5_request(&mut client).await;
        assert_eq!(atyp, 1, "IP 地址应按 IPv4 编码");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, target_port);
        client.write_all(&socks_ok_reply()).await.unwrap();
        let mut upstream = TcpStream::connect(("127.0.0.1", target_port))
            .await
            .unwrap();
        // 客户端断开后两端会各自收到 EOF/reset，这里的错误不值得 panic。
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let mut stream = connect_through(
        &proxy_cfg(ProxyKind::Socks5, proxy_port),
        "127.0.0.1",
        target_port,
    )
    .await
    .unwrap();
    let mut banner = [0u8; 16];
    stream.read_exact(&mut banner).await.unwrap();
    assert_eq!(&banner, b"SSH-2.0-target\r\n");
    stream.write_all(b"ping").await.unwrap();
    let mut echoed = [0u8; 4];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");
    // 不关掉客户端，copy_bidirectional 永远等不到 EOF。
    drop(stream);
    task.await.unwrap();
}

#[tokio::test]
async fn socks5_domain_address_is_sent_as_domain() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        socks5_greeting(&mut client).await;
        client.write_all(&[5, 0]).await.unwrap();
        let (atyp, host, port) = socks5_request(&mut client).await;
        assert_eq!(atyp, 3);
        assert_eq!(host, "nas.internal");
        assert_eq!(port, 2222);
        client.write_all(&socks_ok_reply()).await.unwrap();
        client.write_all(b"OK").await.unwrap();
        client.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let mut stream = connect_through(
        &proxy_cfg(ProxyKind::Socks5, proxy_port),
        "nas.internal",
        2222,
    )
    .await
    .unwrap();
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"OK");
    task.await.unwrap();
}

#[tokio::test]
async fn socks5_auth_success_and_failure() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        let methods = socks5_greeting(&mut client).await;
        assert_eq!(methods, vec![0, 2]);
        client.write_all(&[5, 2]).await.unwrap();
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 1);
        let mut user = vec![0u8; head[1] as usize];
        client.read_exact(&mut user).await.unwrap();
        let plen = client.read_u8().await.unwrap() as usize;
        let mut pass = vec![0u8; plen];
        client.read_exact(&mut pass).await.unwrap();
        assert_eq!(user, b"relay");
        assert_eq!(pass, b"s3cret");
        client.write_all(&[1, 0]).await.unwrap();
        socks5_request(&mut client).await;
        client.write_all(&socks_ok_reply()).await.unwrap();
        client.write_all(b"OK").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let mut cfg = proxy_cfg(ProxyKind::Socks5, proxy_port);
    cfg.username = Some("relay".into());
    cfg.password = Some(Secret::new("s3cret"));
    let mut stream = connect_through(&cfg, "127.0.0.1", 22).await.unwrap();
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"OK");
    task.await.unwrap();

    // 密码错误
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        socks5_greeting(&mut client).await;
        client.write_all(&[5, 2]).await.unwrap();
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await.unwrap();
        let mut rest = vec![0u8; head[1] as usize];
        client.read_exact(&mut rest).await.unwrap();
        let plen = client.read_u8().await.unwrap() as usize;
        let mut pass = vec![0u8; plen];
        client.read_exact(&mut pass).await.unwrap();
        client.write_all(&[1, 1]).await.unwrap();
    });
    let mut bad = proxy_cfg(ProxyKind::Socks5, proxy_port);
    bad.username = Some("relay".into());
    bad.password = Some(Secret::new("wrong"));
    let err = connect_through(&bad, "127.0.0.1", 22).await.unwrap_err();
    assert!(matches!(err, ProxyError::AuthFailed), "{err}");
}

#[tokio::test]
async fn socks5_missing_password_asks_for_credentials() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        socks5_greeting(&mut client).await;
        client.write_all(&[5, 2]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
    let mut cfg = proxy_cfg(ProxyKind::Socks5, proxy_port);
    cfg.username = Some("relay".into());
    let err = connect_through(&cfg, "127.0.0.1", 22).await.unwrap_err();
    assert!(matches!(err, ProxyError::AuthRequired), "{err}");
}

#[tokio::test]
async fn socks5_rejection_is_translated() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        socks5_greeting(&mut client).await;
        client.write_all(&[5, 0]).await.unwrap();
        socks5_request(&mut client).await;
        client
            .write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
    });
    let err = connect_through(
        &proxy_cfg(ProxyKind::Socks5, proxy_port),
        "203.0.113.10",
        22,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("连接被拒绝"), "{err}");
}

#[tokio::test]
async fn http_connect_succeeds_and_keeps_banner() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        let head = read_http_head(&mut client).await;
        assert!(
            head.starts_with("CONNECT 127.0.0.1:2222 HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(head.contains("Proxy-Authorization: Basic "), "{head}");
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nSSH-2.0-http\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
    });

    let mut cfg = proxy_cfg(ProxyKind::Http, proxy_port);
    cfg.username = Some("relay".into());
    cfg.password = Some(Secret::new("s3cret"));
    let mut stream = connect_through(&cfg, "127.0.0.1", 2222).await.unwrap();
    let mut banner = [0u8; 14];
    stream.read_exact(&mut banner).await.unwrap();
    assert_eq!(&banner, b"SSH-2.0-http\r\n");
    task.await.unwrap();
}

#[tokio::test]
async fn http_407_maps_to_auth_errors() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = proxy.accept().await else {
                return;
            };
            let _ = read_http_head(&mut client).await;
            let _ = client
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await;
        }
    });

    let err = connect_through(&proxy_cfg(ProxyKind::Http, proxy_port), "example.test", 22)
        .await
        .unwrap_err();
    assert!(matches!(err, ProxyError::AuthRequired), "{err}");

    let mut cfg = proxy_cfg(ProxyKind::Http, proxy_port);
    cfg.username = Some("relay".into());
    cfg.password = Some(Secret::new("wrong"));
    let err = connect_through(&cfg, "example.test", 22).await.unwrap_err();
    assert!(matches!(err, ProxyError::AuthFailed), "{err}");
}

#[tokio::test]
async fn http_error_status_is_reported() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        read_http_head(&mut client).await;
        client
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
            .await
            .unwrap();
    });
    let err = connect_through(&proxy_cfg(ProxyKind::Http, proxy_port), "example.test", 22)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("502") && text.contains("Bad Gateway"),
        "{text}"
    );
}

#[tokio::test]
async fn handshake_timeout_is_reported() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (_client, _) = proxy.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let err = connect_through_timeout(
        &proxy_cfg(ProxyKind::Socks5, proxy_port),
        "example.test",
        22,
        Duration::from_millis(150),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ProxyError::Timeout { .. }), "{err}");
}
