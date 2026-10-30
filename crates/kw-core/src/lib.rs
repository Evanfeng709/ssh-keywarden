//! 跨端共用的最小底座：敏感凭据包装。
//!
//! 规则层在 `kw-keys`，存储层在 `kw-store`，SSH 在 `kw-ssh`，HTTP 在 `kw-server`。
//! 这里只放两类以上都要用的东西，避免业务 crate 互相依赖。

pub mod secret;

pub use secret::Secret;
