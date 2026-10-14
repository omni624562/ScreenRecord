//! 新功能介紹：更新到新的次版號（例如 3.1）後第一次開啟時顯示一次，列出重點功能（最多 6 項），可以直接「試試看」。
//! 第一次安裝（還沒看過任何介紹）時改成三個步驟的「快速上手」。

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
    Settings(Page),
    /// 在螢幕上框選截圖（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
    Snip,
    /// 開啟全部截圖
    Library,
}

/// 第一次使用（還沒看過任何介紹）：三個步驟的「快速上手」
fn first_run(app: &UiApp) -> bool {
    app.s.seen_version.is_empty()
}

/// 快速鍵的寫法：「（Ctrl+Alt+R）」；停用時是空的
fn key(app: &UiApp, i: usize) -> String {
    let k = app.keys.label(i);
    if k.is_empty() {
        String::new()
    } else {
        trf!("（{k}）", " ({k})")
    }
}

/// 快速上手：選範圍 → 開始錄影 → 在最近錄影播放／剪輯
fn quick_start(app: &UiApp) -> [(String, String); 3] {
    [
        (
            tr!("選要錄的範圍", "Choose what to record").into(),
            tr!(
                "在左邊選「單一螢幕」「所有螢幕」「自訂範圍」（在預覽上拖曳框選），或「只錄聲音」。",
                "On the left, choose “Single screen”, “All screens”, “Custom area” (drag on the preview to select it) or “Audio only”."
            )
            .into(),
        ),
        (
            tr!("開始錄影", "Start recording").into(),
            trf!(
                "按右邊紅色的「開始錄影」{}，倒數後開始；要停止時按螢幕上方控制列的「停止」，或再按一次快速鍵。",
                "Click the red “Start recording” on the right{}. It starts after a countdown; to stop, click “Stop” on the bar at the top of the screen or press the shortcut again.",
                key(app, 0)
            ),
        ),
        (
            tr!("在「最近錄影」播放、剪輯", "Play and edit in “Recent recordings”").into(),
            tr!(
                "錄好的影片會出現在下方「最近錄影」：點縮圖就播放，剪刀按鈕剪輯，按右鍵還有複製檔案、改名等。",
                "Finished recordings appear under “Recent recordings” at the bottom: click a thumbnail to play it, the scissors to edit, and right-click for copy, rename and more."
            )
            .into(),
        ),
    ]
}

/// 這一版的新功能（標題、說明、試試看）：最多 6 項，快速鍵用目前的設定
fn items(_app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("一次選取、刪除很多張", "Select and delete many at once").into(),
            tr!(
                "全部截圖、全部錄影勾選後，搜尋列右邊有「選取全部 N 張」（每一頁），搭配搜尋很好用：例如打「7/27」再選取全部、按 Delete。Ctrl+A 全選這一頁，再按一次就是全部。",
                "After you tick something in All screenshots or All recordings, “Select all” next to the search box picks every page. Combine it with a search: type “7/27”, select all, press Delete. Ctrl+A selects the page; press it again for everything."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("跨頁勾選、只看已選取", "Selections across pages, “Show selected only”").into(),
            tr!(
                "換頁時勾選會保留，下方顯示「已選取 25 張（其他頁 13 張）」；按「只看已選取」把勾選的放在一起，刪除前再確認一次。有勾選時按 Delete 會刪除勾選的全部。",
                "Ticks stay when you change pages, and the footer shows how many are on other pages. “Show selected only” gathers them so you can check before deleting. With items ticked, Delete removes all of them."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("縮圖可以調大小", "Adjustable thumbnail size").into(),
            tr!(
                "全部截圖的排序旁邊可以選縮圖「小、中、大」，小的一頁大約放 35 張；也可以在縮圖上按住 Ctrl 轉滾輪。會記住你的選擇。",
                "Choose small, medium or large thumbnails next to the sort menu in All screenshots (small fits about 35 per page), or hold Ctrl and turn the mouse wheel over them. Your choice is remembered."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("截圖和錄影分開存、讀圖變快", "Screenshots kept apart, faster thumbnails").into(),
            tr!(
                "截圖改存到「圖片 → 螢幕擷取畫面」，「設定 → 儲存位置」可以各自選資料夾；全部截圖的縮圖不再經過 FFmpeg，打開與換頁快很多。",
                "Screenshots now go to Pictures → Screenshots (choose each folder in Settings → Save location), and All screenshots no longer runs FFmpeg for thumbnails, so opening and paging are much faster."
            )
            .into(),
            Try::Settings(Page::Save),
        ),
    ]
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let mut close = false;
    let mut act = Try::None;
    let first = first_run(app);
    let modal = egui::Modal::new(Id::new("whats-new")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width((ctx.content_rect().width() - 80.0).min(600.0));
        ui.horizontal(|ui| {
            let title = if first { tr!("快速上手：三個步驟", "Quick start: three steps").to_string() } else { trf!("{} 版的新功能", "What's new in {}", minor_version()) };
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
            if first {
                for (i, (title, desc)) in quick_start(app).into_iter().enumerate() {
                    ui.add_space(8.0);
                    ui.horizontal_top(|ui| {
                        // 步驟編號：藍色圓圈
                        let (r, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                        ui.painter().circle_filled(r.center(), 13.0, p.accent);
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, (i + 1).to_string(), theme::font_bold(14.0), p.accent_ink);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(RichText::new(title).font(theme::font_bold(14.5)));
                            ui.add(egui::Label::new(RichText::new(desc).font(theme::font(13.0)).color(p.muted)).wrap());
                        });
                    });
                }
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(theme::muted(
                        ui,
                        tr!("要截圖時按「框選截圖」；更多設定在右下角的「設定」。", "To take a screenshot, click “Select area”. More options are under “Settings” at the bottom right."),
                    ))
                    .wrap(),
                );
                return;
            }
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
                if Btn::new(if first { tr!("開始使用", "Get started") } else { tr!("知道了", "Got it") }).primary().show(ui).clicked() {
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
    }
}
