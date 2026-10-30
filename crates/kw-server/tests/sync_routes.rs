//! 同步接口的形状测试：全程不联网，只验鉴权、参数校验与「没有登录态」的报错。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kw_server::routes;
use kw_server::state::AppState;
use kw_ssh::known_hosts::KnownHosts;
use kw_store::Store;
use serde_json::Value;
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123";

fn app(dir: &std::path::Path) -> axum::Router {
    let public = dir.join("public");
    std::fs::create_dir_all(&public).unwrap();
    std::fs::write(public.join("index.html"), "<!doctype html>").unwrap();

    let store = Arc::new(Store::new(dir.join("data")));
    let known_hosts = Arc::new(KnownHosts::new(dir.join("data").join("known_hosts.json")));
    routes::router(Arc::new(AppState::new(
        TOKEN.to_string(),
        7791,
        store,
        known_hosts,
        public,
        "test",
    )))
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn authed(method: &str, path: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", format!("kw={TOKEN}"));
    match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

#[tokio::test]
async fn sync_routes_require_token() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/sync/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fresh_status_is_unconfigured_and_logged_out() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app
        .oneshot(authed("GET", "/api/sync/status", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["configured"], false);
    assert_eq!(body["data"]["loggedIn"], false);
    assert_eq!(body["data"]["loginPending"], false);
    assert_eq!(body["data"]["hasSyncKey"], false);
}

#[tokio::test]
async fn config_rejects_malformed_client_id_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let bad = app
        .clone()
        .oneshot(authed(
            "POST",
            "/api/sync/config",
            Some(serde_json::json!({ "clientId": "not-a-guid" })),
        ))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(bad).await["error"]
        .as_str()
        .unwrap()
        .contains("GUID"));

    let good = app
        .clone()
        .oneshot(authed(
            "POST",
            "/api/sync/config",
            Some(serde_json::json!({
                "clientId": "0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9",
                "syncKey": "correct horse"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(good.status(), StatusCode::OK);
    let data = body_json(good).await["data"].clone();
    assert_eq!(data["configured"], true);
    assert_eq!(data["clientId"], "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9");
    assert_eq!(data["hasSyncKey"], true);

    let cleared = app
        .oneshot(authed(
            "POST",
            "/api/sync/config",
            Some(serde_json::json!({ "syncKey": "" })),
        ))
        .await
        .unwrap();
    assert_eq!(body_json(cleared).await["data"]["hasSyncKey"], false);
}

#[tokio::test]
async fn complete_without_pending_login_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app
        .oneshot(authed("POST", "/api/sync/login/complete", Some(serde_json::json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(res).await["error"]
        .as_str()
        .unwrap()
        .contains("没有待完成的登录"));
}

#[tokio::test]
async fn sync_fails_before_any_network_call() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let no_key = app
        .clone()
        .oneshot(authed("POST", "/api/sync/now", Some(serde_json::json!({}))))
        .await
        .unwrap();
    assert_eq!(no_key.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(no_key).await["error"]
        .as_str()
        .unwrap()
        .contains("同步口令"));

    let no_login = app
        .oneshot(authed(
            "POST",
            "/api/sync/now",
            Some(serde_json::json!({ "passphrase": "x" })),
        ))
        .await
        .unwrap();
    assert_eq!(no_login.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(no_login).await["error"]
        .as_str()
        .unwrap()
        .contains("还没有登录"));
}

#[tokio::test]
async fn logout_keeps_config_but_clears_login() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let res = app
        .oneshot(authed("POST", "/api/sync/logout", Some(serde_json::json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["data"]["loggedIn"], false);
}
