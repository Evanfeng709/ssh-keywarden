//! SSH 传输层：russh 会话、PTY/终端、远端文件读写、主机指纹校验。
//!
//! 手机端只依赖 `kw-ssh` + `kw-keys`，不碰桌面才有的存储/路由。

pub mod known_hosts;
pub mod session;
pub mod sshd;
pub mod terminal;
pub mod util;

pub use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};
