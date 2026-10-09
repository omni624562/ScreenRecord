//! 製作加速版 / GIF（稍後實作）
use super::UiApp;
use eframe::egui;
use screenrecorder_core::types::LibraryEntry;

pub struct ExportDialog {
    pub entry: LibraryEntry,
}

impl ExportDialog {
    pub fn new(entry: LibraryEntry) -> Self {
        Self { entry }
    }
}

pub fn show(_app: &mut UiApp, _ctx: &egui::Context) {}
