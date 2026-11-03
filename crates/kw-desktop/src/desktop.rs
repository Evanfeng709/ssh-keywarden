//! 桌面端本地持久化：数据目录、旧版迁移、主题/视图设置、窗口状态与工作集修剪。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// 与旧版 `electron/main.js` 的 THEME_CHOICES 一致
pub const THEMES: [&str; 8] = [
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

#[allow(dead_code)]
pub const DEFAULT_WIDTH: u32 = 1280;
#[allow(dead_code)]
pub const DEFAULT_HEIGHT: u32 = 820;
pub const MIN_WIDTH: u32 = 1024;
pub const MIN_HEIGHT: u32 = 680;

#[allow(dead_code)]
pub fn default_ui() -> Value {
    json!({ "mode": "keys", "termHeight": 0 })
}

/* ------------------------------------------------------------------ */
/* 数据目录                                                            */
/* ------------------------------------------------------------------ */

/// `KW_DATA_DIR` 优先；否则与旧 Electron 版同址：
/// `%APPDATA%\ssh-keywarden\data`。
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
pub fn migrate_data(target: &Path, candidate: &Path) {
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

pub fn exe_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("data");
    dir.is_dir().then_some(dir)
}

/* ------------------------------------------------------------------ */
/* 主题 / 视图                                                         */
/* ------------------------------------------------------------------ */

pub fn read_theme(data_dir: &Path) -> String {
    let raw = fs::read_to_string(data_dir.join("theme.json")).unwrap_or_default();
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let choice = value.get("choice").and_then(|v| v.as_str()).unwrap_or("");
    if THEMES.contains(&choice) {
        choice.to_string()
    } else {
        String::new()
    }
}

pub fn save_theme(data_dir: &Path, choice: &str) -> Result<(), String> {
    if !THEMES.contains(&choice) {
        return Err("未知主题".into());
    }
    let _ = fs::create_dir_all(data_dir);
    write_json_atomic(&data_dir.join("theme.json"), &json!({ "choice": choice }))
        .map_err(|e| format!("无法保存主题：{e}"))
}

pub fn read_ui(data_dir: &Path) -> Value {
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

pub fn save_ui_mode(data_dir: &Path, mode: &str, term_height: i64) {
    let m = if mode == "term" { "term" } else { "keys" };
    let h = if (180..=10_000).contains(&term_height) {
        term_height
    } else {
        0
    };
    let _ = fs::create_dir_all(data_dir);
    let _ = write_json_atomic(
        &data_dir.join("ui.json"),
        &json!({ "mode": m, "termHeight": h }),
    );
}

pub fn write_json_atomic(file: &Path, value: &Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let tmp = PathBuf::from(format!("{}.tmp", file.display()));
    if fs::write(&tmp, &bytes).is_ok() && fs::rename(&tmp, file).is_ok() {
        return Ok(());
    }
    let _ = fs::remove_file(&tmp);
    fs::write(file, &bytes)
}

#[cfg(windows)]
pub fn is_system_dark() -> bool {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            hkey: isize,
            lp_sub_key: *const u16,
            lp_value: *const u16,
            dw_flags: u32,
            pdw_type: *mut u32,
            pv_data: *mut u32,
            pcb_data: *mut u32,
        ) -> i32;
    }
    const HKEY_CURRENT_USER: isize = -0x7fffffff_isize - 1; // 0x80000001
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    let sub_key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let val_name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            val_name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data,
            &mut size,
        )
    };
    if status == 0 {
        data == 0
    } else {
        true
    }
}

#[cfg(not(windows))]
pub fn is_system_dark() -> bool {
    true
}

/* ------------------------------------------------------------------ */
/* 窗口状态                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

pub fn load_bounds(data_dir: &Path) -> Option<Bounds> {
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

pub fn save_bounds(data_dir: &Path, b: Bounds) {
    if b.width < MIN_WIDTH || b.height < MIN_HEIGHT {
        return;
    }
    let value = json!({
        "x": b.x,
        "y": b.y,
        "width": b.width,
        "height": b.height,
        "scale": b.scale,
    });
    let _ = fs::create_dir_all(data_dir);
    let _ = write_json_atomic(&data_dir.join("window-state.json"), &value);
}

#[cfg(windows)]
pub fn trim_working_set() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn SetProcessWorkingSetSize(process: isize, min: usize, max: usize) -> i32;
    }
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
pub fn trim_working_set() {}

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

        let display = format!("v{}", version.replace('-', "."));
        let changelog = include_str!("../../../CHANGELOG.md");
        assert!(
            changelog.contains(&format!("## {display}")),
            "CHANGELOG.md 缺少 {display} 段"
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

        write_json_atomic(
            &dir.path().join("window-state.json"),
            &json!({ "x": 0, "y": 0, "width": 400, "height": 300 }),
        )
        .unwrap();
        assert!(load_bounds(dir.path()).is_none());
    }
}
