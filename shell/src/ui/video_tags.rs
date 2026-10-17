//! 影片資訊：改 MP4 裡的標題、作者、說明等（檔案總管「內容 → 詳細資料」看到的那些）。見 core::video_tags。

use super::dialogs;
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::types::LibraryEntry;
use screenrecorder_core::video_tags;
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;

pub struct TagsDialog {
    path: String,
    name: String,
    /// 讀出來的內容（判斷有沒有改過）；還在讀時是 None
    orig: Option<HashMap<String, String>>,
    values: HashMap<String, String>,
    saving: bool,
    error: Option<String>,
}

pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    let path = entry.media.path.clone();
    if !video_tags::can_tag(&path) {
        return app.toast(tr!("只有 MP4 影片可以改影片資訊", "Only MP4 videos can have their details changed"), true);
    }
    let Some(ff) = app.core.ffmpeg_path() else {
        return app.toast(tr!("找不到 FFmpeg", "FFmpeg not found"), true);
    };
    app.video_tags = Some(TagsDialog { name: dialogs::file_name(&path), path: path.clone(), orig: None, values: HashMap::new(), saving: false, error: None });
    app.spawn(async move { video_tags::read(&ff, &path).await }, |app, r| {
        let Some(d) = &mut app.video_tags else { return };
        match r {
            Ok(m) => {
                d.values = m.clone();
                d.orig = Some(m);
            }
            Err(e) => d.error = Some(e.message().to_string()),
        }
    });
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.video_tags.take() else { return };
    let mut close = false;
    let mut save = false;
    let changed = d.orig.as_ref().is_some_and(|o| o.iter().any(|(k, v)| d.values.get(k).is_some_and(|n| n.trim() != v.trim())));
    let modal = egui::Modal::new(Id::new("video-tags")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 80.0).clamp(320.0, 520.0));
        let pal = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("影片資訊", "Video details")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&d.name).color(pal.muted)).truncate());
        ui.add_space(8.0);
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(pal.warn).font(theme::font(12.5)));
        } else if d.orig.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr!("正在讀取…", "Reading…"));
            });
        } else {
            ui.add_enabled_ui(!d.saving, |ui| {
                egui::Grid::new("video-tags-form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    for f in video_tags::fields() {
                        ui.label(f.label);
                        let v = d.values.entry(f.key.to_string()).or_default();
                        let edit = if f.multiline { egui::TextEdit::multiline(v).desired_rows(4) } else { egui::TextEdit::singleline(v) };
                        ui.add(edit.desired_width(ui.available_width()).hint_text(f.hint));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(tr!(
                    "寫進影片檔裡：Windows 檔案總管（按右鍵 → 內容 → 詳細資料）和多數播放器看得到。影片內容、封面、章節都不變；欄位清空就是刪掉。",
                    "Written into the video file: shown in File Explorer (right-click → Properties → Details) and most players. The video, cover and chapters stay the same; clear a field to remove it."
                ))
                .font(theme::font(12.0))
                .color(pal.muted),
            );
        }
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if d.saving { tr!("正在儲存…", "Saving…") } else { tr!("儲存", "Save") };
            if Btn::new(label).primary().enabled(changed && !d.saving).show(ui).clicked() {
                save = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
    });
    if (close || modal.should_close()) && !d.saving {
        return;
    }
    if save {
        d.saving = true;
        let path = d.path.clone();
        // 只寫改過的欄位：沒動的照原樣留著
        let orig = d.orig.clone().unwrap_or_default();
        let values: HashMap<String, String> = d.values.iter().filter(|(k, v)| orig.get(*k).is_none_or(|o| o.trim() != v.trim())).map(|(k, v)| (k.clone(), v.clone())).collect();
        // 播放中的檔案換不掉：檢視器正在播這支就先放開，寫好再從同一個位置載入
        let t = super::viewer::release(app, &path);
        let ff = app.core.ffmpeg_path();
        let p2 = path.clone();
        app.spawn(async move { video_tags::write(ff.as_deref(), &p2, &values).await }, move |app, r| {
            match r {
                Ok(()) => {
                    app.video_tags = None;
                    app.toast(trf!("已更新影片資訊：{}", "Video details updated: {}", dialogs::file_name(&path)), false);
                }
                Err(e) => {
                    if let Some(d) = &mut app.video_tags {
                        d.saving = false;
                    }
                    app.toast(e.message().to_string(), true);
                }
            }
            super::viewer::reload(app, &path, t);
        });
    }
    app.video_tags = Some(d);
}
