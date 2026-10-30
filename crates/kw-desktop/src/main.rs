// 桌面版不带控制台窗口；调试时想看日志就用 `cargo run` 的 debug 构建
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;

use desktop::Desktop;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        // 单实例插件必须第一个注册：第二次启动只把已有窗口拉到前面
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            desktop::pick_file,
            desktop::pick_dir,
            desktop::open_data_dir,
            desktop::toggle_maximize,
            desktop::is_maximized,
            desktop::read_clipboard,
            desktop::write_clipboard,
            desktop::system_theme,
            desktop::theme_set,
            desktop::ui_set,
        ])
        .setup(|app| desktop::boot(app))
        .on_window_event(|window, event| {
            let Some(desktop) = window.try_state::<Desktop>() else {
                return;
            };
            desktop::on_window_event(window, event, &desktop.data_dir);
        })
        .build(tauri::generate_context!())
        .expect("无法初始化应用窗口")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                let Some(desktop) = app.try_state::<Desktop>() else {
                    return;
                };
                if let Some(server) = desktop.take_server() {
                    tauri::async_runtime::block_on(server.shutdown());
                }
            }
        });
}
