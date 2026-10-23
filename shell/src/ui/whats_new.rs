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
            tr!("Jellyfin：一次整理好幾支的人物、標籤", "Jellyfin: organize people and tags for many videos").into(),
            tr!(
                "勾選影片按「整理人物、標籤…」，一次加、拿掉人物和標籤，改年份；「依這些找」可以找「沒有人物」「沒有標籤」的影片。播放時按「人物照片」把臉框起來設成那個人的照片。",
                "Check videos and choose “Organize people & tags…” to add or remove people and tags and change the year at once; find-by can list videos with no people or tags. While playing, “Person photo” frames a face as that person's photo."
            )
            .into(),
            Try::Jellyfin,
        ),
        (
            tr!("Jellyfin：合集與播放清單", "Jellyfin: collections and playlists").into(),
            tr!(
                "最上層多了「合集」「播放清單」，可以全部播放；勾選影片按「加進…」或按右鍵放進去，也可以新建。",
                "Collections and playlists now appear at the top level, with Play all; add checked videos with “Add to…” or the right-click menu, or create a new one."
            )
            .into(),
            Try::Jellyfin,
        ),
        (
            tr!("幫 Jellyfin 上的影片產生字幕", "Make subtitles for videos on Jellyfin").into(),
            tr!(
                "勾選影片按「產生字幕」，用這台電腦的語音辨識做字幕傳回 Jellyfin，電視、手機上看也有字幕；不用先下載影片。",
                "Check videos and choose “Make subtitles”: this computer's speech recognition makes them and sends them back to Jellyfin, so TVs and phones show them too. No download needed."
            )
            .into(),
            Try::Jellyfin,
        ),
        (
            tr!("Jellyfin：清單檢視、鍵盤操作", "Jellyfin: list view and keyboard").into(),
            tr!(
                "切成清單一列一支，點欄位標題排序；方向鍵移動、Enter 打開、空白鍵勾選、Delete 刪除、Backspace 回上一層。",
                "Switch to a list with sortable columns; arrows move, Enter opens, Space checks, Delete deletes and Backspace goes up."
            )
            .into(),
            Try::Jellyfin,
        ),
        (
            tr!("新的錄影控制列", "A new recording bar").into(),
            tr!(
                "錄影中的計時和暫停、標記、麥克風靜音、停止收進一條深色的錄影膠囊，拿掉音量條，畫面更清爽。",
                "While recording, the timer, pause, mark, mic mute and stop sit in one dark capsule, and the level meters are gone."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("傳到 Jellyfin 快很多", "Sending to Jellyfin is much faster").into(),
            tr!(
                "傳好後直接請 Jellyfin 掃描那個資料夾，不用再等它的監控延遲（原本要一分鐘左右），影片資訊、封面也比較不會漏設。",
                "After sending, Jellyfin is asked to scan that folder right away instead of waiting out its monitor delay (about a minute before), so details and covers are set more reliably."
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
