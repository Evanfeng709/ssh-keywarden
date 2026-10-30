//! 兼容层外壳测试：不连 SSH，只验鉴权、路由形状与静态资源。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kw_server::routes;
use kw_server::state::AppState;
use kw_ssh::known_hosts::KnownHosts;
use kw_store::Store;
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123";

fn app(dir: &std::path::Path) -> axum::Router {
    app_with_cookie(dir, true)
}

fn app_with_cookie(dir: &std::path::Path, auto_cookie: bool) -> axum::Router {
    let public = dir.join("public");
    std::fs::create_dir_all(&public).unwrap();
    std::fs::write(
        public.join("index.html"),
        "<!doctype html><title>kw</title>",
    )
    .unwrap();
    std::fs::write(public.join("style.css"), "body{}").unwrap();

    let store = Arc::new(Store::new(dir.join("data")));
    store
        .update_settings(kw_store::SettingsPatch {
            local_key_dir: Some(dir.join("keys").display().to_string()),
            save_dir: Some(dir.join("keys").display().to_string()),
            remote_path: None,
        })
        .unwrap();
    let known_hosts = Arc::new(KnownHosts::new(dir.join("data").join("known_hosts.json")));
    let mut state = AppState::new(TOKEN.to_string(), 7791, store, known_hosts, public, "test");
    state.auto_cookie = auto_cookie;
    routes::router(Arc::new(state))
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

fn authed_get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("cookie", format!("kw={TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn browser_login_requires_token_and_issues_private_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_cookie(dir.path(), false);
    let index = app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    assert!(index.headers().get("set-cookie").is_none());
    assert_eq!(
        app.clone()
            .oneshot(get("/api/state"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    for uri in ["/login", "/login?token=wrong"] {
        let response = app.clone().oneshot(get(uri)).await.unwrap();
        assert!(response.status().is_client_error());
        assert!(response.headers().get("set-cookie").is_none());
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    }

    let response = app
        .clone()
        .oneshot(get(&format!("/login?token={TOKEN}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    let request = Request::builder()
        .uri("/api/state")
        .header("cookie", cookie.split(';').next().unwrap())
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
}

#[test]
fn connect_credentials_deserialize_without_debug_leaks() {
    let body: kw_server::connect::ConnectBody =
        serde_json::from_str(r#"{"password":" exact password ","passphrase":" exact phrase "}"#)
            .unwrap();
    assert_eq!(body.password.as_ref().unwrap().expose(), " exact password ");
    assert_eq!(body.passphrase.as_ref().unwrap().expose(), " exact phrase ");
    let debug = format!("{body:?}");
    assert!(!debug.contains("exact password"));
    assert!(!debug.contains("exact phrase"));
    let empty: kw_server::connect::ConnectBody =
        serde_json::from_str(r#"{"password":null}"#).unwrap();
    assert!(empty.password.is_none());
    assert!(serde_json::from_str::<kw_server::connect::ConnectBody>(r#"{"password":42}"#).is_err());
}

#[tokio::test]
async fn api_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let res = app.clone().oneshot(get("/api/state")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(res).await;
    assert_eq!(json["ok"], false);
    assert_eq!(
        json["error"],
        "未授权：请从应用或本次启动生成的登录文件打开页面"
    );

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/state")
                .header("x-token", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn state_has_old_frontend_shape() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app.oneshot(authed_get("/api/state")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    assert_eq!(json["ok"], true);
    for key in [
        "version",
        "hosts",
        "settings",
        "keyNotes",
        "localKeys",
        "sessions",
    ] {
        assert!(json["data"].get(key).is_some(), "missing {key}");
    }
}

#[tokio::test]
async fn unknown_api_is_json_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app.oneshot(authed_get("/api/nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let json = body_json(res).await;
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"], "未知接口：GET /api/nope");
}

#[tokio::test]
async fn host_header_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/state")
                .header("host", "evil.example:7791")
                .header("cookie", format!("kw={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::MISDIRECTED_REQUEST);

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/state")
                .header("host", "127.0.0.1:7791")
                .header("cookie", format!("kw={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn static_index_sets_cookie_and_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let res = app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with(&format!("kw={TOKEN}")));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));

    let res = app.oneshot(get("/js/../data/config.json")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fs_list_returns_entries_and_shortcuts() {
    let dir = tempfile::tempdir().unwrap();
    let browse = dir.path().join("browse");
    std::fs::create_dir_all(browse.join("sub")).unwrap();
    std::fs::write(browse.join("id_ed25519.pub"), "ssh-ed25519 AAAA test\n").unwrap();
    std::fs::write(browse.join("notes.txt"), "hi").unwrap();
    let app = app(dir.path());

    let target = browse
        .display()
        .to_string()
        .replace('\\', "%5C")
        .replace(':', "%3A");
    let res = app
        .oneshot(authed_get(&format!("/api/fs/list?dir={target}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    let entries = json["data"]["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["sub", "id_ed25519.pub", "notes.txt"]);
    assert_eq!(entries[1]["keyKind"], "public");
    assert!(entries[1].get("keyKind").is_some());
    assert!(entries[2].get("keyKind").is_none());
    assert!(!json["data"]["shortcuts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn ops_without_session_report_session_lost() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    for path in ["/api/backups?sid=nope", "/api/sshd?sid=nope"] {
        let res = app.clone().oneshot(authed_get(path)).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT, "{path}");
        let json = body_json(res).await;
        assert_eq!(json["code"], "SESSION_LOST", "{path}");
    }

    for path in [
        "/api/add",
        "/api/replace",
        "/api/delete",
        "/api/backups/restore",
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("cookie", format!("kw={TOKEN}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sid":"nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT, "{path}");
        let json = body_json(res).await;
        assert_eq!(json["code"], "SESSION_LOST", "{path}");
    }
}

#[tokio::test]
async fn hosts_roundtrip_through_api() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let body = serde_json::json!({
        "name": "demo",
        "host": "10.0.0.9",
        "port": 2222,
        "username": "root",
        "identityFile": "C:/keys/demo.key",
        "remotePath": ""
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/hosts")
                .header("cookie", format!("kw={TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    let id = json["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(json["data"]["port"], 2222);

    let res = app.oneshot(authed_get("/api/state")).await.unwrap();
    let json = body_json(res).await;
    let hosts = json["data"]["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0]["id"], id);
}
