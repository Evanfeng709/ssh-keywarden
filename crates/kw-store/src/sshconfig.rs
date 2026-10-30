//! 从 `~/.ssh/config` 里读出现成的主机配置，首次启动时自动导入，
//! 省得用户对着配置文件手抄 HostName / User / IdentityFile。

use std::fs;
use std::path::{Path, PathBuf};

use crate::Host;

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn expand_tilde(p: &str, home: &Path) -> String {
    if p.is_empty() {
        return p.to_string();
    }
    if p == "~" {
        return home.display().to_string();
    }
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return home.join(rest).display().to_string();
    }
    p.to_string()
}

/// 解析 ssh_config 文本；含通配符的段（Host *）不是具体主机，跳过。
pub fn parse_ssh_config(text: &str, home: &Path) -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    let mut cur: Option<usize> = None;

    for raw_line in text.split('\n') {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, raw_val)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let key = key.to_lowercase();
        let val = raw_val.trim().trim_matches('"');

        if key == "host" {
            let names: Vec<&str> = val
                .split_whitespace()
                .filter(|n| !n.is_empty() && !n.contains(['*', '?', '!']))
                .collect();
            if names.is_empty() {
                cur = None;
                continue;
            }
            cur = Some(hosts.len());
            hosts.push(Host {
                name: names[0].to_string(),
                ..Default::default()
            });
            continue;
        }

        let Some(idx) = cur else { continue };
        let entry = &mut hosts[idx];
        match key.as_str() {
            "hostname" => entry.host = val.to_string(),
            "user" => entry.username = val.to_string(),
            "port" => entry.port = val.parse().unwrap_or(22),
            "identityfile" => entry.identity_file = expand_tilde(val, home),
            _ => {}
        }
    }

    hosts
        .into_iter()
        .map(|mut h| {
            if h.host.is_empty() {
                h.host = h.name.clone();
            }
            h
        })
        .filter(|h| !h.host.is_empty() && !h.username.is_empty())
        .collect()
}

pub fn import_from_ssh_config(file: &Path) -> Vec<Host> {
    match fs::read_to_string(file) {
        Ok(text) => parse_ssh_config(&text, &home_dir()),
        Err(_) => Vec::new(),
    }
}
