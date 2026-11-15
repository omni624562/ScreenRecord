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
    /// 開啟檔案庫的相片分頁
    Photos,
    /// 縮小成小視窗
    Compact,
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("挑最好的一張、找好幾個人的合照", "Pick the best shot, find photos of several people").into(),
            tr!(
                "同時間拍的相片裡，拍得最好的那張會標「推薦」，其他的可以一次勾選。「人物」選單還能找有人在笑的、好幾個人都在的（按名字旁的「＋」）、獨照或合照，也能每年挑幾張代表。",
                "Among photos taken at the same time, the best one is marked “Best” and the rest can be selected at once. The People menu can also find smiles, photos with several people together (press “＋” next to a name), solo or group shots, and a few highlights per year."
            )
            .into(),
            Try::Photos,
        ),
        (
            tr!("挑掉閉眼、模糊的相片", "Weed out closed eyes and blurry faces").into(),
            tr!(
                "看「有人閉眼的」時，同時間拍的相片排在一起，綠色眼睛是沒有人閉眼的，勾選不要的一起刪；判斷錯了，看大圖時按臉下面的按鈕改掉。「人物」選單也多了「臉模糊的」。",
                "With “Eyes closed”, photos taken at the same time are listed together; a green eye means nobody's eyes are closed, so select the rest and delete them. If it's wrong, fix it with the button under the face when viewing the photo. The People menu also has “Blurry faces”."
            )
            .into(),
            Try::Photos,
        ),
        (
            tr!("找出閉眼的相片", "Find photos with closed eyes").into(),
            tr!(
                "相片分頁「人物」選單的「有人閉眼的」只列出有人閉上眼睛的相片，縮圖右下角也會標出來，可以選判斷的寬嚴。一樣在這台電腦上判斷，不上傳。",
                "“Eyes closed” in the Photos tab's People menu lists only photos where someone's eyes are closed, with a mark on the thumbnail. You can choose how strict it is. It's checked on this computer; nothing is uploaded."
            )
            .into(),
            Try::Photos,
        ),
        (
            tr!("相片裡的人物", "People in your photos").into(),
            tr!(
                "相片分頁的「人物」會在這台電腦上找出相片裡的臉，把同一個人分在一起。取了名字就能只看有他的相片、用名字搜尋；相片和臉都不會上傳。",
                "People in the Photos tab finds faces in your photos on this computer and groups each person together. Name them to see only their photos or search by name. No photos or faces are uploaded."
            )
            .into(),
            Try::Photos,
        ),
        (
            tr!("看 NAS 上的相片", "Photos on your NAS").into(),
            tr!(
                "檔案庫的「相片」分頁：加入 NAS 或電腦上放相片的資料夾，一層層瀏覽或依日期看、搜尋、篩選年份，加星號、重新命名、刪除；放大看時也能勾選一起刪。",
                "The library's Photos tab: add photo folders on your NAS or PC to browse by folder or date, search, filter by year, star, rename and delete, and select photos while viewing them full size."
            )
            .into(),
            Try::Photos,
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
        Try::Photos => super::library_dialog::open_photos(app),
        Try::Compact => super::title_bar::enter_compact(app, ctx),
    }
}
