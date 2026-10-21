//! 新功能介紹：更新到新的次版號（例如 3.1）後第一次開啟時顯示一次，列出重點功能（最多 6 項），可以直接「試試看」。
//! 第一次安裝時改成一步一步的教學（tour.rs）。

use super::settings_dialog::{self, Page};
use super::theme::{self, Btn};
use super::UiApp;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::version::APP_VERSION;
use screenrecorder_core::{tr, trf};

/// 主版號.次版號（3.1.0 → 3.1）
pub fn minor_version() -> String {
    APP_VERSION.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// 這一版還沒看過介紹
pub fn should_show(app: &UiApp) -> bool {
    app.s.seen_version != minor_version()
}

#[derive(Clone, Copy)]
#[allow(dead_code)] // 有些版本的新功能介紹沒有「到設定看看」
enum Try {
    None,
    /// 打開設定的某一頁
    Settings(Page),
    /// 在螢幕上框選截圖（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Snip,
    /// 開啟全部截圖（true）或全部錄影
    Library(bool),
    /// 開始一步一步的教學（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Tour,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("選好就能刪", "Delete right from the selection").into(),
            tr!(
                "剪輯時在縮圖上拖過不要的部分，選取範圍上方就有「刪除」「速度」按鈕，不用再找右邊的面板。",
                "Drag across the part you don't want and Delete and Speed buttons appear right above the selection."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("點一下只跳過去", "A click only jumps").into(),
            tr!(
                "在時間軸上點一下只移動播放頭，拖曳才是選取，手抖不會選到；分割好的片段按兩下選取。",
                "Clicking the timeline only moves the playhead; dragging selects, so a shaky click won't. Double-click a split section to select it."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("剪頭尾和刪中間一樣", "Trim the ends like the middle").into(),
            tr!(
                "選取碰到開頭或結尾時按「刪除」就是剪掉頭尾；剪掉的頭尾也收起來，旁邊的紅色復原鈕可以放回來。",
                "Deleting a selection that touches the start or end trims it. Trimmed ends fold away too, with a red restore button next to them."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("時間軸可以按右鍵", "Right-click the timeline").into(),
            tr!(
                "在時間軸上按右鍵：在這裡分割、刪除或加速這段、設為開頭 / 結尾、加標記，在剪掉的部分上按右鍵就能還原。",
                "Right-click the timeline to split, delete or speed up a section, set the start or end, or add a marker. Right-click a removed part to restore it."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("一次選好幾段", "Select several sections").into(),
            tr!(
                "按住 Ctrl 在時間軸上拖曳或點片段，選好幾段後一起刪除或加速；按 Ctrl+Z 一次復原。",
                "Hold Ctrl and drag or click on the timeline to pick several sections, then delete or speed them up together. Ctrl+Z undoes it in one step."
            )
            .into(),
            Try::Library(false),
        ),
    ]
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let mut close = false;
    let mut act = Try::None;
    let modal = egui::Modal::new(Id::new("whats-new")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width((ctx.content_rect().width() - 80.0).min(600.0));
        ui.horizontal(|ui| {
            let title = trf!("{} 版的新功能", "What's new in {}", minor_version());
            ui.label(RichText::new(title).font(theme::font_bold(18.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(theme::Icon::Close).ghost().small().tooltip(tr!("關閉", "Close")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 190.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (title, desc, t) in items(app) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 90.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(title).font(theme::font_bold(14.0)));
                        ui.add(egui::Label::new(RichText::new(desc).font(theme::font(12.5)).color(p.muted)).wrap());
                    });
                    if !matches!(t, Try::None) {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if Btn::new(tr!("試試看", "Try it")).small().show(ui).clicked() {
                                act = t;
                            }
                        });
                    }
                });
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if Btn::new(tr!("完整的更新說明", "Full release notes")).ghost().small().show(ui).clicked() {
                act = Try::None;
                close = true;
                app.changelog_open = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(tr!("知道了", "Got it")).primary().show(ui).clicked() {
                    close = true;
                }
            });
        });
    });
    if modal.should_close() {
        close = true;
    }
    if !matches!(act, Try::None) {
        close = true;
    }
    if close {
        app.whats_new_open = false;
        app.s.seen_version = minor_version();
        app.save_settings();
    }
    match act {
        Try::None => {}
        Try::Settings(page) => settings_dialog::open(app, page),
        Try::Snip => super::main_view::run_shot(app, super::main_view::ShotAct::Select(0)),
        Try::Library(shot) => {
            let kind = if shot { super::library_dialog::Kind::Shot } else { super::library_dialog::Kind::Video };
            app.library = Some(super::library_dialog::LibraryDialog::new(kind));
        }
        Try::Tour => super::tour::restart(app),
    }
}
