//! 远端 sshd 登录方式：检测与切换「仅密钥登录」。
//!
//! 判定本身在 `kw-keys::sshd`（纯文本层）；这里只负责跑脚本、拼状态。
//! 改 sshd_config 会直接影响能不能再登进来，调用方必须先过安全闸门。

use std::collections::HashMap;

use base64::Engine;
use serde_json::{json, Map, Value};

use kw_keys::sshd::{parse_config_text, summarize};

use crate::session::{SshClient, SshError};

pub const CONFIG_PATH: &str = "/etc/ssh/sshd_config";

const INSPECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct Section {
    file: String,
    lines: String,
}

/// 检测服务器当前的登录方式。`available=false` 时带 `reason`。
pub async fn inspect(client: &SshClient) -> Result<Value, SshError> {
    let script = [
        "echo \"###UID $(id -u 2>/dev/null)\"",
        "echo \"###SUDO $(sudo -n true 2>/dev/null && echo yes || echo no)\"",
        &format!("echo \"###EXISTS $([ -f {CONFIG_PATH} ] && echo yes || echo no)\""),
        &format!("echo \"###READ $([ -r {CONFIG_PATH} ] && echo yes || echo no)\""),
        &format!(
            "if [ -r {CONFIG_PATH} ]; then echo \"###BEGIN main\"; cat {CONFIG_PATH} 2>/dev/null; fi"
        ),
        "for f in /etc/ssh/sshd_config.d/*.conf; do",
        "  [ -f \"$f\" ] || continue",
        "  [ -r \"$f\" ] || continue",
        "  echo \"###BEGIN $f\"",
        "  cat \"$f\" 2>/dev/null",
        "done",
        "echo \"###END\"",
    ]
    .join("\n");

    let res = client
        .exec_with_timeout(&pipe_script(&script, ""), None, INSPECT_TIMEOUT)
        .await?;

    let mut uid = String::new();
    let mut sudo = false;
    let mut exists = false;
    let mut readable = false;
    let mut sections: Vec<Section> = Vec::new();
    let mut open: Option<usize> = None;

    for line in res.stdout.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(v) = line.strip_prefix("###UID ") {
            uid = v.trim().to_string();
            continue;
        }
        if let Some(v) = line.strip_prefix("###SUDO ") {
            sudo = v.trim() == "yes";
            continue;
        }
        if let Some(v) = line.strip_prefix("###EXISTS ") {
            exists = v.trim() == "yes";
            continue;
        }
        if let Some(v) = line.strip_prefix("###READ ") {
            readable = v.trim() == "yes";
            continue;
        }
        if let Some(v) = line.strip_prefix("###BEGIN ") {
            sections.push(Section {
                file: v.trim().to_string(),
                lines: String::new(),
            });
            open = Some(sections.len() - 1);
            continue;
        }
        if line.starts_with("###END") {
            open = None;
            continue;
        }
        if let Some(i) = open {
            sections[i].lines.push_str(line);
            sections[i].lines.push('\n');
        }
    }

    let is_root = uid == "0";
    let can_write = is_root || sudo;
    let access = json!({ "isRoot": is_root, "canWrite": can_write, "sudo": sudo });

    if !exists {
        return Ok(unavailable(format!("服务器上找不到 {CONFIG_PATH}"), access));
    }
    if !readable {
        return Ok(unavailable(
            format!("没有读取 {CONFIG_PATH} 的权限"),
            access,
        ));
    }

    // drop-in 通常通过 Include 在主配置顶部生效，所以后者覆盖前者
    let mut merged: HashMap<String, String> = HashMap::new();
    let mut source: HashMap<String, String> = HashMap::new();
    for sec in &sections {
        for (k, v) in parse_config_text(&sec.lines) {
            merged.insert(k.clone(), v);
            source.insert(k, sec.file.clone());
        }
    }

    let summary = summarize(&merged);
    let mut obj = match serde_json::to_value(&summary) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let config_path = sections
        .first()
        .map(|s| s.file.clone())
        .unwrap_or_else(|| CONFIG_PATH.to_string());
    obj.insert("available".into(), json!(true));
    obj.insert(
        "files".into(),
        Value::Array(sections.iter().map(|s| json!(s.file)).collect()),
    );
    obj.insert("source".into(), json!(source));
    obj.insert("configPath".into(), json!(config_path));
    obj.insert("isRoot".into(), json!(is_root));
    obj.insert("canWrite".into(), json!(can_write));
    obj.insert("sudo".into(), json!(sudo));
    Ok(Value::Object(obj))
}

fn unavailable(reason: String, access: Value) -> Value {
    let mut obj = Map::new();
    obj.insert("available".into(), json!(false));
    obj.insert("reason".into(), json!(reason));
    if let Value::Object(a) = access {
        for (k, v) in a {
            obj.insert(k, v);
        }
    }
    Value::Object(obj)
}

pub struct SetKeyOnly {
    pub backup: String,
    pub reloaded: bool,
}

/// 切换「仅密钥登录」。同时改 PasswordAuthentication 与 KbdInteractiveAuthentication ——
/// 只改前者会留 PAM 后门。
pub async fn set_key_only(
    client: &SshClient,
    enable: bool,
    is_root: bool,
) -> Result<SetKeyOnly, SshError> {
    let pw_val = if enable { "no" } else { "yes" };
    let kbd_val = pw_val;
    // 开启「仅密钥登录」时必须顺手确保公钥登录是开的，
    // 否则 pubkey=no + password=no 会变成「谁也进不去」
    let pub_val = if enable { "yes" } else { "" };

    let script = format!(
        r#"#!/bin/sh
set -e
CFG={CONFIG_PATH}
PW="{pw_val}"
KBD="{kbd_val}"
PUB="{pub_val}"
STAMP=$(date +%Y%m%d%H%M%S)
BACKUPS=""

[ -f "$CFG" ] || {{ echo "__ERR__ 找不到 $CFG"; exit 1; }}

# append=yes → 替换已有行 + 没有就追加（主配置必须出现这三条）
# append=no  → 只替换已有行（drop-in 里没有的项不碰，让主配置的设置生效）
modify_file() {{
  f="$1"; append="$2"
  cp -p "$f" "$f.bak.$STAMP" || {{ echo "__ERR__ 备份 $f 失败，已中止"; exit 1; }}
  BACKUPS="$BACKUPS $f.bak.$STAMP"
  T=$(mktemp)
  awk -v pw="$PW" -v kbd="$KBD" -v pub="$PUB" -v ap="$append" '
    BEGIN {{ dp = 0; dk = 0; dpu = 0 }}
    tolower($1) == "passwordauthentication" {{
      if (!dp) {{ print "PasswordAuthentication " pw; dp = 1; next }}
    }}
    tolower($1) == "kbdinteractiveauthentication" || tolower($1) == "challengeresponseauthentication" {{
      if (!dk) {{ print "KbdInteractiveAuthentication " kbd; dk = 1; next }}
    }}
    pub != "" && tolower($1) == "pubkeyauthentication" {{
      if (!dpu) {{ print "PubkeyAuthentication " pub; dpu = 1; next }}
    }}
    {{ print }}
    END {{
      if (ap == "yes") {{
        if (!dp)  print "PasswordAuthentication " pw
        if (!dk)  print "KbdInteractiveAuthentication " kbd
        if (pub != "" && !dpu) print "PubkeyAuthentication " pub
      }}
    }}
  ' "$f" > "$T"
  cat "$T" > "$f"   # 用 cat 覆盖，保留原文件的权限与所有者
  rm -f "$T"
}}

modify_file "$CFG" yes
chmod 600 "$CFG" 2>/dev/null || true

# drop-in（云厂商的 50-cloud-init.conf 之类）若写了这些键，会因 Include 在主配置顶部而抢先生效，必须一并改掉
for f in /etc/ssh/sshd_config.d/*.conf; do
  [ -f "$f" ] || continue
  if grep -qiE '^[[:space:]]*(passwordauthentication|kbdinteractiveauthentication|challengeresponseauthentication|pubkeyauthentication)[[:space:]]' "$f"; then
    modify_file "$f" no
  fi
done

ERR_LOG=$(mktemp)
if ! sshd -t 2>"$ERR_LOG"; then
  echo "__ERR__ 新配置语法校验未通过，已全部回滚"
  cat "$ERR_LOG" 2>/dev/null || true   # set -e 下 cat 失败会中断回滚，必须兜住
  rm -f "$ERR_LOG" 2>/dev/null || true
  for b in $BACKUPS; do
    cp -p "$b" "${{b%.bak.$STAMP}}"
  done
  exit 1
fi
rm -f "$ERR_LOG" 2>/dev/null || true

RELOADED="no"
if command -v systemctl >/dev/null 2>&1; then
  systemctl reload sshd 2>/dev/null && RELOADED="yes" || true
  [ "$RELOADED" = "no" ] && systemctl reload ssh 2>/dev/null && RELOADED="yes" || true
fi
if [ "$RELOADED" = "no" ] && command -v service >/dev/null 2>&1; then
  service ssh reload 2>/dev/null && RELOADED="yes" || true
  [ "$RELOADED" = "no" ] && service sshd reload 2>/dev/null && RELOADED="yes" || true
fi

echo "__OK__ $CFG.bak.$STAMP $RELOADED"
"#
    );

    let prefix = if is_root { "" } else { "sudo -n " };
    let res = client
        .exec_with_timeout(&pipe_script(&script, prefix), None, WRITE_TIMEOUT)
        .await?;
    let out = format!("{}\n{}", res.stdout, res.stderr);
    let out = out.trim();

    if out.contains("__OK__") {
        let mut backup = String::new();
        let mut reloaded = false;
        if let Some(pos) = out.find("__OK__") {
            let mut parts = out[pos + "__OK__".len()..].split_whitespace();
            backup = parts.next().unwrap_or("").to_string();
            reloaded = parts.next() == Some("yes");
        }
        return Ok(SetKeyOnly { backup, reloaded });
    }

    let err_line = out
        .split('\n')
        .find(|l| l.contains("__ERR__"))
        .unwrap_or(out)
        .replace("__ERR__", "");
    let msg = err_line.trim();
    Err(SshError::msg(if msg.is_empty() {
        "修改 sshd 配置失败".to_string()
    } else {
        msg.to_string()
    }))
}

/// 脚本走 base64-into-sh：避免多层引号转义，也不依赖 sftp。
fn pipe_script(script: &str, prefix: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    format!("echo '{b64}' | base64 -d | {prefix}sh -s")
}
