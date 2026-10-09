//! 剪輯視窗（稍後實作）
use super::UiApp;
use eframe::egui;
use screenrecorder_core::types::LibraryEntry;

pub struct Editor;

impl Editor {
    pub fn pause(&mut self) {}
}

pub fn open(_app: &mut UiApp, _entry: LibraryEntry) {}

pub fn show(_app: &mut UiApp, _ctx: &egui::Context) {}
