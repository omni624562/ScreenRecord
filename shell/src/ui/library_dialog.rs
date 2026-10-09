//! 全部錄影（稍後實作）
use super::UiApp;
use eframe::egui;
use screenrecorder_core::format::speed_label;
use screenrecorder_core::types::{ExportFormat, ExportInfo};

pub struct LibraryDialog {
    pub dirty: bool,
}

impl LibraryDialog {
    pub fn new() -> Self {
        Self { dirty: true }
    }
}

/// 匯出檔的標籤：4×、GIF 4×、GIF
pub fn export_tag(x: &ExportInfo) -> String {
    if x.format == Some(ExportFormat::Gif) {
        if x.speed > 1.0 {
            format!("GIF {}×", speed_label(x.speed))
        } else {
            "GIF".into()
        }
    } else {
        format!("{}×", speed_label(x.speed))
    }
}

pub fn show(_app: &mut UiApp, _ctx: &egui::Context) {}
