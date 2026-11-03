//! 连接代理：SOCKS5 / HTTP CONNECT。
//!
//! 只做 TCP 隧道，不碰 SSH 协议本身：代理握手完成后把裸 TCP 流交给
//! `russh::client::connect_stream`，主机指纹仍按目标服务器校验。

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kw_core::Secret;

pub const PROXY_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_HTTP_HEAD: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks5,
}

impl ProxyKind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_lowercase().as_str() {
            "http" => Some(ProxyKind::Http),
            "socks5" => Some(ProxyKind::Socks5),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProxyKind::Http => "HTTP",
            ProxyKind::Socks5 => "SOCKS5",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<Secret>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("连接代理失败（{addr}）：{source}")]
    Connect {
        addr: String,
        #[source]
        source: io::Error,
    },
    #[error("代理握手超时（{}s）：{addr}", .budget.as_secs())]
    Timeout { addr: String, budget: Duration },
    #[error("代理要求认证，请填写代理用户名和密码")]
    AuthRequired,
    #[error("代理认证失败（用户名或密码错误）")]
    AuthFailed,
    #[error("代理不支持用户名密码认证")]
    AuthUnsupported,
    #[error("代理无法连接目标 {target}：{reason}")]
    TargetUnreachable { target: String, reason: String },
    #[error("代理协议错误：{0}")]
    Protocol(String),
    #[error("代理 I/O 失败：{0}")]
    Io(#[from] io::Error),
}

/// 先与代理握手，再返回已打通到 `target_host:target_port` 的 TCP 流。
pub async fn connect_through(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, ProxyError> {
    connect_through_timeout(proxy, target_host, target_port, PROXY_TIMEOUT).await
}

pub async fn connect_through_timeout(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
    budget: Duration,
) -> Result<TcpStream, ProxyError> {
    let addr = authority(&proxy.host, proxy.port);
    let fut = async {
        let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
            .await
            .map_err(|e| ProxyError::Connect {
                addr: addr.clone(),
                source: e,
            })?;
        let _ = stream.set_nodelay(true);
        match proxy.kind {
            ProxyKind::Socks5 => {
                socks5_handshake(&mut stream, proxy, target_host, target_port).await?
            }
            ProxyKind::Http => http_handshake(&mut stream, proxy, target_host, target_port).await?,
        }
        Ok(stream)
    };
    match tokio::time::timeout(budget, fut).await {
        Ok(result) => result,
        Err(_) => Err(ProxyError::Timeout { addr, budget }),
    }
}

async fn socks5_handshake(
    stream: &mut TcpStream,
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<(), ProxyError> {
    let username = proxy
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = proxy
        .password
        .as_ref()
        .map(Secret::expose)
        .filter(|s| !s.is_empty());

    let mut methods = vec![0x00u8];
    if username.is_some() {
        methods.push(0x02);
    }
    let mut greeting = vec![0x05u8, methods.len() as u8];
    greeting.extend_from_slice(&methods);
    stream.write_all(&greeting).await?;

    let mut choice = [0u8; 2];
    stream.read_exact(&mut choice).await?;
    if choice[0] != 0x05 {
        return Err(ProxyError::Protocol(format!(
            "SOCKS5 版本不符（0x{:02x}）",
            choice[0]
        )));
    }
    match choice[1] {
        0x00 => {}
        0x02 => {
            let Some(user) = username else {
                return Err(ProxyError::AuthUnsupported);
            };
            let Some(pass) = password else {
                return Err(ProxyError::AuthRequired);
            };
            if user.len() > 255 || pass.len() > 255 {
                return Err(ProxyError::Protocol("代理用户名或密码过长".into()));
            }
            let mut auth = vec![0x01u8, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass.as_bytes());
            stream.write_all(&auth).await?;
            let mut reply = [0u8; 2];
            stream.read_exact(&mut reply).await?;
            if reply[1] != 0x00 {
                return Err(ProxyError::AuthFailed);
            }
        }
        0xff => {
            return Err(if username.is_some() {
                ProxyError::AuthFailed
            } else {
                ProxyError::AuthRequired
            });
        }
        other => {
            return Err(ProxyError::Protocol(format!(
                "SOCKS5 选择了不支持的方法 0x{other:02x}"
            )));
        }
    }

    let mut request = vec![0x05u8, 0x01, 0x00];
    match target_host.parse::<Ipv4Addr>() {
        Ok(ip) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => match target_host.parse::<Ipv6Addr>() {
            Ok(ip) => {
                request.push(0x04);
                request.extend_from_slice(&ip.octets());
            }
            Err(_) => {
                if target_host.as_bytes().len() > 255 {
                    return Err(ProxyError::Protocol("目标主机名过长".into()));
                }
                request.push(0x03);
                request.push(target_host.len() as u8);
                request.extend_from_slice(target_host.as_bytes());
            }
        },
    }
    request.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&request).await?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(ProxyError::Protocol(format!(
            "SOCKS5 回复版本不符（0x{:02x}）",
            head[0]
        )));
    }
    if head[1] != 0x00 {
        return Err(socks5_reply_error(head[1], target_host, target_port));
    }
    match head[3] {
        0x01 => {
            let mut rest = [0u8; 6];
            stream.read_exact(&mut rest).await?;
        }
        0x04 => {
            let mut rest = [0u8; 18];
            stream.read_exact(&mut rest).await?;
        }
        0x03 => {
            let len = stream.read_u8().await? as usize;
            let mut buf = vec![0u8; len + 2];
            stream.read_exact(&mut buf).await?;
        }
        other => {
            return Err(ProxyError::Protocol(format!(
                "SOCKS5 回复地址类型未知（0x{other:02x}）"
            )));
        }
    }
    Ok(())
}

fn socks5_reply_error(rep: u8, host: &str, port: u16) -> ProxyError {
    let target = authority(host, port);
    let reason = match rep {
        0x01 => "一般性失败",
        0x02 => "代理规则不允许该连接",
        0x03 => "网络不可达",
        0x04 => "主机不可达",
        0x05 => "连接被拒绝",
        0x06 => "TTL 过期",
        0x07 => "代理不支持 CONNECT",
        0x08 => "代理不支持该地址类型",
        _ => "未知错误",
    };
    ProxyError::TargetUnreachable {
        target,
        reason: format!("{reason}（0x{rep:02x}）"),
    }
}

async fn http_handshake(
    stream: &mut TcpStream,
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<(), ProxyError> {
    let target = authority(target_host, target_port);
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    let username = proxy
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = proxy
        .password
        .as_ref()
        .map(Secret::expose)
        .filter(|s| !s.is_empty());
    if let (Some(user), Some(pass)) = (username, password) {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    let head = read_http_head(stream).await?;
    let status_line = head.lines().next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let code = parts.next().and_then(|c| c.parse::<u16>().ok());
    let rest = parts.collect::<Vec<_>>().join(" ");
    match code {
        Some(200) => Ok(()),
        Some(407) => Err(if username.is_some() && password.is_some() {
            ProxyError::AuthFailed
        } else {
            ProxyError::AuthRequired
        }),
        Some(code) => Err(ProxyError::Protocol(format!(
            "HTTP 代理返回 {code}{}{}",
            if rest.is_empty() { "" } else { " " },
            rest
        ))),
        None => Err(ProxyError::Protocol(format!(
            "HTTP 代理响应无法解析：{status_line}"
        ))),
    }
}

/// 逐字节读到空行为止，避免把后续 SSH 数据一起吞进缓冲区。
async fn read_http_head(stream: &mut TcpStream) -> Result<String, ProxyError> {
    let mut buf = Vec::with_capacity(256);
    let mut tail = [0u8; 4];
    loop {
        let byte = stream.read_u8().await?;
        buf.push(byte);
        tail.rotate_left(1);
        tail[3] = byte;
        if &tail == b"\r\n\r\n" {
            return String::from_utf8(buf)
                .map_err(|_| ProxyError::Protocol("HTTP 代理响应不是 UTF-8".into()));
        }
        if buf.len() >= MAX_HTTP_HEAD {
            return Err(ProxyError::Protocol("HTTP 代理响应头过长".into()));
        }
    }
}

fn authority(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_brackets_ipv6() {
        assert_eq!(authority("203.0.113.10", 22), "203.0.113.10:22");
        assert_eq!(authority("example.test", 22), "example.test:22");
        assert_eq!(authority("2001:db8::1", 22), "[2001:db8::1]:22");
        assert_eq!(authority("[2001:db8::1]", 22), "[2001:db8::1]:22");
    }

    #[test]
    fn kind_parsing_and_labels() {
        assert_eq!(ProxyKind::parse("SOCKS5"), Some(ProxyKind::Socks5));
        assert_eq!(ProxyKind::parse(" http "), Some(ProxyKind::Http));
        assert_eq!(ProxyKind::parse(""), None);
        assert_eq!(ProxyKind::Http.label(), "HTTP");
    }

    #[test]
    fn reply_errors_are_human_readable() {
        let err = socks5_reply_error(0x05, "203.0.113.10", 22);
        assert!(err.to_string().contains("连接被拒绝"));
    }
}
