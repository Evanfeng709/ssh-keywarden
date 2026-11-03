//! 纯 Rust 桌面客户端主界面（egui / eframe）：1:1 Termius 风格布局与全量业务闭环。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, Event, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Id, Key, Layout, Margin, Order, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Vec2, ViewportCommand,
};
use serde_json::{json, Value};

use kw_core::Secret;
use kw_ffi::MobileEngine;
use kw_keys::localkeys::LocalKeyEntry;
use kw_server::connect::ConnectBody;
use kw_server::routes::bootstrap_hosts;
use kw_store::{Host as HostEntry, HostPatch, SettingsPatch};

use crate::desktop::{
    is_system_dark, read_theme, read_ui, save_bounds, save_theme, save_ui_mode, trim_working_set,
    Bounds,
};
use crate::icons::{draw_icon, Icon};
use crate::theme::{apply_palette, resolve_palette, Palette, PALETTES, THEME_CHOICES};
use crate::vt::{is_wide_char, VtScreen};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Keys,
    Term,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyFilter {
    All,
    Unmatched,
    Dup,
    Used,
}

#[derive(Debug, Clone)]
struct LogItem {
    time: String,
    text: String,
    kind: LogKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogKind {
    Info,
    Ok,
    Warn,
    Err,
}

#[derive(Debug, Clone)]
enum Drawer {
    None,
    HostEdit {
        id: Option<String>,
        name: String,
        host: String,
        port: String,
        username: String,
        identity_file: String,
        remote_path: String,
    },
    ConnectPrompt {
        host: HostEntry,
        target_mode: ViewMode,
        password: String,
        passphrase: String,
        trust_fp: Option<String>,
        hostkey_warning: Option<(String, String, bool)>,
    },
    AddKey {
        tab: usize,
        algo: String,
        label: String,
        comment: String,
        passphrase: String,
        selected_local_pub: String,
        pasted_pub: String,
    },
    ReplaceKey {
        old_index: i64,
        old_comment: String,
        old_fp: String,
        tab: usize,
        algo: String,
        label: String,
        comment: String,
        passphrase: String,
        selected_priv_path: String,
    },
    EditNote {
        fingerprint: String,
        comment: String,
        note: String,
    },
    ConfirmDelete {
        fingerprints: Vec<String>,
        message: String,
    },
    Backups {
        items: Vec<Value>,
        selected_name: String,
        diff: Option<Value>,
    },
    Sshd {
        info: Value,
    },
    Sync {
        client_id: String,
        passphrase: String,
        remember_key: bool,
        device_prompt: Option<(String, String)>,
        conflicts: Vec<Value>,
    },
    Settings {
        theme_choice: String,
        local_key_dir: String,
        save_dir: String,
        remote_path: String,
    },
}

enum BgEvent {
    Log(String, LogKind),
    Connected {
        session: Value,
        target_mode: ViewMode,
    },
    DeleteNeedsForce {
        fingerprints: Vec<String>,
        message: String,
    },
    ConnectHostKeyPrompt {
        host: HostEntry,
        target_mode: ViewMode,
        password: String,
        passphrase: String,
        title: String,
        message: String,
        fingerprint: String,
        danger: bool,
    },
    SessionUpdated(Value),
    Disconnected,
    SshdLoaded {
        info: Value,
        open_drawer: bool,
    },
    BackupsLoaded(Vec<Value>),
    BackupDiffLoaded(String, Value),
    SyncStatus(Value),
    SyncDeviceCode {
        user_code: String,
        verify_uri: String,
    },
    SyncDone {
        report: Value,
        status: Value,
    },
    LocalScanDone(Vec<LocalKeyEntry>),
    TermOpened {
        term_id: String,
        cols: usize,
        rows: usize,
    },
    TermOutput(Vec<u8>),
    TermClosed(Option<String>),
}

const SNIPPETS: [(&str, &str); 5] = [
    ("⚡ tmux", "tmux attach || tmux new\r"),
    (
        "docker ps",
        "docker ps --format \"table {{.Names}}\\t{{.Status}}\\t{{.Ports}}\"\r",
    ),
    ("端口监听", "ss -tulpn\r"),
    ("磁盘/内存", "df -h && free -m\r"),
    ("系统负载", "uptime && top -bn1 | head -n 15\r"),
];

pub struct KwDesktopApp {
    rt: tokio::runtime::Handle,
    engine: Arc<MobileEngine>,
    data_dir: PathBuf,
    tx: Sender<BgEvent>,
    rx: Receiver<BgEvent>,

    theme_choice: String,
    palette: Palette,
    mode: ViewMode,
    show_local_panel: bool,
    show_log_drawer: bool,
    was_minimized: bool,
    startup_frames: u8,
    busy: bool,

    hosts: Vec<HostEntry>,
    key_notes: HashMap<String, String>,
    local_keys: Vec<LocalKeyEntry>,
    vault_search: String,

    session: Option<Value>,
    sshd_info: Option<Value>,
    selected_indices: HashSet<i64>,
    key_search: String,
    key_filter: KeyFilter,

    term_id: Option<String>,
    vt: VtScreen,
    term_scroll_offset: usize,
    term_focused: bool,
    last_sent_size: (usize, usize),

    ms_status: Value,
    drawer: Drawer,
    logs: Vec<LogItem>,
    last_bounds_save: Instant,
}

impl KwDesktopApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        rt: tokio::runtime::Handle,
        data_dir: PathBuf,
    ) -> Self {
        setup_fonts(&cc.egui_ctx);

        let base_dir = data_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| data_dir.clone());
        let engine = Arc::new(MobileEngine::new(&base_dir).expect("初始化核心引擎失败"));
        bootstrap_hosts(engine.state());

        let mut theme_choice = read_theme(&data_dir);
        if theme_choice.is_empty() {
            theme_choice = "obsidian".to_string();
            let _ = save_theme(&data_dir, &theme_choice);
        }
        let sys_dark = is_system_dark();
        let palette = resolve_palette(&theme_choice, sys_dark);
        apply_palette(&cc.egui_ctx, &palette);

        let ui_val = read_ui(&data_dir);
        let mode = if ui_val.get("mode").and_then(|v| v.as_str()) == Some("term") {
            ViewMode::Term
        } else {
            ViewMode::Keys
        };

        let (tx, rx) = mpsc::channel();
        let hosts = engine.state().store.list_hosts();
        let key_notes = engine.state().store.list_key_notes().into_iter().map(|(k, v)| (k, v.note)).collect();
        let ms_status = engine.ms_status();

        let mut app = Self {
            rt,
            engine,
            data_dir,
            tx,
            rx,
            theme_choice,
            palette,
            mode,
            show_local_panel: false,
            show_log_drawer: false,
            was_minimized: false,
            startup_frames: 0,
            busy: false,
            hosts,
            key_notes,
            local_keys: Vec::new(),
            vault_search: String::new(),
            session: None,
            sshd_info: None,
            selected_indices: HashSet::new(),
            key_search: String::new(),
            key_filter: KeyFilter::All,
            term_id: None,
            vt: VtScreen::new(100, 30),
            term_scroll_offset: 0,
            term_focused: false,
            last_sent_size: (100, 30),
            ms_status,
            drawer: Drawer::None,
            logs: Vec::new(),
            last_bounds_save: Instant::now(),
        };
        app.push_log("就绪：选择主机卡片即可直连管理密钥或开启 SSH 终端", LogKind::Info);
        app.trigger_local_scan(false);
        app
    }

    fn push_log(&mut self, text: impl Into<String>, kind: LogKind) {
        let stamp = kw_ssh::util::shanghai_stamp(std::time::SystemTime::now());
        let time = if stamp.len() >= 14 {
            format!("{}:{}:{}", &stamp[8..10], &stamp[10..12], &stamp[12..14])
        } else {
            "00:00:00".to_string()
        };
        self.logs.push(LogItem {
            time,
            text: text.into(),
            kind,
        });
        if self.logs.len() > 200 {
            self.logs.remove(0);
        }
    }

    fn reload_store_data(&mut self) {
        self.hosts = self.engine.state().store.list_hosts();
        self.key_notes = self.engine.state().store.list_key_notes().into_iter().map(|(k, v)| (k, v.note)).collect();
        self.ms_status = self.engine.ms_status();
    }

    fn trigger_local_scan(&self, force: bool) {
        let state = Arc::clone(self.engine.state());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let settings = state.current_settings();
            if settings.local_key_dir.is_empty() {
                let _ = tx.send(BgEvent::LocalScanDone(Vec::new()));
                return;
            }
            let scan = state.scan_local(&settings.local_key_dir, force).await;
            let _ = tx.send(BgEvent::LocalScanDone(scan.entries));
        });
    }

    fn switch_mode(&mut self, next: ViewMode) {
        if self.mode == next {
            return;
        }
        self.mode = next;
        let m_str = match next {
            ViewMode::Keys => "keys",
            ViewMode::Term => "term",
        };
        save_ui_mode(&self.data_dir, m_str, 0);

        if next == ViewMode::Term {
            self.show_local_panel = false;
            if self.session.is_some() && self.term_id.is_none() {
                self.open_pty();
            }
            self.term_focused = true;
        } else if next == ViewMode::Keys {
            if let Some(sess) = &self.session {
                if sess.get("keysLoaded").and_then(|v| v.as_bool()) == Some(false) {
                    self.refresh_session();
                }
            }
        }
    }

    fn start_connect_flow(&mut self, host: HostEntry, target_mode: ViewMode) {
        self.switch_mode(target_mode);
        self.drawer = Drawer::ConnectPrompt {
            host,
            target_mode,
            password: String::new(),
            passphrase: String::new(),
            trust_fp: None,
            hostkey_warning: None,
        };
    }

    fn execute_connect(
        &mut self,
        host: HostEntry,
        target_mode: ViewMode,
        password: String,
        passphrase: String,
        trust_fp: Option<String>,
    ) {
        self.busy = true;
        self.drawer = Drawer::None;
        let label = if host.name.is_empty() {
            host.host.clone()
        } else {
            host.name.clone()
        };
        self.push_log(
            format!("正在连接 {} ({}@{}:{})…", label, host.username, host.host, host.port),
            LogKind::Info,
        );

        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        let with_keys = target_mode == ViewMode::Keys;
        let pw_clone = password.clone();
        let pp_clone = passphrase.clone();
        self.rt.spawn(async move {
            let body = ConnectBody {
                host_id: Some(host.id.clone()),
                name: Some(host.name.clone()),
                host: Some(host.host.clone()),
                port: Some(host.port),
                username: Some(host.username.clone()),
                identity_file: Some(host.identity_file.clone()),
                remote_path: Some(host.remote_path.clone()),
                password: Some(Secret::new(password)),
                passphrase: Some(Secret::new(passphrase)),
                trust_fingerprint: trust_fp,
                with_keys: Some(with_keys),
            };
            match engine.connect(body).await {
                Ok(val) => {
                    let _ = tx.send(BgEvent::Connected {
                        session: val,
                        target_mode,
                    });
                }
                Err(err) => {
                    let code = err.get("code").and_then(|v| v.as_str()).unwrap_or("");
                    let msg = err
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("连接失败")
                        .to_string();
                    if code == "HOSTKEY_UNKNOWN" || code == "HOSTKEY_CHANGED" {
                        let fp = err
                            .get("details")
                            .and_then(|d| {
                                d.get("fingerprint")
                                    .or_else(|| d.get("current"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("")
                            .to_string();
                        let title = if code == "HOSTKEY_CHANGED" {
                            "警告：远程主机密钥已变化！"
                        } else {
                            "首次连接：请确认服务器指纹"
                        };
                        let _ = tx.send(BgEvent::ConnectHostKeyPrompt {
                            host,
                            target_mode,
                            password: pw_clone,
                            passphrase: pp_clone,
                            title: title.to_string(),
                            message: msg,
                            fingerprint: fp,
                            danger: code == "HOSTKEY_CHANGED",
                        });
                    } else {
                        let _ = tx.send(BgEvent::Log(format!("连接失败：{msg}"), LogKind::Err));
                    }
                }
            }
        });
    }

    fn refresh_session(&mut self) {
        let Some(sid) = self.current_sid() else {
            return;
        };
        self.busy = true;
        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match engine.refresh(&sid).await {
                Ok(val) => {
                    let _ = tx.send(BgEvent::SessionUpdated(val));
                    let _ = tx.send(BgEvent::Log("已刷新服务器公钥列表".into(), LogKind::Ok));
                }
                Err(e) => {
                    let msg = e
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("刷新失败");
                    let _ = tx.send(BgEvent::Log(msg.to_string(), LogKind::Err));
                }
            }
        });
    }

    fn disconnect_session(&mut self) {
        let Some(sid) = self.current_sid() else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let _ = engine.disconnect(&sid).await;
            let _ = tx.send(BgEvent::Disconnected);
        });
    }

    fn load_sshd(&mut self, open_drawer: bool) {
        let Some(sid) = self.current_sid() else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Ok(info) = engine.sshd_status(&sid).await {
                let _ = tx.send(BgEvent::SshdLoaded { info, open_drawer });
            }
        });
    }

    fn open_pty(&mut self) {
        let Some(sid) = self.current_sid() else {
            return;
        };
        let (cols, rows) = (self.vt.cols(), self.vt.rows());
        self.vt = VtScreen::new(cols, rows);
        self.term_scroll_offset = 0;
        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match engine
                .term_open(json!({ "sid": sid, "cols": cols, "rows": rows }))
                .await
            {
                Ok(v) => {
                    let tid = v
                        .get("termId")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let c = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(cols as u64) as usize;
                    let r = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(rows as u64) as usize;
                    let _ = tx.send(BgEvent::TermOpened {
                        term_id: tid.clone(),
                        cols: c,
                        rows: r,
                    });
                    loop {
                        match engine.term_read(&tid, Some(150)).await {
                            Ok(chunk) => {
                                if let Some(b64) = chunk.get("data").and_then(|x| x.as_str()) {
                                    if !b64.is_empty() {
                                        if let Ok(bytes) = B64.decode(b64) {
                                            let _ = tx.send(BgEvent::TermOutput(bytes));
                                        }
                                    }
                                }
                                if chunk.get("closed").and_then(|x| x.as_bool()) == Some(true) {
                                    let _ = tx.send(BgEvent::TermClosed(None));
                                    break;
                                }
                            }
                            Err(e) => {
                                let msg = e
                                    .get("error")
                                    .and_then(|x| x.as_str())
                                    .map(|s| s.to_string());
                                let _ = tx.send(BgEvent::TermClosed(msg));
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    let msg = e
                        .get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("打开终端失败")
                        .to_string();
                    let _ = tx.send(BgEvent::TermClosed(Some(msg)));
                }
            }
        });
    }

    fn send_term_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Some(tid) = self.term_id.clone() else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let data = B64.encode(bytes);
        self.rt.spawn(async move {
            let _ = engine
                .term_write(json!({ "termId": tid, "data": data }))
                .await;
        });
    }

    fn current_sid(&self) -> Option<String> {
        self.session
            .as_ref()?
            .get("sid")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// 当前主机可辨识的名字（备注名优先，其次地址），用作私钥文件名前缀。
    fn current_host_label(&self) -> Option<String> {
        self.session
            .as_ref()
            .and_then(|s| s.get("host"))
            .and_then(|h| {
                h.get("name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .or_else(|| h.get("host").and_then(|v| v.as_str()).map(str::trim))
            })
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }

    /// 本次登录私钥所在目录，新增/更换密钥默认保存到这里。
    fn current_login_key_dir(&self) -> Option<String> {
        let file = self
            .session
            .as_ref()?
            .get("host")?
            .get("identityFile")?
            .as_str()?
            .trim();
        if file.is_empty() {
            return None;
        }
        std::path::Path::new(file)
            .parent()
            .map(|p| p.display().to_string())
            .filter(|p| !p.is_empty())
    }

    /// 新公钥注释默认值：用户名@服务器名-日期，同一天重复生成时带 -2、-3。
    fn default_new_key_comment(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let host = session.get("host")?;
        let username = host.get("username")?.as_str()?.trim();
        let server_name = host.get("name")?.as_str()?.trim();
        if username.is_empty() || server_name.is_empty() {
            return None;
        }
        let date: String = kw_ssh::util::shanghai_stamp(SystemTime::now())
            .chars()
            .take(8)
            .collect();
        let existing: Vec<String> = session
            .get("items")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.get("comment").and_then(|v| v.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Some(kw_server::keys_ops::default_key_comment(
            username,
            server_name,
            &date,
            &existing,
        ))
    }

    fn copy_to_clipboard(&mut self, text: &str, label: &str) {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text.to_string());
            self.push_log(format!("已复制{label}到剪贴板"), LogKind::Ok);
        }
    }

    fn read_from_clipboard(&self) -> String {
        arboard::Clipboard::new()
            .ok()
            .and_then(|mut cb| cb.get_text().ok())
            .unwrap_or_default()
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::Log(msg, kind) => {
                    self.busy = false;
                    self.push_log(msg, kind);
                }
                BgEvent::Connected {
                    session,
                    target_mode,
                } => {
                    self.busy = false;
                    let endpoint = session
                        .get("host")
                        .map(|h| {
                            format!(
                                "{}@{}:{}",
                                h.get("username").and_then(|v| v.as_str()).unwrap_or("root"),
                                h.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                                h.get("port").and_then(|v| v.as_u64()).unwrap_or(22)
                            )
                        })
                        .unwrap_or_default();
                    self.session = Some(session);
                    self.selected_indices.clear();
                    self.sshd_info = None;
                    self.push_log(format!("已连接 {endpoint}"), LogKind::Ok);
                    if target_mode == ViewMode::Term {
                        self.mode = ViewMode::Term;
                        self.open_pty();
                        self.term_focused = true;
                    } else {
                        self.mode = ViewMode::Keys;
                        self.load_sshd(false);
                    }
                }
                BgEvent::ConnectHostKeyPrompt {
                    host,
                    target_mode,
                    password,
                    passphrase,
                    title,
                    message,
                    fingerprint,
                    danger,
                } => {
                    self.busy = false;
                    self.drawer = Drawer::ConnectPrompt {
                        host,
                        target_mode,
                        password,
                        passphrase,
                        trust_fp: Some(fingerprint),
                        hostkey_warning: Some((title, message, danger)),
                    };
                }
                BgEvent::SessionUpdated(session) => {
                    self.busy = false;
                    self.session = Some(session);
                    self.selected_indices.clear();
                    self.reload_store_data();
                }
                BgEvent::DeleteNeedsForce {
                    fingerprints,
                    message,
                } => {
                    self.busy = false;
                    self.drawer = Drawer::ConfirmDelete {
                        fingerprints,
                        message,
                    };
                }
                BgEvent::Disconnected => {
                    self.busy = false;
                    self.session = None;
                    self.sshd_info = None;
                    self.term_id = None;
                    self.selected_indices.clear();
                    self.push_log("已断开 SSH 会话", LogKind::Info);
                }
                BgEvent::SshdLoaded { info, open_drawer } => {
                    self.busy = false;
                    self.sshd_info = Some(info.clone());
                    if open_drawer {
                        self.drawer = Drawer::Sshd { info };
                    }
                }
                BgEvent::BackupsLoaded(items) => {
                    self.busy = false;
                    self.drawer = Drawer::Backups {
                        items,
                        selected_name: String::new(),
                        diff: None,
                    };
                }
                BgEvent::BackupDiffLoaded(name, diff) => {
                    self.busy = false;
                    if let Drawer::Backups {
                        selected_name,
                        diff: d_slot,
                        ..
                    } = &mut self.drawer
                    {
                        *selected_name = name;
                        *d_slot = Some(diff);
                    }
                }
                BgEvent::SyncStatus(st) => {
                    self.busy = false;
                    self.ms_status = st;
                }
                BgEvent::SyncDeviceCode {
                    user_code,
                    verify_uri,
                } => {
                    self.busy = false;
                    self.push_log(
                        format!("请在浏览器输入微软设备码：{user_code}"),
                        LogKind::Info,
                    );
                    if let Drawer::Sync { device_prompt, .. } = &mut self.drawer {
                        *device_prompt = Some((user_code, verify_uri));
                    }
                }
                BgEvent::SyncDone { report, status } => {
                    self.busy = false;
                    self.ms_status = status;
                    self.reload_store_data();
                    let conflicts = report
                        .get("conflicts")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if !conflicts.is_empty() {
                        if let Drawer::Sync {
                            conflicts: c_slot, ..
                        } = &mut self.drawer
                        {
                            *c_slot = conflicts;
                        }
                        self.push_log("同步发现主机指纹冲突，请在抽屉中选择处理方式", LogKind::Warn);
                    } else {
                        self.push_log("微软 OneDrive 密钥库元数据同步完成", LogKind::Ok);
                    }
                }
                BgEvent::LocalScanDone(entries) => {
                    self.local_keys = entries;
                }
                BgEvent::TermOpened {
                    term_id,
                    cols,
                    rows,
                } => {
                    self.term_id = Some(term_id);
                    self.vt.resize(cols, rows);
                    self.last_sent_size = (cols, rows);
                    self.push_log(format!("SSH 终端已就绪 ({cols}×{rows})"), LogKind::Ok);
                }
                BgEvent::TermOutput(bytes) => {
                    self.vt.feed(&bytes);
                    self.term_scroll_offset = 0;
                    ctx.request_repaint();
                }
                BgEvent::TermClosed(err) => {
                    self.term_id = None;
                    if let Some(msg) = err {
                        self.push_log(format!("终端已关闭：{msg}"), LogKind::Warn);
                    } else {
                        self.push_log("SSH 终端会话已结束", LogKind::Info);
                    }
                }
            }
        }
    }
}

impl eframe::App for KwDesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);

        if self.startup_frames < 5 {
            self.startup_frames += 1;
            if self.startup_frames == 1 && std::env::var_os("KW_OPEN_HOST_MANAGER").is_some() {
                self.open_host_edit(self.hosts.first().cloned());
            }
            if self.startup_frames == 3 && std::env::var_os("KW_SCREENSHOT_PATH").is_some() {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
            }
            if self.startup_frames == 4 {
                trim_working_set();
            } else {
                ctx.request_repaint_after(Duration::from_millis(80));
            }
        }

        if let Some(shot_path) = std::env::var_os("KW_SCREENSHOT_PATH") {
            ctx.input(|i| {
                for ev in &i.events {
                    if let Event::Screenshot { image, .. } = ev {
                        let _ = image::save_buffer(
                            &shot_path,
                            image.as_raw(),
                            image.width() as u32,
                            image.height() as u32,
                            image::ColorType::Rgba8,
                        );
                    }
                }
            });
        }

        // 定期请求重绘以便后台任务与终端输出实时刷新
        if self.busy || self.term_id.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }

        // 最小化时主动回收物理内存工作集，未最大化时定期持久化窗口坐标尺寸
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if minimized && !self.was_minimized {
            trim_working_set();
        }
        self.was_minimized = minimized;

        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !minimized && !maximized && self.last_bounds_save.elapsed() > Duration::from_secs(2) {
            self.last_bounds_save = Instant::now();
            ctx.input(|i| {
                let vp = i.viewport();
                if let (Some(outer), Some(inner)) = (vp.outer_rect, vp.inner_rect) {
                    let scale = vp.native_pixels_per_point.unwrap_or(1.0) as f64;
                    save_bounds(
                        &self.data_dir,
                        Bounds {
                            x: (outer.min.x as f64 * scale).round() as i32,
                            y: (outer.min.y as f64 * scale).round() as i32,
                            width: (inner.width() as f64 * scale).round() as u32,
                            height: (inner.height() as f64 * scale).round() as u32,
                            scale,
                        },
                    );
                }
            });
        }

        self.render_topbar(ctx, maximized);
        self.render_bottom_bar(ctx);
        self.render_sidebar(ctx);

        if self.show_local_panel && self.mode == ViewMode::Keys {
            self.render_local_panel(ctx);
        }

        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(self.palette.bg)
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| match self.mode {
                ViewMode::Keys => self.render_keys_canvas(ui),
                ViewMode::Term => self.render_term_canvas(ui, ctx),
            });

        self.render_drawer(ctx);
    }
}

impl KwDesktopApp {
    fn render_topbar(&mut self, ctx: &egui::Context, maximized: bool) {
        let p = self.palette;
        egui::TopBottomPanel::top("kw_topbar")
            .exact_height(44.0)
            .frame(
                Frame::NONE
                    .fill(p.bg_elev)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| {
                let bar_rect = ui.max_rect();
                ui.horizontal_centered(|ui| {
                    // 品牌矢量钥匙 Logo 与标题
                    let (logo_rect, _) =
                        ui.allocate_exact_size(Vec2::new(26.0, 26.0), Sense::hover());
                    ui.painter()
                        .rect_filled(logo_rect, CornerRadius::same(7), p.accent_soft);
                    ui.painter().rect_stroke(
                        logo_rect,
                        CornerRadius::same(7),
                        Stroke::new(1.0_f32, p.accent.gamma_multiply(0.45)),
                        StrokeKind::Inside,
                    );
                    draw_icon(ui.painter(), logo_rect.center(), 14.0, Icon::Key, p.accent);

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("SSH Keywarden")
                            .size(13.5)
                            .strong()
                            .color(p.text),
                    );
                    ui.add_space(6.0);

                    // 固定高度版本胶囊（避免在 horizontal_centered 下被纵向拉伸）
                    let ver = format!("v{}", env!("CARGO_PKG_VERSION").replace('-', "."));
                    let (ver_rect, _) =
                        ui.allocate_exact_size(Vec2::new(84.0, 20.0), Sense::hover());
                    ui.painter()
                        .rect_filled(ver_rect, CornerRadius::same(10), p.surface3);
                    ui.painter().rect_stroke(
                        ver_rect,
                        CornerRadius::same(10),
                        Stroke::new(1.0_f32, p.border),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        ver_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        ver,
                        FontId::monospace(10.5),
                        p.dim,
                    );

                    if self.busy {
                        ui.add_space(8.0);
                        ui.spinner();
                    }

                    // 右侧功能按钮与原生窗口控制按钮
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;

                        if Self::win_ctrl_btn(ui, &p, Icon::WinClose, true)
                            .on_hover_text("关闭")
                            .clicked()
                        {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }

                        let max_ico = if maximized {
                            Icon::WinRestore
                        } else {
                            Icon::WinMax
                        };
                        if Self::win_ctrl_btn(ui, &p, max_ico, false)
                            .on_hover_text(if maximized { "向下还原" } else { "最大化" })
                            .clicked()
                        {
                            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                        }

                        if Self::win_ctrl_btn(ui, &p, Icon::WinMin, false)
                            .on_hover_text("最小化")
                            .clicked()
                        {
                            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                        }

                        // 细分隔竖线
                        let (sep_r, _) =
                            ui.allocate_exact_size(Vec2::new(8.0, 18.0), Sense::hover());
                        ui.painter().line_segment(
                            [
                                Pos2::new(sep_r.center().x, sep_r.min.y),
                                Pos2::new(sep_r.center().x, sep_r.max.y),
                            ],
                            Stroke::new(1.0_f32, p.border),
                        );

                        if Self::top_icon_btn(ui, &p, Some(Icon::Settings), "设置", false, false)
                            .on_hover_text("外观主题与默认路径设置")
                            .clicked()
                        {
                            self.open_settings_drawer();
                        }

                        // 一键切换主题配色按钮
                        let theme_short = match self.palette.id {
                            "obsidian" => "曜石",
                            "midnight" => "午夜",
                            "graphite" => "石墨",
                            "cloud" => "云白",
                            "linen" => "亚麻",
                            "mist" => "雾青",
                            _ => "主题",
                        };
                        if Self::top_icon_btn(ui, &p, Some(Icon::Theme), theme_short, false, false)
                            .on_hover_text("点击切换主题配色（曜石 / 午夜 / 石墨 / 云白 / 亚麻 / 雾青）")
                            .clicked()
                        {
                            let idx = PALETTES
                                .iter()
                                .position(|x| x.id == self.palette.id)
                                .unwrap_or(0);
                            let next_p = PALETTES[(idx + 1) % PALETTES.len()];
                            self.theme_choice = next_p.id.to_string();
                            self.palette = next_p;
                            apply_palette(ctx, &self.palette);
                            let _ = save_theme(&self.data_dir, &self.theme_choice);
                        }

                        let ms_logged_in = self
                            .ms_status
                            .get("loggedIn")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if Self::top_icon_btn(
                            ui,
                            &p,
                            Some(Icon::Cloud),
                            "同步",
                            ms_logged_in,
                            ms_logged_in,
                        )
                        .on_hover_text("微软账号加密云同步")
                        .clicked()
                        {
                            self.open_sync_drawer();
                        }

                        if Self::top_icon_btn(ui, &p, Some(Icon::Server), "管理主机", false, false)
                            .on_hover_text("新增、编辑或删除服务器配置")
                            .clicked()
                        {
                            self.open_host_edit(self.hosts.first().cloned());
                        }

                        if self.mode == ViewMode::Keys {
                            if Self::top_icon_btn(
                                ui,
                                &p,
                                Some(Icon::Folder),
                                "本机密钥",
                                self.show_local_panel,
                                false,
                            )
                            .on_hover_text("展开/收起右侧本机私钥库对照面板")
                            .clicked()
                            {
                                self.show_local_panel = !self.show_local_panel;
                            }
                        }
                    });
                });

                // 标题栏空白区域支持拖拽与双击最大化
                let pointer_pos = ui.input(|i| i.pointer.interact_pos());
                if let Some(pos) = pointer_pos {
                    if bar_rect.contains(pos) && !ui.ctx().is_using_pointer() {
                        if ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary))
                        {
                            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                        } else if ui
                            .input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
                        {
                            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                        }
                    }
                }
            });
    }

    fn win_ctrl_btn(ui: &mut egui::Ui, p: &Palette, icon: Icon, is_close: bool) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 28.0), Sense::click());
        let hovered = resp.hovered();
        if hovered {
            let bg = if is_close { p.danger } else { p.surface3 };
            ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let fg = if hovered && is_close {
            Color32::WHITE
        } else if hovered {
            p.text
        } else {
            p.dim
        };
        draw_icon(ui.painter(), rect.center(), 11.0, icon, fg);
        resp
    }

    fn top_icon_btn(
        ui: &mut egui::Ui,
        p: &Palette,
        icon: Option<Icon>,
        text: &str,
        active: bool,
        dot: bool,
    ) -> egui::Response {
        let font_id = FontId::proportional(12.0);
        let text_w = ui.fonts(|f| {
            f.layout_no_wrap(text.to_string(), font_id.clone(), p.text)
                .size()
                .x
        });
        let ico_space = if icon.is_some() { 18.0 } else { 0.0 };
        let dot_space = if dot { 10.0 } else { 0.0 };
        let w = (text_w + ico_space + dot_space + 20.0).max(34.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
        let hovered = resp.hovered();

        let fill = if active {
            p.accent_soft
        } else if hovered {
            p.panel
        } else {
            p.surface3
        };
        let border_col = if active || hovered { p.accent } else { p.border };
        let fg = if active { p.accent } else { p.text };

        ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.0_f32, border_col),
            StrokeKind::Inside,
        );

        let mut cur_x = rect.min.x + 10.0;
        if let Some(ic) = icon {
            draw_icon(
                ui.painter(),
                Pos2::new(cur_x + 6.0, rect.center().y),
                12.5,
                ic,
                if active { p.accent } else { p.dim },
            );
            cur_x += 18.0;
        }
        ui.painter().text(
            Pos2::new(cur_x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            font_id,
            fg,
        );
        if dot {
            ui.painter().circle_filled(
                Pos2::new(rect.max.x - 9.0, rect.center().y),
                3.0,
                p.ok,
            );
        }
        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        resp
    }

    fn top_tool_btn(ui: &mut egui::Ui, p: &Palette, text: &str, active: bool) -> egui::Response {
        Self::top_icon_btn(ui, p, None, text, active, false)
    }

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        egui::SidePanel::left("kw_sidebar")
            .exact_width(172.0)
            .resizable(false)
            .frame(
                Frame::NONE
                    .fill(p.bg_elev)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(10, 12)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                // 侧栏只保留「主机与密钥」；添加主机与终端入口都在主机与密钥页内
                if Self::nav_item(
                    ui,
                    &p,
                    Icon::Server,
                    "主机与密钥",
                    self.mode == ViewMode::Keys && matches!(self.drawer, Drawer::None),
                )
                .clicked()
                {
                    self.drawer = Drawer::None;
                    self.switch_mode(ViewMode::Keys);
                }

                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    if Self::nav_item(
                        ui,
                        &p,
                        Icon::Settings,
                        "全局设置",
                        matches!(self.drawer, Drawer::Settings { .. }),
                    )
                    .clicked()
                    {
                        self.open_settings_drawer();
                    }

                    if Self::nav_item(
                        ui,
                        &p,
                        Icon::Cloud,
                        "云端同步",
                        matches!(self.drawer, Drawer::Sync { .. }),
                    )
                    .clicked()
                    {
                        self.open_sync_drawer();
                    }
                });
            });
    }

    fn nav_item(
        ui: &mut egui::Ui,
        p: &Palette,
        icon: Icon,
        label: &str,
        active: bool,
    ) -> egui::Response {
        let desired = Vec2::new(ui.available_width(), 36.0);
        let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
        let hovered = resp.hovered();
        if active {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(8), p.accent_soft);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(8),
                Stroke::new(1.0_f32, p.accent.gamma_multiply(0.35)),
                StrokeKind::Inside,
            );
            let bar = Rect::from_min_size(
                Pos2::new(rect.min.x, rect.center().y - 9.0),
                Vec2::new(3.0, 18.0),
            );
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), p.accent);
        } else if hovered {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(8), p.surface3);
        }
        let fg = if active || hovered { p.text } else { p.dim };
        let ico_fg = if active { p.accent } else { fg };
        draw_icon(
            ui.painter(),
            Pos2::new(rect.min.x + 18.0, rect.center().y),
            14.0,
            icon,
            ico_fg,
        );
        ui.painter().text(
            Pos2::new(rect.min.x + 36.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            fg,
        );
        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        resp
    }

    fn render_bottom_bar(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        let height = if self.show_log_drawer { 145.0 } else { 30.0 };
        egui::TopBottomPanel::bottom("kw_logbar")
            .exact_height(height)
            .frame(
                Frame::NONE
                    .fill(p.bg_elev)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(12, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let connected = self.session.is_some();
                    let dot_color = if connected { p.ok } else { p.faint };
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 3.5, dot_color);

                    let conn_str = if let Some(sess) = &self.session {
                        let h = sess.get("host");
                        format!(
                            "已连接 {}@{}:{}",
                            h.and_then(|x| x.get("username"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("root"),
                            h.and_then(|x| x.get("host"))
                                .and_then(|x| x.as_str())
                                .unwrap_or(""),
                            h.and_then(|x| x.get("port"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(22)
                        )
                    } else {
                        "未连接".to_string()
                    };
                    ui.label(
                        RichText::new(conn_str)
                            .size(11.5)
                            .strong()
                            .color(if connected { p.ok } else { p.dim }),
                    );
                    ui.separator();

                    let latest = self
                        .logs
                        .last()
                        .map(|l| format!("[{}] {}", l.time, l.text))
                        .unwrap_or_default();
                    ui.label(RichText::new(latest).size(11.5).monospace().color(p.dim));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let toggle_lbl = if self.show_log_drawer {
                            "▼ 收起日志"
                        } else {
                            "▲ 展开日志"
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(toggle_lbl).size(11.0).color(p.dim))
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::NONE),
                            )
                            .clicked()
                        {
                            self.show_log_drawer = !self.show_log_drawer;
                        }
                        if self.show_log_drawer
                            && ui
                                .add(
                                    egui::Button::new(RichText::new("清空").size(11.0).color(p.dim))
                                        .fill(Color32::TRANSPARENT)
                                        .stroke(Stroke::NONE),
                                )
                                .clicked()
                        {
                            self.logs.clear();
                        }
                    });
                });

                if self.show_log_drawer {
                    ui.separator();
                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for item in &self.logs {
                                let col = match item.kind {
                                    LogKind::Info => p.dim,
                                    LogKind::Ok => p.ok,
                                    LogKind::Warn => p.warn,
                                    LogKind::Err => p.danger,
                                };
                                ui.label(
                                    RichText::new(format!("[{}] {}", item.time, item.text))
                                        .size(11.5)
                                        .monospace()
                                        .color(col),
                                );
                            }
                        });
                }
            });
    }

    fn render_keys_canvas(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        let connected = self.session.is_some();

        if !connected {
            // 页头不显示标题；添加入口在服务器保险库工具栏
            self.render_vault_grid(ui, true);
            return;
        }

        // 已连接：路径与 SSHD 状态 + 远端密钥操作
        Frame::NONE
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(11))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if let Some(sess) = &self.session {
                        let path = sess
                            .get("remotePath")
                            .and_then(|v| v.as_str())
                            .unwrap_or("~/.ssh/authorized_keys");
                        ui.label(RichText::new(path).size(11.5).monospace().color(p.dim));
                    }
                    if let Some(sshd) = &self.sshd_info {
                        let key_only = sshd
                            .get("keyOnly")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let (badge_txt, badge_col) = if key_only {
                            ("SSHD: 仅密钥登录", p.ok)
                        } else {
                            ("SSHD: 密码登录已开启", p.warn)
                        };
                        Frame::NONE
                            .fill(badge_col.gamma_multiply(0.14))
                            .stroke(Stroke::new(1.0_f32, badge_col.gamma_multiply(0.45)))
                            .corner_radius(CornerRadius::same(99))
                            .inner_margin(Margin::symmetric(8, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(badge_txt)
                                        .size(11.0)
                                        .strong()
                                        .color(badge_col),
                                );
                            });
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if Self::action_btn(ui, &p, "断开", false, true).clicked() {
                            self.disconnect_session();
                        }
                        if Self::action_btn(ui, &p, "＋ 添加公钥", true, false).clicked() {
                            self.drawer = Drawer::AddKey {
                                tab: 0,
                                algo: "ed25519".to_string(),
                                label: self
                                    .current_host_label()
                                    .unwrap_or_else(|| "key".to_string()),
                                comment: self.default_new_key_comment().unwrap_or_default(),
                                passphrase: String::new(),
                                selected_local_pub: String::new(),
                                pasted_pub: String::new(),
                            };
                        }
                        if Self::action_btn(ui, &p, ">_ 打开终端", false, false).clicked() {
                            self.switch_mode(ViewMode::Term);
                        }
                        if Self::action_btn(ui, &p, "SSHD 安全", false, false).clicked() {
                            self.load_sshd(true);
                        }
                        if Self::action_btn(ui, &p, "历史备份", false, false).clicked() {
                            self.open_backups_drawer();
                        }
                        if Self::action_btn(ui, &p, "↻ 刷新", false, false).clicked() {
                            self.refresh_session();
                        }
                    });
                });
            });

        ui.add_space(10.0);

        // 已连接：渲染搜索栏、筛选按钮与远端密钥列表
        // 服务端 sessionPayload 的字段名是 items / usedKeyFp
        let keys_arr = self
            .session
            .as_ref()
            .and_then(|s| s.get("items"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let active_fp = self
            .session
            .as_ref()
            .and_then(|s| s.get("usedKeyFp"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut fp_counts: HashMap<String, usize> = HashMap::new();
        for k in &keys_arr {
            if let Some(fp) = k.get("fingerprint").and_then(|v| v.as_str()) {
                if !fp.is_empty() {
                    *fp_counts.entry(fp.to_string()).or_insert(0) += 1;
                }
            }
        }

        let local_fps: HashSet<String> = self
            .local_keys
            .iter()
            .filter(|lk| !lk.fingerprint.is_empty())
            .map(|lk| lk.fingerprint.clone())
            .collect();

        ui.horizontal(|ui| {
            let (s_rect, _) = ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
            draw_icon(ui.painter(), s_rect.center(), 13.0, Icon::Search, p.dim);
            ui.add(
                egui::TextEdit::singleline(&mut self.key_search)
                    .hint_text("搜索注释、指纹、类型或备注…")
                    .desired_width(280.0),
            );
            ui.add_space(6.0);
            for (kf, lbl) in [
                (KeyFilter::All, "全部"),
                (KeyFilter::Unmatched, "本机无匹配"),
                (KeyFilter::Dup, "重复指纹"),
                (KeyFilter::Used, "当前会话"),
            ] {
                if Self::top_tool_btn(ui, &p, lbl, self.key_filter == kf).clicked() {
                    self.key_filter = kf;
                }
            }

            ui.separator();
            ui.label(
                RichText::new(format!(
                    "共 {} 把公钥 · 已选 {} 项",
                    keys_arr.len(),
                    self.selected_indices.len()
                ))
                .size(12.0)
                .color(p.dim),
            );

            if !self.selected_indices.is_empty() {
                if Self::action_btn(ui, &p, "批量删除所选", false, true).clicked() {
                    self.delete_selected_keys();
                }
                if ui
                    .button(RichText::new("取消全选").size(11.5).color(p.dim))
                    .clicked()
                {
                    self.selected_indices.clear();
                }
            }
        });

        ui.add_space(8.0);

        let q = self.key_search.trim().to_lowercase();
        let kf = self.key_filter;
        let filtered: Vec<Value> = keys_arr
            .into_iter()
            .filter(|k| {
                let fp = k
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let matched = !fp.is_empty() && local_fps.contains(fp);
                let is_dup = fp_counts.get(fp).copied().unwrap_or(0) > 1;
                let is_used = !active_fp.is_empty() && fp == active_fp;

                match kf {
                    KeyFilter::All => {}
                    KeyFilter::Unmatched => {
                        if matched {
                            return false;
                        }
                    }
                    KeyFilter::Dup => {
                        if !is_dup {
                            return false;
                        }
                    }
                    KeyFilter::Used => {
                        if !is_used {
                            return false;
                        }
                    }
                }

                if q.is_empty() {
                    return true;
                }
                let note = self.key_notes.get(fp).map(|s| s.as_str()).unwrap_or("");
                let hay = format!(
                    "{} {} {} {}",
                    k.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                    fp,
                    k.get("comment").and_then(|v| v.as_str()).unwrap_or(""),
                    note
                )
                .to_lowercase();
                hay.contains(&q)
            })
            .collect();

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if filtered.is_empty() {
                    Frame::NONE
                        .fill(p.panel)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(20, 28))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new("没有匹配的远端公钥条目")
                                        .size(13.5)
                                        .strong()
                                        .color(p.dim),
                                );
                            });
                        });
                    return;
                }

                for k in filtered {
                    let idx = k.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                    let key_type = k
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ssh-key")
                        .to_string();
                    let fp = k
                        .get("fingerprint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let comment = k
                        .get("comment")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let note = self.key_notes.get(&fp).cloned().unwrap_or_default();
                    let is_used = !active_fp.is_empty() && fp == active_fp;
                    let is_dup = fp_counts.get(&fp).copied().unwrap_or(0) > 1;
                    let local_match_name = self
                        .local_keys
                        .iter()
                        .find(|lk| !fp.is_empty() && lk.fingerprint == fp)
                        .map(|lk| lk.name.clone());

                    let is_selected = self.selected_indices.contains(&idx);
                    let stroke_col = if is_selected { p.accent } else { p.border };

                    Frame::NONE
                        .fill(p.panel)
                        .stroke(Stroke::new(1.0_f32, stroke_col))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let mut checked = is_selected;
                                if ui.checkbox(&mut checked, "").changed() {
                                    if checked {
                                        self.selected_indices.insert(idx);
                                    } else {
                                        self.selected_indices.remove(&idx);
                                    }
                                }

                                ui.label(
                                    RichText::new(format!("#{}", idx + 1))
                                        .size(11.5)
                                        .monospace()
                                        .color(p.faint),
                                );

                                let badge_col = if key_type.contains("ed25519") {
                                    p.ok
                                } else if key_type.contains("rsa") {
                                    p.accent
                                } else {
                                    p.warn
                                };
                                Frame::NONE
                                    .fill(badge_col.gamma_multiply(0.14))
                                    .stroke(Stroke::new(1.0_f32, badge_col.gamma_multiply(0.4)))
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::symmetric(7, 2))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(&key_type)
                                                .size(11.0)
                                                .monospace()
                                                .strong()
                                                .color(badge_col),
                                        );
                                    });

                                if is_used {
                                    Frame::NONE
                                        .fill(p.ok.gamma_multiply(0.15))
                                        .stroke(Stroke::new(1.0_f32, p.ok.gamma_multiply(0.45)))
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(7, 2))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("● 当前会话")
                                                    .size(11.0)
                                                    .strong()
                                                    .color(p.ok),
                                            );
                                        });
                                }

                                if is_dup {
                                    Frame::NONE
                                        .fill(p.warn.gamma_multiply(0.15))
                                        .stroke(Stroke::new(1.0_f32, p.warn.gamma_multiply(0.45)))
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(7, 2))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("⚠ 重复")
                                                    .size(11.0)
                                                    .strong()
                                                    .color(p.warn),
                                            );
                                        });
                                }

                                if let Some(lname) = &local_match_name {
                                    Frame::NONE
                                        .fill(p.accent_soft)
                                        .stroke(Stroke::new(1.0_f32, p.accent.gamma_multiply(0.4)))
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(7, 2))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(format!("本机: {lname}"))
                                                    .size(11.0)
                                                    .color(p.accent),
                                            );
                                        });
                                }

                                if !comment.is_empty() {
                                    ui.label(
                                        RichText::new(&comment)
                                            .size(13.0)
                                            .strong()
                                            .color(p.text),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new("(无注释)").size(12.5).color(p.faint),
                                    );
                                }

                                if !note.is_empty() {
                                    Frame::NONE
                                        .fill(p.surface3)
                                        .corner_radius(CornerRadius::same(5))
                                        .inner_margin(Margin::symmetric(6, 2))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(format!("备注: {note}"))
                                                    .size(11.5)
                                                    .color(p.dim),
                                            );
                                        });
                                }

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if Self::action_btn(ui, &p, "删除", false, true).clicked() {
                                        self.selected_indices.clear();
                                        self.selected_indices.insert(idx);
                                        self.delete_selected_keys();
                                    }
                                    if Self::action_btn(ui, &p, "更换密钥", false, false).clicked()
                                    {
                                        self.drawer = Drawer::ReplaceKey {
                                            old_index: idx,
                                            old_comment: comment.clone(),
                                            old_fp: fp.clone(),
                                            tab: 0,
                                            algo: "ed25519".to_string(),
                                            label: self.current_host_label().unwrap_or_else(|| {
                                                if comment.is_empty() {
                                                    "replaced".to_string()
                                                } else {
                                                    comment.clone()
                                                }
                                            }),
                                            comment: self
                                                .default_new_key_comment()
                                                .unwrap_or_else(|| comment.clone()),
                                            passphrase: String::new(),
                                            selected_priv_path: String::new(),
                                        };
                                    }
                                    if Self::action_btn(ui, &p, "备注", false, false).clicked() {
                                        self.drawer = Drawer::EditNote {
                                            fingerprint: fp.clone(),
                                            comment: comment.clone(),
                                            note: note.clone(),
                                        };
                                    }
                                });
                            });

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(format!("复制指纹: {fp}"))
                                                .size(11.5)
                                                .monospace()
                                                .color(p.dim),
                                        )
                                        .fill(p.surface2)
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(5)),
                                    )
                                    .on_hover_text("点击复制指纹")
                                    .clicked()
                                {
                                    self.copy_to_clipboard(&fp, "公钥指纹");
                                }
                            });
                        });
                    ui.add_space(6.0);
                }
            });
    }

    fn delete_selected_keys(&mut self) {
        if self.selected_indices.is_empty() {
            return;
        }
        let items: Vec<Value> = self
            .session
            .as_ref()
            .and_then(|s| s.get("items"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut indices: Vec<i64> = self.selected_indices.iter().copied().collect();
        indices.sort_unstable();

        // 服务端按指纹删除，先把选中的行号映射成指纹
        let mut fps: Vec<String> = Vec::new();
        for idx in indices {
            let fp = items
                .iter()
                .find(|k| k.get("index").and_then(|v| v.as_i64()) == Some(idx))
                .and_then(|k| k.get("fingerprint"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !fp.is_empty() && !fps.iter().any(|x| x == fp) {
                fps.push(fp.to_string());
            }
        }
        if fps.is_empty() {
            self.push_log("未找到所选公钥的指纹，请先刷新列表", LogKind::Err);
            return;
        }
        self.delete_keys_by_fingerprints(fps, false);
    }

    fn delete_keys_by_fingerprints(&mut self, fingerprints: Vec<String>, force: bool) {
        let Some(sid) = self.current_sid() else {
            return;
        };
        self.busy = true;
        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let mut body = json!({
                "sid": sid,
                "fingerprints": fingerprints.clone(),
            });
            if force {
                body["force"] = json!(true);
            }
            match engine.delete_keys(body).await {
                Ok(val) => {
                    let changed = val
                        .get("changed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let needs_force = val
                        .get("needsForce")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if changed {
                        if let Some(sess) = val.get("session") {
                            let _ = tx.send(BgEvent::SessionUpdated(sess.clone()));
                        }
                        let _ = tx.send(BgEvent::Log("已删除所选远端公钥".into(), LogKind::Ok));
                    } else if needs_force {
                        let message = val
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let _ = tx.send(BgEvent::DeleteNeedsForce {
                            fingerprints,
                            message,
                        });
                    } else {
                        let message = val
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("删除未生效");
                        let _ = tx.send(BgEvent::Log(message.to_string(), LogKind::Warn));
                    }
                }
                Err(e) => {
                    let msg = e
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("删除公钥失败");
                    let _ = tx.send(BgEvent::Log(msg.to_string(), LogKind::Err));
                }
            }
        });
    }

    fn render_vault_grid(&mut self, ui: &mut egui::Ui, allow_add_card: bool) {
        let p = self.palette;
        let avail_w = (ui.available_width() - 4.0).max(260.0);

        let mut add_clicked = false;
        ui.horizontal(|ui| {
            ui.set_width(avail_w);
            ui.label(
                RichText::new("服务器保险库 (HOSTS)")
                    .size(12.0)
                    .strong()
                    .color(p.dim),
            );
            ui.label(
                RichText::new(format!("({})", self.hosts.len()))
                    .size(12.0)
                    .monospace()
                    .color(p.faint),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !self.hosts.is_empty() {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.vault_search)
                            .hint_text("筛选主机名称、IP 或用户名…")
                            .desired_width(240.0),
                    );
                    let (s_rect, _) =
                        ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
                    draw_icon(ui.painter(), s_rect.center(), 13.0, Icon::Search, p.dim);
                }
                if allow_add_card
                    && Self::action_btn(ui, &p, "＋ 添加主机", true, false).clicked()
                {
                    add_clicked = true;
                }
            });
        });
        if add_clicked {
            self.open_host_edit(None);
        }

        ui.add_space(10.0);

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.render_host_grid(ui, allow_add_card);
            });
    }

    fn render_host_grid(&mut self, ui: &mut egui::Ui, allow_add_card: bool) {
        let p = self.palette;
        let avail_w = (ui.available_width() - 4.0).max(260.0);

        let q = self.vault_search.trim().to_lowercase();
        let filtered: Vec<HostEntry> = self
            .hosts
            .iter()
            .filter(|h| {
                if q.is_empty() {
                    return true;
                }
                let hay = format!("{} {} {}", h.name, h.host, h.username).to_lowercase();
                hay.contains(&q)
            })
            .cloned()
            .collect();

        let gap = 14.0;
        let cols = ((avail_w + gap) / (286.0 + gap)).floor().clamp(1.0, 4.0) as usize;
        let card_w =
            ((avail_w - (cols.saturating_sub(1) as f32) * gap) / (cols as f32)).floor().max(220.0);
        let card_h = 108.0;

        let total_items = filtered.len();
        if total_items == 0 {
            if allow_add_card && self.hosts.is_empty() {
                // 空库：全宽添加卡
                let (add_rect, add_resp) =
                    ui.allocate_exact_size(Vec2::new(avail_w, 108.0), Sense::click());
                let hovered = add_resp.hovered();
                let border_col = if hovered {
                    p.accent
                } else {
                    p.accent.gamma_multiply(0.45)
                };
                let fill_col = if hovered { p.accent_soft } else { p.panel };
                ui.painter()
                    .rect_filled(add_rect, CornerRadius::same(11), fill_col);
                ui.painter().rect_stroke(
                    add_rect,
                    CornerRadius::same(11),
                    Stroke::new(1.0_f32, border_col),
                    StrokeKind::Inside,
                );
                let plus_circle =
                    Pos2::new(add_rect.center().x - 42.0, add_rect.center().y - 10.0);
                ui.painter()
                    .circle_filled(plus_circle, 13.0, p.accent.gamma_multiply(0.18));
                draw_icon(ui.painter(), plus_circle, 13.0, Icon::Plus, p.accent);
                ui.painter().text(
                    Pos2::new(add_rect.center().x - 22.0, add_rect.center().y - 10.0),
                    egui::Align2::LEFT_CENTER,
                    "添加服务器",
                    FontId::proportional(14.0),
                    p.accent,
                );
                ui.painter().text(
                    Pos2::new(add_rect.center().x, add_rect.center().y + 18.0),
                    egui::Align2::CENTER_CENTER,
                    "保存 IP、端口与私钥路径",
                    FontId::proportional(11.5),
                    p.dim,
                );
                if hovered {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if add_resp.clicked() {
                    self.open_host_edit(None);
                }
                return;
            }
            let (empty_rect, _) =
                ui.allocate_exact_size(Vec2::new(avail_w, 96.0), Sense::hover());
            ui.painter()
                .rect_filled(empty_rect, CornerRadius::same(11), p.panel);
            ui.painter().rect_stroke(
                empty_rect,
                CornerRadius::same(11),
                Stroke::new(1.0_f32, p.border),
                StrokeKind::Inside,
            );
            ui.painter().text(
                Pos2::new(empty_rect.center().x, empty_rect.center().y - 10.0),
                egui::Align2::CENTER_CENTER,
                "暂无匹配的服务器卡片",
                FontId::proportional(13.5),
                p.dim,
            );
            let hint = if self.hosts.is_empty() {
                "切换到「主机与密钥」页，点击「＋ 添加主机」新增服务器配置"
            } else {
                "试试清空筛选条件"
            };
            ui.painter().text(
                Pos2::new(empty_rect.center().x, empty_rect.center().y + 12.0),
                egui::Align2::CENTER_CENTER,
                hint,
                FontId::proportional(12.0),
                p.faint,
            );
            return;
        }

        let mut pending_edit: Option<Option<HostEntry>> = None;
        let mut pending_connect: Option<(HostEntry, ViewMode)> = None;

        let mut row_idx = 0;
        while row_idx * cols < total_items {
            let (row_rect, _) =
                ui.allocate_exact_size(Vec2::new(avail_w, card_h), Sense::hover());

            for c in 0..cols {
                let item_i = row_idx * cols + c;
                if item_i >= total_items {
                    break;
                }
                let x0 = row_rect.min.x + (c as f32) * (card_w + gap);
                let card_rect =
                    Rect::from_min_size(Pos2::new(x0, row_rect.min.y), Vec2::new(card_w, card_h));

                if item_i < filtered.len() {
                    let h = filtered[item_i].clone();
                    let name = if h.name.trim().is_empty() {
                        h.host.clone()
                    } else {
                        h.name.clone()
                    };
                    let first_ch = name
                        .chars()
                        .next()
                        .unwrap_or('S')
                        .to_uppercase()
                        .to_string();

                    // 子按钮矩形定义（绝对坐标，杜绝任何水平/垂直挤压重叠）
                    let av_rect = Rect::from_min_size(
                        Pos2::new(card_rect.min.x + 14.0, card_rect.min.y + 13.0),
                        Vec2::new(38.0, 38.0),
                    );
                    let edit_rect = Rect::from_min_size(
                        Pos2::new(card_rect.max.x - 66.0, card_rect.min.y + 13.0),
                        Vec2::new(52.0, 24.0),
                    );
                    let term_rect = Rect::from_min_size(
                        Pos2::new(card_rect.max.x - 84.0, card_rect.max.y - 35.0),
                        Vec2::new(70.0, 26.0),
                    );
                    let keys_rect = Rect::from_min_size(
                        Pos2::new(card_rect.max.x - 160.0, card_rect.max.y - 35.0),
                        Vec2::new(70.0, 26.0),
                    );

                    let edit_resp = ui
                        .interact(edit_rect, ui.id().with(("card_edit", item_i)), Sense::click())
                        .on_hover_text("编辑此服务器配置");
                    let term_resp = ui
                        .interact(term_rect, ui.id().with(("card_term", item_i)), Sense::click())
                        .on_hover_text("直连打开交互式 SSH 终端");
                    let keys_resp = ui
                        .interact(keys_rect, ui.id().with(("card_keys", item_i)), Sense::click())
                        .on_hover_text("连接并管理远端 authorized_keys");

                    let card_hovered = ui.rect_contains_pointer(card_rect);
                    let border_col = if card_hovered {
                        p.accent.gamma_multiply(0.65)
                    } else {
                        p.border
                    };

                    // 1. 卡片背景与边框
                    ui.painter()
                        .rect_filled(card_rect, CornerRadius::same(11), p.panel);
                    ui.painter().rect_stroke(
                        card_rect,
                        CornerRadius::same(11),
                        Stroke::new(1.0_f32, border_col),
                        StrokeKind::Inside,
                    );

                    // 2. 左侧首字母头像方块
                    ui.painter()
                        .rect_filled(av_rect, CornerRadius::same(9), p.accent_soft);
                    ui.painter().rect_stroke(
                        av_rect,
                        CornerRadius::same(9),
                        Stroke::new(1.0_f32, p.accent.gamma_multiply(0.4)),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        av_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &first_ch,
                        FontId::proportional(15.5),
                        p.accent,
                    );

                    // 3. 主机名称与 user@host:port（带文字裁剪区，绝不越界覆盖右侧编辑图标）
                    let text_clip = Rect::from_min_max(
                        Pos2::new(av_rect.max.x + 10.0, card_rect.min.y + 10.0),
                        Pos2::new(edit_rect.min.x - 4.0, card_rect.min.y + 56.0),
                    );
                    let text_painter = ui.painter().with_clip_rect(text_clip);
                    text_painter.text(
                        Pos2::new(av_rect.max.x + 10.0, card_rect.min.y + 22.0),
                        egui::Align2::LEFT_CENTER,
                        &name,
                        FontId::proportional(13.5),
                        p.text,
                    );
                    text_painter.text(
                        Pos2::new(av_rect.max.x + 10.0, card_rect.min.y + 41.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{}@{}:{}", h.username, h.host, h.port),
                        FontId::monospace(11.5),
                        p.dim,
                    );

                    // 4. 右上角「编辑」胶囊按钮（带矢量铅笔图标 + 文字标签 + 边框）
                    let edit_bg = if edit_resp.hovered() {
                        p.accent_soft
                    } else {
                        p.surface3
                    };
                    let edit_border = if edit_resp.hovered() {
                        p.accent
                    } else {
                        p.border
                    };
                    let edit_fg = if edit_resp.hovered() { p.accent } else { p.dim };
                    ui.painter()
                        .rect_filled(edit_rect, CornerRadius::same(6), edit_bg);
                    ui.painter().rect_stroke(
                        edit_rect,
                        CornerRadius::same(6),
                        Stroke::new(1.0_f32, edit_border),
                        StrokeKind::Inside,
                    );
                    draw_icon(
                        ui.painter(),
                        Pos2::new(edit_rect.min.x + 12.5, edit_rect.center().y),
                        11.5,
                        Icon::Edit,
                        edit_fg,
                    );
                    ui.painter().text(
                        Pos2::new(edit_rect.min.x + 22.0, edit_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "编辑",
                        FontId::proportional(11.0),
                        if edit_resp.hovered() { p.accent } else { p.text },
                    );
                    if edit_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    // 5. 卡片内部分隔细线
                    let div_y = card_rect.min.y + 62.0;
                    ui.painter().line_segment(
                        [
                            Pos2::new(card_rect.min.x + 14.0, div_y),
                            Pos2::new(card_rect.max.x - 14.0, div_y),
                        ],
                        Stroke::new(1.0_f32, p.border.gamma_multiply(0.6)),
                    );

                    // 6. 左下角认证方式徽章
                    let has_key = !h.identity_file.trim().is_empty()
                        || !h.identity_fingerprint.trim().is_empty();
                    let badge_rect = Rect::from_min_size(
                        Pos2::new(card_rect.min.x + 14.0, card_rect.max.y - 34.0),
                        Vec2::new(62.0, 24.0),
                    );
                    ui.painter()
                        .rect_filled(badge_rect, CornerRadius::same(6), p.surface3);
                    ui.painter().rect_stroke(
                        badge_rect,
                        CornerRadius::same(6),
                        Stroke::new(1.0_f32, p.border),
                        StrokeKind::Inside,
                    );
                    draw_icon(
                        ui.painter(),
                        Pos2::new(badge_rect.min.x + 13.0, badge_rect.center().y),
                        11.5,
                        if has_key { Icon::Key } else { Icon::Lock },
                        if has_key { p.accent } else { p.dim },
                    );
                    ui.painter().text(
                        Pos2::new(badge_rect.min.x + 24.0, badge_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        if has_key { "密钥" } else { "密码" },
                        FontId::proportional(11.0),
                        p.dim,
                    );

                    // 7. 右下角「密钥」按钮
                    let keys_bg = if keys_resp.hovered() {
                        p.accent_soft
                    } else {
                        p.surface3
                    };
                    let keys_border = if keys_resp.hovered() {
                        p.accent
                    } else {
                        p.border
                    };
                    ui.painter()
                        .rect_filled(keys_rect, CornerRadius::same(7), keys_bg);
                    ui.painter().rect_stroke(
                        keys_rect,
                        CornerRadius::same(7),
                        Stroke::new(1.0_f32, keys_border),
                        StrokeKind::Inside,
                    );
                    draw_icon(
                        ui.painter(),
                        Pos2::new(keys_rect.min.x + 15.0, keys_rect.center().y),
                        12.0,
                        Icon::Shield,
                        if keys_resp.hovered() { p.accent } else { p.dim },
                    );
                    ui.painter().text(
                        Pos2::new(keys_rect.min.x + 27.0, keys_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "密钥",
                        FontId::proportional(12.0),
                        if keys_resp.hovered() { p.accent } else { p.text },
                    );

                    // 8. 右下角「终端」主按钮
                    let term_bg = if term_resp.hovered() {
                        p.accent.gamma_multiply(0.88)
                    } else {
                        p.accent
                    };
                    ui.painter()
                        .rect_filled(term_rect, CornerRadius::same(7), term_bg);
                    draw_icon(
                        ui.painter(),
                        Pos2::new(term_rect.min.x + 15.0, term_rect.center().y),
                        12.0,
                        Icon::Terminal,
                        Color32::WHITE,
                    );
                    ui.painter().text(
                        Pos2::new(term_rect.min.x + 27.0, term_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "终端",
                        FontId::proportional(12.0),
                        Color32::WHITE,
                    );

                    if edit_resp.hovered() || keys_resp.hovered() || term_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    if edit_resp.clicked() {
                        pending_edit = Some(Some(h));
                    } else if term_resp.clicked() {
                        pending_connect = Some((h, ViewMode::Term));
                    } else if keys_resp.clicked() {
                        pending_connect = Some((h, ViewMode::Keys));
                    }
                }
            }

            ui.add_space(gap);
            row_idx += 1;
        }

        if let Some(ed) = pending_edit {
            self.open_host_edit(ed);
        }
        if let Some((h, m)) = pending_connect {
            self.start_connect_flow(h, m);
        }
    }

    fn render_term_canvas(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette;

        // 未连接服务器时：不显示引导横幅与添加卡，直接展示主机网格
        if self.session.is_none() {
            self.render_vault_grid(ui, false);
            return;
        }

        let term_active = self.term_id.is_some();
        let endpoint = self
            .session
            .as_ref()
            .and_then(|s| s.get("host"))
            .map(|h| {
                format!(
                    "{}@{}:{}",
                    h.get("username").and_then(|v| v.as_str()).unwrap_or("root"),
                    h.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                    h.get("port").and_then(|v| v.as_u64()).unwrap_or(22)
                )
            })
            .unwrap_or_else(|| "SSH 终端".to_string());

        Frame::NONE
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let dot_col = if term_active { p.ok } else { p.warn };
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 4.0, dot_col);

                    ui.label(
                        RichText::new(&endpoint)
                            .size(13.0)
                            .monospace()
                            .strong()
                            .color(p.text),
                    );

                    ui.separator();
                    ui.label(RichText::new("快捷指令:").size(11.5).color(p.dim));
                    for (lbl, cmd) in SNIPPETS {
                        if ui
                            .add(
                                egui::Button::new(RichText::new(lbl).size(11.0).color(p.text))
                                    .fill(p.surface3)
                                    .stroke(Stroke::new(1.0_f32, p.border))
                                    .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            self.send_term_bytes(cmd.as_bytes());
                            self.term_focused = true;
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if Self::action_btn(ui, &p, "断开会话", false, true).clicked() {
                            self.disconnect_session();
                        }
                        if Self::action_btn(ui, &p, "重连终端", false, false).clicked() {
                            self.open_pty();
                        }
                        if Self::action_btn(ui, &p, "清屏", false, false).clicked() {
                            self.vt.clear_scrollback();
                            self.send_term_bytes(b"\x0c");
                        }
                        if Self::action_btn(ui, &p, "粘贴", false, false).clicked() {
                            let txt = self.read_from_clipboard();
                            if !txt.is_empty() {
                                self.send_term_bytes(txt.as_bytes());
                            }
                        }
                        if Self::action_btn(ui, &p, "复制全屏", false, false).clicked() {
                            let dump = self.vt.all_text();
                            self.copy_to_clipboard(&dump, "终端屏幕内容");
                        }
                    });
                });
            });

        ui.add_space(8.0);

        let avail = ui.available_size();
        let pad_x = 12.0;
        let pad_y = 10.0;
        let font_id = FontId::monospace(13.5);
        let char_w = ui.fonts(|f| f.glyph_width(&font_id, 'M')).max(7.2);
        let line_h = 19.0;

        let inner_w = (avail.x - pad_x * 2.0).max(160.0);
        let inner_h = (avail.y - pad_y * 2.0).max(100.0);
        let new_cols = (inner_w / char_w).floor().clamp(20.0, 300.0) as usize;
        let new_rows = (inner_h / line_h).floor().clamp(6.0, 150.0) as usize;

        if new_cols != self.vt.cols() || new_rows != self.vt.rows() {
            self.vt.resize(new_cols, new_rows);
            if let Some(tid) = self.term_id.clone() {
                if self.last_sent_size != (new_cols, new_rows) {
                    self.last_sent_size = (new_cols, new_rows);
                    let engine = Arc::clone(&self.engine);
                    self.rt.spawn(async move {
                        let _ = engine
                            .term_resize(json!({
                                "termId": tid,
                                "cols": new_cols,
                                "rows": new_rows,
                            }))
                            .await;
                    });
                }
            }
        }

        let (term_rect, term_resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());

        ui.painter()
            .rect_filled(term_rect, CornerRadius::same(10), p.surface2);
        ui.painter().rect_stroke(
            term_rect,
            CornerRadius::same(10),
            Stroke::new(
                1.0_f32,
                if self.term_focused {
                    p.accent
                } else {
                    p.border
                },
            ),
            StrokeKind::Inside,
        );

        if term_resp.clicked() {
            self.term_focused = true;
            term_resp.request_focus();
        } else if ui.input(|i| i.pointer.any_pressed()) && !term_resp.hovered() {
            self.term_focused = false;
        }

        if term_resp.secondary_clicked() {
            let txt = self.read_from_clipboard();
            if !txt.is_empty() {
                self.send_term_bytes(txt.as_bytes());
            }
        }

        if term_resp.hovered() {
            let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_y.abs() > 1.0 {
                let lines = (scroll_y / 18.0).round() as isize;
                let max_hist = self.vt.history().len() as isize;
                let next = (self.term_scroll_offset as isize + lines).clamp(0, max_hist);
                self.term_scroll_offset = next as usize;
            }
        }

        if self.term_focused && term_active && matches!(self.drawer, Drawer::None) {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    term_resp.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });

            let mut to_send: Vec<u8> = Vec::new();
            ui.input(|input| {
                for ev in &input.events {
                    match ev {
                        Event::Paste(s) => {
                            to_send.extend_from_slice(s.as_bytes());
                        }
                        Event::Text(s) => {
                            if !input.modifiers.ctrl && !input.modifiers.command {
                                to_send.extend_from_slice(s.as_bytes());
                            }
                        }
                        Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => {
                            if modifiers.ctrl {
                                match key {
                                    Key::C => to_send.push(0x03),
                                    Key::D => to_send.push(0x04),
                                    Key::Z => to_send.push(0x1a),
                                    Key::L => to_send.push(0x0c),
                                    Key::A => to_send.push(0x01),
                                    Key::E => to_send.push(0x05),
                                    Key::U => to_send.push(0x15),
                                    Key::K => to_send.push(0x0b),
                                    Key::W => to_send.push(0x17),
                                    Key::R => to_send.push(0x12),
                                    _ => {}
                                }
                            } else {
                                match key {
                                    Key::Enter => to_send.push(b'\r'),
                                    Key::Backspace => to_send.push(0x7f),
                                    Key::Tab => to_send.push(b'\t'),
                                    Key::Escape => to_send.push(0x1b),
                                    Key::ArrowUp => to_send.extend_from_slice(b"\x1b[A"),
                                    Key::ArrowDown => to_send.extend_from_slice(b"\x1b[B"),
                                    Key::ArrowRight => to_send.extend_from_slice(b"\x1b[C"),
                                    Key::ArrowLeft => to_send.extend_from_slice(b"\x1b[D"),
                                    Key::Home => to_send.extend_from_slice(b"\x1b[H"),
                                    Key::End => to_send.extend_from_slice(b"\x1b[F"),
                                    Key::Delete => to_send.extend_from_slice(b"\x1b[3~"),
                                    Key::PageUp => to_send.extend_from_slice(b"\x1b[5~"),
                                    Key::PageDown => to_send.extend_from_slice(b"\x1b[6~"),
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });

            if !to_send.is_empty() {
                self.term_scroll_offset = 0;
                self.send_term_bytes(&to_send);
            }
        }

        let origin = Pos2::new(term_rect.min.x + pad_x, term_rect.min.y + pad_y);
        let painter = ui.painter().with_clip_rect(term_rect.shrink(4.0));
        let hist = self.vt.history();
        let grid = self.vt.grid();
        let rows = self.vt.rows();
        let cols = self.vt.cols();
        let total_lines = hist.len() + grid.len();
        let start_line = total_lines
            .saturating_sub(rows)
            .saturating_sub(self.term_scroll_offset.min(hist.len()));

        for vis_r in 0..rows {
            let line_idx = start_line + vis_r;
            let row_slice = if line_idx < hist.len() {
                hist.get(line_idx).map(|v| v.as_slice())
            } else {
                grid.get(line_idx - hist.len()).map(|v| v.as_slice())
            };
            let Some(cells) = row_slice else {
                continue;
            };
            let y = origin.y + (vis_r as f32) * line_h;

            for (c_idx, cell) in cells.iter().enumerate().take(cols) {
                if cell.continuation {
                    continue;
                }
                let x = origin.x + (c_idx as f32) * char_w;
                let w = if is_wide_char(cell.ch) {
                    char_w * 2.0
                } else {
                    char_w
                };
                let (mut fg, mut bg) = (
                    cell.pen.fg.unwrap_or(p.text),
                    cell.pen.bg.unwrap_or(Color32::TRANSPARENT),
                );
                if cell.pen.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                    if fg == Color32::TRANSPARENT {
                        fg = p.surface2;
                    }
                }
                if bg != Color32::TRANSPARENT {
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(x, y), Vec2::new(w + 0.5, line_h)),
                        CornerRadius::ZERO,
                        bg,
                    );
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    let mut buf = [0u8; 4];
                    let s = cell.ch.encode_utf8(&mut buf);
                    painter.text(
                        Pos2::new(x, y + line_h * 0.5),
                        egui::Align2::LEFT_CENTER,
                        s,
                        font_id.clone(),
                        fg,
                    );
                }
            }
        }

        if self.term_scroll_offset == 0 {
            let (cx, cy) = self.vt.cursor();
            if cx < cols && cy < rows {
                let cur_rect = Rect::from_min_size(
                    Pos2::new(
                        origin.x + (cx as f32) * char_w,
                        origin.y + (cy as f32) * line_h + 1.0,
                    ),
                    Vec2::new(char_w, line_h - 2.0),
                );
                if self.term_focused {
                    painter.rect_filled(
                        cur_rect,
                        CornerRadius::same(2),
                        p.accent.gamma_multiply(0.65),
                    );
                } else {
                    painter.rect_stroke(
                        cur_rect,
                        CornerRadius::same(2),
                        Stroke::new(1.0_f32, p.accent),
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }

    fn render_local_panel(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        egui::SidePanel::right("kw_local_panel")
            .exact_width(275.0)
            .resizable(true)
            .frame(
                Frame::NONE
                    .fill(p.bg_elev)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (f_rect, _) = ui.allocate_exact_size(Vec2::new(16.0, 16.0), Sense::hover());
                    draw_icon(ui.painter(), f_rect.center(), 13.0, Icon::Folder, p.accent);
                    ui.label(
                        RichText::new("本机私钥库对照")
                            .size(13.0)
                            .strong()
                            .color(p.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("✕").size(11.5).color(p.dim))
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::NONE),
                            )
                            .clicked()
                        {
                            self.show_local_panel = false;
                        }
                        if ui
                            .add(
                                egui::Button::new(RichText::new("↻").size(12.0).color(p.dim))
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::NONE),
                            )
                            .on_hover_text("强制重新扫描本机密钥目录")
                            .clicked()
                        {
                            self.trigger_local_scan(true);
                        }
                    });
                });

                let dir = self.engine.state().current_settings().local_key_dir;
                ui.label(RichText::new(dir).size(11.0).monospace().color(p.faint));
                ui.separator();

                let remote_fps: HashSet<String> = self
                    .session
                    .as_ref()
                    .and_then(|s| s.get("items"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.get("fingerprint").and_then(|v| v.as_str()))
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();

                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.local_keys.is_empty() {
                            ui.label(
                                RichText::new("未扫描到本机密钥文件")
                                    .size(12.0)
                                    .color(p.dim),
                            );
                            return;
                        }

                        for lk in &self.local_keys {
                            let matched =
                                !lk.fingerprint.is_empty() && remote_fps.contains(&lk.fingerprint);
                            Frame::NONE
                                .fill(p.panel)
                                .stroke(Stroke::new(
                                    1.0_f32,
                                    if matched {
                                        p.ok.gamma_multiply(0.5)
                                    } else {
                                        p.border
                                    },
                                ))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(&lk.name)
                                                .size(12.5)
                                                .strong()
                                                .color(p.text),
                                        );
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if matched {
                                                    ui.label(
                                                        RichText::new("● 已授权")
                                                            .size(11.0)
                                                            .strong()
                                                            .color(p.ok),
                                                    );
                                                } else {
                                                    ui.label(
                                                        RichText::new(&lk.key_type)
                                                            .size(10.5)
                                                            .monospace()
                                                            .color(p.dim),
                                                    );
                                                }
                                            },
                                        );
                                    });
                                    if !lk.fingerprint.is_empty() {
                                        ui.label(
                                            RichText::new(&lk.fingerprint)
                                                .size(10.5)
                                                .monospace()
                                                .color(p.faint),
                                        );
                                    }
                                });
                            ui.add_space(6.0);
                        }
                    });
            });
    }

    fn open_host_edit(&mut self, entry: Option<HostEntry>) {
        if let Some(h) = entry {
            self.drawer = Drawer::HostEdit {
                id: Some(h.id),
                name: h.name,
                host: h.host,
                port: h.port.to_string(),
                username: h.username,
                identity_file: h.identity_file,
                remote_path: h.remote_path,
            };
        } else {
            let def_path = self.engine.state().current_settings().remote_path;
            self.drawer = Drawer::HostEdit {
                id: None,
                name: String::new(),
                host: String::new(),
                port: "22".to_string(),
                username: "root".to_string(),
                identity_file: String::new(),
                remote_path: def_path,
            };
        }
    }

    fn open_settings_drawer(&mut self) {
        let s = self.engine.state().current_settings();
        self.drawer = Drawer::Settings {
            theme_choice: self.theme_choice.clone(),
            local_key_dir: s.local_key_dir,
            save_dir: s.save_dir,
            remote_path: s.remote_path,
        };
    }

    fn open_sync_drawer(&mut self) {
        self.ms_status = self.engine.ms_status();
        let cid = self
            .ms_status
            .get("clientId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let remembered = self
            .ms_status
            .get("hasSyncKey")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.drawer = Drawer::Sync {
            client_id: cid,
            passphrase: String::new(),
            remember_key: remembered,
            device_prompt: None,
            conflicts: Vec::new(),
        };
    }

    fn open_backups_drawer(&mut self) {
        let Some(sid) = self.current_sid() else {
            return;
        };
        self.busy = true;
        let engine = Arc::clone(&self.engine);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match engine.backups(&sid).await {
                Ok(v) => {
                    let list = v
                        .get("backups")
                        .and_then(|x| x.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let _ = tx.send(BgEvent::BackupsLoaded(list));
                }
                Err(e) => {
                    let msg = e
                        .get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("获取历史备份失败");
                    let _ = tx.send(BgEvent::Log(msg.to_string(), LogKind::Err));
                }
            }
        });
    }

    fn render_drawer(&mut self, ctx: &egui::Context) {
        if matches!(self.drawer, Drawer::None) {
            return;
        }
        let p = self.palette;
        let mut close_drawer = false;

        egui::Area::new(Id::new("kw_modal_backdrop"))
            .order(Order::Foreground)
            .fixed_pos(Pos2::ZERO)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(135));
                if resp.clicked() {
                    close_drawer = true;
                }
            });

        let is_host_modal = matches!(self.drawer, Drawer::HostEdit { .. });
        let modal_w = if is_host_modal { 720.0 } else { 540.0 };

        egui::Window::new("kw_drawer_modal")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(modal_w)
            .min_width(modal_w)
            .max_width(modal_w)
            .frame(
                Frame::NONE
                    .fill(p.bg_elev)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(13))
                    .inner_margin(Margin::symmetric(20, 18)),
            )
            .show(ctx, |ui| {
                ui.set_width(modal_w);
                let mut current = std::mem::replace(&mut self.drawer, Drawer::None);
                match &mut current {
                    Drawer::None => {}

                    Drawer::HostEdit {
                        id,
                        name,
                        host,
                        port,
                        username,
                        identity_file,
                        remote_path,
                    } => {
                        // 顶部标题栏：图标 + 标题 + 数量徽章 + 右侧「＋ 新建服务器」「关闭」
                        ui.horizontal(|ui| {
                            let (ico_r, _) =
                                ui.allocate_exact_size(Vec2::new(26.0, 26.0), Sense::hover());
                            ui.painter()
                                .rect_filled(ico_r, CornerRadius::same(7), p.accent_soft);
                            draw_icon(ui.painter(), ico_r.center(), 14.0, Icon::Server, p.accent);
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("服务器管理")
                                    .size(15.5)
                                    .strong()
                                    .color(p.text),
                            );
                            let (cnt_r, _) =
                                ui.allocate_exact_size(Vec2::new(56.0, 20.0), Sense::hover());
                            ui.painter()
                                .rect_filled(cnt_r, CornerRadius::same(10), p.surface3);
                            ui.painter().text(
                                cnt_r.center(),
                                egui::Align2::CENTER_CENTER,
                                format!("共 {} 台", self.hosts.len()),
                                FontId::proportional(11.0),
                                p.dim,
                            );

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "关闭", false, false).clicked() {
                                    close_drawer = true;
                                }
                                if Self::action_btn(ui, &p, "＋ 新建服务器", id.is_none(), false)
                                    .clicked()
                                {
                                    let def_path =
                                        self.engine.state().current_settings().remote_path;
                                    *id = None;
                                    name.clear();
                                    host.clear();
                                    *port = "22".to_string();
                                    *username = "root".to_string();
                                    identity_file.clear();
                                    *remote_path = def_path;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // 左右双栏布局：左侧服务器列表 + 右侧配置表单（注意：在 horizontal 内必须显式开启 vertical 布局）
                        let mut switch_to: Option<HostEntry> = None;
                        ui.horizontal_top(|ui| {
                            // 左侧列表容器
                            Frame::NONE
                                .fill(p.surface2)
                                .stroke(Stroke::new(1.0_f32, p.border))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(Margin::symmetric(8, 8))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(210.0);
                                        ScrollArea::vertical()
                                            .id_salt("host_mgr_left_scroll")
                                            .max_height(310.0)
                                            .auto_shrink([false, false])
                                            .show(ui, |ui| {
                                                if self.hosts.is_empty() {
                                                    ui.add_space(24.0);
                                                    ui.vertical_centered(|ui| {
                                                        ui.label(
                                                            RichText::new("暂无保存的服务器")
                                                                .size(12.0)
                                                                .color(p.dim),
                                                        );
                                                    });
                                                }
                                                for (idx, h) in self.hosts.iter().enumerate() {
                                                    let selected =
                                                        id.as_deref() == Some(h.id.as_str());
                                                    let nm = if h.name.trim().is_empty() {
                                                        &h.host
                                                    } else {
                                                        &h.name
                                                    };
                                                    let first_ch = nm
                                                        .chars()
                                                        .next()
                                                        .unwrap_or('S')
                                                        .to_uppercase()
                                                        .to_string();

                                                    let (row_r, row_resp) = ui.allocate_exact_size(
                                                        Vec2::new(198.0, 50.0),
                                                        Sense::click(),
                                                    );
                                                    let hovered = row_resp.hovered();
                                                    let bg = if selected {
                                                        p.accent_soft
                                                    } else if hovered {
                                                        p.surface3
                                                    } else {
                                                        p.panel
                                                    };
                                                    let stroke_c = if selected {
                                                        p.accent
                                                    } else if hovered {
                                                        p.accent.gamma_multiply(0.45)
                                                    } else {
                                                        p.border
                                                    };
                                                    ui.painter().rect_filled(
                                                        row_r,
                                                        CornerRadius::same(8),
                                                        bg,
                                                    );
                                                    ui.painter().rect_stroke(
                                                        row_r,
                                                        CornerRadius::same(8),
                                                        Stroke::new(1.0_f32, stroke_c),
                                                        StrokeKind::Inside,
                                                    );
                                                    if selected {
                                                        let bar = Rect::from_min_size(
                                                            Pos2::new(
                                                                row_r.min.x,
                                                                row_r.center().y - 10.0,
                                                            ),
                                                            Vec2::new(3.0, 20.0),
                                                        );
                                                        ui.painter().rect_filled(
                                                            bar,
                                                            CornerRadius::same(2),
                                                            p.accent,
                                                        );
                                                    }

                                                    let av = Rect::from_min_size(
                                                        Pos2::new(
                                                            row_r.min.x + 10.0,
                                                            row_r.min.y + 10.0,
                                                        ),
                                                        Vec2::new(30.0, 30.0),
                                                    );
                                                    ui.painter().rect_filled(
                                                        av,
                                                        CornerRadius::same(7),
                                                        if selected {
                                                            p.accent
                                                        } else {
                                                            p.surface3
                                                        },
                                                    );
                                                    ui.painter().text(
                                                        av.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        &first_ch,
                                                        FontId::proportional(13.0),
                                                        if selected {
                                                            Color32::WHITE
                                                        } else {
                                                            p.accent
                                                        },
                                                    );

                                                    let clip = Rect::from_min_max(
                                                        Pos2::new(av.max.x + 8.0, row_r.min.y + 4.0),
                                                        Pos2::new(
                                                            row_r.max.x - 8.0,
                                                            row_r.max.y - 4.0,
                                                        ),
                                                    );
                                                    let tp = ui.painter().with_clip_rect(clip);
                                                    tp.text(
                                                        Pos2::new(
                                                            av.max.x + 8.0,
                                                            row_r.min.y + 17.0,
                                                        ),
                                                        egui::Align2::LEFT_CENTER,
                                                        nm,
                                                        FontId::proportional(12.5),
                                                        p.text,
                                                    );
                                                    tp.text(
                                                        Pos2::new(
                                                            av.max.x + 8.0,
                                                            row_r.min.y + 34.0,
                                                        ),
                                                        egui::Align2::LEFT_CENTER,
                                                        format!(
                                                            "{}@{}:{}",
                                                            h.username, h.host, h.port
                                                        ),
                                                        FontId::monospace(10.5),
                                                        p.dim,
                                                    );

                                                    if hovered {
                                                        ui.ctx().set_cursor_icon(
                                                            CursorIcon::PointingHand,
                                                        );
                                                    }
                                                    if row_resp.clicked() {
                                                        switch_to = Some(h.clone());
                                                    }
                                                    if idx + 1 < self.hosts.len() {
                                                        ui.add_space(5.0);
                                                    }
                                                }
                                            });
                                    });
                                });

                            ui.add_space(10.0);

                            // 右侧编辑/新建表单容器
                            Frame::NONE
                                .fill(p.panel)
                                .stroke(Stroke::new(1.0_f32, p.border))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(Margin::symmetric(16, 14))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(446.0);
                                        let is_edit = id.is_some();
                                        ui.horizontal(|ui| {
                                            let (ic_r, _) = ui.allocate_exact_size(
                                                Vec2::new(18.0, 18.0),
                                                Sense::hover(),
                                            );
                                            draw_icon(
                                                ui.painter(),
                                                ic_r.center(),
                                                13.0,
                                                if is_edit { Icon::Edit } else { Icon::Plus },
                                                p.accent,
                                            );
                                            ui.label(
                                                RichText::new(if is_edit {
                                                    format!(
                                                        "编辑服务器：{}",
                                                        if name.trim().is_empty() {
                                                            host.as_str()
                                                        } else {
                                                            name.as_str()
                                                        }
                                                    )
                                                } else {
                                                    "新建服务器配置".to_string()
                                                })
                                                .size(13.5)
                                                .strong()
                                                .color(p.text),
                                            );
                                        });
                                        ui.add_space(4.0);
                                        ui.separator();
                                        ui.add_space(8.0);

                                        egui::Grid::new("host_edit_grid")
                                            .num_columns(2)
                                            .spacing([14.0, 12.0])
                                            .min_col_width(82.0)
                                            .show(ui, |ui| {
                                                ui.label(RichText::new("备注名称").color(p.dim));
                                                ui.add(
                                                    egui::TextEdit::singleline(name)
                                                        .hint_text("例如：生产服务器 HK-01")
                                                        .desired_width(336.0),
                                                );
                                                ui.end_row();

                                                ui.label(RichText::new("主机地址 *").color(p.dim));
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        egui::TextEdit::singleline(host)
                                                            .hint_text("IP 或域名，如 203.0.113.10")
                                                            .desired_width(230.0),
                                                    );
                                                    ui.label(RichText::new("端口").color(p.dim));
                                                    ui.add(
                                                        egui::TextEdit::singleline(port)
                                                            .desired_width(58.0),
                                                    );
                                                });
                                                ui.end_row();

                                                ui.label(RichText::new("登录用户 *").color(p.dim));
                                                ui.add(
                                                    egui::TextEdit::singleline(username)
                                                        .hint_text("root / ubuntu")
                                                        .desired_width(336.0),
                                                );
                                                ui.end_row();

                                                ui.label(RichText::new("私钥路径").color(p.dim));
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        egui::TextEdit::singleline(identity_file)
                                                            .hint_text("可选，留空则在连接时输入密码")
                                                            .desired_width(272.0),
                                                    );
                                                    if ui.button("浏览…").clicked() {
                                                        if let Some(path) =
                                                            rfd::FileDialog::new().pick_file()
                                                        {
                                                            *identity_file =
                                                                path.to_string_lossy().to_string();
                                                        }
                                                    }
                                                });
                                                ui.end_row();

                                                ui.label(RichText::new("远端路径").color(p.dim));
                                                ui.add(
                                                    egui::TextEdit::singleline(remote_path)
                                                        .hint_text("~/.ssh/authorized_keys")
                                                        .desired_width(336.0),
                                                );
                                                ui.end_row();
                                            });

                                        ui.add_space(16.0);
                                        let id_clone = id.clone();
                                        let nm_val = name.trim().to_string();
                                        let h_val = host.trim().to_string();
                                        let p_val = port.trim().parse::<u16>().unwrap_or(22);
                                        let u_val = if username.trim().is_empty() {
                                            "root".to_string()
                                        } else {
                                            username.trim().to_string()
                                        };
                                        let idf_val = identity_file.trim().to_string();
                                        let rp_val = remote_path.trim().to_string();

                                        ui.horizontal(|ui| {
                                            if let Some(exist_id) = id_clone.as_ref() {
                                                if Self::action_btn(
                                                    ui,
                                                    &p,
                                                    "删除此服务器",
                                                    false,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    let _ = self.engine.delete_host(exist_id);
                                                    self.reload_store_data();
                                                    self.push_log(
                                                        "已删除服务器配置",
                                                        LogKind::Info,
                                                    );
                                                    if let Some(next_h) =
                                                        self.hosts.first().cloned()
                                                    {
                                                        switch_to = Some(next_h);
                                                    } else {
                                                        *id = None;
                                                        name.clear();
                                                        host.clear();
                                                        *port = "22".to_string();
                                                        *username = "root".to_string();
                                                        identity_file.clear();
                                                    }
                                                }
                                            }
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if Self::action_btn(
                                                        ui,
                                                        &p,
                                                        "保存配置",
                                                        true,
                                                        false,
                                                    )
                                                    .clicked()
                                                    {
                                                        if h_val.is_empty() {
                                                            self.push_log(
                                                                "请填写主机 IP 或域名",
                                                                LogKind::Warn,
                                                            );
                                                        } else {
                                                            let patch = HostPatch {
                                                                id: id_clone,
                                                                name: Some(if nm_val.is_empty() {
                                                                    h_val.clone()
                                                                } else {
                                                                    nm_val
                                                                }),
                                                                host: Some(h_val),
                                                                port: Some(p_val),
                                                                username: Some(u_val),
                                                                identity_file: Some(idf_val),
                                                                identity_fingerprint: None,
                                                                remote_path: Some(rp_val),
                                                            };
                                                            match self.engine.save_host(patch) {
                                                                Ok(saved) => {
                                                                    self.reload_store_data();
                                                                    if let Some(new_id) = saved
                                                                        .get("id")
                                                                        .and_then(|v| v.as_str())
                                                                    {
                                                                        *id = Some(
                                                                            new_id.to_string(),
                                                                        );
                                                                    }
                                                                    self.push_log(
                                                                        "已保存主机配置",
                                                                        LogKind::Ok,
                                                                    );
                                                                }
                                                                Err(e) => {
                                                                    let msg = e
                                                                        .get("error")
                                                                        .and_then(|v| v.as_str())
                                                                        .unwrap_or("保存失败");
                                                                    self.push_log(
                                                                        msg.to_string(),
                                                                        LogKind::Err,
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                },
                                            );
                                        });
                                    });
                                });
                        });

                        if let Some(target) = switch_to {
                            *id = Some(target.id);
                            *name = target.name;
                            *host = target.host;
                            *port = target.port.to_string();
                            *username = target.username;
                            *identity_file = target.identity_file;
                            *remote_path = target.remote_path;
                        }
                    }

                    Drawer::ConnectPrompt {
                        host,
                        target_mode,
                        password,
                        passphrase,
                        trust_fp,
                        hostkey_warning,
                    } => {
                        let title = if host.name.is_empty() {
                            &host.host
                        } else {
                            &host.name
                        };
                        ui.label(
                            RichText::new(format!(
                                "连接认证 — {title} ({}@{}:{})",
                                host.username, host.host, host.port
                            ))
                            .size(14.5)
                            .strong()
                            .color(p.text),
                        );
                        ui.separator();

                        if let Some((w_title, w_msg, is_danger)) = hostkey_warning.as_ref() {
                            let col = if *is_danger { p.danger } else { p.warn };
                            Frame::NONE
                                .fill(col.gamma_multiply(0.14))
                                .stroke(Stroke::new(1.0_f32, col.gamma_multiply(0.45)))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(w_title).size(12.5).strong().color(col),
                                    );
                                    ui.label(RichText::new(w_msg).size(11.5).color(p.text));
                                    if let Some(fp) = trust_fp.as_ref() {
                                        ui.label(
                                            RichText::new(format!("指纹: {fp}"))
                                                .size(11.5)
                                                .monospace()
                                                .strong()
                                                .color(p.text),
                                        );
                                    }
                                });
                            ui.add_space(8.0);
                        }

                        if !host.identity_file.is_empty() {
                            ui.label(
                                RichText::new(format!("使用私钥: {}", host.identity_file))
                                    .size(11.5)
                                    .monospace()
                                    .color(p.dim),
                            );
                            ui.add_space(4.0);
                        }

                        ui.label(
                            RichText::new("SSH 登录密码（若使用免密私钥可留空）：")
                                .size(12.0)
                                .color(p.dim),
                        );
                        let pw_resp = ui.add(
                            egui::TextEdit::singleline(password)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("私钥解密口令（若私钥无加密可留空）：")
                                .size(12.0)
                                .color(p.dim),
                        );
                        let pp_resp = ui.add(
                            egui::TextEdit::singleline(passphrase)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );

                        let enter_pressed = (pw_resp.lost_focus() || pp_resp.lost_focus())
                            && ui.input(|i| i.key_pressed(Key::Enter));

                        ui.add_space(12.0);
                        let h_clone = host.clone();
                        let tm = *target_mode;
                        let pw = password.clone();
                        let pp = passphrase.clone();
                        let tfp = trust_fp.clone();
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let btn_lbl = if tfp.is_some() {
                                    "信任指纹并连接"
                                } else {
                                    "立即连接"
                                };
                                if Self::action_btn(ui, &p, btn_lbl, true, false).clicked()
                                    || enter_pressed
                                {
                                    self.execute_connect(h_clone, tm, pw, pp, tfp);
                                    close_drawer = true;
                                }
                                if Self::action_btn(ui, &p, "取消", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::AddKey {
                        tab,
                        algo,
                        label,
                        comment,
                        passphrase,
                        selected_local_pub,
                        pasted_pub,
                    } => {
                        ui.label(
                            RichText::new("＋ 新增远端授权公钥")
                                .size(15.5)
                                .strong()
                                .color(p.text),
                        );
                        ui.separator();

                        ui.horizontal(|ui| {
                            for (idx, t_lbl) in [
                                (0, "生成新密钥对并部署"),
                                (1, "从本机公钥选择"),
                                (2, "粘贴现有公钥"),
                            ] {
                                if ui.selectable_label(*tab == idx, t_lbl).clicked() {
                                    *tab = idx;
                                }
                            }
                        });
                        ui.add_space(8.0);

                        match *tab {
                            0 => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("算法:").color(p.dim));
                                    for a in ["ed25519", "rsa"] {
                                        if ui
                                            .selectable_label(algo == a, a.to_uppercase())
                                            .clicked()
                                        {
                                            *algo = a.to_string();
                                        }
                                    }
                                });
                                ui.add_space(4.0);
                                ui.label(RichText::new("文件名前缀标签:").size(12.0).color(p.dim));
                                ui.add(
                                    egui::TextEdit::singleline(label)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(4.0);
                                ui.label(RichText::new("公钥注释 (comment):").size(12.0).color(p.dim));
                                ui.add(
                                    egui::TextEdit::singleline(comment)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new("私钥加密口令 (可选):").size(12.0).color(p.dim),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(passphrase)
                                        .password(true)
                                        .desired_width(f32::INFINITY),
                                );
                            }
                            1 => {
                                ui.label(
                                    RichText::new("选择本机已扫描到的公钥/私钥自动提取公钥：")
                                        .size(12.0)
                                        .color(p.dim),
                                );
                                ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                                    for lk in &self.local_keys {
                                        if let Some(pub_line) = &lk.public_key_line {
                                            let is_sel = selected_local_pub == pub_line;
                                            if ui
                                                .selectable_label(
                                                    is_sel,
                                                    format!("{} ({})", lk.name, lk.fingerprint),
                                                )
                                                .clicked()
                                            {
                                                *selected_local_pub = pub_line.clone();
                                            }
                                        }
                                    }
                                });
                            }
                            _ => {
                                ui.label(
                                    RichText::new("粘贴 ssh-ed25519 / ssh-rsa 公钥行：")
                                        .size(12.0)
                                        .color(p.dim),
                                );
                                ui.add(
                                    egui::TextEdit::multiline(pasted_pub)
                                        .desired_rows(4)
                                        .font(FontId::monospace(12.0))
                                        .desired_width(f32::INFINITY),
                                );
                            }
                        }

                        ui.add_space(12.0);
                        let cur_tab = *tab;
                        let al = algo.clone();
                        let lb = label.trim().to_string();
                        let cm = comment.trim().to_string();
                        let pp = passphrase.clone();
                        let sel_pub = selected_local_pub.trim().to_string();
                        let pst_pub = pasted_pub.trim().to_string();

                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "写入远端 authorized_keys", true, false)
                                    .clicked()
                                {
                                    if let Some(sid) = self.current_sid() {
                                        let save_dir = self.current_login_key_dir().unwrap_or_else(|| {
                                            self.engine.state().current_settings().save_dir
                                        });
                                        let body = if cur_tab == 0 {
                                            json!({
                                                "sid": sid,
                                                "mode": "generate",
                                                "generator": al,
                                                "label": if lb.is_empty() { "key".to_string() } else { lb },
                                                "comment": cm,
                                                "passphrase": pp,
                                                "saveDir": save_dir,
                                            })
                                        } else {
                                            let line = if cur_tab == 1 { sel_pub } else { pst_pub };
                                            json!({
                                                "sid": sid,
                                                "mode": "paste",
                                                "publicKey": line,
                                            })
                                        };
                                        self.busy = true;
                                        let engine = Arc::clone(&self.engine);
                                        let tx = self.tx.clone();
                                        self.rt.spawn(async move {
                                            match engine.add_key(body).await {
                                                Ok(v) => {
                                                    if let Some(sess) = v.get("session") {
                                                        let _ = tx.send(BgEvent::SessionUpdated(
                                                            sess.clone(),
                                                        ));
                                                    }
                                                    let _ = tx.send(BgEvent::Log(
                                                        "已成功添加公钥至服务器".into(),
                                                        LogKind::Ok,
                                                    ));
                                                }
                                                Err(e) => {
                                                    let msg = e
                                                        .get("error")
                                                        .and_then(|x| x.as_str())
                                                        .unwrap_or("添加公钥失败");
                                                    let _ = tx.send(BgEvent::Log(
                                                        msg.to_string(),
                                                        LogKind::Err,
                                                    ));
                                                }
                                            }
                                        });
                                        self.trigger_local_scan(true);
                                        close_drawer = true;
                                    }
                                }
                                if Self::action_btn(ui, &p, "取消", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::ReplaceKey {
                        old_index,
                        old_comment,
                        old_fp,
                        tab,
                        algo,
                        label,
                        comment,
                        passphrase,
                        selected_priv_path,
                    } => {
                        ui.label(
                            RichText::new(format!(
                                "↻ 安全更换密钥 — #{} {}",
                                *old_index + 1,
                                if old_comment.is_empty() {
                                    old_fp.as_str()
                                } else {
                                    old_comment.as_str()
                                }
                            ))
                            .size(15.0)
                            .strong()
                            .color(p.text),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new("三步安全闭环：先追加新公钥 → 用新私钥建立独立 SSH 连接验证可登录 → 验证通过后再移除旧公钥。")
                                .size(11.5)
                                .color(p.dim),
                        );
                        ui.add_space(6.0);

                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(*tab == 0, "生成新密钥对并替换")
                                .clicked()
                            {
                                *tab = 0;
                            }
                            if ui
                                .selectable_label(*tab == 1, "使用本机已有私钥替换")
                                .clicked()
                            {
                                *tab = 1;
                            }
                        });
                        ui.add_space(6.0);

                        if *tab == 0 {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("算法:").color(p.dim));
                                for a in ["ed25519", "rsa"] {
                                    if ui.selectable_label(algo == a, a.to_uppercase()).clicked() {
                                        *algo = a.to_string();
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(RichText::new("私钥保存名称标签:").size(12.0).color(p.dim));
                            ui.add(
                                egui::TextEdit::singleline(label).desired_width(f32::INFINITY),
                            );
                            ui.add_space(4.0);
                            ui.label(RichText::new("新公钥注释:").size(12.0).color(p.dim));
                            ui.add(
                                egui::TextEdit::singleline(comment).desired_width(f32::INFINITY),
                            );
                            ui.add_space(4.0);
                            ui.label(RichText::new("新私钥口令 (可选):").size(12.0).color(p.dim));
                            ui.add(
                                egui::TextEdit::singleline(passphrase)
                                    .password(true)
                                    .desired_width(f32::INFINITY),
                            );
                        } else {
                            ui.label(RichText::new("本机私钥文件路径:").size(12.0).color(p.dim));
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(selected_priv_path)
                                        .desired_width(380.0),
                                );
                                if ui.button("浏览…").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                                        *selected_priv_path = path.to_string_lossy().to_string();
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(RichText::new("该私钥解密口令 (可选):").size(12.0).color(p.dim));
                            ui.add(
                                egui::TextEdit::singleline(passphrase)
                                    .password(true)
                                    .desired_width(f32::INFINITY),
                            );
                        }

                        ui.add_space(12.0);
                        let cur_tab = *tab;
                        let al = algo.clone();
                        let lb = label.trim().to_string();
                        let cm = comment.trim().to_string();
                        let pp = passphrase.clone();
                        let sp = selected_priv_path.trim().to_string();
                        let ofp = old_fp.clone();

                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "执行验证并替换", true, false)
                                    .clicked()
                                {
                                    if let Some(sid) = self.current_sid() {
                                        let save_dir = self.current_login_key_dir().unwrap_or_else(|| {
                                            self.engine.state().current_settings().save_dir
                                        });
                                        let body = if cur_tab == 0 {
                                            json!({
                                                "sid": sid,
                                                "oldFingerprint": ofp,
                                                "mode": "generate",
                                                "generator": al,
                                                "label": if lb.is_empty() { "replaced".to_string() } else { lb },
                                                "comment": cm,
                                                "passphrase": pp,
                                                "saveDir": save_dir,
                                            })
                                        } else {
                                            json!({
                                                "sid": sid,
                                                "oldFingerprint": ofp.clone(),
                                                "mode": "local",
                                                "path": sp,
                                                "passphrase": pp,
                                            })
                                        };
                                        self.busy = true;
                                        let engine = Arc::clone(&self.engine);
                                        let tx = self.tx.clone();
                                        self.rt.spawn(async move {
                                            match engine.replace_key(body).await {
                                                Ok(v) => {
                                                    if let Some(sess) = v.get("session") {
                                                        let _ = tx.send(BgEvent::SessionUpdated(
                                                            sess.clone(),
                                                        ));
                                                    }
                                                    let _ = tx.send(BgEvent::Log(
                                                        "新密钥验证通过并已完成替换".into(),
                                                        LogKind::Ok,
                                                    ));
                                                }
                                                Err(e) => {
                                                    let msg = e
                                                        .get("error")
                                                        .and_then(|x| x.as_str())
                                                        .unwrap_or("更换密钥失败");
                                                    let _ = tx.send(BgEvent::Log(
                                                        msg.to_string(),
                                                        LogKind::Err,
                                                    ));
                                                }
                                            }
                                        });
                                        self.trigger_local_scan(true);
                                        close_drawer = true;
                                    }
                                }
                                if Self::action_btn(ui, &p, "取消", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::EditNote {
                        fingerprint,
                        comment,
                        note,
                    } => {
                        ui.label(
                            RichText::new("✎ 编辑公钥归属备注")
                                .size(15.0)
                                .strong()
                                .color(p.text),
                        );
                        ui.separator();
                        if !comment.is_empty() {
                            ui.label(RichText::new(comment.as_str()).size(12.5).color(p.text));
                        }
                        ui.label(
                            RichText::new(fingerprint.as_str())
                                .size(11.5)
                                .monospace()
                                .color(p.dim),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::TextEdit::singleline(note)
                                .hint_text("输入便于识别的用途或持有人备注（仅存本机）…")
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(12.0);
                        let fp_val = fingerprint.clone();
                        let nt_val = note.trim().to_string();
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "保存备注", true, false).clicked() {
                                    let _ = self.engine.set_key_note(&fp_val, &nt_val);
                                    self.reload_store_data();
                                    self.push_log("已更新公钥备注", LogKind::Ok);
                                    close_drawer = true;
                                }
                                if Self::action_btn(ui, &p, "取消", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::ConfirmDelete {
                        fingerprints,
                        message,
                    } => {
                        ui.label(
                            RichText::new("确认强制删除公钥")
                                .size(15.0)
                                .strong()
                                .color(p.danger),
                        );
                        ui.separator();
                        ui.label(RichText::new(message.as_str()).size(12.5).color(p.text));
                        ui.add_space(8.0);
                        ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                            for fp in fingerprints.iter() {
                                ui.label(
                                    RichText::new(fp.as_str())
                                        .size(11.0)
                                        .monospace()
                                        .color(p.dim),
                                );
                            }
                        });
                        ui.add_space(12.0);
                        let fps = fingerprints.clone();
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "强制删除", false, true).clicked() {
                                    self.delete_keys_by_fingerprints(fps, true);
                                    close_drawer = true;
                                }
                                if Self::action_btn(ui, &p, "取消", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::Backups {
                        items,
                        selected_name,
                        diff,
                    } => {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("远端 authorized_keys 历史备份")
                                    .size(15.0)
                                    .strong()
                                    .color(p.text),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "关闭", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                        ui.separator();

                        ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                            if items.is_empty() {
                                ui.label(
                                    RichText::new("当前远端尚无历史备份文件")
                                        .size(12.5)
                                        .color(p.dim),
                                );
                            }
                            for b in items.iter() {
                                let name = b
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let mtime = b.get("mtime").and_then(|v| v.as_str()).unwrap_or("");
                                Frame::NONE
                                    .fill(p.panel)
                                    .stroke(Stroke::new(1.0_f32, p.border))
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(Margin::symmetric(12, 8))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new(&name)
                                                        .size(12.5)
                                                        .monospace()
                                                        .strong()
                                                        .color(p.text),
                                                );
                                                ui.label(
                                                    RichText::new(mtime).size(11.0).color(p.dim),
                                                );
                                            });
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if Self::action_btn(
                                                        ui, &p, "恢复此备份", false, false,
                                                    )
                                                    .clicked()
                                                    {
                                                        if let Some(sid) = self.current_sid() {
                                                            self.busy = true;
                                                            let engine = Arc::clone(&self.engine);
                                                            let tx = self.tx.clone();
                                                            let nm = name.clone();
                                                            self.rt.spawn(async move {
                                                                let body = json!({
                                                                    "sid": sid,
                                                                    "name": nm,
                                                                });
                                                                match engine
                                                                    .restore_backup(body)
                                                                    .await
                                                                {
                                                                    Ok(v) => {
                                                                        if let Some(sess) =
                                                                            v.get("session")
                                                                        {
                                                                            let _ = tx.send(BgEvent::SessionUpdated(sess.clone()));
                                                                        }
                                                                        let _ = tx.send(
                                                                            BgEvent::Log(
                                                                                "已从备份恢复远端公钥"
                                                                                    .into(),
                                                                                LogKind::Ok,
                                                                            ),
                                                                        );
                                                                    }
                                                                    Err(e) => {
                                                                        let msg = e
                                                                            .get("error")
                                                                            .and_then(|x| {
                                                                                x.as_str()
                                                                            })
                                                                            .unwrap_or(
                                                                                "恢复备份失败",
                                                                            );
                                                                        let _ = tx.send(
                                                                            BgEvent::Log(
                                                                                msg.to_string(),
                                                                                LogKind::Err,
                                                                            ),
                                                                        );
                                                                    }
                                                                }
                                                            });
                                                            close_drawer = true;
                                                        }
                                                    }
                                                    if Self::action_btn(
                                                        ui, &p, "对比差异", false, false,
                                                    )
                                                    .clicked()
                                                    {
                                                        if let Some(sid) = self.current_sid() {
                                                            let engine = Arc::clone(&self.engine);
                                                            let tx = self.tx.clone();
                                                            let nm = name.clone();
                                                            self.rt.spawn(async move {
                                                                if let Ok(d) = engine
                                                                    .backup_diff(&sid, &nm)
                                                                    .await
                                                                {
                                                                    let _ = tx.send(
                                                                        BgEvent::BackupDiffLoaded(
                                                                            nm, d,
                                                                        ),
                                                                    );
                                                                }
                                                            });
                                                        }
                                                    }
                                                },
                                            );
                                        });
                                    });
                                ui.add_space(6.0);
                            }
                        });

                        if let Some(d) = diff.as_ref() {
                            ui.separator();
                            ui.label(
                                RichText::new(format!("与当前文件对比 ({selected_name}):"))
                                    .size(12.0)
                                    .strong()
                                    .color(p.text),
                            );
                            let summary = serde_json::to_string_pretty(d).unwrap_or_default();
                            ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                                ui.label(
                                    RichText::new(summary)
                                        .size(11.0)
                                        .monospace()
                                        .color(p.dim),
                                );
                            });
                        }
                    }

                    Drawer::Sshd { info } => {
                        ui.label(
                            RichText::new("远端 SSHD 安全加固")
                                .size(15.0)
                                .strong()
                                .color(p.text),
                        );
                        ui.separator();

                        let key_only = info
                            .get("keyOnly")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let pub_auth = info
                            .get("pubkeyAuthentication")
                            .and_then(|v| v.as_str())
                            .unwrap_or("yes");
                        let pwd_auth = info
                            .get("passwordAuthentication")
                            .and_then(|v| v.as_str())
                            .unwrap_or("yes");

                        ui.label(
                            RichText::new(format!(
                                "PubkeyAuthentication: {pub_auth}  |  PasswordAuthentication: {pwd_auth}"
                            ))
                            .size(12.5)
                            .monospace()
                            .color(p.text),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("开启「仅密钥登录」将在校验 sshd -t 无误后关闭密码认证并平滑重载 sshd，防暴力破解。")
                                .size(11.5)
                                .color(p.dim),
                        );

                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let target_enable = !key_only;
                                let lbl = if target_enable {
                                    "开启「仅密钥登录」(禁用密码)"
                                } else {
                                    "恢复允许密码登录"
                                };
                                if Self::action_btn(ui, &p, lbl, target_enable, !target_enable)
                                    .clicked()
                                {
                                    if let Some(sid) = self.current_sid() {
                                        self.busy = true;
                                        let engine = Arc::clone(&self.engine);
                                        let tx = self.tx.clone();
                                        self.rt.spawn(async move {
                                            match engine.set_key_only(&sid, target_enable).await {
                                                Ok(v) => {
                                                    let _ = tx.send(BgEvent::SshdLoaded {
                                                        info: v,
                                                        open_drawer: false,
                                                    });
                                                    let _ = tx.send(BgEvent::Log(
                                                        "已更新远端 SSHD 登录策略".into(),
                                                        LogKind::Ok,
                                                    ));
                                                }
                                                Err(e) => {
                                                    let msg = e
                                                        .get("error")
                                                        .and_then(|x| x.as_str())
                                                        .unwrap_or("修改 SSHD 配置失败");
                                                    let _ = tx.send(BgEvent::Log(
                                                        msg.to_string(),
                                                        LogKind::Err,
                                                    ));
                                                }
                                            }
                                        });
                                        close_drawer = true;
                                    }
                                }
                                if Self::action_btn(ui, &p, "关闭", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::Sync {
                        client_id,
                        passphrase,
                        remember_key,
                        device_prompt,
                        conflicts,
                    } => {
                        ui.label(
                            RichText::new("微软 OneDrive 端到端加密云同步")
                                .size(15.0)
                                .strong()
                                .color(p.text),
                        );
                        ui.separator();

                        let logged_in = self
                            .ms_status
                            .get("loggedIn")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let account = self
                            .ms_status
                            .get("account")
                            .and_then(|a| a.get("displayName").or_else(|| a.get("userPrincipalName")))
                            .and_then(|v| v.as_str())
                            .unwrap_or("已登录微软账号");

                        if logged_in {
                            ui.label(
                                RichText::new(format!("● 当前账号: {account}"))
                                    .size(12.5)
                                    .strong()
                                    .color(p.ok),
                            );
                        } else {
                            ui.label(
                                RichText::new("Azure App Client ID (可选，留空用默认):").color(p.dim),
                            );
                            ui.add(
                                egui::TextEdit::singleline(client_id)
                                    .desired_width(f32::INFINITY),
                            );
                        }

                        if let Some((code, uri)) = device_prompt.as_ref() {
                            ui.add_space(6.0);
                            Frame::NONE
                                .fill(p.surface2)
                                .stroke(Stroke::new(1.0_f32, p.accent))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("设备登录码: {code}"))
                                            .size(14.0)
                                            .monospace()
                                            .strong()
                                            .color(p.accent),
                                    );
                                    if ui.button("复制验证码并打开浏览器").clicked() {
                                        self.copy_to_clipboard(code, "微软设备验证码");
                                        let _ = open::that(uri);
                                    }
                                });
                        }

                        ui.add_space(8.0);
                        ui.label(RichText::new("端到端加密口令:").size(12.0).color(p.dim));
                        ui.add(
                            egui::TextEdit::singleline(passphrase)
                                .password(true)
                                .hint_text("用于 Argon2id + AES-256-GCM 加密主机与备注元数据")
                                .desired_width(f32::INFINITY),
                        );
                        ui.checkbox(remember_key, "在本机记住加密口令");

                        if !conflicts.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!(
                                    "⚠ 发现 {} 项主机指纹冲突，点击同步将信任新指纹",
                                    conflicts.len()
                                ))
                                .size(11.5)
                                .color(p.warn),
                            );
                        }

                        ui.add_space(12.0);
                        let cid_val = client_id.trim().to_string();
                        let pp_val = passphrase.clone();
                        let rem = *remember_key;
                        ui.horizontal(|ui| {
                            if !logged_in {
                                if Self::action_btn(ui, &p, "登录微软账号", true, false).clicked()
                                {
                                    let engine = Arc::clone(&self.engine);
                                    let tx = self.tx.clone();
                                    self.rt.spawn(async move {
                                        let cid_opt = if cid_val.is_empty() {
                                            None
                                        } else {
                                            Some(cid_val.as_str())
                                        };
                                        match engine.ms_start_login(cid_opt).await {
                                            Ok(flow) => {
                                                let uc = flow
                                                    .get("userCode")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                let uri = flow
                                                    .get("verificationUri")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("https://microsoft.com/devicelogin")
                                                    .to_string();
                                                let dc = flow
                                                    .get("deviceCode")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                let _ = tx.send(BgEvent::SyncDeviceCode {
                                                    user_code: uc,
                                                    verify_uri: uri.clone(),
                                                });
                                                let _ = open::that(&uri);
                                                if let Ok(st) =
                                                    engine.ms_complete_login(&dc, 300).await
                                                {
                                                    let _ = tx.send(BgEvent::SyncStatus(st));
                                                }
                                            }
                                            Err(e) => {
                                                let _ = tx.send(BgEvent::Log(e, LogKind::Err));
                                            }
                                        }
                                    });
                                }
                            } else {
                                if Self::action_btn(ui, &p, "↻ 立即双向同步", true, false)
                                    .clicked()
                                {
                                    if rem && !pp_val.is_empty() {
                                        let _ = self.engine.ms_save_config(None, Some(&pp_val));
                                    }
                                    self.busy = true;
                                    let engine = Arc::clone(&self.engine);
                                    let tx = self.tx.clone();
                                    self.rt.spawn(async move {
                                        match engine.ms_sync(&pp_val, &[], false).await {
                                            Ok(rep) => {
                                                let st = engine.ms_status();
                                                let _ = tx.send(BgEvent::SyncDone {
                                                    report: rep,
                                                    status: st,
                                                });
                                            }
                                            Err(e) => {
                                                let _ = tx.send(BgEvent::Log(e, LogKind::Err));
                                            }
                                        }
                                    });
                                }
                                if Self::action_btn(ui, &p, "退出账号", false, true).clicked() {
                                    let _ = self.engine.ms_logout();
                                    self.ms_status = self.engine.ms_status();
                                }
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "关闭", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }

                    Drawer::Settings {
                        theme_choice,
                        local_key_dir,
                        save_dir,
                        remote_path,
                    } => {
                        ui.label(
                            RichText::new("全局外观与路径设置")
                                .size(15.5)
                                .strong()
                                .color(p.text),
                        );
                        ui.separator();

                        ui.label(RichText::new("配色主题:").color(p.dim));
                        ui.horizontal_wrapped(|ui| {
                            for (tid, tname) in THEME_CHOICES {
                                if ui
                                    .selectable_label(theme_choice.as_str() == tid, tname)
                                    .clicked()
                                {
                                    *theme_choice = tid.to_string();
                                    self.theme_choice = tid.to_string();
                                    let sys_dark = is_system_dark();
                                    self.palette = resolve_palette(&self.theme_choice, sys_dark);
                                    apply_palette(ctx, &self.palette);
                                    let _ = save_theme(&self.data_dir, &self.theme_choice);
                                }
                            }
                        });

                        ui.add_space(8.0);
                        ui.label(RichText::new("本机密钥扫描目录:").color(p.dim));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(local_key_dir).desired_width(380.0),
                            );
                            if ui.button("选择目录…").clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    *local_key_dir = dir.to_string_lossy().to_string();
                                }
                            }
                        });

                        ui.add_space(6.0);
                        ui.label(RichText::new("生成密钥默认保存目录:").color(p.dim));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(save_dir).desired_width(380.0));
                            if ui.button("选择目录…").clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    *save_dir = dir.to_string_lossy().to_string();
                                }
                            }
                        });

                        ui.add_space(6.0);
                        ui.label(RichText::new("远端默认 authorized_keys 路径:").color(p.dim));
                        ui.add(
                            egui::TextEdit::singleline(remote_path).desired_width(f32::INFINITY),
                        );

                        ui.add_space(12.0);
                        let lkd = local_key_dir.trim().to_string();
                        let sd = save_dir.trim().to_string();
                        let rp = remote_path.trim().to_string();
                        ui.horizontal(|ui| {
                            if Self::action_btn(ui, &p, "打开数据目录", false, false).clicked() {
                                let _ = open::that(&self.data_dir);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if Self::action_btn(ui, &p, "保存设置", true, false).clicked() {
                                    let _ = save_theme(&self.data_dir, &self.theme_choice);
                                    let _ = self.engine.state().store.update_settings(
                                        SettingsPatch {
                                            local_key_dir: Some(lkd),
                                            save_dir: Some(sd),
                                            remote_path: Some(rp),
                                        },
                                    );
                                    self.trigger_local_scan(true);
                                    self.push_log("全局设置已保存", LogKind::Ok);
                                    close_drawer = true;
                                }
                                if Self::action_btn(ui, &p, "关闭", false, false).clicked() {
                                    close_drawer = true;
                                }
                            });
                        });
                    }
                }

                if !close_drawer {
                    self.drawer = current;
                }
            });
    }

    fn action_btn(
        ui: &mut egui::Ui,
        p: &Palette,
        label: &str,
        primary: bool,
        danger: bool,
    ) -> egui::Response {
        let (fill, stroke, fg) = if primary {
            (p.accent, Stroke::new(1.0_f32, p.accent), Color32::WHITE)
        } else if danger {
            (
                p.danger.gamma_multiply(0.14),
                Stroke::new(1.0_f32, p.danger.gamma_multiply(0.45)),
                p.danger,
            )
        } else {
            (p.surface3, Stroke::new(1.0_f32, p.border), p.text)
        };
        ui.add(
            egui::Button::new(RichText::new(label).size(12.0).strong().color(fg))
                .fill(fill)
                .stroke(stroke)
                .corner_radius(CornerRadius::same(7))
                .min_size(Vec2::new(0.0, 28.0)),
        )
    }
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let cjk_candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ];
    for path in cjk_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "sys_cjk".to_owned(),
                Arc::new(FontData::from_owned(bytes)),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "sys_cjk".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("sys_cjk".to_owned());
            break;
        }
    }

    let mono_candidates = [
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\consola.ttf",
    ];
    for path in mono_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "sys_mono".to_owned(),
                Arc::new(FontData::from_owned(bytes)),
            );
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "sys_mono".to_owned());
            break;
        }
    }

    let sym_candidates = [r"C:\Windows\Fonts\seguisym.ttf"];
    for (idx, path) in sym_candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let key = format!("sys_sym_{idx}");
            fonts
                .font_data
                .insert(key.clone(), Arc::new(FontData::from_owned(bytes)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push(key.clone());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push(key);
        }
    }

    ctx.set_fonts(fonts);
}
