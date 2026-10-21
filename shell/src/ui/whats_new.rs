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
            tr!("清單上的操作變簡單了", "Simpler actions in lists").into(),
            tr!(
                "每一列只留播放、剪輯和「⋯」；設為封面、旋轉、複製操作步驟這些要看著畫面做的，打開影片後在上方。",
                "Each row now has just Play, Edit and “⋯”. Set as cover, rotate, copy steps and other picture-related actions are at the top once you open the video."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("Jellyfin 記住看到哪裡", "Jellyfin remembers where you stopped").into(),
            tr!(
                "在這裡看到一半，電視上可以接著看，反過來也一樣；瀏覽視窗一打開先列「繼續觀看」「最近加入」。",
                "Stop halfway here and pick up on the TV, or the other way round. The browser now starts with Continue watching and Recently added."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("下載家人傳的影片", "Download family videos").into(),
            tr!(
                "Jellyfin 瀏覽視窗可以下載影片到這台電腦，就能剪輯、做加速版；勾選好幾支可以一次下載或刪除。",
                "Download videos from the Jellyfin browser to edit them or make sped-up versions. Select several to download or delete them at once."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("排序、篩選、字幕", "Sort, filter, subtitles").into(),
            tr!(
                "資料夾裡可以只看還沒看過的、看到一半的、我的最愛；串流時也顯示 Jellyfin 上的字幕和章節。",
                "In a folder, show only unwatched, partly watched or favorite videos. Streaming shows Jellyfin's subtitles and chapters too."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("NAS 空間不夠先提醒", "Low NAS space warning").into(),
            tr!(
                "傳之前先看 NAS 還有多少空間；傳好之後右下角顯示 Jellyfin 還在整理什麼，做完才拿掉。",
                "Free space on the NAS is checked before sending, and the corner shows what Jellyfin is still processing until it's done."
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
