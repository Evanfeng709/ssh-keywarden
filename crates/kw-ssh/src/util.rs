//! SSH 层不依赖连接的纯工具：报错翻译、shell 转义、备份时间戳。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// shell 单引号转义。
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn is_disconnect_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "unable to exec",
        "no response from server",
        "client is not connected",
        "not connected",
        "connection lost",
        "econnreset",
        "epipe",
        "channel open failure",
        "connection is closed",
        "socket is closed",
        "channel closed",
        "connection closed",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// 把底层报错翻译成用户能看懂的话。`passphrase_set` 影响「私钥解不开」的措辞。
pub fn friendly_error(err: &str, host: &str, port: u16, passphrase_set: bool) -> String {
    let msg = err;
    let host = if host.is_empty() {
        "目标主机"
    } else {
        host
    };

    // 断线类优先判断：这类错误发生在连接已建立之后，跟地址端口无关
    if is_disconnect_error(msg) {
        return "SSH 连接已断开（服务器可能主动断开了空闲连接或网络中断），请重新连接后再操作"
            .to_string();
    }
    if msg.contains("ECONNREFUSED") {
        return format!("连接被拒绝：{host}:{port} 上没有 SSH 服务，或防火墙拦了");
    }
    if msg.contains("ENOTFOUND") || msg.contains("EAI_AGAIN") {
        return format!("域名解析失败：{host}");
    }
    if msg.contains("ETIMEDOUT") || msg.to_lowercase().contains("timed out") {
        return format!("连接超时：{host}:{port} 无响应（内网地址需确认是否同一网络）");
    }
    if msg.contains("All configured authentication methods failed") {
        return "认证失败：私钥/密码不被服务器接受（检查用户名、私钥是否匹配、authorized_keys 是否含这把钥匙）".to_string();
    }
    if msg.contains("Encrypted private OpenSSH key detected") || msg.contains("no passphrase given")
    {
        return if passphrase_set {
            "私钥密码（passphrase）不正确，这把私钥解不开。请再确认一下".to_string()
        } else {
            "这把私钥已加密，需要填写 passphrase 才能使用".to_string()
        };
    }
    if msg.contains("Cannot parse privateKey")
        || msg.contains("Bad passphrase")
        || msg.contains("Invalid PEM")
        || msg.contains("bad decrypt")
    {
        return if passphrase_set {
            format!("私钥解析失败，passphrase 可能不正确（{msg}）")
        } else {
            format!("私钥无法解析：{msg}")
        };
    }
    if msg.contains("Host key verification") {
        return format!("主机指纹校验失败：{msg}");
    }
    msg.to_string()
}

/// 上海时间 `YYYYMMDDHHMMSS`（备份名用）。中国无夏令时，固定 +08:00。
pub fn shanghai_stamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let local = secs + 8 * 3600;
    let days = local.div_euclid(86_400);
    let secs_of_day = local.rem_euclid(86_400);

    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}")
}

/// Howard Hinnant 的 days → (y, m, d) 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 读远端文本：文件不存在返回 null，空文件返回 ''。
/// 走 base64 而不是 SFTP：不依赖 sftp 子系统（部分 sshd/受限账号会关掉它）。
pub fn read_remote_text_command(path: &str) -> String {
    format!(
        "if [ -f {p} ]; then base64 < {p}; else echo __KW_MISSING__; fi",
        p = quote(path)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_single_quotes() {
        assert_eq!(quote("/a/b"), "'/a/b'");
        assert_eq!(quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn stamp_is_shanghai_time() {
        assert_eq!(shanghai_stamp(UNIX_EPOCH), "19700101080000");
        // 2026-10-22T01:30:15Z == 09:30:15 +08:00
        let t = UNIX_EPOCH + Duration::from_secs(1_792_632_615);
        assert_eq!(shanghai_stamp(t), "20261022093015");
    }

    #[test]
    fn friendly_error_maps_common_cases() {
        assert!(friendly_error("ECONNREFUSED", "h", 22, false).contains("连接被拒绝"));
        assert!(friendly_error("getaddrinfo ENOTFOUND h", "h", 22, false).contains("域名解析失败"));
        assert!(friendly_error("Connection lost", "h", 22, false).contains("已断开"));
        assert!(friendly_error(
            "All configured authentication methods failed",
            "h",
            22,
            false
        )
        .contains("认证失败"));
        assert!(
            friendly_error("Encrypted private OpenSSH key detected", "h", 22, false)
                .contains("需要填写 passphrase")
        );
        assert!(
            friendly_error("Encrypted private OpenSSH key detected", "h", 22, true)
                .contains("不正确")
        );
        assert_eq!(friendly_error("weird", "h", 22, false), "weird");
    }

    #[test]
    fn read_command_uses_base64_and_missing_marker() {
        let cmd = read_remote_text_command("/home/a/.ssh/authorized_keys");
        assert!(cmd.contains("base64 < '/home/a/.ssh/authorized_keys'"));
        assert!(cmd.contains("__KW_MISSING__"));
    }
}
