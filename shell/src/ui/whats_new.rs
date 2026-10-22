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
    /// 打開檔案庫的 Jellyfin 分頁
    Jellyfin,
    /// 縮小成小視窗
    Compact,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("錄影中看得到聲音大小", "See audio levels while recording").into(),
            tr!(
                "錄影時控制列中間有麥克風、電腦聲音的音量條，講話時麥克風那條會跳動，不用錄完才發現沒有聲音；麥克風靜音時按鈕和音量條都會寫「已靜音」。",
                "While recording, the control bar shows microphone and system audio levels. The mic bar moves when you talk, so you won't find out afterwards that nothing was recorded. When the mic is muted, the button and the level both say “Muted”."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("訊息更好找、更好按", "Clearer messages and actions").into(),
            tr!(
                "錄好了、做好了的訊息左邊有色條（綠＝完成、藍＝進行中、橘＝提醒），第二行寫檔名，「▶ 播放」等按鈕就在文字旁邊；好幾則時用「‹ 1/2 ›」翻看，新的排第一。停止改成紅底實心，麥克風按鈕會寫出是不是靜音。",
                "Messages have a colored stripe (green = done, blue = in progress, orange = note) and the file name on a second line, with Play and other buttons right next to the text. Page through several with “‹ 1/2 ›”; the newest is first. Stop is now solid red, and the mic button says whether it's muted."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("Jellyfin 人物、標籤：常用的在前", "Jellyfin people and tags: most used first").into(),
            tr!(
                "影片資訊的「常用」依使用次數排、重複的合成一個；★ 釘選常用的，「全部 N 個」可以搜尋、勾選多個一次加入。",
                "“Used before” in the video details is ordered by how often each is used, with duplicates merged. Pin favorites with ★, and use “All N” to search and add several at once."
            )
            .into(),
            Try::Jellyfin,
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
        Try::Jellyfin => super::library_dialog::open_jellyfin(app),
        Try::Compact => super::title_bar::enter_compact(app, ctx),
    }
}
