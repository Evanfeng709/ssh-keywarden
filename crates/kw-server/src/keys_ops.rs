//! 密钥增删换：新增、更换、删除、从备份恢复。
//!
//! 这四个都是「读 → 改 → 备份 → 写 → 回读」多步流程，调用方必须已经持有
//! `Session::write_lock`，否则并发请求会互相把修改盖掉。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};

use kw_core::Secret;
use kw_keys::authorized::{append_key, render_without};
use kw_keys::backups::{diff_backup, parse_backup_name};
use kw_keys::keygen::{
    generate_ed25519, generate_rsa, load_private_key, parse_public_key_line, parse_public_keys,
    GeneratedKey,
};
use kw_keys::parse_authorized_keys;
use kw_ssh::session::{verify_login, SshConfig};
use kw_ssh::util::shanghai_stamp;
use kw_store::HostPatch;

use crate::error::ApiError;
use crate::fsbrowse::{home_dir, safe_name};
use crate::sessions::Session;
use crate::state::AppState;

pub struct FreshKey {
    pub public_key_line: String,
    pub fingerprint: String,
    pub key_type: String,
    pub private_key: Option<Secret>,
    pub private_key_path: Option<String>,
    pub steps: Vec<String>,
}

struct SwitchOutcome {
    switched: bool,
    identity_file: String,
    reason: String,
    skipped_by_user: bool,
}

impl SwitchOutcome {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            switched: false,
            identity_file: String::new(),
            reason: reason.into(),
            skipped_by_user: false,
        }
    }
}

fn body_str<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn body_bool(body: &Value, key: &str) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(false)
}

async fn read_current(session: &Session) -> Result<String, ApiError> {
    if !session.remote.read().await.file_exists {
        return Ok(String::new());
    }
    let path = session.remote_path().await;
    Ok(session.client.read_text(&path).await?.unwrap_or_default())
}

/// 远端落盘 + 回读，返回回读文本；步骤说明追加进 steps。
async fn commit(
    session: &Session,
    text: &str,
    steps: &mut Vec<String>,
) -> Result<String, ApiError> {
    let path = session.remote_path().await;
    let out = session.client.commit_text(&path, text).await?;
    steps.push(match &out.backup {
        Some(name) => format!("已备份原文件：{name}"),
        None => "目标文件原本不存在，无需备份".to_string(),
    });
    steps.push(format!("已写入 {path}"));
    session.remote.write().await.file_exists = true;
    Ok(out.readback)
}

fn save_key_pair(dir: &Path, gen: &GeneratedKey, label: &str) -> Result<PathBuf, ApiError> {
    fs::create_dir_all(dir)
        .map_err(|e| ApiError::bad(format!("创建目录失败：{}（{e}）", dir.display())))?;

    // 命名成 服务器名_日期.key，一眼看出是谁的钥匙
    let stem = format!(
        "{}_{}",
        safe_name(label),
        shanghai_stamp(SystemTime::now())
            .chars()
            .take(8)
            .collect::<String>()
    );
    let mut priv_path = dir.join(format!("{stem}.key"));
    let mut i = 2;
    while priv_path.exists() || Path::new(&format!("{}.pub", priv_path.display())).exists() {
        priv_path = dir.join(format!("{stem}_{i}.key"));
        i += 1;
    }
    write_private(&priv_path, &gen.private_key_pem)?;
    let pub_path = PathBuf::from(format!("{}.pub", priv_path.display()));
    fs::write(&pub_path, format!("{}\n", gen.public_key_line))
        .map_err(|e| ApiError::bad(format!("写入公钥文件失败：{e}")))?;
    Ok(priv_path)
}

fn write_private(path: &Path, text: &str) -> Result<(), ApiError> {
    fs::write(path, text).map_err(|e| ApiError::bad(format!("写入私钥文件失败：{e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn find_private_key_for(public_path: &str) -> Option<String> {
    let stripped = public_path
        .strip_suffix(".pub")
        .or_else(|| public_path.strip_suffix(".PUB"))?;
    if Path::new(stripped).exists() {
        Some(stripped.to_string())
    } else {
        None
    }
}

async fn resolve_new_key(
    state: &AppState,
    session: &Arc<Session>,
    body: &Value,
) -> Result<FreshKey, ApiError> {
    let mode = body.get("mode").and_then(Value::as_str).unwrap_or("");
    let passphrase = body
        .get("passphrase")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    match mode {
        "generate" => {
            let generator = body
                .get("generator")
                .and_then(Value::as_str)
                .unwrap_or("ed25519");
            let comment = body_str(body, "comment");
            let gen = if generator == "rsa" {
                generate_rsa(comment, 3072, passphrase)
            } else {
                generate_ed25519(comment, passphrase)
            }
            .map_err(|e| ApiError::bad(format!("生成密钥失败：{e}")))?;

            let settings = state.current_settings();
            let dir = body_str(body, "saveDir")
                .map(str::to_string)
                .or_else(|| Some(settings.save_dir).filter(|s| !s.is_empty()))
                .or_else(|| Some(settings.local_key_dir).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| home_dir().join(".ssh").display().to_string());

            let label = {
                let target = session.target.read().await;
                if target.name.is_empty() {
                    target.host.clone()
                } else {
                    target.name.clone()
                }
            };
            let dir_path = PathBuf::from(&dir);
            let priv_path = save_key_pair(&dir_path, &gen, &label)?;
            state.invalidate_scan(&dir).await;
            let priv_str = priv_path.display().to_string();
            let pub_str = format!("{priv_str}.pub");
            Ok(FreshKey {
                public_key_line: gen.public_key_line.clone(),
                fingerprint: gen.fingerprint.clone(),
                key_type: gen.key_type.clone(),
                private_key: Some(Secret::new(gen.private_key_pem)),
                private_key_path: Some(priv_str.clone()),
                steps: vec![
                    format!(
                        "已生成 {} 密钥对{}",
                        gen.key_type,
                        if gen.encrypted {
                            "（私钥已用 passphrase 加密）"
                        } else {
                            ""
                        }
                    ),
                    format!("私钥：{priv_str}"),
                    format!("公钥：{pub_str}"),
                ],
            })
        }

        "local" => {
            let p = body_str(body, "path").ok_or_else(|| ApiError::bad("没有指定本地公钥文件"))?;
            if !Path::new(p).exists() {
                return Err(ApiError::bad(format!("本地文件不存在：{p}")));
            }

            if p.to_ascii_lowercase().ends_with(".pub") {
                let pub_text = fs::read_to_string(p)
                    .map_err(|e| ApiError::bad(format!("读取公钥文件失败：{p}（{e}）")))?;
                let keys = parse_public_keys(&pub_text);
                let Some(k) = keys.first() else {
                    return Err(ApiError::bad(format!("文件里没有可识别的公钥：{p}")));
                };
                let priv_path = find_private_key_for(p);
                let private_key = priv_path
                    .as_deref()
                    .and_then(|pp| fs::read_to_string(pp).ok())
                    .map(Secret::new);
                Ok(FreshKey {
                    public_key_line: line_of(k),
                    fingerprint: k.fingerprint.clone(),
                    key_type: k.key_type.clone(),
                    private_key,
                    private_key_path: priv_path,
                    steps: vec![format!("读取本地公钥：{p}")],
                })
            } else {
                // 直接选了私钥：从私钥导出公钥行
                let priv_text = Secret::new(
                    fs::read_to_string(p)
                        .map_err(|e| ApiError::bad(format!("读取私钥文件失败：{p}（{e}）")))?,
                );
                let key = load_private_key(priv_text.expose(), passphrase)
                    .map_err(|e| ApiError::bad(format!("无法解析私钥 {p}：{e}")))?;
                let pub_line = key
                    .public_key()
                    .to_openssh()
                    .map_err(|e| ApiError::bad(format!("无法导出公钥 {p}：{e}")))?;
                let parsed = parse_public_key_line(&pub_line)
                    .ok_or_else(|| ApiError::bad(format!("无法解析私钥 {p} 里的公钥")))?;
                Ok(FreshKey {
                    public_key_line: line_of(&parsed),
                    fingerprint: parsed.fingerprint.clone(),
                    key_type: parsed.key_type.clone(),
                    private_key: Some(priv_text),
                    private_key_path: Some(p.to_string()),
                    steps: vec![format!("读取本地私钥：{p}")],
                })
            }
        }

        "paste" => {
            let raw = body.get("publicKey").and_then(Value::as_str).unwrap_or("");
            let keys = parse_public_keys(raw);
            let Some(k) = keys.first() else {
                return Err(ApiError::bad(
                    "粘贴的内容里没有识别到公钥（应以 ssh-ed25519 / ssh-rsa 等开头）",
                ));
            };
            if !k.valid {
                return Err(ApiError::bad(format!(
                    "公钥格式无效：{}",
                    if k.error.is_empty() {
                        "结构损坏"
                    } else {
                        &k.error
                    }
                )));
            }
            Ok(FreshKey {
                public_key_line: line_of(k),
                fingerprint: k.fingerprint.clone(),
                key_type: k.key_type.clone(),
                private_key: None,
                private_key_path: None,
                steps: vec!["已解析粘贴的公钥".to_string()],
            })
        }

        other => Err(ApiError::bad(format!("未知的新增方式：{other}"))),
    }
}

fn line_of(k: &kw_keys::ParsedPublicKey) -> String {
    if k.comment.is_empty() {
        format!("{} {}", k.key_type, k.b64)
    } else {
        format!("{} {} {}", k.key_type, k.b64, k.comment)
    }
}

/// 把主机配置里的「登录私钥」指向新文件。
///
/// 同时更新 `identity_fingerprint`：这个字段要跟着同步包走到别的设备，
/// 只改路径的话，手机拿到的还是旧指纹，会去找已经废弃的那把钥匙。
async fn switch_host_identity(
    state: &AppState,
    session: &Arc<Session>,
    private_key_path: &str,
    fingerprint: &str,
) -> Result<SwitchOutcome, ApiError> {
    if private_key_path.is_empty() {
        return Ok(SwitchOutcome::skipped("没有本地私钥文件路径"));
    }
    let target = session.target.read().await.clone();
    if target.id.is_empty() {
        return Ok(SwitchOutcome::skipped(
            "当前是临时连接（没保存成主机），配置未更新",
        ));
    }
    let Some(host) = state
        .store
        .list_hosts()
        .into_iter()
        .find(|h| h.id == target.id)
    else {
        return Ok(SwitchOutcome::skipped("主机配置已不存在"));
    };
    state
        .store
        .upsert_host(HostPatch {
            id: Some(host.id.clone()),
            name: Some(host.name),
            host: Some(host.host),
            port: Some(host.port),
            username: Some(host.username),
            identity_file: Some(private_key_path.to_string()),
            identity_fingerprint: Some(fingerprint.to_string()),
            remote_path: Some(host.remote_path),
        })
        .map_err(|e| ApiError::bad(e.to_string()))?;
    session.target.write().await.identity_file = private_key_path.to_string();
    Ok(SwitchOutcome {
        switched: true,
        identity_file: private_key_path.to_string(),
        reason: String::new(),
        skipped_by_user: false,
    })
}

/// 切换登录密钥前必须用新私钥真连一次 —— 否则等于拿没验证过的钥匙换门锁。
async fn maybe_switch_identity(
    state: &AppState,
    session: &Arc<Session>,
    fresh: &FreshKey,
    body: &Value,
    steps: &mut Vec<String>,
) -> Result<SwitchOutcome, ApiError> {
    if !body_bool(body, "switchToNew") {
        return Ok(SwitchOutcome {
            switched: false,
            identity_file: String::new(),
            reason: String::new(),
            skipped_by_user: true,
        });
    }
    let Some(text) = fresh.private_key.clone() else {
        let reason = "这把公钥在本地没有对应的私钥文件".to_string();
        steps.push(format!("跳过切换：{reason}"));
        return Ok(SwitchOutcome::skipped(reason));
    };
    let Some(path) = fresh.private_key_path.clone() else {
        let reason = "这把公钥在本地没有对应的私钥文件".to_string();
        steps.push(format!("跳过切换：{reason}"));
        return Ok(SwitchOutcome::skipped(reason));
    };

    let target = session.target.read().await.clone();
    let cfg = SshConfig {
        host: target.host,
        port: target.port,
        username: target.username,
        private_key: Some(text),
        passphrase: body
            .get("passphrase")
            .and_then(Value::as_str)
            .map(Secret::new),
        password: None,
        trust_fingerprint: None,
    };
    match verify_login(&cfg, Arc::clone(&state.known_hosts)).await {
        Ok(()) => steps.push("已用新私钥实际登录验证通过".to_string()),
        Err(e) => {
            let reason = format!("新私钥登录验证未通过 —— {e}");
            steps.push(format!("跳过切换：{reason}"));
            return Ok(SwitchOutcome::skipped(reason));
        }
    }

    let info = switch_host_identity(state, session, &path, &fresh.fingerprint).await?;
    if info.switched {
        steps.push(format!(
            "已把本机登录密钥切换为新钥匙：{}",
            info.identity_file
        ));
    } else {
        steps.push(format!("未更新主机配置：{}", info.reason));
    }
    Ok(info)
}

fn switch_json(sw: &SwitchOutcome) -> Value {
    json!({
        "switched": sw.switched,
        "identityFile": sw.identity_file,
        "switchReason": sw.reason,
        "skippedByUser": sw.skipped_by_user,
    })
}

pub async fn add_key(
    state: &AppState,
    session: &Arc<Session>,
    body: &Value,
) -> Result<Value, ApiError> {
    let fresh = resolve_new_key(state, session, body).await?;
    let mut steps = fresh.steps.clone();

    let raw = read_current(session).await?;
    let parsed = parse_authorized_keys(&raw);

    let outcome =
        append_key(&parsed, &fresh.public_key_line).map_err(|e| ApiError::bad(e.to_string()))?;

    if !outcome.appended {
        let sw = maybe_switch_identity(state, session, &fresh, body, &mut steps).await?;
        let mut data = json!({
            "changed": false,
            "message": outcome.reason.clone().unwrap_or_default(),
            "fingerprint": fresh.fingerprint,
            "steps": steps,
            "session": session.payload(&raw).await,
        });
        merge_switch(&mut data, &sw);
        return Ok(data);
    }

    steps.push("公钥不在服务器上，准备写入".to_string());
    let new_text = outcome
        .text
        .ok_or_else(|| ApiError::bad("生成新内容失败（内部状态异常）"))?;
    let readback = commit(session, &new_text, &mut steps).await?;

    let found = parse_authorized_keys(&readback)
        .key_items()
        .any(|i| i.key().is_some_and(|k| k.fingerprint == fresh.fingerprint));
    if !found {
        return Err(ApiError::bad(
            "回读校验失败：新公钥没有出现在 authorized_keys 里",
        ));
    }
    steps.push(format!("校验通过：指纹 {}", fresh.fingerprint));

    let sw = maybe_switch_identity(state, session, &fresh, body, &mut steps).await?;
    let mut data = json!({
        "changed": true,
        "fingerprint": fresh.fingerprint,
        "type": fresh.key_type,
        "privateKeyPath": fresh.private_key_path.clone().unwrap_or_default(),
        "steps": steps,
        "session": session.payload(&readback).await,
    });
    merge_switch(&mut data, &sw);
    Ok(data)
}

fn merge_switch(data: &mut Value, sw: &SwitchOutcome) {
    let sw_json = switch_json(sw);
    for (k, v) in sw_json.as_object().into_iter().flatten() {
        data[k] = v.clone();
    }
}

pub async fn replace_key(
    state: &AppState,
    session: &Arc<Session>,
    body: &Value,
) -> Result<Value, ApiError> {
    let mut steps: Vec<String> = Vec::new();
    let old_fp = body_str(body, "oldFingerprint")
        .ok_or_else(|| ApiError::bad("没有指定要更换的旧密钥"))?
        .to_string();

    let raw = read_current(session).await?;
    let parsed = parse_authorized_keys(&raw);
    let old_exists = parsed
        .key_items()
        .any(|i| i.key().is_some_and(|k| k.fingerprint == old_fp));
    if !old_exists {
        return Err(ApiError::bad(
            "服务器上找不到这把旧密钥（可能已被删除或文件已变化），请刷新后重试",
        ));
    }

    let fresh = resolve_new_key(state, session, body).await?;
    steps.extend(fresh.steps.clone());

    if fresh.fingerprint == old_fp {
        return Err(ApiError::bad(
            "新密钥和旧密钥是同一把（指纹相同），无需更换",
        ));
    }

    // 1) 先加新
    let with_new =
        append_key(&parsed, &fresh.public_key_line).map_err(|e| ApiError::bad(e.to_string()))?;
    let mut after_add_raw = raw.clone();
    if with_new.appended {
        after_add_raw = commit(
            session,
            with_new.text.as_deref().unwrap_or(&raw),
            &mut steps,
        )
        .await?;
        steps.push("第 1 步完成：新密钥已加入".to_string());
    } else {
        steps.push(format!(
            "第 1 步跳过：{}",
            with_new.reason.clone().unwrap_or_default()
        ));
    }

    // 2) 验证新钥匙真的能登录（拿得到私钥才验证）
    let mut verified = false;
    let mut verify_error = String::new();
    if let Some(text) = fresh.private_key.clone() {
        let target = session.target.read().await.clone();
        let cfg = SshConfig {
            host: target.host,
            port: target.port,
            username: target.username,
            private_key: Some(text),
            passphrase: body
                .get("passphrase")
                .and_then(Value::as_str)
                .map(Secret::new),
            password: None,
            trust_fingerprint: None,
        };
        match verify_login(&cfg, Arc::clone(&state.known_hosts)).await {
            Ok(()) => {
                verified = true;
                steps.push("第 2 步完成：用新私钥实际登录测试通过".to_string());
            }
            Err(e) => {
                verify_error = e.to_string();
                steps.push(format!(
                    "第 2 步失败：新私钥登录测试未通过 —— {verify_error}"
                ));
            }
        }
    } else {
        verify_error = "新密钥没有对应的本地私钥，无法自动验证能否登录".to_string();
        steps.push(format!("第 2 步跳过：{verify_error}"));
    }

    if !verified && !body_bool(body, "force") {
        return Ok(json!({
            "changed": with_new.appended,
            "removedOld": false,
            "needsForce": true,
            "message": format!("{verify_error}。为防把自己锁在门外，旧密钥【没有删除】。确认新钥匙能登录后，可勾选强制完成。"),
            "steps": steps,
            "session": session.payload(&after_add_raw).await,
        }));
    }

    // 3) 删旧
    let cur_raw = session
        .client
        .read_text(&session.remote_path().await)
        .await?
        .unwrap_or_default();
    let cur_parsed = parse_authorized_keys(&cur_raw);
    let drop_indexes: Vec<usize> = cur_parsed
        .key_items()
        .filter(|i| i.key().is_some_and(|k| k.fingerprint == old_fp))
        .map(|i| i.index)
        .collect();
    if drop_indexes.is_empty() {
        steps.push("旧密钥在回读时已不存在，无需删除".to_string());
        return Ok(json!({
            "changed": true,
            "removedOld": true,
            "steps": steps,
            "session": session.payload(&cur_raw).await,
        }));
    }
    let remaining = cur_parsed.key_count() - drop_indexes.len();
    if remaining <= 0 {
        return Err(ApiError::bad(
            "旧密钥是服务器上最后一根钥匙，拒绝删除（否则你将无法再登录）",
        ));
    }

    let final_text = render_without(&cur_parsed, &drop_indexes);
    let fin = commit(session, &final_text, &mut steps).await?;
    steps.push("第 3 步完成：旧密钥已移除".to_string());
    if session.used_key_fp.read().await.as_str() == old_fp {
        *session.used_key_fp.write().await = fresh.fingerprint.clone();
        steps.push("当前会话的钥匙标记已切换到新密钥".to_string());
    }

    // 旧公钥已从服务器删掉，本机配置还指着旧私钥会导致下次登录失败
    let mut sw = SwitchOutcome::skipped("新密钥在本地没有对应私钥文件");
    if let Some(path) = fresh.private_key_path.clone() {
        sw = switch_host_identity(state, session, &path, &fresh.fingerprint).await?;
        if sw.switched {
            steps.push(format!(
                "已把本机登录密钥切换为新钥匙：{}",
                sw.identity_file
            ));
        } else {
            steps.push(format!("未更新主机配置：{}", sw.reason));
        }
    } else {
        steps.push(
            "注意：新密钥在本地没有对应私钥文件，请手动把主机私钥改成对应文件，否则下次连接会失败"
                .to_string(),
        );
    }

    let still_there = parse_authorized_keys(&fin)
        .key_items()
        .any(|i| i.key().is_some_and(|k| k.fingerprint == old_fp));
    if still_there {
        return Err(ApiError::bad("回读校验失败：旧密钥仍然存在"));
    }

    let mut data = json!({
        "changed": true,
        "removedOld": true,
        "steps": steps,
        "session": session.payload(&fin).await,
    });
    merge_switch(&mut data, &sw);
    Ok(data)
}

pub async fn delete_keys(session: &Arc<Session>, body: &Value) -> Result<Value, ApiError> {
    let mut steps: Vec<String> = Vec::new();
    let targets: Vec<String> = body
        .get("fingerprints")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if targets.is_empty() {
        return Err(ApiError::bad("没有选择要删除的密钥"));
    }

    let raw = read_current(session).await?;
    let parsed = parse_authorized_keys(&raw);

    let drop_items: Vec<(usize, String)> = parsed
        .key_items()
        .filter_map(|i| {
            let k = i.key()?;
            targets
                .contains(&k.fingerprint)
                .then(|| (i.index, k.fingerprint.clone()))
        })
        .collect();
    if drop_items.is_empty() {
        return Err(ApiError::bad("服务器上找不到选中的密钥，请刷新后重试"));
    }

    let used_fp = session.used_key_fp.read().await.clone();
    if !used_fp.is_empty() && targets.contains(&used_fp) && !body_bool(body, "force") {
        return Ok(json!({
            "changed": false,
            "needsForce": true,
            "message": "选中的密钥里包含【本次连接正在使用】的那把，删掉它会立刻把你锁在门外。确认要删请勾选强制删除。",
            "steps": steps,
            "session": session.payload(&raw).await,
        }));
    }

    let remaining = parsed.key_count() - drop_items.len();
    if remaining <= 0 {
        return Err(ApiError::bad(
            "不能把 authorized_keys 清空 —— 那等于把服务器锁死。请至少保留一把能登录的钥匙",
        ));
    }

    let indexes: Vec<usize> = drop_items.iter().map(|(idx, _)| *idx).collect();
    let text = render_without(&parsed, &indexes);
    let mut unique = targets.clone();
    unique.sort();
    unique.dedup();
    steps.push(format!(
        "准备删除 {} 条：{}",
        drop_items.len(),
        unique.join(", ")
    ));
    let fin = commit(session, &text, &mut steps).await?;

    let left: Vec<kw_keys::ParsedPublicKey> = parse_authorized_keys(&fin)
        .key_items()
        .filter_map(|i| i.key().cloned())
        .collect();
    let leaked = left
        .iter()
        .filter(|k| targets.contains(&k.fingerprint))
        .count();
    if leaked > 0 {
        return Err(ApiError::bad(format!(
            "删除未完全生效，仍有 {leaked} 条匹配的密钥"
        )));
    }

    steps.push(format!("剩余 {} 把钥匙", left.len()));
    Ok(json!({
        "changed": true,
        "steps": steps,
        "session": session.payload(&fin).await,
    }))
}

pub async fn restore_backup(session: &Arc<Session>, body: &Value) -> Result<Value, ApiError> {
    let mut steps: Vec<String> = Vec::new();
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let remote_path = session.remote_path().await;
    let backup_path = kw_keys::backups::backup_path_for(&remote_path, name)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let parsed_name =
        parse_backup_name(&remote_path, name).ok_or_else(|| ApiError::bad("备份文件名不合法"))?;

    let current_raw = session.client.read_text(&remote_path).await?;
    let backup_text = session
        .client
        .read_text(&backup_path)
        .await?
        .ok_or_else(|| ApiError::bad("备份文件不存在，请刷新备份列表"))?;

    let used_fp = session.used_key_fp.read().await.clone();
    let diff = diff_backup(
        current_raw.as_deref().unwrap_or(""),
        &backup_text,
        Some(used_fp.as_str()).filter(|s| !s.is_empty()),
        current_raw.is_some(),
    );
    if diff.empty_backup {
        return Err(ApiError::bad(
            "这份备份里没有任何可用的密钥，恢复会清空 authorized_keys，已拒绝",
        ));
    }
    if diff.used_key_in_backup == Some(false) && !body_bool(body, "force") {
        return Ok(json!({
            "changed": false,
            "needsForce": true,
            "message": "这份备份不包含本次连接正在使用的钥匙。恢复后这个会话仍能操作，但下次连接会失败。确认要恢复请勾选强制。",
            "diff": diff,
            "steps": steps,
            "session": session.payload(current_raw.as_deref().unwrap_or("")).await,
        }));
    }
    if current_raw.as_deref().unwrap_or("") == backup_text {
        return Ok(json!({
            "changed": false,
            "message": "这份备份与当前 authorized_keys 完全一致，无需恢复",
            "diff": diff,
            "steps": steps,
            "session": session.payload(current_raw.as_deref().unwrap_or("")).await,
        }));
    }

    steps.push(format!(
        "准备从备份恢复：{}（{name}）",
        parsed_name.display_time
    ));
    let fin = commit(session, &backup_text, &mut steps).await?;

    let expected: std::collections::HashSet<String> = parse_authorized_keys(&backup_text)
        .key_items()
        .filter_map(|i| i.key().map(|k| k.fingerprint.clone()))
        .collect();
    let actual: std::collections::HashSet<String> = parse_authorized_keys(&fin)
        .key_items()
        .filter_map(|i| i.key().map(|k| k.fingerprint.clone()))
        .collect();
    if expected != actual {
        return Err(ApiError::bad(
            "恢复后回读的钥匙集合与备份不一致，请检查服务器",
        ));
    }
    steps.push(format!("恢复完成：{} 把钥匙", expected.len()));

    Ok(json!({
        "changed": true,
        "restored": true,
        "diff": diff,
        "steps": steps,
        "session": session.payload(&fin).await,
    }))
}

/// 列出同目录下的备份，新的在前。HTTP 层与手机 FFI 共用。
pub async fn list_backups(session: &Arc<Session>) -> Result<Value, ApiError> {
    let remote_path = session.remote_path().await;
    let backups = session.client.list_backup_names(&remote_path).await?;
    Ok(json!({ "remotePath": remote_path, "backups": backups }))
}

/// 单份备份与当前文件的差异（恢复前预览用）。
pub async fn backup_diff(session: &Arc<Session>, name: &str) -> Result<Value, ApiError> {
    let remote_path = session.remote_path().await;
    let backup_path = kw_keys::backups::backup_path_for(&remote_path, name)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let parsed_name =
        parse_backup_name(&remote_path, name).ok_or_else(|| ApiError::bad("备份文件名不合法"))?;

    let current_raw = session.client.read_text(&remote_path).await?;
    let backup_text = session
        .client
        .read_text(&backup_path)
        .await?
        .ok_or_else(|| ApiError::bad("备份文件不存在，请刷新备份列表"))?;

    let used_fp = session.used_key_fp.read().await.clone();
    let diff = diff_backup(
        current_raw.as_deref().unwrap_or(""),
        &backup_text,
        Some(used_fp.as_str()).filter(|s| !s.is_empty()),
        current_raw.is_some(),
    );

    Ok(json!({
        "name": parsed_name.name,
        "stamp": parsed_name.stamp,
        "displayTime": parsed_name.display_time,
        "backupKeyCount": diff.backup_key_count,
        "currentKeyCount": diff.current_key_count,
        "sameCount": diff.same_count,
        "added": diff.added,
        "removed": diff.removed,
        "backupMalformed": diff.backup_malformed,
        "currentMalformed": diff.current_malformed,
        "emptyBackup": diff.empty_backup,
        "usedKeyInBackup": diff.used_key_in_backup,
        "currentFileExists": diff.current_file_exists,
    }))
}

/// 切换「仅密钥登录」。安全闸门集中在这里，桌面 HTTP 与手机端走同一条路径：
/// 文件非空、当前确为密钥登录、本次所用指纹确实在 authorized_keys 里。
pub async fn set_key_only(session: &Arc<Session>, enable: bool) -> Result<Value, ApiError> {
    let before = kw_ssh::sshd::inspect(&session.client).await?;
    if !before["available"].as_bool().unwrap_or(false) {
        return Err(ApiError::bad(
            before["reason"].as_str().unwrap_or("无法读取 sshd 配置"),
        ));
    }
    if !before["canWrite"].as_bool().unwrap_or(false) {
        return Err(ApiError::bad("需要 root 或免密 sudo 才能修改 sshd 配置"));
    }

    // 开启前必须确认：自己现在就是用密钥登录，且这把钥匙在服务器上
    if enable {
        let keys_text = session
            .client
            .read_text(&session.remote_path().await)
            .await?
            .unwrap_or_default();
        let parsed = parse_authorized_keys(&keys_text);
        if parsed.key_count() == 0 {
            return Err(ApiError::bad(
                "服务器上没有任何授权密钥，开启仅密钥登录会让你彻底进不去，已拒绝",
            ));
        }
        if session.auth_method != "key" {
            return Err(ApiError::bad(
                "本次是密码登录。开启仅密钥登录后，这个会话一断就再也连不回来 —— 请先用密钥成功登录一次再操作",
            ));
        }
        let used_fp = session.used_key_fp.read().await.clone();
        if used_fp.is_empty() {
            return Err(ApiError::bad(
                "无法确认本次连接所用的密钥，为安全起见已拒绝",
            ));
        }
        let in_file = parsed
            .key_items()
            .any(|i| i.key().is_some_and(|k| k.fingerprint == used_fp));
        if !in_file {
            return Err(ApiError::bad(
                "本次连接所用的钥匙不在 authorized_keys 里，开启后会把你锁死，已拒绝",
            ));
        }
    }

    let is_root = before["isRoot"].as_bool().unwrap_or(false);
    let result = kw_ssh::sshd::set_key_only(&session.client, enable, is_root).await?;
    let after = kw_ssh::sshd::inspect(&session.client).await?;
    let password_auth = after["passwordAuth"].as_bool().unwrap_or(false);
    let effective = if enable {
        !password_auth
    } else {
        password_auth
    };

    Ok(json!({
        "ok": true,
        "backup": result.backup,
        "reloaded": result.reloaded,
        "status": after,
        "effective": effective,
        "warning": if effective {
            ""
        } else {
            "配置已写入，但检测到密码登录状态没变 —— 可能被 sshd_config.d 里的 drop-in 覆盖，请检查该目录"
        },
    }))
}
