// 桌面版不带控制台窗口；调试时想看日志就用 `cargo run` 的 debug 构建
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod desktop;
mod icons;
mod theme;
mod vt;

use eframe::egui;
use std::sync::Arc;

fn load_app_icon() -> Option<Arc<egui::IconData>> {
    let bytes = include_bytes!("../icons/icon.png");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(Arc::new(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }))
}

fn main() -> eframe::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(1024 * 1024)
        .enable_all()
        .build()
        .expect("无法创建异步运行时");
    let handle = rt.handle().clone();

    let data_dir = desktop::resolve_data_dir();
    let _ = std::fs::create_dir_all(&data_dir);
    if let Some(cand) = desktop::exe_data_dir() {
        desktop::migrate_data(&data_dir, &cand);
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("SSH Keywarden")
        .with_decorations(false)
        .with_inner_size([1240.0, 800.0])
        .with_min_inner_size([920.0, 600.0]);

    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(icon);
    }

    if let Some(b) = desktop::load_bounds(&data_dir) {
        let scale = if b.scale > 0.25 { b.scale as f32 } else { 1.0 };
        let w = (b.width as f32 / scale).clamp(920.0, 3840.0);
        let h = (b.height as f32 / scale).clamp(600.0, 2160.0);
        let x = (b.x as f32 / scale).clamp(-1000.0, 3800.0);
        let y = (b.y as f32 / scale).clamp(0.0, 2000.0);
        viewport = viewport
            .with_inner_size([w, h])
            .with_position(egui::pos2(x, y));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "SSH Keywarden",
        options,
        Box::new(move |cc| Ok(Box::new(app::KwDesktopApp::new(cc, handle, data_dir)))),
    )
}
