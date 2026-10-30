use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kw_server::{serve, ServeOptions, DEFAULT_PORT};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("启动失败：{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(std::env::args().skip(1))?;
    if args.help {
        print_help();
        return Ok(());
    }
    let port = args.port(std::env::var("PORT").ok().as_deref())?;
    let public_dir = resolve_public_dir()?;
    std::env::set_var("KW_NO_AUTO_COOKIE", "1");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "无法启动异步运行时")?;
    runtime.block_on(async move {
        let server = serve(ServeOptions {
            port,
            public_dir,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .await
        .map_err(|_| "无法启动本地服务，请检查监听端口与数据目录")?;

        let result = async {
            let login = login_file(&server)?;
            println!("SSH Keywarden 浏览器模式已启动，仅监听本机。");
            println!("登录文件：{}", login.path().display());
            println!("该文件含本次登录凭据，请勿共享；正常退出时自动删除。");
            if !args.no_open && open::that(login.path()).is_err() {
                eprintln!("未能打开浏览器，请手动打开登录文件。");
            }
            println!("按 Ctrl+C 退出。");
            shutdown_signal()
                .await
                .map_err(|_| "无法监听退出信号".to_string())
        }
        .await;
        server.shutdown().await;
        result
    })
}

fn login_file(server: &kw_server::RunningServer) -> Result<tempfile::NamedTempFile, String> {
    // tempfile 默认以 0600 创建；不把令牌写入终端输出或持久化应用数据。
    let mut file = tempfile::Builder::new()
        .prefix("kw-login-")
        .suffix(".html")
        .tempfile()
        .map_err(|_| "无法创建临时登录文件")?;
    let html = zeroize::Zeroizing::new(format!(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><title>SSH Keywarden 登录</title><p>此文件仅对本次运行有效，请勿共享。</p><a href=\"{}/login?token={}\" rel=\"noreferrer\">打开 SSH Keywarden</a></html>",
        server.url(), server.token
    ));
    file.write_all(html.as_bytes())
        .map_err(|_| "无法写入临时登录文件")?;
    file.flush().map_err(|_| "无法保存临时登录文件")?;
    Ok(file)
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = term.recv() => Ok(()),
        }
    }
    #[cfg(windows)]
    {
        let mut interrupt = tokio::signal::windows::ctrl_c()?;
        let mut terminate = tokio::signal::windows::ctrl_break()?;
        tokio::select! {
            _ = interrupt.recv() => Ok(()),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(any(unix, windows)))]
    tokio::signal::ctrl_c().await
}

#[derive(Default)]
struct Args {
    port: Option<u16>,
    help: bool,
    no_open: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut out = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => out.help = true,
                "--no-open" => out.no_open = true,
                "-p" | "--port" => {
                    let value = args.next().ok_or("--port 后面缺少端口号")?;
                    out.port = Some(parse_port(&value)?);
                }
                other => match other.strip_prefix("--port=") {
                    Some(value) => out.port = Some(parse_port(value)?),
                    None => return Err("未知参数（用 --help 查看用法）".into()),
                },
            }
        }
        Ok(out)
    }

    fn port(&self, env: Option<&str>) -> Result<u16, String> {
        match self.port {
            Some(port) => Ok(port),
            None => match env.filter(|s| !s.trim().is_empty()) {
                Some(value) => parse_port(value),
                None => Ok(DEFAULT_PORT),
            },
        }
    }
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .trim()
        .parse()
        .map_err(|_| "端口必须是合法的无符号 16 位整数".into())
}

fn print_help() {
    println!("SSH Keywarden 浏览器模式\n\n用法：kw-server [选项]\n\n  -p, --port <端口>   优先于 PORT 环境变量，0 表示自动分配\n  --no-open          不自动打开浏览器（仍生成临时登录文件）\n  -h, --help         显示帮助\n\n环境变量：\n  KW_DATA_DIR        数据目录（默认 ./data）\n  KW_PUBLIC_DIR      静态资源目录（必须包含 index.html）\n\n仅监听环回地址；远程使用需自行建立 SSH 本地转发，并安全取用登录文件。\n令牌不会写入终端输出。强制终止后请自行删除遗留的登录文件。");
}

fn resolve_public_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("KW_PUBLIC_DIR") {
        return checked_public_dir(Path::new(&dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("public");
            if candidate.join("index.html").is_file() {
                return checked_public_dir(&candidate);
            }
        }
    }
    checked_public_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/public"))
}

fn checked_public_dir(path: &Path) -> Result<PathBuf, String> {
    if !path.join("index.html").is_file() {
        return Err("找不到前端资源，请将 KW_PUBLIC_DIR 指向包含 index.html 的目录".into());
    }
    std::path::absolute(path).map_err(|_| "无法解析静态资源目录".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Result<Args, String> {
        Args::parse(values.iter().map(|s| s.to_string()))
    }

    #[test]
    fn cli_port_overrides_environment() {
        for input in [vec!["-p", "0"], vec!["--port", "0"], vec!["--port=0"]] {
            assert_eq!(args(&input).unwrap().port(Some("invalid")).unwrap(), 0);
        }
        assert_eq!(args(&[]).unwrap().port(Some("0")).unwrap(), 0);
        assert_eq!(args(&[]).unwrap().port(None).unwrap(), DEFAULT_PORT);
        assert_eq!(args(&[]).unwrap().port(Some(" ")).unwrap(), DEFAULT_PORT);
    }

    #[test]
    fn cli_rejects_invalid_values_without_echoing_them() {
        for value in ["-1", "65536", "private-value", ""] {
            assert!(parse_port(value).is_err());
        }
        assert!(args(&["--port"]).is_err());
        assert!(args(&["--unexpected"]).is_err());
        assert!(args(&[]).unwrap().port(Some("invalid")).is_err());
        assert!(args(&["--help"]).unwrap().help);
        assert!(args(&["--no-open"]).unwrap().no_open);
    }

    #[test]
    fn assets_require_an_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(checked_public_dir(dir.path()).is_err());
        std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
        assert!(checked_public_dir(dir.path()).is_ok());
    }
}
