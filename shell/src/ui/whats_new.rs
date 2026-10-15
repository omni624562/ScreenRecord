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
            tr!("用點的找截圖", "Find screenshots by clicking").into(),
            tr!(
                "不用記得關鍵字：全部截圖左邊的「用點的找」列出截圖時在用的程式、內容（錯誤訊息、網頁、對話、表格…）、圖裡常出現的詞、時間與顏色，點幾下就縮小範圍，每個選項旁都有張數。",
                "No need to remember keywords: “Browse” on the left of All screenshots lists the app you were using, the content (errors, web pages, chats, tables…), common words, dates and colours. Click a few to narrow down; each shows how many match."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("記下截圖時在用的程式", "Remembers the app you captured").into(),
            tr!(
                "之後的截圖會記下當時在用的程式與視窗標題（例如 Chrome 的網頁標題），用程式就能找到。",
                "New screenshots remember the app and window title you were using (such as the web page title in Chrome), so you can find them by app."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("搜尋時給建議", "Search suggestions").into(),
            tr!(
                "打一兩個字就列出圖裡真的有的詞與張數（打「報」出現「報價單 18 張」）；辨識常認錯的 0 和 O、1 和 l 也找得到，「錯誤」也會找 error。",
                "Type a letter or two to see words that really appear in your screenshots, with counts. Common recognition mix-ups (0/O, 1/l) are tolerated, and Chinese and English equivalents are searched together."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("相關的截圖", "Related screenshots").into(),
            tr!(
                "檢視截圖時，下方列出同一個程式、時間接近、內容相近的截圖。先找到附近的一張，就能順著找到要的那張。",
                "When viewing a screenshot, related ones (same app, around the same time, similar content) appear below. Find one nearby and follow the trail to the one you want."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("月曆", "Calendar").into(),
            tr!(
                "全部截圖的「月曆」一個月一頁，看每天截了幾張、那天圖裡常出現什麼字；點一天就列出那天的截圖。",
                "“Calendar” in All screenshots shows a month at a time: how many screenshots each day and what they were about. Click a day to list them."
            )
            .into(),
            Try::Library,
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
