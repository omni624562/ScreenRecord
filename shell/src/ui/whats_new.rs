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
            tr!("快速鍵統一", "Same keys everywhere").into(),
            tr!(
                "剪輯、檢視器、主畫面播放都一樣：← → 跳 5 秒（Shift 1 秒）、, . 一格一格、M 靜音；剪輯的標記改成 B。檢視器的鍵盤圖示可以看全部。",
                "Editor, viewer and main-view player share the same keys: ← → jump 5 s (Shift 1 s), , . step one frame, M mutes; editor markers are now B. The keyboard icon in the viewer lists them all."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("長時間的工作可以取消", "Stop long jobs midway").into(),
            tr!(
                "Jellyfin 批次整理、人物和標籤重新命名、步驟教學做成影片都顯示百分比，可以隨時按「取消」。",
                "Jellyfin batch edits, renaming people and tags, and building step-guide videos show a percentage and can be cancelled at any time."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("只用鍵盤看全部錄影", "Browse the library with the keyboard").into(),
            tr!(
                "在搜尋框按 ↓ 進到清單，↑↓ 移動、空白鍵勾選、Enter 開啟。",
                "Press ↓ in the search box to enter the list, then ↑↓ to move, Space to tick and Enter to open."
            )
            .into(),
            Try::Library(false),
        ),
        (
            tr!("出錯時說清楚、可以復原", "Clearer errors, and undo").into(),
            tr!(
                "檔案出錯時直接說原因（被其他程式使用、空間不足、沒有權限…），錯誤提示會留到你關掉；從合集移除可以復原，刪除的提示可以開啟資源回收筒。",
                "File errors say why (in use, disk full, no permission…) and stay until you close them; removing from a collection can be undone, and the delete toast opens the Recycle Bin."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("更安全、更穩定、更快", "Safer, steadier, faster").into(),
            tr!(
                "Jellyfin 金鑰加密保存、更新檢查數位簽章；設定檔不會寫到一半壞掉；影片資訊記在硬碟，開啟檔案庫更快。",
                "The Jellyfin key is stored encrypted and updates check their signature; settings can't be half-written; video info is cached on disk so the library opens faster."
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
