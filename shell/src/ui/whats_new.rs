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
    /// 打開設定的某一頁（這一版的介紹沒用到，保留給之後）
    #[allow(dead_code)]
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
            tr!("搜尋截圖裡的字", "Search the text inside screenshots").into(),
            tr!(
                "全部截圖會在背景用 Windows 內建的文字辨識讀出每張圖裡的字，搜尋時一起找：打「502」就找到那張錯誤畫面。找到的縮圖左下角有「字」。",
                "All screenshots reads the text in each image in the background (with Windows' built-in text recognition), so searching also finds words inside images: type “502” to find that error page. Matches show a “T” badge."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("找出相似的截圖、清理舊截圖", "Find similar screenshots, clean up old ones").into(),
            tr!(
                "全部截圖的「整理」選單：「找出相似的截圖」把一樣或很像的排成一組，每組留最新的一張；「清理舊截圖」一次把超過一週到一年的截圖移到資源回收筒。",
                "The “Tidy up” menu in All screenshots: “Find similar screenshots” groups identical or near-identical images and keeps the newest in each group; “Clean up old screenshots” moves everything older than a week to a year to the Recycle Bin."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("加星號、依日期分組", "Stars and grouping by day").into(),
            tr!(
                "重要的截圖、錄影按右上角的星號，「選取全部」和清理都會略過它們，「有星號的」只看它們。全部截圖依日期分組，日期旁的勾選框一次選一整天。",
                "Star important screenshots and recordings: “Select all” and clean-up skip them, and “Starred” shows only them. All screenshots is grouped by day; the box next to a date selects the whole day."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("直接拖到其他程式", "Drag files into other apps").into(),
            tr!(
                "把縮圖拖到 LINE、Teams、Word、瀏覽器或檔案總管就能直接傳過去（勾選多張時一起拖）；從空白處拖曳則是框選。",
                "Drag a thumbnail into chat apps, Word, a browser or File Explorer to share it (ticked items go together); dragging from empty space draws a selection box."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("剪輯：找出沒動靜、沒說話的片段", "Editor: find idle and quiet parts").into(),
            tr!(
                "剪輯視窗「修剪」分頁的「找出沒動靜、沒說話的片段」會在時間軸上標出畫面不動又沒聲音、以及沒說話（打字、等待）的地方，再選全部刪除或加速 2～8 倍。",
                "In the editor's Trim tab, “Find idle and quiet parts” marks still, silent parts and quiet parts (typing, waiting) on the timeline, then removes them or speeds them up 2–8×."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("依字幕自動分章節", "Chapters from subtitles").into(),
            tr!(
                "有字幕的影片，在「字幕」分頁按「依字幕產生章節」，在講話停頓最久的地方切成幾章、用第一句當標題，輸出時寫進影片，也能貼到 YouTube。",
                "For videos with subtitles, “Chapters from subtitles” in the Subtitles tab splits at the longest pauses, titles each chapter with its first line, and writes the chapters into the output video (also ready for YouTube)."
            )
            .into(),
            Try::None,
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
