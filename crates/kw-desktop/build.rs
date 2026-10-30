fn main() {
    // 自定义命令要靠自动生成的权限才能被 webview 调用；窗口加载的是外网式 URL
    // （http://127.0.0.1:<随机端口>），Tauri 把它当 remote origin，没有 ACL 一律拒绝。
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "pick_file",
            "pick_dir",
            "open_data_dir",
            "toggle_maximize",
            "is_maximized",
            "read_clipboard",
            "write_clipboard",
            "system_theme",
            "theme_set",
            "ui_set",
        ]));
    tauri_build::try_build(attributes).expect("tauri-build 失败");
}
