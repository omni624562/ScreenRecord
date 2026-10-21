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
            tr!("Jellyfin 連續播放", "Keep watching on Jellyfin").into(),
            tr!(
                "從瀏覽視窗點開的影片，同一排可以上一部、下一部，看完自動接下一支；拖進度列時看得到那一秒的小畫面。",
                "Videos opened from the Jellyfin browser play one after another, with previous / next. Hover the progress bar to preview that second."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("依人物、標籤、年份找", "Find by person, tag or year").into(),
            tr!(
                "Jellyfin 瀏覽視窗最上層多了「人物」「標籤」「年份」，一次列出整個伺服器上符合的影片。",
                "The top of the Jellyfin browser now has People, Tag and Year to list matching videos across the server."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("直接改 Jellyfin 上的影片資訊", "Edit details on Jellyfin").into(),
            tr!(
                "家人傳上去的影片，滑鼠移上去按筆的圖示，就能改標題、人物、標籤、年份、封面，不用先下載。",
                "Hover a video in the Jellyfin browser and click the pencil to change its title, people, tags, year or cover without downloading it."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("J / K / L 找要剪的地方", "J / K / L to find the spot").into(),
            tr!(
                "剪輯時 L 播放、再按更快，J 倒轉，K 停；拖曳選取時會貼齊標記、分割點和播放頭。",
                "In the editor, L plays (press again to go faster), J rewinds and K stops. Selections snap to markers, splits and the playhead."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("復原清單", "Undo history").into(),
            tr!("復原鈕旁的「▾」列出每一步改了什麼，點一步就一次復原到那裡之前。", "The ▾ next to Undo lists every step by name; pick one to undo back to before it.").into(),
            Try::Library(false),
        ),
        (
            tr!("工作列看得到進度", "Progress on the taskbar").into(),
            tr!(
                "製作、壓縮、傳檔、下載時，工作列上的程式圖示顯示進度，做完會閃一下。",
                "Exports, compression, transfers and downloads show progress on the taskbar icon, which flashes when they finish."
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
