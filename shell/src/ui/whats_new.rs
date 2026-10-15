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
enum Try {
    None,
    /// 打開設定的某一頁
    Settings(Page),
    /// 在螢幕上框選截圖（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Snip,
    /// 開啟全部截圖（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Library,
    /// 開始一步一步的教學（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Tour,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("只抓文字", "Grab text").into(),
            tr!(
                "框選畫面上的一塊，裡面的字直接複製起來，不用存成圖；是表格時會照欄位排好，可以直接貼到 Excel。在「截圖 → 小工具」或系統匣選單裡。",
                "Select part of the screen and the text in it is copied right away, no image saved. Tables keep their columns, so you can paste straight into Excel. Find it under Screenshot → Tools or in the tray menu."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("用文字剪影片", "Edit video by text").into(),
            tr!(
                "有字幕的影片，在剪輯視窗的「字幕」分頁按句子旁邊的剪刀，那一句講話的片段就剪掉了；Ctrl 或 Shift＋點時間可以選好幾句一起剪，按還原放回去。",
                "For videos with subtitles, click the scissors next to a sentence in the editor's Subtitles tab to cut that part. Ctrl- or Shift-click times to cut several at once; click restore to put one back."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("自動命名", "Automatic names").into(),
            tr!(
                "依說的話、畫面上的字、錄影時在用的程式取名（例如「報價單的填法_10-15」）。改名時會先給一個建議名稱；也可以設定錄完自動命名，自己取過的名稱不會動。",
                "Names recordings from what was said, text on screen and the app you used (e.g. “Quote form_10-15”). Rename suggests a name, and you can have recordings named automatically after recording; names you chose are kept."
            )
            .into(),
            Try::Settings(Page::After),
        ),
        (
            tr!("英文字幕、中英對照", "English and bilingual subtitles").into(),
            tr!(
                "自動產生字幕時可以選「翻成英文」或「中英對照」，英文字幕另存成「同名.en.srt」；錄完自動處理也可以順便做英文字幕。全部在這台電腦上辨識，不用上傳。",
                "When generating subtitles, choose “English” or “Both” for bilingual subtitles; English is also saved as “name.en.srt”. After-recording processing can make English subtitles too. Everything runs on this computer; nothing is uploaded."
            )
            .into(),
            Try::Settings(Page::After),
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
        Try::Library => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot)),
        Try::Tour => super::tour::restart(app),
    }
}
