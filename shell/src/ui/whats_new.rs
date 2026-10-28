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
    /// 開啟設定的某一頁
    Settings(Page),
    /// 在螢幕上框選截圖（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Snip,
    /// 開啟全部截圖（true）或全部錄影
    Library(bool),
    /// 開始一步一步的教學（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Tour,
    /// 開啟檔案庫的 Jellyfin 分頁
    Jellyfin,
    /// 縮小成小視窗
    Compact,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("標註範本", "Annotation templates").into(),
            tr!(
                "剪輯或截圖編輯的「標註」分頁：把畫面上的幾個標註（例如紅框＋說明文字）存成範本，之後在別支影片、截圖一按就放上去，位置、大小、顏色都一樣。",
                "In the Annotations tab of the editor: save the annotations on screen (for example a red box with a note) as a template, then add them to another video or screenshot with one click, in the same place, size and color."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("在每次點擊的地方加上圓框", "Circle every click").into(),
            tr!(
                "錄影時有記下點擊（「點擊、按鍵」）的影片，剪輯時在「標註」分頁按一下，每次點擊的地方都會加上淡入淡出的圓框，做教學影片不用一個一個放。",
                "For recordings with clicks recorded (“Clicks and keys”), one click in the Annotations tab adds a fading circle at every click, so tutorials don't need them placed one by one."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("影片標註可以複製、貼上", "Copy and paste annotations in videos").into(),
            tr!(
                "選取標註後 Ctrl+C、Ctrl+V 貼到播放位置，或 Ctrl+D 直接在播放位置再做一份；數字鍵 1～0 選工具。",
                "Select an annotation and press Ctrl+C, then Ctrl+V to paste it at the playhead, or Ctrl+D to duplicate it there. Number keys 1–0 pick tools."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("新的標註跟著上一個", "New annotations follow the last one").into(),
            tr!(
                "同一種標註會沿用上一個的出現長度和出現方式（淡入淡出、放大出現）：調好一個，接下來放的都一樣。",
                "Annotations of the same kind reuse the previous one's length and appear animation: set it once and the rest follow."
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
        Try::Jellyfin => super::library_dialog::open_jellyfin(app),
        Try::Compact => super::title_bar::enter_compact(app, ctx),
    }
}
