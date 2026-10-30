//! 桌面端内置 HTTP 兼容层：127.0.0.1 上跑一套与旧 Node 版同形的 `/api/*`，
//! 前端不需要改动就能跑在 Rust 后端上。

pub mod connect;
pub mod error;
pub mod fsbrowse;
pub mod keys_ops;
pub mod ms_sync;
pub mod routes;
pub mod sessions;
pub mod state;
pub mod sync;
pub mod sync_api;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::session::known_hosts_file;
use kw_store::Store;

pub use state::AppState;

pub const DEFAULT_PORT: u16 = 7791;
const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// 0 = 由系统分配（测试与桌面端都用 0，避免端口冲突）
    pub port: u16,
    pub public_dir: PathBuf,
    pub version: String,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            public_dir: PathBuf::from("public"),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

pub struct RunningServer {
    pub addr: SocketAddr,
    /// 会话令牌：由桌面壳注入 WebView，不打印、不写日志
    pub token: String,
    pub state: Arc<AppState>,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl RunningServer {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        for session in self.state.sessions.list().await {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.client.disconnect()).await;
            self.state.sessions.remove(&session.sid).await;
        }
        if tokio::time::timeout(Duration::from_secs(5), &mut self.join)
            .await
            .is_err()
        {
            self.join.abort();
            let _ = self.join.await;
        }
    }
}

pub async fn serve(opts: ServeOptions) -> std::io::Result<RunningServer> {
    let store = Arc::new(Store::from_env());
    let known_hosts = Arc::new(KnownHosts::new(known_hosts_file(store.dir())));

    let listener = TcpListener::bind(("127.0.0.1", opts.port)).await?;
    let addr = listener.local_addr()?;
    let token = random_token();

    let state = Arc::new(AppState::new(
        token.clone(),
        addr.port(),
        store,
        known_hosts,
        opts.public_dir,
        opts.version,
    ));
    routes::bootstrap_hosts(&state);

    let app = routes::router(Arc::clone(&state));
    let (tx, rx) = oneshot::channel::<()>();
    let (stop_sweep, mut sweep_rx) = oneshot::channel::<()>();

    let sweep_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    sweep_state.sessions.sweep(SESSION_IDLE).await;
                }
                _ = &mut sweep_rx => break,
            }
        }
    });

    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
        let _ = stop_sweep.send(());
    });

    Ok(RunningServer {
        addr,
        token,
        state,
        shutdown: Some(tx),
        join,
    })
}

/// 18 字节随机 → 36 位十六进制，与旧版 `crypto.randomBytes(18).toString('hex')` 同长度。
fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 18];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_36_hex_chars() {
        let t = random_token();
        assert_eq!(t.len(), 36);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, random_token());
    }
}
