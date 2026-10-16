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
    /// 開啟全部截圖（true）或全部錄影（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Library(bool),
    /// 開始一步一步的教學（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Tour,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("手動放大一塊、調整片段順序", "Zoom into an area, reorder sections").into(),
            tr!(
                "剪輯時在影片上框一塊，平順地放大過去再拉回；保留的片段可以改變前後順序，例如把結論移到開頭。",
                "In the editor, frame an area to smoothly zoom in and back out, and move kept sections around, e.g. put the conclusion first."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("字幕樣式、人聲加強", "Subtitle style, voice enhancement").into(),
            tr!(
                "字幕可以改大小、顏色、放在上方或下方，一次套用到全部；「聲音」分頁的人聲加強讓講話更清楚。",
                "Change subtitle size, color and position for all subtitles at once; Enhance voice in the Audio tab makes speech clearer."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("我的樣式、游標閒置時隱藏", "My styles, hide idle cursor").into(),
            tr!(
                "常用的標註顏色與大小存起來一鍵套用；平滑游標停著不動時自動藏起來。",
                "Save annotation colors and sizes you use often and apply them in one click; the smooth cursor hides itself when idle."
            )
            .into(),
            Try::None,
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
