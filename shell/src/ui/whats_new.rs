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
            tr!("全部錄影更好讀", "An easier-to-read recordings list").into(),
            tr!(
                "日期改成「12/30 週二」，去年以前的在下面寫年份和這天有幾支；點日期就能勾選這一天全部。畫面大小改成「4K 直」「1080p 橫」，只有沒聲音的影片才標出來；合併時下方有進度條，可以取消。",
                "Dates now read “Tue 12/30”, with the year and the number of videos underneath for older days; click a date to select that whole day. Sizes now read “4K tall” or “1080p wide”, only videos without audio are marked, and merging shows a progress bar you can cancel."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("旋轉影片", "Rotate videos").into(),
            tr!(
                "拍歪了、橫的變直的？按檢視器上方的「向左轉」「向右轉」，或錄影卡片「⋯」選單的「旋轉」：直接改原檔的旋轉設定，幾秒就好、畫質不變，轉錯再轉回來就好。少數舊播放器不看這個設定時，可以用「另存畫面轉好的影片」。",
                "Recorded sideways? Click “Rotate left” or “Rotate right” at the top of the viewer, or “Rotate” in a recording card's ⋯ menu: the original's rotation setting changes in seconds without losing quality, and you can rotate back anytime. For old players that ignore the setting, use “Save an upright copy”."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("合併影片快很多、畫質不變", "Much faster merging, same quality").into(),
            tr!(
                "全部錄影勾兩支以上 →「合併成一支」：同一支手機或同樣設定錄的影片直接接起來，幾秒就好，畫質跟原檔一樣；iPhone、相機的 MOV 也可以。想要檔案小一點，可以在「畫質」選高畫質、標準或省空間。",
                "Select two or more in All recordings → “Merge into one”: videos from the same phone or recorded with the same settings are joined directly in seconds, keeping the original quality; iPhone and camera MOV files work too. For a smaller file, pick High quality, Standard or Smallest under “Quality”."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("壓縮：保留畫質、縮小檔案", "Compress: keep quality, smaller file").into(),
            tr!(
                "錄影卡片「⋯」選單或檢視器上方的「壓縮」，除了壓縮到 LINE、Email 傳得出去的大小，也可以選「保留畫質，縮小檔案」：手機拍的、合併好的大檔案選「標準」，通常會小很多又看不太出差別。",
                "“Compress” in a recording card's ⋯ menu or at the top of the viewer can now also “Keep quality, smaller file”, besides fitting a size for LINE or email. For big phone videos or merged files, “Standard” usually saves a lot of space with little visible difference."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("播放更順：mpv 播放元件", "Smoother playback: mpv component").into(),
            tr!(
                "到設定 →「外部元件」下載 mpv 播放元件（約 32 MB），檢視器和主畫面的播放都改用 mpv：顯示卡解碼，手機的 4K、60 fps 影片也很順，拖曳進度條畫面馬上跟上。播放有問題時可以在同一頁關掉。",
                "Download the mpv component (about 32 MB) in Settings → Components and the viewer and the main screen play videos with mpv: decoded on the graphics card, smooth even for 4K 60 fps phone videos, and the picture follows the seek bar as you drag. You can turn it off on the same page if playback has problems."
            )
            .into(),
            Try::Settings(settings_dialog::Page::Components),
        ),
        (
            tr!("拿掉「做 YouTube 封面圖」", "Removed “Make YouTube cover”").into(),
            tr!(
                "比較少人用的「做 YouTube 封面圖」拿掉了。「設為封面」（選一格當檔案總管看到的縮圖）照樣可以用。",
                "The rarely used “Make YouTube cover” is gone. “Set as cover” (pick a frame as the thumbnail File Explorer shows) still works."
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
