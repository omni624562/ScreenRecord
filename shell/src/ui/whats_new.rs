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
    /// 打開設定的某一頁（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
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
            tr!("一鍵剪掉贅字和停頓", "Cut fillers and pauses in one go").into(),
            tr!(
                "有字幕的影片，在「字幕」分頁按「找出贅字和停頓」：列出只有「嗯、呃、那個」的句子和超過 1 秒沒講話的地方，勾選後一次剪掉。",
                "For videos with subtitles, click “Find fillers and pauses” in the Subtitles tab to list sentences that are only “um” or “uh” and gaps of over a second, then cut the ones you tick."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("時間軸聲音波形", "Audio waveform on the timeline").into(),
            tr!(
                "剪輯視窗的時間軸下方畫出聲音大小，一眼看出哪裡在講話、哪裡沒聲音，剪得更準。",
                "The editor's timeline shows how loud the audio is, so you can see where people talk and where it's quiet."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("片頭、片尾", "Intro and outro").into(),
            tr!(
                "在「畫面」分頁打開片頭或片尾，輸入標題、副標，選深色、淺色或藍色，影片前後就會多一張幾秒的標題卡（淡入淡出）。",
                "Turn on an intro or outro in the Picture tab, type a title and subtitle and pick dark, light or blue to add a title card with fades before or after the video."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("背景音樂", "Background music").into(),
            tr!(
                "在「聲音」分頁選一首音樂：重複播放到影片結束、最後淡出，講話的時候自動調小聲。",
                "Pick a song in the Audio tab: it loops to the end of the video, fades out, and turns down automatically while you talk."
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
        Try::Library => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot)),
        Try::Tour => super::tour::restart(app),
    }
}
