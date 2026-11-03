//! 代理参数的合并规则与「测试代理」接口；握手细节由 kw-ssh 的 proxy 测试覆盖。

use std::sync::Arc;
use std::time::Duration;

use kw_core::Secret;
use kw_server::connect::{resolve_proxy, test_proxy, ProxyProbeBody, ProxyRequest};
use kw_server::state::AppState;
use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::proxy::ProxyKind;
use kw_store::{HostPatch, Store};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn state(dir: &std::path::Path) -> AppState {
    let data = dir.join("data");
    let keys = dir.join("keys");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    let store = Arc::new(Store::new(&data));
    store
        .update_settings(kw_store::SettingsPatch {
            local_key_dir: Some(keys.display().to_string()),
            save_dir: Some(keys.display().to_string()),
            remote_path: None,
        })
        .unwrap();
    AppState::new(
        "0123456789abcdef0123456789abcdef0123".to_string(),
        7791,
        store,
        Arc::new(KnownHosts::new(data.join("known_hosts.json"))),
        data.clone(),
        "test",
    )
}

fn host_with_proxy(store: &Store) -> kw_store::Host {
    store
        .upsert_host(HostPatch {
            host: Some("srv.example".into()),
            username: Some("root".into()),
            proxy_kind: Some("SOCKS5".into()),
            proxy_host: Some(" proxy.local ".into()),
            proxy_port: Some(1080),
            proxy_username: Some(" relay ".into()),
            ..Default::default()
        })
        .unwrap()
}

#[test]
fn saved_proxy_and_remembered_password_are_used() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let host = host_with_proxy(&state.store);
    state
        .store
        .set_proxy_password(&host.id, "stored-pw")
        .unwrap();

    let cfg = resolve_proxy(&state, &ProxyRequest::default(), Some(&host))
        .unwrap()
        .expect("已保存的代理应生效");
    assert_eq!(cfg.kind, ProxyKind::Socks5);
    assert_eq!(cfg.host, "proxy.local");
    assert_eq!(cfg.port, 1080);
    assert_eq!(cfg.username.as_deref(), Some("relay"));
    assert_eq!(cfg.password.as_ref().map(Secret::expose), Some("stored-pw"));
}

#[test]
fn request_fields_override_saved_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let host = host_with_proxy(&state.store);
    state
        .store
        .set_proxy_password(&host.id, "stored-pw")
        .unwrap();

    let req = ProxyRequest {
        proxy_kind: Some("http".into()),
        proxy_host: Some("other.local".into()),
        proxy_port: Some(1081),
        proxy_username: None,
        proxy_password: Some(Secret::new("typed-pw")),
    };
    let cfg = resolve_proxy(&state, &req, Some(&host)).unwrap().unwrap();
    assert_eq!(cfg.kind, ProxyKind::Http);
    assert_eq!(cfg.host, "other.local");
    assert_eq!(cfg.port, 1081);
    assert_eq!(
        cfg.username.as_deref(),
        Some("relay"),
        "未填时沿用主机用户名"
    );
    assert_eq!(cfg.password.as_ref().map(Secret::expose), Some("typed-pw"));
}

#[test]
fn no_proxy_and_invalid_kind() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let host = state
        .store
        .upsert_host(HostPatch {
            host: Some("srv.example".into()),
            username: Some("root".into()),
            ..Default::default()
        })
        .unwrap();

    assert!(resolve_proxy(&state, &ProxyRequest::default(), Some(&host))
        .unwrap()
        .is_none());

    let bad = ProxyRequest {
        proxy_kind: Some("quic".into()),
        proxy_host: Some("proxy.local".into()),
        ..Default::default()
    };
    let err = resolve_proxy(&state, &bad, Some(&host)).unwrap_err();
    assert!(err.message.contains("不支持的代理类型"), "{}", err.message);
}

#[tokio::test]
async fn test_proxy_handshakes_without_ssh_login() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await.unwrap();
        let mut methods = vec![0u8; head[1] as usize];
        client.read_exact(&mut methods).await.unwrap();
        client.write_all(&[5, 0]).await.unwrap();
        let mut req = [0u8; 4];
        client.read_exact(&mut req).await.unwrap();
        assert_eq!(req[3], 1, "测试目标应按 IPv4 编码");
        let mut addr = [0u8; 6];
        client.read_exact(&mut addr).await.unwrap();
        client
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let probe = ProxyProbeBody {
        host_id: None,
        host: Some("127.0.0.1".into()),
        port: Some(22),
        proxy: ProxyRequest {
            proxy_kind: Some("socks5".into()),
            proxy_host: Some("127.0.0.1".into()),
            proxy_port: Some(proxy_port),
            ..Default::default()
        },
    };
    let out = test_proxy(&state, probe).await.unwrap();
    assert_eq!(out["kind"], "SOCKS5");
    assert!(out["ms"].as_u64().is_some());

    let err = test_proxy(&state, ProxyProbeBody::default())
        .await
        .unwrap_err();
    assert!(err.message.contains("还没有配置代理"), "{}", err.message);
}
