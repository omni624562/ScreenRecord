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
    /// 開啟全部截圖（true）或全部錄影
    Library(bool),
    /// 開始一步一步的教學（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Tour,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(app: &UiApp) -> Vec<(String, String, Try)> {
    let mute = app.keys.label(7);
    let mute = if mute.is_empty() { String::new() } else { trf!("（{mute}）", " ({mute})") };
    vec![
        (
            tr!("開會時提醒錄影", "Reminders to record meetings").into(),
            tr!(
                "Teams、Zoom、Google Meet、LINE 等開始通話時，右下角會問要不要錄影；通話結束時再問要不要停止。只提醒，不會自己錄。",
                "When Teams, Zoom, Google Meet, LINE and similar apps start a call, a prompt asks whether to record; when the call ends it asks whether to stop. It only asks."
            )
            .into(),
            Try::Settings(Page::Record),
        ),
        (
            tr!("錄影中麥克風靜音", "Mute the microphone while recording").into(),
            trf!(
                "要咳嗽、接電話時，按快速鍵{mute}或錄影面板的麥克風按鈕暫時靜音，剪輯時時間軸上會標出靜音的段落。",
                "Need to cough or take a call? Press the shortcut{mute} or the microphone button to mute for a moment. Muted parts are marked on the editing timeline."
            ),
            Try::Settings(Page::Keys),
        ),
        (
            tr!("做封面", "Make a cover").into(),
            tr!(
                "錄影的「更多」選單或檢視器按「做封面」，挑一格畫面打上標題，做成 YouTube 用的封面圖，也能設成影片在檔案總管的縮圖。",
                "Choose “Make cover” to pick a frame, add a title and save a YouTube cover image. It can also become the video's thumbnail in File Explorer."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("接縫淡入淡出", "Fade at cuts").into(),
            tr!(
                "剪輯時刪掉片段的地方，勾選「接縫淡入淡出」，畫面和聲音會先淡出再淡入，不會突然跳過去或「啪」一聲。",
                "Turn on “Fade at cuts” when editing: where a section was removed, the picture and sound fade out and back in instead of jumping or popping."
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
