//! 桌面端微软账号（OneDrive 应用专属文件夹）同步工具。
//!
//! 只同步主机配置、钥匙备注、已知主机指纹，**不碰私钥**；同步包在上传前
//! 用同步口令加密（Argon2id + AES-256-GCM），云端只有密文，文件放在
//! OneDrive 的「应用」专属文件夹里（Graph `special/approot`）。
//!
//! 登录走设备代码流：屏幕上给 8 位验证码，浏览器里输一次即可，桌面上不用开端口。
//! Azure 客户端 ID 用你自己注册的应用，`--client-id` 或环境变量 `KW_MS_CLIENT_ID`。

use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use kw_server::ms_sync::{self, MsSyncStatus};
use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::session::known_hosts_file;
use kw_store::Store;

const PASSPHRASE_ENV: &str = "KW_SYNC_PASSPHRASE";
const EXIT_CONFLICTS: u8 = 2;

struct Args {
    command: String,
    data_dir: Option<PathBuf>,
    client_id: Option<String>,
    trust_changed: Vec<String>,
    json: bool,
    passphrase_env: String,
    remember_key: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse(env::args().skip(1).collect()) {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("参数错误：{e}\n");
            print_help();
            return ExitCode::FAILURE;
        }
    };

    match run(&args).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("失败：{e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &Args) -> Result<u8, String> {
    let data_dir = resolve_data_dir(args.data_dir.as_deref());

    match args.command.as_str() {
        "login" => {
            let prompt = ms_sync::start_device_login(&data_dir, args.client_id.as_deref()).await?;
            say(
                args.json,
                &format!("浏览器打开：{}", prompt.verification_uri),
            );
            say(args.json, &format!("输入验证码：{}", prompt.user_code));
            say(
                args.json,
                &format!(
                    "验证码 {} 秒内有效，等你在浏览器里确认…",
                    prompt.expires_in
                ),
            );
            if !args.json {
                open_browser(&prompt.verification_uri);
            }

            let timeout = Duration::from_secs(prompt.expires_in.max(60));
            let status = ms_sync::complete_device_login(
                &data_dir,
                &prompt.device_code,
                args.client_id.as_deref(),
                timeout,
            )
            .await?;

            if args.json {
                println!("{}", serde_json::json!({ "ok": true, "status": status }));
            } else {
                println!(
                    "已登录：{}（{}）",
                    status.account_name,
                    if status.account_email.is_empty() {
                        "未取到邮箱"
                    } else {
                        &status.account_email
                    }
                );
            }
            Ok(0)
        }

        "status" => {
            let status = ms_sync::status(&data_dir);
            if args.json {
                println!("{}", serde_json::json!({ "ok": true, "status": status }));
                return Ok(0);
            }
            print_status(&status);
            Ok(0)
        }

        "logout" => {
            ms_sync::logout(&data_dir)?;
            if args.json {
                println!("{}", serde_json::json!({ "ok": true }));
            } else {
                println!("已退出微软账号（本机的客户端 ID 和同步口令留着没动）");
            }
            Ok(0)
        }

        "sync" => {
            if !data_dir.join("config.json").exists() {
                return Err(format!(
                    "{} 下没有 config.json；先运行一次桌面端，或用 --data-dir 指定数据目录",
                    data_dir.display()
                ));
            }

            let passphrase = resolve_passphrase(args, &data_dir)?;
            if args.remember_key {
                ms_sync::save_config(&data_dir, None, Some(&passphrase))?;
            }

            let store = Store::new(&data_dir);
            let known_hosts = KnownHosts::new(known_hosts_file(&data_dir));
            let out =
                ms_sync::sync_now(&store, &known_hosts, &passphrase, &args.trust_changed).await?;
            let conflicts = out.report.known_host_conflicts.len();

            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": conflicts == 0,
                        "pulled": out.pulled,
                        "pushed": out.pushed,
                        "lastSyncAt": out.last_sync_at,
                        "report": out.report,
                    })
                );
                return Ok(if conflicts == 0 { 0 } else { EXIT_CONFLICTS });
            }

            if !out.pulled {
                println!("云端还没有同步包，这次是首次上传。");
            }
            print_report(&out.report);
            if out.pushed {
                println!(
                    "已把合并后的结果加密上传到 OneDrive（上次同步：{}）。",
                    fmt_time(out.last_sync_at)
                );
            } else {
                println!("有主机指纹冲突，这次没有上传。");
                print_conflicts(&out.report.known_host_conflicts);
                return Ok(EXIT_CONFLICTS);
            }
            Ok(0)
        }

        "help" | "--help" | "-h" => {
            print_help();
            Ok(0)
        }

        other => Err(format!("未知命令：{other}")),
    }
}

impl Args {
    fn parse(argv: Vec<String>) -> Result<Option<Self>, String> {
        if argv.is_empty() {
            print_help();
            return Ok(None);
        }
        let mut args = Args {
            command: argv[0].clone(),
            data_dir: None,
            client_id: None,
            trust_changed: Vec::new(),
            json: false,
            passphrase_env: PASSPHRASE_ENV.to_string(),
            remember_key: false,
        };
        let mut i = 1;
        while i < argv.len() {
            let item = &argv[i];
            match item.as_str() {
                "--data-dir" => {
                    i += 1;
                    args.data_dir = Some(PathBuf::from(
                        argv.get(i).ok_or("--data-dir 缺少路径")?.to_string(),
                    ));
                }
                "--client-id" => {
                    i += 1;
                    args.client_id = Some(
                        argv
                            .get(i)
                            .ok_or("--client-id 缺少 Azure 应用 ID")?
                            .to_string(),
                    );
                }
                "--trust-changed" => {
                    i += 1;
                    let raw = argv.get(i).ok_or("--trust-changed 缺少 host:port")?;
                    args.trust_changed
                        .extend(raw.split(',').filter(|s| !s.is_empty()).map(str::to_string));
                }
                "--passphrase-env" => {
                    i += 1;
                    args.passphrase_env = argv
                        .get(i)
                        .ok_or("--passphrase-env 缺少变量名")?
                        .to_string();
                }
                "--remember-key" => args.remember_key = true,
                "--json" => args.json = true,
                "-h" | "--help" => args.command = "help".to_string(),
                other if other.starts_with('-') => {
                    return Err(format!("未知参数：{other}"));
                }
                other => return Err(format!("多余的参数：{other}")),
            }
            i += 1;
        }
        Ok(Some(args))
    }
}

fn print_status(status: &MsSyncStatus) {
    if status.configured {
        println!("Azure 客户端 ID：{}", status.client_id);
    } else {
        println!("Azure 客户端 ID：还没配置");
        println!("  {}", ms_sync::CLIENT_ID_HELP);
    }
    if status.logged_in {
        println!(
            "微软账号：{}（{}）",
            status.account_name,
            if status.account_email.is_empty() {
                "未取到邮箱"
            } else {
                &status.account_email
            }
        );
        println!("上次同步：{}", fmt_time(status.last_sync_at));
    } else {
        println!("微软账号：未登录（运行 kw-sync login）");
    }
    println!(
        "同步口令：{}",
        if status.has_sync_key {
            "本机已记住"
        } else {
            "未记住（用环境变量或交互输入）"
        }
    );
}

fn print_report(report: &kw_server::sync::ImportReport) {
    println!(
        "主机：新增 {}，更新 {}，删除 {}，跳过 {}",
        report.added_hosts, report.updated_hosts, report.deleted_hosts, report.skipped_hosts
    );
    println!(
        "备注：应用 {} 条，跳过 {} 条；主机指纹：新增 {}，覆盖 {}",
        report.synced_notes,
        report.skipped_notes,
        report.added_known_hosts,
        report.updated_known_hosts
    );
}

fn print_conflicts(conflicts: &[kw_server::sync::KnownHostConflict]) {
    eprintln!("\n以下主机的指纹与本机记录不一致，已跳过（不会自动覆盖）：");
    for c in conflicts {
        eprintln!(
            "  {}  本机 {}  云端 {}",
            c.key, c.local_fingerprint, c.remote_fingerprint
        );
    }
    let keys = conflicts
        .iter()
        .map(|c| c.key.clone())
        .collect::<Vec<_>>()
        .join(",");
    eprintln!("\n确认对方可信后重跑：\n  kw-sync sync --trust-changed {keys}");
}

fn resolve_passphrase(args: &Args, data_dir: &Path) -> Result<String, String> {
    if let Ok(v) = env::var(&args.passphrase_env) {
        if !v.trim().is_empty() {
            return Ok(v);
        }
    }
    if let Some(key) = ms_sync::stored_sync_key(data_dir) {
        return Ok(key);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "没有同步口令：设置环境变量 {}，或在交互终端里运行一次",
            args.passphrase_env
        ));
    }
    let pass = rpassword::prompt_password("同步口令（不会回显）：")
        .map_err(|e| format!("读取口令失败：{e}"))?;
    if pass.is_empty() {
        return Err("口令不能为空".to_string());
    }
    Ok(pass)
}

/// `--json` 时把提示文字挪到 stderr，stdout 只留最后那份 JSON。
fn say(json: bool, text: &str) {
    if json {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
}

/// 只放行 https 链接，且不含 shell 元字符：Windows 上这里要经过 `cmd /C start`。
fn open_browser(url: &str) {
    if !url.starts_with("https://")
        || url
            .chars()
            .any(|c| matches!(c, '&' | '|' | ';' | '<' | '>' | '"' | '\'' | '%' | '^'))
    {
        return;
    }
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn fmt_time(ms: u64) -> String {
    if ms == 0 {
        return "从来没有".to_string();
    }
    let stamp = kw_ssh::util::shanghai_stamp(SystemTime::UNIX_EPOCH + Duration::from_millis(ms));
    format!(
        "{}-{}-{} {}:{}:{}",
        &stamp[0..4],
        &stamp[4..6],
        &stamp[6..8],
        &stamp[8..10],
        &stamp[10..12],
        &stamp[12..14]
    )
}

fn resolve_data_dir(arg: Option<&Path>) -> PathBuf {
    if let Some(dir) = arg {
        return dir.to_path_buf();
    }
    if let Some(dir) = env::var_os("KW_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(base) = env::var_os("APPDATA") {
        return PathBuf::from(base).join("ssh-keywarden").join("data");
    }
    PathBuf::from("data")
}

fn print_help() {
    println!(
        "kw-sync — SSH Keywarden 微软账号同步（只含主机/备注/主机指纹，不含私钥）

用法：
  kw-sync login  [--client-id <GUID>] [--data-dir <目录>] [--json]
  kw-sync sync   [--trust-changed host:port,...] [--remember-key] [--data-dir <目录>] [--json]
  kw-sync status [--data-dir <目录>] [--json]
  kw-sync logout [--data-dir <目录>] [--json]
  kw-sync help

登录：
  第一次要先在 https://portal.azure.com 的「Microsoft Entra ID → 应用注册」里注册一个
  个人账户应用（公共客户端流 + Files.ReadWrite.AppFolder / User.Read / offline_access），
  然后把应用 ID 填给 --client-id，或设环境变量 KW_MS_CLIENT_ID。
  之后 `kw-sync login` 会给出 8 位验证码，输进浏览器即可，桌面上不用开端口。

同步：
  云端文件是 OneDrive「应用」专属文件夹里的 ssh-keywarden.kwsync，只有密文。
  同步口令默认读环境变量 KW_SYNC_PASSPHRASE，其次读本机记住的那份，最后才交互询问。
  --remember-key 会把这次用的口令记在本机（data\\ms-config.json）。
  忘了口令就只能删掉云端的 ssh-keywarden.kwsync 重新开始（旧的那个谁也解不开）。

数据目录：
  默认取 KW_DATA_DIR，否则 %APPDATA%\\ssh-keywarden\\data。
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_login_with_client_id() {
        let args = Args::parse(vec![
            "login".into(),
            "--client-id".into(),
            "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9".into(),
            "--json".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(args.command, "login");
        assert_eq!(
            args.client_id.unwrap(),
            "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"
        );
        assert!(args.json);
    }

    #[test]
    fn parse_sync_trust_list_and_flags() {
        let args = Args::parse(vec![
            "sync".into(),
            "--trust-changed".into(),
            "a:22,b:2222".into(),
            "--remember-key".into(),
            "--data-dir".into(),
            r"C:/data".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(args.trust_changed, vec!["a:22", "b:2222"]);
        assert!(args.remember_key);
        assert_eq!(args.data_dir.unwrap().to_string_lossy(), r"C:/data");
    }

    #[test]
    fn fmt_time_handles_zero_and_formats_stamp() {
        assert_eq!(fmt_time(0), "从来没有");
        // 2026-10-30T02:22:48Z → 上海时间 10:22:48
        assert_eq!(fmt_time(1_793_326_968_000), "2026-10-30 10:22:48");
    }
}
