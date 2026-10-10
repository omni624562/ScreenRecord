//! 文字辨識（OCR）：把截圖裡的文字轉成可以複製的文字（Windows 內建的辨識，core::ocr_win），結果顯示在對話框。

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::actions;
use screenrecorder_core::shot_edit::ShotSpec;

/// 辨識中或辨識好的結果
pub struct Ocr {
    /// None = 辨識中
    pub text: Option<String>,
    pub lang: String,
}

/// 辨識套用編輯後的圖（裁切、馬賽克後的樣子）
pub fn start_spec(app: &mut UiApp, source: String, spec: ShotSpec) {
    app.ocr = Some(Ocr { text: None, lang: String::new() });
    app.spawn(async move { actions::shot_ocr(&source, Some(&spec)).await }, done);
}

/// 辨識一張圖
pub fn start(app: &mut UiApp, path: String) {
    app.ocr = Some(Ocr { text: None, lang: String::new() });
    app.spawn(async move { actions::shot_ocr(&path, None).await }, done);
}

fn done(app: &mut UiApp, r: screenrecorder_core::error::Result<(String, String)>) {
    match r {
        Ok((text, lang)) => app.ocr = Some(Ocr { text: Some(text), lang }),
        Err(e) => {
            app.ocr = None;
            app.toast(e.message().to_string(), true);
        }
    }
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut o) = app.ocr.take() else { return };
    let mut close = false;
    let mut copy = None;
    let modal = egui::Modal::new(Id::new("ocr")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(560.0);
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("文字辨識").font(theme::font_bold(17.0)));
            if !o.lang.is_empty() {
                ui.label(RichText::new(format!("（{}）", o.lang)).color(p.muted));
            }
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip("關閉（Esc）").show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(8.0);
        match &mut o.text {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("辨識中…");
                });
            }
            Some(text) if text.trim().is_empty() => {
                ui.label(RichText::new("圖裡找不到文字。").color(p.muted));
            }
            Some(text) => {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(text).desired_width(f32::INFINITY).desired_rows(10).font(theme::font(14.0)));
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("可以直接修改，再按「複製全部」；也可以選一段後按 Ctrl+C。").font(theme::font(12.0)).color(p.muted));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if Btn::new("複製全部").primary().show(ui).clicked() {
                            copy = Some(text.clone());
                        }
                    });
                });
            }
        }
        ui.add_space(4.0);
    });
    if let Some(t) = copy {
        ctx.copy_text(t);
        app.toast("已複製辨識出的文字", false);
    }
    if close || modal.should_close() {
        return;
    }
    app.ocr = Some(o);
}
