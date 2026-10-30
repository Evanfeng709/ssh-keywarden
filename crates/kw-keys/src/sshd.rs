//! sshd 登录方式的文本判定层。
//!
//! 只做「配置文本 → 结论」，不碰网络与文件。
//! 判定要点跟旧版一致：光把 PasswordAuthentication 设成 no 不够，
//! KbdInteractiveAuthentication / ChallengeResponseAuthentication 任一开着，
//! 仍然可能用密码登进来。

use serde::Serialize;
use std::collections::HashMap;

/// 一行 `Key value` 的解析：键取小写，同一文件内首次出现生效。
fn parse_line(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let key = line[..i].to_ascii_lowercase();

    let rest = &line[i..];
    let mut consumed = 0;
    for (idx, ch) in rest.char_indices() {
        if ch.is_whitespace() || ch == '=' {
            consumed = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if consumed == 0 {
        return None;
    }
    let value = rest[consumed..].trim().to_lowercase();
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

pub fn parse_config_text(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for raw in text.split(['\n', '\r']) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = parse_line(line) {
            values.entry(key).or_insert(value);
        }
    }
    values
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshdState {
    Both,
    KeyOnly,
    PasswordOnly,
    None,
}

impl SshdState {
    pub fn as_str(self) -> &'static str {
        match self {
            SshdState::Both => "both",
            SshdState::KeyOnly => "key-only",
            SshdState::PasswordOnly => "password-only",
            SshdState::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshdExplicit {
    pub password_auth: Option<String>,
    pub pubkey_auth: Option<String>,
    pub kbd_interactive: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshdSummary {
    pub password_auth: bool,
    pub pubkey_auth: bool,
    pub kbd_interactive: bool,
    pub challenge_response: bool,
    pub pw_allowed: bool,
    pub state: SshdState,
    pub key_only: bool,
    pub root_login: String,
    pub explicit: SshdExplicit,
}

/// 默认值按 OpenSSH 约定：
/// PasswordAuthentication / PubkeyAuthentication / KbdInteractiveAuthentication 默认 yes。
pub fn summarize(values: &HashMap<String, String>) -> SshdSummary {
    let password_explicit = values.get("passwordauthentication").cloned();
    let kbd_explicit = values.get("kbdinteractiveauthentication").cloned();
    let chal_explicit = values.get("challengeresponseauthentication").cloned();

    let password_auth = password_explicit.as_deref() != Some("no");
    let kbd_interactive = kbd_explicit.as_deref() != Some("no");
    // ChallengeResponseAuthentication 是历史选项，只有显式写成 yes 才算它开着
    let challenge_response = matches!(chal_explicit.as_deref(), Some(v) if v != "no");
    let pubkey_auth = values
        .get("pubkeyauthentication")
        .map(|v| v != "no")
        .unwrap_or(true);

    let pw_allowed = password_auth || kbd_interactive || challenge_response;

    let state = match (pubkey_auth, pw_allowed) {
        (true, true) => SshdState::Both,
        (true, false) => SshdState::KeyOnly,
        (false, true) => SshdState::PasswordOnly,
        (false, false) => SshdState::None,
    };

    SshdSummary {
        password_auth,
        pubkey_auth,
        kbd_interactive,
        challenge_response,
        pw_allowed,
        state,
        key_only: state == SshdState::KeyOnly,
        root_login: values
            .get("permitrootlogin")
            .cloned()
            .unwrap_or_else(|| "prohibit-password".to_string()),
        explicit: SshdExplicit {
            password_auth: password_explicit,
            pubkey_auth: values.get("pubkeyauthentication").cloned(),
            kbd_interactive: kbd_explicit,
        },
    }
}
