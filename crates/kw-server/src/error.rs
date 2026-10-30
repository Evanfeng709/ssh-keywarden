//! 统一错误 → HTTP 响应。
//!
//! 旧版的所有失败都是 `{ ok:false, error, ...extra }`；
//! 主机指纹错误额外带 `code` 与 `details`（HTTP 409），会话失效带 `code: SESSION_LOST`。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use kw_ssh::session::SshError;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<&'static str>,
    pub details: Option<Value>,
}

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
            code: None,
            details: None,
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
            code: None,
            details: None,
        }
    }

    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "未授权：请从应用或本次启动生成的登录文件打开页面".into(),
            code: None,
            details: None,
        }
    }

    pub fn session_lost() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "会话已失效（超过 30 分钟未操作或服务重启），请重新连接".into(),
            code: Some("SESSION_LOST"),
            details: None,
        }
    }

    pub fn disconnected() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "SSH 连接已断开，请重新连接".into(),
            code: Some("SESSION_LOST"),
            details: None,
        }
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "ok": false, "error": self.message });
        if let (Some(code), Value::Object(map)) = (self.code, &mut body) {
            map.insert("code".into(), Value::String(code.into()));
        }
        if let (Some(details), Value::Object(map)) = (self.details, &mut body) {
            map.insert("details".into(), details);
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<SshError> for ApiError {
    fn from(err: SshError) -> Self {
        if let (Some(code), Some(details)) = (err.host_key_code(), err.host_key_details()) {
            return Self {
                status: StatusCode::CONFLICT,
                message: err.to_string(),
                code: Some(code),
                details: Some(details.clone()),
            };
        }
        let text = err.to_string();
        if kw_ssh::util::is_disconnect_error(&text) || text.contains("连接已断开") {
            return Self {
                status: StatusCode::CONFLICT,
                message: "SSH 连接已断开（服务器可能主动断开了空闲连接），请重新连接后再操作"
                    .into(),
                code: Some("SESSION_LOST"),
                details: None,
            };
        }
        Self::bad(text)
    }
}

impl From<kw_store::StoreError> for ApiError {
    fn from(err: kw_store::StoreError) -> Self {
        Self::bad(err.to_string())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> Self {
        Self::bad(err.to_string())
    }
}
