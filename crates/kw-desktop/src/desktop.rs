//! 桌面壳的杂活：数据目录、主题/视图持久化、窗口状态、原生选择器与剪贴板。
//!
//! 页面永远从内置的 127.0.0.1 服务加载（与旧 Electron 版同一套前端），
//! 壳只负责窗口和系统能力，业务全在 kw-server 里。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use serde_json::{json, Value};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Manager, PhysicalPosition, State, Url, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

/// 与旧版 `electron/main.js` 的 THEME_CHOICES 一致
const THEMES: [&str; 8] = [
    "obsidian",
    "midnight",
    "graphite",
    "cloud",
    "linen",
    "mist",
    "auto",
    "auto-soft",
];

const DATA_FILES: [&str; 3] = ["config.json", "known_hosts.json", "key-notes.json"];

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 820;
const MIN_WIDTH: u32 = 1024;
const MIN_HEIGHT: u32 = 680;

pub struct Desktop {
    pub data_dir: PathBuf,
    theme: RwLock<String>,
    ui: RwLock<Value>,
    server: Mutex<Option<kw_server::RunningServer>>,
}

impl Desktop {
    pub fn load(data_dir: PathBuf) -> Self {
        let theme = read_theme(&data_dir);
        let ui = read_ui(&data_dir);
        Self {
            data_dir,
            theme: RwLock::new(theme),
            ui: RwLock::new(ui),
            server: Mutex::new(None),
        }
    }

    fn theme(&self) -> String {
        self.theme.read().map(|v| v.clone()).unwrap_or_default()
    }

    fn ui(&self) -> Value {
        self.ui
            .read()
            .map(|v| v.clone())
            .unwrap_or_else(|_| default_ui())
    }

    /// 注入到页面最前面的脚本：先把主题/视图/会话令牌同步塞进 window，再挂桌面桥。
    ///
    /// 令牌只在这里出现一次：bridge.js 立刻把它写进当前 origin 的 Cookie，
    /// 然后从 `window.__KW_INIT` 上删掉，页面脚本读不到它。
    fn init_script(&self, token: &str) -> String {
        let payload = json!({ "theme": self.theme(), "ui": self.ui(), "token": token });
        format!(
            "window.__KW_INIT = {payload};\n{}",
            include_str!("../bridge.js")
        )
    }

    pub fn put_server(&self, server: kw_server::RunningServer) {
        if let Ok(mut slot) = self.server.lock() {
            *slot = Some(server);
        }
    }

    pub fn take_server(&self) -> Option<kw_server::RunningServer> {
        self.server.lock().ok().and_then(|mut slot| slot.take())
    }
}

fn default_ui() -> Value {
    json!({ "mode": "keys", "termHeight": 0 })
}

/* ------------------------------------------------------------------ */
/* 数据目录                                                            */
/* ------------------------------------------------------------------ */

/// `KW_DATA_DIR` 优先；否则与旧 Electron 版同址：
/// `app.getPath('userData')` 取 package name，即 `%APPDATA%\ssh-keywarden\data`。
pub fn resolve_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KW_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(base) = std::env::var_os("APPDATA") {
        return PathBuf::from(base).join("ssh-keywarden").join("data");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".ssh-keywarden");
    }
    PathBuf::from("data")
}

/// 目标目录还没有 config.json 时，从安装目录旁的 `data` 复制三个数据文件。
/// 便携版（exe 旁边带 data）升级到正式安装时靠这一步接管旧配置。
fn migrate_data(target: &Path, candidate: &Path) {
    if target.join("config.json").exists() || !candidate.join("config.json").exists() {
        return;
    }
    if candidate == target {
        return;
    }
    let _ = fs::create_dir_all(target);
    for name in DATA_FILES {
        let from = candidate.join(name);
        if !from.is_file() {
            continue;
        }
        let _ = fs::copy(&from, target.join(name));
    }
}

fn exe_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("data");
    dir.is_dir().then_some(dir)
}

/* ------------------------------------------------------------------ */
/* 主题 / 视图                                                         */
/* ------------------------------------------------------------------ */

fn read_theme(data_dir: &Path) -> String {
    let raw = fs::read_to_string(data_dir.join("theme.json")).unwrap_or_default();
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let choice = value.get("choice").and_then(|v| v.as_str()).unwrap_or("");
    if THEMES.contains(&choice) {
        choice.to_string()
    } else {
        String::new()
    }
}

fn read_ui(data_dir: &Path) -> Value {
    let raw = fs::read_to_string(data_dir.join("ui.json")).unwrap_or_default();
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mode = match value.get("mode").and_then(|v| v.as_str()) {
        Some("term") => "term",
        _ => "keys",
    };
    let term_height = value
        .get("termHeight")
        .and_then(|v| v.as_f64())
        .filter(|h| *h == 0.0 || (*h >= 180.0 && *h <= 10_000.0))
        .map(|h| h.round() as i64)
        .unwrap_or(0);
    json!({ "mode": mode, "termHeight": term_height })
}

fn write_json_atomic(file: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", file.display()));
    fs::write(&tmp, serde_json::to_vec(value)?)?;
    fs::rename(&tmp, file)
}

/* ------------------------------------------------------------------ */
/* 窗口状态                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy)]
struct Bounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    /// 保存时的缩放比：尺寸/坐标都是物理像素，还原成逻辑值要用它
    scale: f64,
}

fn load_bounds(data_dir: &Path) -> Option<Bounds> {
    let raw = fs::read_to_string(data_dir.join("window-state.json")).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let num = |key: &str| v.get(key).and_then(|x| x.as_f64());
    let (width, height) = (num("width")?, num("height")?);
    if width < MIN_WIDTH as f64 || height < MIN_HEIGHT as f64 {
        return None;
    }
    let scale = num("scale").filter(|s| *s > 0.0).unwrap_or(1.0);
    Some(Bounds {
        x: num("x")? as i32,
        y: num("y")? as i32,
        width: width as u32,
        height: height as u32,
        scale,
    })
}

/// 上次的位置可能落在一块已经拔掉的副屏上；无边框窗口没有系统标题栏可拖，
/// 落在可视区外就救不回来了，所以把坐标夹回最近的显示器。
fn clamp_to_monitor(app: &AppHandle, b: Bounds) -> Bounds {
    let monitors = match app.available_monitors() {
        Ok(list) if !list.is_empty() => list,
        _ => return b,
    };
    let cx = b.x as i64 + b.width as i64 / 2;
    let cy = b.y as i64 + b.height as i64 / 2;
    let mut best = monitors[0].clone();
    let mut best_dist = i64::MAX;
    for m in monitors {
        let left = m.position().x as i64;
        let top = m.position().y as i64;
        let right = left + m.size().width as i64;
        let bottom = top + m.size().height as i64;
        let dx = if cx < left {
            left - cx
        } else if cx > right {
            cx - right
        } else {
            0
        };
        let dy = if cy < top {
            top - cy
        } else if cy > bottom {
            cy - bottom
        } else {
            0
        };
        let dist = dx * dx + dy * dy;
        if dist < best_dist {
            best_dist = dist;
            best = m.clone();
        }
    }

    let left = best.position().x as i64;
    let top = best.position().y as i64;
    let width = (b.width as i64).min(best.size().width as i64) as u32;
    let height = (b.height as i64).min(best.size().height as i64) as u32;
    let max_x = left + best.size().width as i64 - width as i64;
    let max_y = top + best.size().height as i64 - height as i64;
    Bounds {
        x: (b.x as i64).clamp(left, max_x.max(left)) as i32,
        y: (b.y as i64).clamp(top, max_y.max(top)) as i32,
        width,
        height,
        scale: b.scale,
    }
}

fn save_window_state(window: &tauri::Window, data_dir: &Path) {
    // 最大化时拿到的是整屏尺寸，存下去下次就不是「上次的大小」了，宁可不写。
    if window.is_maximized().unwrap_or(false) {
        return;
    }
    let (Ok(pos), Ok(size), Ok(scale)) = (
        window.outer_position(),
        window.inner_size(),
        window.scale_factor(),
    ) else {
        return;
    };
    let value = json!({
        "x": pos.x,
        "y": pos.y,
        "width": size.width,
        "height": size.height,
        "scale": scale,
    });
    let _ = fs::create_dir_all(data_dir);
    let _ = write_json_atomic(&data_dir.join("window-state.json"), &value);
}

/* ------------------------------------------------------------------ */
/* 启动                                                                */
/* ------------------------------------------------------------------ */

pub fn boot(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = resolve_data_dir();
    fs::create_dir_all(&data_dir)?;
    if let Some(candidate) = exe_data_dir() {
        migrate_data(&data_dir, &candidate);
    }
    std::env::set_var("KW_DATA_DIR", &data_dir);
    // 桌面端不能靠「访问首页就发 Cookie」：本机任何进程扫到端口都能白拿会话。
    // 令牌由下面注入的 init 脚本写进 WebView 自己的 Cookie，服务端只认它。
    std::env::set_var("KW_NO_AUTO_COOKIE", "1");
    std::env::set_var("KW_SCAN_CACHE_MS", "60000");

    let desktop = Desktop::load(data_dir);

    let options = kw_server::ServeOptions {
        port: 0,
        public_dir: public_dir(app),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let server = tauri::async_runtime::block_on(kw_server::serve(options))?;
    let script = desktop.init_script(&server.token);
    let url = Url::parse(&format!("{}/", server.url()))?;
    desktop.put_server(server);
    let restore = load_bounds(&desktop.data_dir);
    app.manage(desktop);

    // 页面是我们自己起的本地服务，除它之外不许跳转，外链交给系统浏览器
    let origin = url.origin().ascii_serialization();
    let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("SSH Keywarden")
        .decorations(false)
        .visible(false)
        .background_color(tauri::window::Color(0x0d, 0x11, 0x17, 255))
        .initialization_script(script)
        .on_navigation(move |target| target.origin().ascii_serialization() == origin)
        .on_new_window(|target, _features| {
            if matches!(target.scheme(), "http" | "https") {
                let _ = tauri_plugin_opener::open_url(target.as_str(), None::<&str>);
            }
            NewWindowResponse::Deny
        })
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Started {
                let _ = window.show();
            }
        });

    let (width, height) = match restore {
        Some(b) => (b.width as f64 / b.scale, b.height as f64 / b.scale),
        None => (DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64),
    };
    builder = builder
        .inner_size(width, height)
        .min_inner_size(MIN_WIDTH as f64, MIN_HEIGHT as f64);

    let window = builder.build()?;

    if let Some(b) = restore {
        let b = clamp_to_monitor(app.handle(), b);
        let _ = window.set_position(PhysicalPosition::new(b.x, b.y));
    }

    Ok(())
}

/// 开发态读源码树里的 assets，装好之后读安装包 resources 里的 public。
fn public_dir(app: &tauri::App) -> PathBuf {
    if let Ok(dir) = app.path().resource_dir() {
        let candidate = dir.join("public");
        if candidate.join("index.html").is_file() {
            return candidate;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/public")
}

pub fn on_window_event(window: &tauri::Window, event: &WindowEvent, data_dir: &Path) {
    if matches!(event, WindowEvent::CloseRequested { .. }) {
        save_window_state(window, data_dir);
    }
}

/* ------------------------------------------------------------------ */
/* 命令                                                                */
/* ------------------------------------------------------------------ */

/// 弹原生选择框；`dir=true` 选目录，否则选文件。取消返回空串。
async fn pick(app: AppHandle, opts: Option<Value>, dir: bool) -> Result<String, String> {
    let default_title = if dir {
        "选择目录"
    } else {
        "选择私钥文件"
    };
    let title = opts
        .as_ref()
        .and_then(|o| o.get("title"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_title)
        .to_string();
    let start = opts
        .as_ref()
        .and_then(|o| o.get("start"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut dialog = app.dialog().file().set_title(title);
    if !start.is_empty() && Path::new(&start).exists() {
        dialog = dialog.set_directory(start);
    }
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    if dir {
        dialog.pick_folder(move |picked| {
            let _ = tx.send(picked);
        });
    } else {
        dialog.pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    }

    let picked = rx.await.map_err(|_| "选择框没有返回结果".to_string())?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
        .unwrap_or_default())
}

#[tauri::command]
pub async fn pick_file(app: AppHandle, opts: Option<Value>) -> Result<String, String> {
    pick(app, opts, false).await
}

#[tauri::command]
pub async fn pick_dir(app: AppHandle, opts: Option<Value>) -> Result<String, String> {
    pick(app, opts, true).await
}

#[tauri::command]
pub fn open_data_dir(app: AppHandle, desktop: State<'_, Desktop>) -> Result<String, String> {
    let dir = desktop.data_dir.clone();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())?;
    Ok(String::new())
}

#[tauri::command]
pub fn is_maximized(window: WebviewWindow) -> Result<bool, String> {
    window.is_maximized().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn toggle_maximize(window: WebviewWindow) -> Result<bool, String> {
    let maximized = window.is_maximized().map_err(|e| e.to_string())?;
    if maximized {
        window.unmaximize().map_err(|e| e.to_string())?;
    } else {
        window.maximize().map_err(|e| e.to_string())?;
    }
    Ok(!maximized)
}

#[tauri::command]
pub fn read_clipboard(app: AppHandle) -> Result<String, String> {
    app.clipboard().read_text().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

/// 前端 `theme.js` 直接把它交给 `setSystemDark(dark)` 并做 `dark === systemDark` 比较，
/// 必须是布尔值；返回 `"light"` 这种字符串会被 `Boolean()` 当成 true，浅色系统永远判成深色。
#[tauri::command]
pub fn system_theme(window: WebviewWindow) -> Result<bool, String> {
    let theme = window.theme().map_err(|e| e.to_string())?;
    Ok(matches!(theme, tauri::Theme::Dark))
}

#[tauri::command]
pub fn theme_set(desktop: State<'_, Desktop>, choice: String) -> Result<String, String> {
    if !THEMES.contains(&choice.as_str()) {
        return Err("未知主题".into());
    }
    if let Ok(mut slot) = desktop.theme.write() {
        *slot = choice.clone();
    }
    write_json_atomic(
        &desktop.data_dir.join("theme.json"),
        &json!({ "choice": choice }),
    )
    .map_err(|e| format!("无法保存主题：{e}"))?;
    Ok(choice)
}

#[tauri::command]
pub fn ui_set(desktop: State<'_, Desktop>, patch: Value) -> Result<Value, String> {
    let mut next = desktop.ui();
    let Some(patch) = patch.as_object() else {
        return Ok(next);
    };
    if let Some(mode) = patch.get("mode").and_then(|v| v.as_str()) {
        if mode == "keys" || mode == "term" {
            next["mode"] = Value::String(mode.to_string());
        }
    }
    if let Some(h) = patch.get("termHeight").and_then(|v| v.as_f64()) {
        if h == 0.0 || (180.0..=10_000.0).contains(&h) {
            next["termHeight"] = json!(h.round() as i64);
        }
    }
    if let Ok(mut slot) = desktop.ui.write() {
        *slot = next.clone();
    }
    write_json_atomic(&desktop.data_dir.join("ui.json"), &next)
        .map_err(|_| "无法保存视图设置".to_string())?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_tauri_conf_and_changelog() {
        let version = env!("CARGO_PKG_VERSION");
        let conf: Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json 应能解析");
        assert_eq!(
            conf["version"].as_str(),
            Some(version),
            "tauri.conf.json 的 version 与 Cargo.toml 不一致"
        );

        let changelog = include_str!("../../../CHANGELOG.md");
        assert!(
            changelog.contains(&format!("## v{version}")),
            "CHANGELOG.md 缺少 v{version} 段"
        );
    }

    #[test]
    fn theme_file_accepts_known_choice_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_theme(dir.path()), "");

        write_json_atomic(
            &dir.path().join("theme.json"),
            &json!({ "choice": "linen" }),
        )
        .unwrap();
        assert_eq!(read_theme(dir.path()), "linen");

        write_json_atomic(&dir.path().join("theme.json"), &json!({ "choice": "pink" })).unwrap();
        assert_eq!(read_theme(dir.path()), "");

        fs::write(dir.path().join("theme.json"), "not json").unwrap();
        assert_eq!(read_theme(dir.path()), "");
    }

    #[test]
    fn ui_file_falls_back_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_ui(dir.path()), default_ui());

        write_json_atomic(
            &dir.path().join("ui.json"),
            &json!({ "mode": "term", "termHeight": 420.4 }),
        )
        .unwrap();
        assert_eq!(
            read_ui(dir.path()),
            json!({ "mode": "term", "termHeight": 420 })
        );

        // 低于下限的高度当没设；未知模式退回密钥视图
        write_json_atomic(
            &dir.path().join("ui.json"),
            &json!({ "mode": "whatever", "termHeight": 12 }),
        )
        .unwrap();
        assert_eq!(
            read_ui(dir.path()),
            json!({ "mode": "keys", "termHeight": 0 })
        );
    }

    #[test]
    fn migrate_copies_data_files_only_once() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::write(src.path().join("config.json"), "{}").unwrap();
        fs::write(src.path().join("known_hosts.json"), "{}").unwrap();
        fs::write(src.path().join("key-notes.json"), "{}").unwrap();
        fs::write(src.path().join("unrelated.txt"), "x").unwrap();

        migrate_data(dst.path(), src.path());
        assert!(dst.path().join("config.json").is_file());
        assert!(dst.path().join("known_hosts.json").is_file());
        assert!(dst.path().join("key-notes.json").is_file());
        assert!(!dst.path().join("unrelated.txt").exists());

        // 目标已有 config.json 就不再覆盖（用户新数据优先）
        fs::write(src.path().join("config.json"), "{\"from\":\"old\"}").unwrap();
        migrate_data(dst.path(), src.path());
        assert_eq!(
            fs::read_to_string(dst.path().join("config.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn window_state_needs_sane_size() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_bounds(dir.path()).is_none());

        write_json_atomic(
            &dir.path().join("window-state.json"),
            &json!({ "x": 10, "y": 20, "width": 1600, "height": 900, "scale": 1.5 }),
        )
        .unwrap();
        let b = load_bounds(dir.path()).unwrap();
        assert_eq!((b.x, b.y, b.width, b.height), (10, 20, 1600, 900));
        assert_eq!(b.scale, 1.5);

        // 比最小尺寸还小的记录直接忽略，走默认大小
        write_json_atomic(
            &dir.path().join("window-state.json"),
            &json!({ "x": 0, "y": 0, "width": 400, "height": 300 }),
        )
        .unwrap();
        assert!(load_bounds(dir.path()).is_none());
    }
}
