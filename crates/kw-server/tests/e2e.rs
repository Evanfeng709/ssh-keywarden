//! 真实 HTTP 路由 / 环回 SSH 传输；远端文件与命令由内存模型模拟。
//! 不执行系统 shell、sshd -t 或 reload，不替代实机 sshd 脚本验收。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use http_body_util::BodyExt;
use russh::server::{self, Msg, Server as _, Session as SshSession};
use russh::{ChannelId, Pty};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex, Notify};
use tokio::task::{JoinHandle, JoinSet};
use tower::ServiceExt;

use kw_keys::generate_ed25519;
use kw_server::routes;
use kw_server::state::AppState;
use kw_ssh::known_hosts::KnownHosts;
use kw_ssh::terminal::Terminal;
use kw_ssh::util::{quote, read_remote_text_command};
use kw_store::{SettingsPatch, Store};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123";
const USER: &str = "kwtest";
const PASSWORD: &str = " test-password ";
const HOME: &str = "/home/kwtest";
const AUTHORIZED: &str = "/home/kwtest/.ssh/authorized_keys";
const SSHD_CONFIG: &str = "/etc/ssh/sshd_config";
const DEADLINE: Duration = Duration::from_secs(15);

// 不把 Result 的错误、响应体或凭据带进测试失败日志。
fn must<T, E>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|_| panic!("test operation failed"))
}

struct Exec {
    code: u32,
    stdout: String,
    stderr: String,
}

impl Exec {
    fn ok(stdout: impl Into<String>) -> Self {
        Self {
            code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn fail() -> Self {
        Self {
            code: 1,
            stdout: String::new(),
            stderr: "command rejected".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Event {
    PasswordAuth(bool),
    KeyAuth {
        fingerprint: String,
        authorized: bool,
        accepted: bool,
    },
    Read,
    Backup,
    WriteStart,
    Staged,
    Commit(BTreeSet<String>),
    LoginProbe,
    SshdInspect,
    SshdWrite(bool),
    UnknownCommand,
}

#[derive(Default)]
struct WriteGate {
    entered: Notify,
    release: Notify,
}

#[derive(Default)]
struct FakeFs {
    files: BTreeMap<String, String>,
    events: Vec<Event>,
    unknown_commands: usize,
    reject_keys: BTreeSet<String>,
    write_delay: Duration,
    next_write_gate: Option<Arc<WriteGate>>,
    /// 客户端 pty-req 的原始参数，用来断言终端真的按请求的行列开起来了。
    pty_requests: Vec<(String, u32, u32)>,
    resizes: Vec<(u32, u32)>,
}

fn fingerprints(text: &str) -> BTreeSet<String> {
    kw_keys::parse_authorized_keys(text)
        .key_items()
        .filter_map(|item| item.key().map(|key| key.fingerprint.clone()))
        .collect()
}

// 跳过完整的引号对，并支持 quote() 产生的嵌入单引号。
fn quoted(cmd: &str, n: usize) -> String {
    let mut rest = cmd;
    for index in 0..=n {
        let Some(start) = rest.find('\'') else {
            return String::new();
        };
        rest = &rest[start + 1..];
        let mut value = String::new();
        loop {
            let Some(end) = rest.find('\'') else {
                return String::new();
            };
            value.push_str(&rest[..end]);
            rest = &rest[end + 1..];
            if let Some(next) = rest.strip_prefix("\\''") {
                value.push('\'');
                rest = next;
            } else {
                break;
            }
        }
        if index == n {
            return value;
        }
    }
    String::new()
}

impl FakeFs {
    fn exec(&mut self, cmd: &str, stdin: &[u8]) -> Exec {
        let t = cmd.trim();
        match t {
            "printf \"%s\" \"$HOME\"" | "getent passwd \"$(id -u)\" | cut -d: -f6" => {
                return Exec::ok(HOME)
            }
            "uname -srm 2>/dev/null" => return Exec::ok("Linux 6.8.0 x86_64\n"),
            "id -un 2>/dev/null" => return Exec::ok(format!("{USER}\n")),
            "hostname 2>/dev/null" => return Exec::ok("fakebox\n"),
            "printf __LOGIN_OK__" => {
                self.events.push(Event::LoginProbe);
                return Exec::ok("__LOGIN_OK__");
            }
            _ => {}
        }
        let p = quoted(t, 0);
        let q = quoted(t, 1);
        let r = quoted(t, 2);
        if t == format!("echo {} | base64 -d | sh -s", quote(&p)) {
            return self.sshd_script(&p);
        }
        if t == read_remote_text_command(&p) {
            if p == AUTHORIZED {
                self.events.push(Event::Read);
            }
            return match self.files.get(&p) {
                Some(text) => Exec::ok(base64::engine::general_purpose::STANDARD.encode(text)),
                None => Exec::ok("__KW_MISSING__\n"),
            };
        }
        if t == format!(
            "if [ -f {p} ]; then cp {p} {r} && chmod 600 {r} && echo \"__OK__\"; else echo \"__SKIP__\"; fi",
            p = quote(&p), r = quote(&r)
        ) {
            return match self.files.get(&p).cloned() {
                Some(text) => {
                    self.events.push(Event::Backup);
                    self.files.insert(r, text);
                    Exec::ok("__OK__\n")
                }
                None => Exec::ok("__SKIP__\n"),
            };
        }
        if t == format!("base64 -d > {}", quote(&p)) {
            let payload: Vec<u8> = stdin
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            let text = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            return match text {
                Some(text) => {
                    self.files.insert(p, text);
                    self.events.push(Event::Staged);
                    Exec::ok("")
                }
                None => Exec::fail(),
            };
        }
        if t == format!(
            "mv -f {p} {q} && chmod 600 {q}",
            p = quote(&p),
            q = quote(&q)
        ) {
            let Some(text) = self.files.remove(&p) else {
                return Exec::fail();
            };
            if q == AUTHORIZED {
                self.events.push(Event::Commit(fingerprints(&text)));
            }
            self.files.insert(q, text);
            return Exec::ok("");
        }
        if t == format!("rm -f {}", quote(&p)) {
            self.files.remove(&p);
            return Exec::ok("");
        }
        let mkdir = format!("mkdir -p {p} && chmod 700 {p}", p = quote(&p));
        if t == mkdir {
            return Exec::ok("");
        }
        if t == format!("{mkdir} && echo __OK__") {
            return Exec::ok("__OK__\n");
        }
        if t == format!(
            "for f in {}.bak.*; do printf '%s\\n' \"$f\"; done",
            quote(&p)
        ) {
            let prefix = format!("{p}.bak.");
            let names: Vec<_> = self
                .files
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            return Exec::ok(names.join("\n"));
        }
        self.unknown_commands += 1;
        self.events.push(Event::UnknownCommand);
        Exec::fail()
    }

    fn sshd_script(&mut self, b64: &str) -> Exec {
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            return Exec::fail();
        };
        let script = String::from_utf8_lossy(&bytes);
        if script.starts_with("echo \"###UID ") && script.ends_with("echo \"###END\"") {
            self.events.push(Event::SshdInspect);
            let Some(text) = self.files.get(SSHD_CONFIG) else {
                return Exec::fail();
            };
            return Exec::ok(format!(
                "###UID 0\n###SUDO no\n###EXISTS yes\n###READ yes\n###BEGIN main\n{text}\n###END\n"
            ));
        }
        if script.starts_with("#!/bin/sh\nset -e\nCFG=/etc/ssh/sshd_config\n")
            && script.contains("modify_file \"$CFG\" yes")
            && script.contains("sshd -t")
        {
            let pw = script_value(&script, "PW");
            let kbd = script_value(&script, "KBD");
            let public = script_value(&script, "PUB");
            if !matches!(
                (pw.as_str(), kbd.as_str(), public.as_str()),
                ("no", "no", "yes") | ("yes", "yes", "")
            ) {
                self.unknown_commands += 1;
                self.events.push(Event::UnknownCommand);
                return Exec::fail();
            }
            let Some(old) = self.files.get(SSHD_CONFIG).cloned() else {
                return Exec::fail();
            };
            let backup = format!(
                "{SSHD_CONFIG}.bak.{}",
                kw_ssh::util::shanghai_stamp(SystemTime::now())
            );
            self.files.insert(backup.clone(), old);
            self.files.insert(SSHD_CONFIG.into(), format!(
                "PasswordAuthentication {pw}\nKbdInteractiveAuthentication {kbd}\nPubkeyAuthentication yes\n"
            ));
            self.events.push(Event::SshdWrite(pw == "no"));
            return Exec::ok(format!("__OK__ {backup} yes\n"));
        }
        self.unknown_commands += 1;
        self.events.push(Event::UnknownCommand);
        Exec::fail()
    }
}

fn script_value(script: &str, name: &str) -> String {
    script
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("{name}=\""))
                .and_then(|s| s.strip_suffix('"'))
        })
        .unwrap_or_default()
        .to_string()
}

#[derive(Default)]
struct Pending {
    cmd: String,
    stdin: Vec<u8>,
}

struct FakeHandler {
    fs: Arc<Mutex<FakeFs>>,
    chans: HashMap<ChannelId, Pending>,
    /// 开了 PTY 并请求 shell 的通道：输入按回显处理，而不是攒着等 exec。
    shells: HashSet<ChannelId>,
    jobs: JoinSet<()>,
}

struct FakeServer {
    fs: Arc<Mutex<FakeFs>>,
}

impl server::Server for FakeServer {
    type Handler = FakeHandler;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> FakeHandler {
        FakeHandler {
            fs: Arc::clone(&self.fs),
            chans: HashMap::new(),
            shells: HashSet::new(),
            jobs: JoinSet::new(),
        }
    }
}

impl server::Handler for FakeHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::reject())
    }

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        let mut fs = self.fs.lock().await;
        let allowed = !fs
            .files
            .get(SSHD_CONFIG)
            .is_some_and(|text| text.contains("PasswordAuthentication no"));
        let accepted = user == USER && password == PASSWORD && allowed;
        fs.events.push(Event::PasswordAuth(accepted));
        Ok(if accepted {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: server::VerifiedPublicKey<'_>,
    ) -> Result<server::Auth, Self::Error> {
        let fingerprint = key
            .public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        let mut fs = self.fs.lock().await;
        let authorized = fingerprints(
            fs.files
                .get(AUTHORIZED)
                .map(String::as_str)
                .unwrap_or_default(),
        )
        .contains(&fingerprint);
        let accepted = user == USER && authorized && !fs.reject_keys.contains(&fingerprint);
        fs.events.push(Event::KeyAuth {
            fingerprint,
            authorized,
            accepted,
        });
        Ok(if accepted {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _: russh::Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut SshSession,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.fs
            .lock()
            .await
            .pty_requests
            .push((term.to_string(), col_width, row_height));
        self.shells.insert(channel);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _: u32,
        _: u32,
        _: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if self.shells.contains(&channel) {
            self.fs.lock().await.resizes.push((col_width, row_height));
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.chans.insert(
            channel,
            Pending {
                cmd: String::from_utf8_lossy(data).into_owned(),
                stdin: Vec::new(),
            },
        );
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if self.shells.contains(&channel) {
            // 模拟 PTY：输入回显成输出；带 STDERR 的走 stderr 通道，带 EXIT 的发退出状态。
            let handle = session.handle();
            let text = String::from_utf8_lossy(data);
            if text.contains("STDERR") {
                let _ = handle
                    .extended_data(channel, 1, b"stderr-stream\n".to_vec())
                    .await;
            } else {
                let echo = data.to_vec();
                let _ = handle.data(channel, echo).await;
                if text.contains("EXIT") {
                    let _ = handle.exit_status_request(channel, 0).await;
                    let _ = handle.eof(channel).await;
                    self.shells.remove(&channel);
                }
            }
            return Ok(());
        }
        if let Some(p) = self.chans.get_mut(&channel) {
            p.stdin.extend_from_slice(data);
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        let Some(pending) = self.chans.remove(&channel) else {
            return Ok(());
        };
        while let Some(result) = self.jobs.try_join_next() {
            must(result);
        }
        let fs = self.fs.clone();
        let handle = session.handle();
        // handler 立即返回；一个通道暂停写入时，其他通道仍能读/执行。
        self.jobs.spawn(async move {
            if pending.cmd.starts_with("base64 -d >") {
                let (delay, gate) = {
                    let mut fs = fs.lock().await;
                    fs.events.push(Event::WriteStart);
                    (fs.write_delay, fs.next_write_gate.take())
                };
                if let Some(gate) = gate {
                    gate.entered.notify_one();
                    gate.release.notified().await;
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            let out = fs.lock().await.exec(&pending.cmd, &pending.stdin);
            if !out.stdout.is_empty() {
                let _ = handle.data(channel, out.stdout.into_bytes()).await;
            }
            if !out.stderr.is_empty() {
                let _ = handle
                    .extended_data(channel, 1, out.stderr.into_bytes())
                    .await;
            }
            let _ = handle.exit_status_request(channel, out.code).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}

struct Env {
    _dir: tempfile::TempDir,
    app: Router,
    state: Arc<AppState>,
    fs: Arc<Mutex<FakeFs>>,
    port: u16,
    keys: PathBuf,
    shutdown: server::RunningServerHandle,
    task: JoinHandle<std::io::Result<()>>,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.shutdown.shutdown(String::new());
        self.task.abort();
    }
}

struct Key {
    path: String,
    fingerprint: String,
    public: String,
}

impl Env {
    async fn start(authorized: &str) -> Self {
        Self::start_mode(authorized, true).await
    }

    async fn start_mode(authorized: &str, auto_cookie: bool) -> Self {
        let dir = must(tempfile::tempdir());
        let public = dir.path().join("public");
        must(std::fs::create_dir_all(&public));
        must(std::fs::write(
            public.join("index.html"),
            "<!doctype html><title>kw</title>",
        ));
        let keys = dir.path().join("keys");
        must(std::fs::create_dir_all(&keys));
        let data = dir.path().join("data");
        let fs = Arc::new(Mutex::new(FakeFs {
            files: BTreeMap::from([
                (AUTHORIZED.into(), authorized.into()),
                (
                    SSHD_CONFIG.into(),
                    "PasswordAuthentication yes\nPubkeyAuthentication yes\n".into(),
                ),
            ]),
            ..Default::default()
        }));
        let generated = must(generate_ed25519(Some("fake-host-key"), None));
        let host_key = must(russh::keys::PrivateKey::from_openssh(
            &generated.private_key_pem,
        ));
        let config = Arc::new(server::Config {
            keys: vec![host_key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = must(TcpListener::bind("127.0.0.1:0").await);
        let addr = must(listener.local_addr());
        assert!(addr.ip().is_loopback());
        let mut server = FakeServer { fs: fs.clone() };
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let running = server.run_on_socket(config, &listener);
            let _ = tx.send(running.handle());
            running.await
        });
        let shutdown = must(rx.await);
        let store = Arc::new(Store::new(data.clone()));
        must(store.update_settings(SettingsPatch {
            local_key_dir: Some(keys.display().to_string()),
            save_dir: Some(keys.display().to_string()),
            remote_path: Some(AUTHORIZED.into()),
        }));
        let known_hosts = Arc::new(KnownHosts::new(data.join("known_hosts.json")));
        let mut state = AppState::new(
            TOKEN.into(),
            addr.port(),
            store,
            known_hosts,
            public,
            "test",
        );
        state.auto_cookie = auto_cookie;
        let state = Arc::new(state);
        Self {
            _dir: dir,
            app: routes::router(state.clone()),
            state,
            fs,
            port: addr.port(),
            keys,
            shutdown,
            task,
        }
    }

    fn keypair(&self, label: &str, passphrase: Option<&str>) -> Key {
        let generated = must(generate_ed25519(Some(label), passphrase));
        let private = self.keys.join(format!("{label}.key"));
        must(std::fs::write(&private, &generated.private_key_pem));
        must(std::fs::write(
            self.keys.join(format!("{label}.key.pub")),
            format!("{}\n", generated.public_key_line),
        ));
        Key {
            path: private.display().to_string(),
            fingerprint: generated.fingerprint,
            public: generated.public_key_line,
        }
    }

    async fn authorize(&self, keys: &[&Key]) {
        let text = keys
            .iter()
            .map(|key| key.public.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        self.fs.lock().await.files.insert(AUTHORIZED.into(), text);
    }

    async fn authorized(&self) -> String {
        self.fs
            .lock()
            .await
            .files
            .get(AUTHORIZED)
            .cloned()
            .unwrap_or_default()
    }

    async fn assert_keys(&self, keys: &[&Key]) {
        let raw = self.authorized().await;
        let expected: BTreeSet<_> = keys.iter().map(|k| k.fingerprint.clone()).collect();
        assert!(
            fingerprints(&raw) == expected,
            "authorized key set mismatch"
        );
        assert!(
            kw_keys::parse_authorized_keys(&raw).key_count() == keys.len(),
            "duplicate key written"
        );
    }

    async fn events(&self) -> Vec<Event> {
        self.fs.lock().await.events.clone()
    }
    async fn clear_events(&self) {
        self.fs.lock().await.events.clear();
    }

    async fn assert_clean(&self) {
        assert!(
            self.fs.lock().await.unknown_commands == 0,
            "unrecognized SSH command"
        );
        assert!(
            !self
                .fs
                .lock()
                .await
                .files
                .keys()
                .any(|p| p.contains(".tmp-")),
            "temporary remote file leaked"
        );
        assert!(self.state.store.dir().starts_with(self._dir.path()));
        assert!(self.state.known_hosts.file().starts_with(self._dir.path()));
        let settings = self.state.current_settings();
        assert!(Path::new(&settings.local_key_dir).starts_with(self._dir.path()));
        assert!(Path::new(&settings.save_dir).starts_with(self._dir.path()));
        for session in self.state.sessions.list().await {
            session.client.disconnect().await;
        }
    }
}

async fn request(app: Router, method: &str, path: &str, body: Value) -> axum::response::Response {
    let request = must(
        Request::builder()
            .method(method)
            .uri(path)
            .header("cookie", format!("kw={TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
    );
    must(must(
        tokio::time::timeout(DEADLINE, app.oneshot(request)).await,
    ))
}

async fn api(env: &Env, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = request(env.app.clone(), method, path, body).await;
    let status = response.status();
    let bytes = must(response.into_body().collect().await).to_bytes();
    (status, must(serde_json::from_slice(&bytes)))
}

async fn post(env: &Env, path: &str, body: Value) -> (StatusCode, Value) {
    api(env, "POST", path, body).await
}
async fn get(env: &Env, path: &str) -> (StatusCode, Value) {
    api(env, "GET", path, Value::Null).await
}

fn ok(response: (StatusCode, Value)) -> Value {
    assert!(
        response.0 == StatusCode::OK,
        "expected successful HTTP status"
    );
    assert!(response.1["ok"] == true, "expected successful API status");
    response.1["data"].clone()
}

fn bad(response: (StatusCode, Value)) -> Value {
    assert!(
        response.0 == StatusCode::BAD_REQUEST,
        "expected rejected HTTP status"
    );
    assert!(response.1["ok"] == false, "expected rejected API status");
    response.1
}

fn code(response: (StatusCode, Value), expected: &str) -> Value {
    assert!(
        response.0 == StatusCode::CONFLICT,
        "expected conflict HTTP status"
    );
    assert!(
        response.1["ok"] == false && response.1["code"] == expected,
        "expected API error code"
    );
    response.1
}

fn text(value: &Value) -> String {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("expected string field"));
    assert!(!text.is_empty(), "expected nonempty string field");
    text.to_string()
}

fn connect_body(env: &Env, extra: Value) -> Value {
    let mut body = json!({ "host": "127.0.0.1", "port": env.port, "username": USER });
    for (key, value) in extra.as_object().expect("expected request object") {
        body[key] = value.clone();
    }
    body
}

async fn connect(env: &Env, extra: Value) -> Value {
    let first = post(env, "/api/connect", connect_body(env, extra.clone())).await;
    if first.0 == StatusCode::OK {
        return ok(first);
    }
    let unknown = code(first, "HOSTKEY_UNKNOWN");
    let mut retry = extra;
    retry["trustFingerprint"] = unknown["details"]["fingerprint"].clone();
    ok(post(env, "/api/connect", connect_body(env, retry)).await)
}

async fn save_host(env: &Env, key: &Key) -> String {
    text(
        &ok(post(
            env,
            "/api/hosts",
            json!({
                "name": "fixture", "host": "127.0.0.1", "port": env.port,
                "username": USER, "identityFile": key.path, "remotePath": AUTHORIZED,
            }),
        )
        .await)["id"],
    )
}

fn stored_identity(env: &Env, id: &str) -> String {
    env.state
        .store
        .list_hosts()
        .into_iter()
        .find(|host| host.id == id)
        .expect("saved host missing")
        .identity_file
}

fn auth(key: &Key, authorized: bool, accepted: bool) -> Event {
    Event::KeyAuth {
        fingerprint: key.fingerprint.clone(),
        authorized,
        accepted,
    }
}

fn commit(keys: &[&Key]) -> Event {
    Event::Commit(keys.iter().map(|k| k.fingerprint.clone()).collect())
}

fn transaction(keys: &[&Key]) -> Vec<Event> {
    vec![
        Event::Read,
        Event::Backup,
        Event::WriteStart,
        Event::Staged,
        commit(keys),
        Event::Read,
    ]
}

#[test]
fn command_model_rejects_unknown_and_parses_complete_quoted_arguments() {
    let command = format!(
        "mv -f {} {} && chmod 600 {}",
        quote("/tmp/a'b"),
        quote(AUTHORIZED),
        quote(AUTHORIZED)
    );
    assert!(quoted(&command, 0) == "/tmp/a'b");
    assert!(quoted(&command, 1) == AUTHORIZED);
    assert!(quoted(&command, 2) == AUTHORIZED);
    assert!(quoted(&command, 3).is_empty());
    let mut fs = FakeFs::default();
    assert!(fs.exec("unexpected command", b"").code != 0);
    assert!(fs.exec("uname unsupported", b"").code != 0);
    assert!(fs.exec("base64 -d > '/tmp/key'", b" YQ==\n").code == 0);
    assert!(fs.files.get("/tmp/key").is_some_and(|s| s == "a"));
}

#[tokio::test]
async fn tofu_blocks_before_auth_requires_exact_confirmation_and_remembers_trust() {
    let env = Env::start("").await;
    let credentials = json!({ "password": PASSWORD });
    let unknown = code(
        post(
            &env,
            "/api/connect",
            connect_body(&env, credentials.clone()),
        )
        .await,
        "HOSTKEY_UNKNOWN",
    );
    assert!(
        env.events().await.is_empty(),
        "TOFU must precede authentication"
    );
    assert!(env.state.sessions.list().await.is_empty());
    assert!(env.state.known_hosts.list().is_empty());
    code(
        post(
            &env,
            "/api/connect",
            connect_body(
                &env,
                json!({
                    "password": PASSWORD, "trustFingerprint": "SHA256:not-the-server-key"
                }),
            ),
        )
        .await,
        "HOSTKEY_UNKNOWN",
    );
    assert!(
        env.events().await.is_empty(),
        "wrong confirmation must not authenticate"
    );
    assert!(env.state.known_hosts.list().is_empty());
    let first = ok(post(
        &env,
        "/api/connect",
        connect_body(
            &env,
            json!({
                "password": PASSWORD, "trustFingerprint": unknown["details"]["fingerprint"]
            }),
        ),
    )
    .await);
    assert!(first["authMethod"] == "password");
    let second = ok(post(&env, "/api/connect", connect_body(&env, credentials)).await);
    assert!(text(&first["sid"]) != text(&second["sid"]));
    assert!(
        env.events()
            .await
            .iter()
            .filter(|e| **e == Event::PasswordAuth(true))
            .count()
            == 2
    );
    let persisted = KnownHosts::new(env.state.known_hosts.file());
    assert!(persisted.list().len() == 1);
    assert!(persisted.list()[0].fingerprint == text(&unknown["details"]["fingerprint"]));
    env.assert_clean().await;
}

#[tokio::test]
async fn password_private_key_and_wrong_credentials_use_real_authentication() {
    let env = Env::start("").await;
    let key = env.keypair("accepted", None);
    let other = env.keypair("unlisted", None);
    env.authorize(&[&key]).await;
    let password = connect(&env, json!({ "password": PASSWORD })).await;
    assert!(password["authMethod"] == "password" && password["usedKeyFp"] == "");
    env.clear_events().await;
    bad(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "password": "wrong" })),
    )
    .await);
    assert!(env.events().await == vec![Event::PasswordAuth(false)]);
    env.clear_events().await;
    bad(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "identityFile": other.path })),
    )
    .await);
    assert!(env.events().await == vec![auth(&other, false, false)]);
    env.clear_events().await;
    let login = ok(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "identityFile": key.path })),
    )
    .await);
    assert!(login["authMethod"] == "key" && login["usedKeyFp"] == key.fingerprint);
    assert!(env.events().await == vec![auth(&key, true, true), Event::Read]);
    assert!(env.state.sessions.list().await.len() == 2);
    let config = must(std::fs::read_to_string(env.state.store.config_path()));
    assert!(
        !config.contains(PASSWORD) && !config.contains("PRIVATE KEY"),
        "credentials persisted"
    );
    env.assert_clean().await;
}

#[tokio::test]
async fn encrypted_private_key_checks_passphrase_and_secret_debug_is_redacted() {
    let env = Env::start("").await;
    let key = env.keypair("encrypted", Some("key-passphrase"));
    env.authorize(&[&key]).await;
    let body = json!({ "identityFile": key.path, "passphrase": "key-passphrase" });
    let decoded: kw_server::connect::ConnectBody = must(serde_json::from_value(connect_body(
        &env,
        json!({
            "identityFile": key.path, "password": PASSWORD, "passphrase": "key-passphrase"
        }),
    )));
    let debug = format!("{decoded:?}");
    assert!(
        !debug.contains("key-passphrase") && !debug.contains(PASSWORD),
        "credential Debug leaked plaintext"
    );
    let login = connect(&env, body).await;
    assert!(login["authMethod"] == "key" && login["usedKeyFp"] == key.fingerprint);
    env.clear_events().await;
    for passphrase in ["wrong", ""] {
        bad(post(
            &env,
            "/api/connect",
            connect_body(
                &env,
                json!({
                    "identityFile": key.path, "passphrase": passphrase
                }),
            ),
        )
        .await);
        assert!(
            env.events().await.is_empty(),
            "locked key must not reach authentication"
        );
    }
    let fallback = ok(post(
        &env,
        "/api/connect",
        connect_body(
            &env,
            json!({
                "identityFile": key.path, "passphrase": "wrong", "password": PASSWORD
            }),
        ),
    )
    .await);
    assert!(fallback["authMethod"] == "password" && fallback["usedKeyFp"] == "");
    assert!(env.events().await == vec![Event::PasswordAuth(true), Event::Read]);
    env.assert_clean().await;
}

#[tokio::test]
async fn encrypted_private_key_preserves_spaced_and_all_whitespace_passphrases() {
    let env = Env::start("").await;
    for (index, passphrase) in [" spaced passphrase ", "   "].iter().enumerate() {
        let key = env.keypair(&format!("whitespace-{index}"), Some(passphrase));
        env.authorize(&[&key]).await;
        let login = connect(
            &env,
            json!({ "identityFile": key.path, "passphrase": passphrase }),
        )
        .await;
        assert!(login["authMethod"] == "key" && login["usedKeyFp"] == key.fingerprint);
        bad(post(
            &env,
            "/api/connect",
            connect_body(
                &env,
                json!({
                    "identityFile": key.path, "passphrase": passphrase.trim()
                }),
            ),
        )
        .await);
    }
    env.assert_clean().await;
}

#[tokio::test]
async fn add_switches_saved_identity_only_after_new_key_authentication_and_deduplicates() {
    let env = Env::start("").await;
    let old = env.keypair("old", None);
    let new = env.keypair("new", None);
    env.authorize(&[&old]).await;
    let id = save_host(&env, &old).await;
    let sid = text(&connect(&env, json!({ "hostId": id })).await["sid"]);
    env.clear_events().await;
    env.fs
        .lock()
        .await
        .reject_keys
        .insert(new.fingerprint.clone());
    let body = json!({ "sid": sid, "mode": "local", "path": new.path, "switchToNew": true });
    let rejected = ok(post(&env, "/api/add", body.clone()).await);
    assert!(rejected["changed"] == true && rejected["switched"] == false);
    assert!(
        stored_identity(&env, &id) == old.path,
        "unverified identity was saved"
    );
    let mut expected = transaction(&[&old, &new]);
    expected.push(auth(&new, true, false));
    assert!(
        env.events().await == expected,
        "add must write before verifying the new key"
    );
    env.assert_keys(&[&old, &new]).await;
    env.clear_events().await;
    env.fs.lock().await.reject_keys.clear();
    let switched = ok(post(&env, "/api/add", body.clone()).await);
    assert!(switched["changed"] == false && switched["switched"] == true);
    assert!(stored_identity(&env, &id) == new.path);
    assert!(env.events().await == vec![Event::Read, auth(&new, true, true), Event::LoginProbe]);
    env.clear_events().await;
    let mut no_switch = body;
    no_switch["switchToNew"] = json!(false);
    let duplicate = ok(post(&env, "/api/add", no_switch).await);
    assert!(duplicate["changed"] == false && duplicate["skippedByUser"] == true);
    assert!(env.events().await == vec![Event::Read]);
    env.assert_keys(&[&old, &new]).await;
    env.assert_clean().await;
}

#[tokio::test]
async fn generated_add_stays_in_isolated_settings_directory_and_verifies_before_switch() {
    let env = Env::start("").await;
    let old = env.keypair("old", None);
    env.authorize(&[&old]).await;
    let id = save_host(&env, &old).await;
    let sid = text(&connect(&env, json!({ "hostId": id })).await["sid"]);
    env.clear_events().await;
    let added = ok(post(
        &env,
        "/api/add",
        json!({
            "sid": sid, "mode": "generate", "generator": "ed25519", "switchToNew": true
        }),
    )
    .await);
    let path = text(&added["privateKeyPath"]);
    assert!(
        Path::new(&path).starts_with(&env.keys),
        "generated private key escaped tempdir"
    );
    assert!(Path::new(&path).is_file() && Path::new(&format!("{path}.pub")).is_file());
    assert!(added["changed"] == true && added["switched"] == true);
    assert!(stored_identity(&env, &id) == path);
    let generated = Key {
        path,
        fingerprint: text(&added["fingerprint"]),
        public: must(std::fs::read_to_string(format!(
            "{}.pub",
            stored_identity(&env, &id)
        ))),
    };
    let mut expected = transaction(&[&old, &generated]);
    expected.extend([auth(&generated, true, true), Event::LoginProbe]);
    assert!(
        env.events().await == expected,
        "generated key switch order mismatch"
    );
    env.assert_keys(&[&old, &generated]).await;
    env.assert_clean().await;
}

#[tokio::test]
async fn replace_strictly_adds_authenticates_then_removes_and_updates_saved_identity() {
    let env = Env::start("").await;
    let old = env.keypair("old", None);
    let new = env.keypair("new", None);
    env.authorize(&[&old]).await;
    let id = save_host(&env, &old).await;
    let sid = text(&connect(&env, json!({ "hostId": id })).await["sid"]);
    env.clear_events().await;
    let replaced = ok(post(
        &env,
        "/api/replace",
        json!({
            "sid": sid, "oldFingerprint": old.fingerprint, "mode": "local", "path": new.path
        }),
    )
    .await);
    assert!(replaced["removedOld"] == true && replaced["switched"] == true);
    assert!(replaced["session"]["usedKeyFp"] == new.fingerprint);
    assert!(stored_identity(&env, &id) == new.path);
    let mut expected = transaction(&[&old, &new]);
    expected.extend([auth(&new, true, true), Event::LoginProbe]);
    expected.extend(transaction(&[&new]));
    assert!(
        env.events().await == expected,
        "replacement violated add-authenticate-remove order"
    );
    env.assert_keys(&[&new]).await;
    let reconnect = ok(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "hostId": id })),
    )
    .await);
    assert!(reconnect["usedKeyFp"] == new.fingerprint);
    bad(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "identityFile": old.path })),
    )
    .await);
    env.assert_clean().await;
}

#[tokio::test]
async fn replace_authentication_failure_keeps_old_key_and_identity() {
    let env = Env::start("").await;
    let old = env.keypair("old", None);
    let new = env.keypair("new", None);
    env.authorize(&[&old]).await;
    let id = save_host(&env, &old).await;
    let sid = text(&connect(&env, json!({ "hostId": id })).await["sid"]);
    env.clear_events().await;
    env.fs
        .lock()
        .await
        .reject_keys
        .insert(new.fingerprint.clone());
    let replaced = ok(post(
        &env,
        "/api/replace",
        json!({
            "sid": sid, "oldFingerprint": old.fingerprint, "mode": "local", "path": new.path
        }),
    )
    .await);
    assert!(replaced["needsForce"] == true && replaced["removedOld"] == false);
    assert!(replaced["session"]["usedKeyFp"] == old.fingerprint);
    assert!(stored_identity(&env, &id) == old.path);
    let mut expected = transaction(&[&old, &new]);
    expected.push(auth(&new, true, false));
    assert!(
        env.events().await == expected,
        "failed authentication must not remove old key"
    );
    env.assert_keys(&[&old, &new]).await;
    env.assert_clean().await;
}

#[tokio::test]
async fn replace_public_only_keeps_old_key_without_attempting_authentication() {
    let env = Env::start("").await;
    let old = env.keypair("old", None);
    let new = env.keypair("public-only", None);
    must(std::fs::remove_file(&new.path));
    env.authorize(&[&old]).await;
    let id = save_host(&env, &old).await;
    let sid = text(&connect(&env, json!({ "hostId": id })).await["sid"]);
    env.clear_events().await;
    let replaced = ok(post(&env, "/api/replace", json!({
        "sid": sid, "oldFingerprint": old.fingerprint, "mode": "local", "path": format!("{}.pub", new.path)
    })).await);
    assert!(replaced["needsForce"] == true && replaced["removedOld"] == false);
    assert!(stored_identity(&env, &id) == old.path);
    assert!(env.events().await == transaction(&[&old, &new]));
    env.assert_keys(&[&old, &new]).await;
    env.assert_clean().await;
}

#[tokio::test]
async fn delete_guards_current_and_last_key_but_allows_normal_and_forced_nonempty_deletion() {
    let env = Env::start("").await;
    let current = env.keypair("current", None);
    let spare = env.keypair("spare", None);
    env.authorize(&[&current, &spare]).await;
    let sid = text(&connect(&env, json!({ "identityFile": current.path })).await["sid"]);
    env.clear_events().await;
    let guarded = ok(post(
        &env,
        "/api/delete",
        json!({ "sid": sid, "fingerprints": [current.fingerprint] }),
    )
    .await);
    assert!(guarded["changed"] == false && guarded["needsForce"] == true);
    assert!(env.events().await == vec![Event::Read]);
    env.assert_keys(&[&current, &spare]).await;
    env.clear_events().await;
    let deleted = ok(post(
        &env,
        "/api/delete",
        json!({ "sid": sid, "fingerprints": [spare.fingerprint] }),
    )
    .await);
    assert!(deleted["changed"] == true);
    assert!(env.events().await == transaction(&[&current]));
    env.assert_keys(&[&current]).await;
    env.clear_events().await;
    let denied = bad(post(
        &env,
        "/api/delete",
        json!({ "sid": sid, "fingerprints": [current.fingerprint], "force": true }),
    )
    .await);
    assert!(text(&denied["error"]).contains("清空"));
    assert!(env.events().await == vec![Event::Read]);
    env.assert_keys(&[&current]).await;
    ok(post(
        &env,
        "/api/add",
        json!({ "sid": sid, "mode": "paste", "publicKey": spare.public }),
    )
    .await);
    env.clear_events().await;
    let forced = ok(post(
        &env,
        "/api/delete",
        json!({ "sid": sid, "fingerprints": [current.fingerprint], "force": true }),
    )
    .await);
    assert!(forced["changed"] == true);
    assert!(env.events().await == transaction(&[&spare]));
    env.assert_keys(&[&spare]).await;
    env.assert_clean().await;
}

#[tokio::test]
async fn backups_list_diff_restore_and_noop_match_remote_contents() {
    let env = Env::start("").await;
    let current = env.keypair("current", None);
    let extra = env.keypair("extra", None);
    env.authorize(&[&current]).await;
    let original = env.authorized().await;
    let sid = text(&connect(&env, json!({ "identityFile": current.path })).await["sid"]);
    ok(post(
        &env,
        "/api/add",
        json!({ "sid": sid, "mode": "paste", "publicKey": extra.public }),
    )
    .await);
    let listed = ok(get(&env, &format!("/api/backups?sid={sid}")).await);
    let backups = listed["backups"].as_array().expect("backup list missing");
    assert!(backups.len() == 1);
    let name = text(&backups[0]["name"]);
    let backup_path = must(kw_keys::backups::backup_path_for(AUTHORIZED, &name));
    assert!(
        env.fs.lock().await.files.get(&backup_path) == Some(&original),
        "backup must contain pre-write content"
    );
    let diff = ok(get(&env, &format!("/api/backups/diff?sid={sid}&name={name}")).await);
    assert!(diff["backupKeyCount"] == 1 && diff["currentKeyCount"] == 2 && diff["sameCount"] == 1);
    assert!(diff["emptyBackup"] == false && diff["usedKeyInBackup"] == true);
    assert!(diff["added"].as_array().is_some_and(Vec::is_empty));
    assert!(diff["removed"][0]["fingerprint"] == extra.fingerprint);
    env.clear_events().await;
    let restored = ok(post(
        &env,
        "/api/backups/restore",
        json!({ "sid": sid, "name": name }),
    )
    .await);
    assert!(restored["changed"] == true && restored["restored"] == true);
    assert!(env.authorized().await == original);
    assert!(env.events().await == transaction(&[&current]));
    // 独立旧快照只用于幂等分支；上面恢复的是真实写流程产生的备份。
    let stable = "authorized_keys.bak.20000101000000";
    env.fs
        .lock()
        .await
        .files
        .insert(format!("{AUTHORIZED}.bak.20000101000000"), original.clone());
    env.clear_events().await;
    let noop = ok(post(
        &env,
        "/api/backups/restore",
        json!({ "sid": sid, "name": stable }),
    )
    .await);
    assert!(noop["changed"] == false && env.events().await == vec![Event::Read]);
    let listed = ok(get(&env, &format!("/api/backups?sid={sid}")).await);
    let names = listed["backups"].as_array().expect("backup list missing");
    assert!(names
        .windows(2)
        .all(|pair| pair[0]["stamp"].as_str() > pair[1]["stamp"].as_str()));
    env.assert_clean().await;
}

#[tokio::test]
async fn restore_rejects_empty_backup_even_with_force_and_guards_missing_current_key() {
    let env = Env::start("").await;
    let current = env.keypair("current", None);
    let spare = env.keypair("spare", None);
    env.authorize(&[&current]).await;
    let sid = text(&connect(&env, json!({ "identityFile": current.path })).await["sid"]);
    let empty = "authorized_keys.bak.20000101000000";
    let missing = "authorized_keys.bak.20000102000000";
    {
        let mut fs = env.fs.lock().await;
        fs.files.insert(
            format!("{AUTHORIZED}.bak.20000101000000"),
            "# no usable keys\n".into(),
        );
        fs.files.insert(
            format!("{AUTHORIZED}.bak.20000102000000"),
            format!("{}\n", spare.public),
        );
    }
    for force in [false, true] {
        env.clear_events().await;
        let denied = bad(post(
            &env,
            "/api/backups/restore",
            json!({ "sid": sid, "name": empty, "force": force }),
        )
        .await);
        assert!(text(&denied["error"]).contains("清空"));
        assert!(env.events().await == vec![Event::Read]);
        env.assert_keys(&[&current]).await;
    }
    env.clear_events().await;
    let guarded = ok(post(
        &env,
        "/api/backups/restore",
        json!({ "sid": sid, "name": missing }),
    )
    .await);
    assert!(guarded["changed"] == false && guarded["needsForce"] == true);
    assert!(guarded["diff"]["usedKeyInBackup"] == false);
    assert!(env.events().await == vec![Event::Read]);
    env.assert_keys(&[&current]).await;
    env.clear_events().await;
    let restored = ok(post(
        &env,
        "/api/backups/restore",
        json!({ "sid": sid, "name": missing, "force": true }),
    )
    .await);
    assert!(restored["restored"] == true);
    assert!(env.events().await == transaction(&[&spare]));
    env.assert_keys(&[&spare]).await;
    env.clear_events().await;
    bad(post(
        &env,
        "/api/backups/restore",
        json!({ "sid": sid, "name": "../sshd_config" }),
    )
    .await);
    assert!(
        env.events().await.is_empty(),
        "invalid backup path reached SSH"
    );
    env.assert_clean().await;
}

#[tokio::test]
async fn sshd_key_only_switch_requires_key_auth_and_can_be_disabled() {
    let env = Env::start("").await;
    let key = env.keypair("current", None);
    env.authorize(&[&key]).await;
    let password_sid = text(&connect(&env, json!({ "password": PASSWORD })).await["sid"]);
    env.clear_events().await;
    let denied = bad(post(
        &env,
        "/api/sshd/password-auth",
        json!({ "sid": password_sid, "enable": true }),
    )
    .await);
    assert!(text(&denied["error"]).contains("密码登录"));
    assert!(env.events().await == vec![Event::SshdInspect, Event::Read]);
    let sid = text(&connect(&env, json!({ "identityFile": key.path })).await["sid"]);
    let before = ok(get(&env, &format!("/api/sshd?sid={sid}")).await);
    assert!(before["passwordAuth"] == true && before["available"] == true);
    env.clear_events().await;
    let enabled = ok(post(
        &env,
        "/api/sshd/password-auth",
        json!({ "sid": sid, "enable": true }),
    )
    .await);
    assert!(enabled["effective"] == true && enabled["reloaded"] == true);
    assert!(enabled["status"]["passwordAuth"] == false);
    assert!(
        env.events().await
            == vec![
                Event::SshdInspect,
                Event::Read,
                Event::SshdWrite(true),
                Event::SshdInspect
            ]
    );
    let config = env
        .fs
        .lock()
        .await
        .files
        .get(SSHD_CONFIG)
        .cloned()
        .expect("sshd config missing");
    assert!(
        config.contains("PasswordAuthentication no")
            && config.contains("KbdInteractiveAuthentication no")
    );
    assert!(config.contains("PubkeyAuthentication yes"));
    assert!(env
        .fs
        .lock()
        .await
        .files
        .contains_key(&text(&enabled["backup"])));
    bad(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "password": PASSWORD })),
    )
    .await);
    env.clear_events().await;
    let disabled = ok(post(
        &env,
        "/api/sshd/password-auth",
        json!({ "sid": sid, "enable": false }),
    )
    .await);
    assert!(disabled["effective"] == true && disabled["status"]["passwordAuth"] == true);
    assert!(
        env.events().await
            == vec![
                Event::SshdInspect,
                Event::SshdWrite(false),
                Event::SshdInspect
            ]
    );
    ok(post(
        &env,
        "/api/connect",
        connect_body(&env, json!({ "password": PASSWORD })),
    )
    .await);
    env.assert_clean().await;
}

#[tokio::test]
async fn concurrent_adds_are_serialized_without_lost_keys_while_other_ssh_channels_run() {
    let env = Env::start("").await;
    let old = env.keypair("old", None);
    let a = env.keypair("a", None);
    let b = env.keypair("b", None);
    let c = env.keypair("c", None);
    env.authorize(&[&old]).await;
    let sid = text(&connect(&env, json!({ "identityFile": old.path })).await["sid"]);
    let session = env.state.sessions.get(&sid).await.expect("session missing");
    let gate = Arc::new(WriteGate::default());
    env.clear_events().await;
    {
        let mut fs = env.fs.lock().await;
        fs.next_write_gate = Some(gate.clone());
        fs.write_delay = Duration::from_millis(25);
    }
    let first = post(
        &env,
        "/api/add",
        json!({ "sid": sid, "mode": "paste", "publicKey": a.public }),
    );
    tokio::pin!(first);
    tokio::select! {
        _ = &mut first => panic!("first transaction completed before write gate"),
        _ = gate.entered.notified() => {},
        _ = tokio::time::sleep(DEADLINE) => panic!("write gate not reached"),
    }
    assert!(
        session.write_lock.try_lock().is_err(),
        "transaction does not own write lock"
    );
    let second = post(
        &env,
        "/api/add",
        json!({ "sid": sid, "mode": "paste", "publicKey": b.public }),
    );
    let third = post(
        &env,
        "/api/add",
        json!({ "sid": sid, "mode": "paste", "publicKey": c.public }),
    );
    tokio::pin!(second, third);
    assert!(futures_util::poll!(&mut second).is_pending());
    assert!(futures_util::poll!(&mut third).is_pending());
    let probe = must(must(
        tokio::time::timeout(DEADLINE, session.client.exec("printf __LOGIN_OK__", None)).await,
    ));
    assert!(
        probe.code == 0 && probe.stdout == "__LOGIN_OK__",
        "delayed write blocked the SSH connection"
    );
    assert!(
        env.events().await
            == vec![
                Event::Read,
                Event::Backup,
                Event::WriteStart,
                Event::LoginProbe
            ],
        "queued writes interleaved before first commit"
    );
    gate.release.notify_one();
    let (one, two, three) = tokio::join!(first, second, third);
    for response in [one, two, three] {
        assert!(ok(response)["changed"] == true);
    }
    env.assert_keys(&[&old, &a, &b, &c]).await;
    let events: Vec<_> = env
        .events()
        .await
        .into_iter()
        .filter(|e| *e != Event::LoginProbe)
        .collect();
    let mut expected = transaction(&[&old, &a]);
    expected.extend(transaction(&[&old, &a, &b]));
    expected.extend(transaction(&[&old, &a, &b, &c]));
    assert!(
        events == expected,
        "read-backup-stage-commit-readback transactions interleaved"
    );
    env.assert_clean().await;
}

#[tokio::test]
async fn terminal_only_skips_authorized_keys_and_disconnect_invalidates_session() {
    let env = Env::start("").await;
    let login = connect(&env, json!({ "password": PASSWORD, "withKeys": false })).await;
    let sid = text(&login["sid"]);
    assert!(login["keysLoaded"] == false && login["remotePath"] == "" && login["home"] == "");
    assert!(
        env.events().await == vec![Event::PasswordAuth(true)],
        "terminal-only connection read authorized_keys"
    );
    bad(post(
        &env,
        "/api/add",
        json!({ "sid": sid, "mode": "paste", "publicKey": "unused" }),
    )
    .await);
    assert!(env.events().await == vec![Event::PasswordAuth(true)]);
    let refreshed = ok(post(&env, "/api/refresh", json!({ "sid": sid })).await);
    assert!(refreshed["keysLoaded"] == true && refreshed["remotePath"] == AUTHORIZED);
    assert!(env.events().await == vec![Event::PasswordAuth(true), Event::Read]);
    assert!(ok(post(&env, "/api/disconnect", json!({ "sid": sid })).await) == true);
    env.clear_events().await;
    code(
        post(&env, "/api/refresh", json!({ "sid": sid })).await,
        "SESSION_LOST",
    );
    code(
        post(
            &env,
            "/api/delete",
            json!({ "sid": sid, "fingerprints": [] }),
        )
        .await,
        "SESSION_LOST",
    );
    assert!(env.events().await.is_empty());
    assert!(env.state.sessions.list().await.is_empty());
    env.assert_clean().await;
}

#[tokio::test]
async fn server_disconnect_turns_existing_session_into_session_lost() {
    let env = Env::start("").await;
    let sid = text(&connect(&env, json!({ "password": PASSWORD })).await["sid"]);
    let session = env.state.sessions.get(&sid).await.expect("session missing");
    env.shutdown.shutdown(String::new());
    must(
        tokio::time::timeout(DEADLINE, async {
            while !session.client.is_closed().await {
                tokio::task::yield_now().await;
            }
        })
        .await,
    );
    env.clear_events().await;
    code(
        post(&env, "/api/refresh", json!({ "sid": sid })).await,
        "SESSION_LOST",
    );
    assert!(env.state.sessions.list().await.is_empty());
    assert!(env.events().await.is_empty());
    env.assert_clean().await;
}

#[tokio::test]
async fn password_fallback_must_not_bypass_key_only_safety_gate() {
    let env = Env::start("").await;
    let key = env.keypair("rejected", None);
    env.authorize(&[&key]).await;
    env.fs
        .lock()
        .await
        .reject_keys
        .insert(key.fingerprint.clone());
    let login = connect(
        &env,
        json!({ "identityFile": key.path, "password": PASSWORD }),
    )
    .await;
    assert!(
        env.events().await
            == vec![
                auth(&key, true, false),
                Event::PasswordAuth(true),
                Event::Read
            ]
    );
    let sid = text(&login["sid"]);
    env.clear_events().await;
    let response = post(
        &env,
        "/api/sshd/password-auth",
        json!({ "sid": sid, "enable": true }),
    )
    .await;
    assert!(
        response.0 == StatusCode::BAD_REQUEST,
        "password fallback bypassed key-only safety gate"
    );
    assert!(
        login["authMethod"] == "password" && login["usedKeyFp"] == "",
        "password fallback mislabeled as key authentication"
    );
    assert!(env.events().await == vec![Event::SshdInspect, Event::Read]);
    env.assert_clean().await;
}

struct BrowserService {
    port: u16,
    task: JoinHandle<()>,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for BrowserService {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}

struct WireResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

impl BrowserService {
    async fn start(env: &Env) -> Self {
        let listener = must(TcpListener::bind("127.0.0.1:0").await);
        let address = must(listener.local_addr());
        assert!(address.ip().is_loopback());
        let mut state = AppState::new(
            TOKEN.into(),
            address.port(),
            env.state.store.clone(),
            env.state.known_hosts.clone(),
            env.state.public_dir.clone(),
            "test",
        );
        state.auto_cookie = false;
        let app = routes::router(Arc::new(state));
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            must(
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await,
            );
        });
        Self {
            port: address.port(),
            task,
            stop: Some(stop),
        }
    }

    async fn get(&self, path: &str, cookie: Option<&str>, hostile_host: bool) -> WireResponse {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let host = if hostile_host {
            "attacker.invalid".to_string()
        } else {
            format!("127.0.0.1:{}", self.port)
        };
        let cookie = cookie
            .map(|c| format!("Cookie: {c}\r\n"))
            .unwrap_or_default();
        let raw =
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{cookie}\r\n");
        let bytes = must(
            tokio::time::timeout(DEADLINE, async {
                let mut stream = must(
                    tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, self.port))
                        .await,
                );
                must(stream.write_all(raw.as_bytes()).await);
                let mut response = Vec::new();
                must(stream.read_to_end(&mut response).await);
                response
            })
            .await,
        );
        let response = must(String::from_utf8(bytes));
        let (headers, body) = response
            .split_once("\r\n\r\n")
            .expect("invalid HTTP response");
        let mut lines = headers.lines();
        let status = must(
            lines
                .next()
                .expect("HTTP status missing")
                .split_whitespace()
                .nth(1)
                .expect("HTTP status missing")
                .parse(),
        );
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(':').expect("invalid HTTP header");
                (name.to_ascii_lowercase(), value.trim().to_string())
            })
            .collect();
        WireResponse {
            status,
            headers,
            body: body.into(),
        }
    }
}

#[tokio::test]
async fn desktop_cookie_and_standalone_browser_login_enforce_token_and_host_guard() {
    let env = Env::start("").await;
    let desktop = request(env.app.clone(), "GET", "/", Value::Null).await;
    assert!(desktop.status() == StatusCode::OK);
    let cookie = desktop
        .headers()
        .get("set-cookie")
        .expect("desktop cookie missing");
    assert!(
        must(cookie.to_str()).contains("HttpOnly")
            && must(cookie.to_str()).contains("SameSite=Strict")
    );
    let browser = BrowserService::start(&env).await;
    let index = browser.get("/", None, false).await;
    assert!(index.status == 200 && !index.headers.contains_key("set-cookie"));
    assert!(index.body.contains("<title>kw</title>") && !index.body.contains(TOKEN));
    assert!(browser.get("/api/state", None, false).await.status == 401);
    assert!(
        browser
            .get("/api/state", Some("kw=wrong"), false)
            .await
            .status
            == 401
    );
    let wrong = browser.get("/login?token=wrong", None, false).await;
    assert!(wrong.status == 401 && !wrong.headers.contains_key("set-cookie"));
    let blocked = browser
        .get(&format!("/login?token={TOKEN}"), None, true)
        .await;
    assert!(blocked.status == 421 && !blocked.headers.contains_key("set-cookie"));
    let login = browser
        .get(&format!("/login?token={TOKEN}"), None, false)
        .await;
    assert!(login.status == 303 && login.headers.get("location").is_some_and(|v| v == "/"));
    let cookie = login
        .headers
        .get("set-cookie")
        .expect("browser login cookie missing");
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Path=/")
    );
    let pair = cookie.split(';').next().expect("cookie pair missing");
    let authorized = browser.get("/api/state", Some(pair), false).await;
    assert!(authorized.status == 200);
    let body: Value = must(serde_json::from_str(&authorized.body));
    assert!(body["ok"] == true);
    assert!(body["data"]["settings"]["localKeyDir"] == env.keys.display().to_string());
    assert!(!authorized.body.contains(TOKEN) && !authorized.body.contains(PASSWORD));
    assert!(browser.get("/api/state", Some(pair), true).await.status == 421);
    env.assert_clean().await;
}

/// 手机端终端走的缓冲句柄：真 russh 通道上的打开 / 回显 / stderr / 改宽高 / 退出。
#[tokio::test]
async fn pty_terminal_buffers_echo_tracks_size_and_closes_on_exit() {
    let env = Env::start("").await;
    let key = env.keypair("pty", None);
    env.authorize(&[&key]).await;
    let sid = text(&connect(&env, json!({ "identityFile": key.path })).await["sid"]);
    let session = env
        .state
        .sessions
        .must(&sid)
        .await
        .expect("live session missing");

    let term = must(Terminal::open(&session.client, &session.sid, 90, 26).await);
    assert_eq!(term.dims(), (90, 26));
    assert!(!term.is_closed());

    must(term.input(b"echo hello").await);
    let (echo, dropped) = read_until(&term, b"echo hello").await;
    assert!(!dropped, "首轮输出不该触发丢弃");
    assert_eq!(echo, b"echo hello".to_vec(), "回显要原样进缓冲");
    // 能回显就说明服务端的 pty-req 分支跑过了，此时记录必定已落盘。
    assert_eq!(
        env.fs.lock().await.pty_requests.clone(),
        vec![("xterm-256color".to_string(), 90, 26)],
        "pty-req 必须带着界面请求的行列到达服务端"
    );

    must(term.input(b"boom STDERR").await);
    let (stderr, _) = read_until(&term, b"stderr-stream").await;
    assert!(
        String::from_utf8_lossy(&stderr).contains("stderr-stream"),
        "stderr 要走 ext==1 分支并进同一个缓冲"
    );

    must(term.resize(120, 40).await);
    assert_eq!(term.dims(), (120, 40));
    must(term.input(b"after-resize").await);
    read_until(&term, b"after-resize").await;
    assert_eq!(env.fs.lock().await.resizes.clone(), vec![(120, 40)]);

    must(term.input(b"logout EXIT").await);
    wait_closed(&term).await;
    let (tail, _) = term.drain().await;
    assert!(
        String::from_utf8_lossy(&tail).contains("logout EXIT"),
        "退出状态之前塞进队列的输出必须还能取到"
    );
    assert!(
        term.input(b"too late").await.is_err(),
        "退出后再打字必须失败"
    );
    env.assert_clean().await;
}

/// 通道的输出是异步到达的，轮询缓冲直到看见目标字节或超时。
async fn read_until(term: &Arc<Terminal>, needle: &[u8]) -> (Vec<u8>, bool) {
    let mut collected = Vec::new();
    let mut dropped = false;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (bytes, flag) = term.drain().await;
        collected.extend_from_slice(&bytes);
        dropped |= flag;
        if contains(&collected, needle) || Instant::now() >= deadline {
            assert!(
                contains(&collected, needle),
                "terminal stream did not carry the expected bytes"
            );
            return (collected, dropped);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_closed(term: &Arc<Terminal>) {
    let deadline = Instant::now() + DEADLINE;
    while !term.is_closed() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(term.is_closed(), "远端退出后句柄必须标记为已关闭");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
