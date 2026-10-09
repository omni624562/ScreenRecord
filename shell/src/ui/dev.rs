//! 開發用（只在 debug 版）：自動開啟某個畫面並截圖，在 Linux 的 Xvfb 上檢查介面。
//! - SCREENRECORDER_DEV：要開啟的畫面（library、changelog、export:<路徑>、edit:<路徑>），以 ; 分隔可依序執行
//! - SCREENRECORDER_SHOT：截圖存檔的路徑；SCREENRECORDER_SHOT_AFTER：幾毫秒後截圖（預設 3000）

use super::{EntryAction, UiApp};
use eframe::egui;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Dev {
    started: Option<Instant>,
    done_actions: bool,
    shot_requested: bool,
}

pub fn tick(app: &mut UiApp, ctx: &egui::Context) {
    if app.dev.started.is_none() {
        // 測試用：SCREENRECORDER_THEME=light / dark
        match std::env::var("SCREENRECORDER_THEME").as_deref() {
            Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
            Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
            _ => {}
        }
    }
    let start = *app.dev.started.get_or_insert_with(Instant::now);
    if !app.dev.done_actions && app.env_ready && start.elapsed() > Duration::from_millis(1200) {
        app.dev.done_actions = true;
        if let Ok(list) = std::env::var("SCREENRECORDER_DEV") {
            for a in list.split(';').map(str::trim).filter(|a| !a.is_empty()) {
                match a.split_once(':') {
                    Some(("export", p)) => app.act_path(EntryAction::Export, p.to_string()),
                    Some(("edit", p)) => app.act_path(EntryAction::Edit, p.to_string()),
                    _ if a == "library" => app.library = Some(super::library_dialog::LibraryDialog::new()),
                    _ if a == "changelog" => app.changelog_open = true,
                    _ => {}
                }
            }
        }
    }
    let Ok(path) = std::env::var("SCREENRECORDER_SHOT") else { return };
    let after = std::env::var("SCREENRECORDER_SHOT_AFTER").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    if !app.dev.shot_requested && start.elapsed() > Duration::from_millis(after) {
        app.dev.shot_requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    let shots: Vec<_> = ctx.input(|i| i.raw.events.iter().filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }).collect());
    for img in shots {
        let mut pm = tiny_skia::Pixmap::new(img.size[0] as u32, img.size[1] as u32).unwrap();
        pm.data_mut().copy_from_slice(img.as_raw());
        let _ = pm.save_png(&path);
        std::process::exit(0);
    }
    ctx.request_repaint_after(Duration::from_millis(100));
}
