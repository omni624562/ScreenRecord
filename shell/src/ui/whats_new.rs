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
    /// 在螢幕上框選截圖
    Snip,
    /// 開啟全部錄影
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
fn items(app: &UiApp) -> Vec<(String, String, Try)> {
    vec![
        (
            tr!("框選截圖更好找、放開後還能調整", "Area screenshots: easier to find, adjustable").into(),
            trf!(
                "「開始錄影」下面就是「框選截圖」{}；框好後可以拉邊調整，再選完成、編輯、複製或釘在桌面。延遲截圖會在畫面上倒數。",
                "“Select area”{} is right below “Start recording”. After selecting, drag the edges to adjust, then finish, edit, copy or pin it. Delayed screenshots count down on screen.",
                key(app, 3)
            ),
            Try::Snip,
        ),
        (
            tr!("剪輯更順手，也不怕白做", "Easier editing, no lost work").into(),
            tr!(
                "時間軸上方有時間尺規，按 S 在播放位置分割；字幕、聲音有自己的分頁。輸出失敗或取消時可以「回到剪輯」，關閉前會問要不要儲存。",
                "A time ruler sits above the timeline, and S splits at the playhead. Subtitles and audio have their own tabs. If saving fails or is canceled, “Back to editing” restores everything, and closing asks whether to save."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("截圖編輯可以放大、另存新檔", "Zoom and Save As in the screenshot editor").into(),
            tr!(
                "Ctrl+滾輪放大縮小，長截圖用「符合寬度」捲動標註；「儲存」旁的箭頭可以另存新檔，選 PNG 或 JPG（檔案較小）。",
                "Ctrl+wheel zooms; long screenshots open at “Fit width” so you can scroll and annotate. The arrow next to “Save” offers Save As, as PNG or JPG (smaller file)."
            )
            .into(),
            Try::None,
        ),
        (
            tr!("壓縮影片傳 LINE、Email", "Compress videos for LINE or email").into(),
            tr!(
                "全部錄影、檢視器的「⋯」有「壓縮（傳 LINE / Email）」；每一列可按右鍵、可搜尋「10/11」「昨天」；合併、拼圖前可以調整順序。",
                "“⋯” in All recordings and the viewer has “Compress (for LINE / email)”. Right-click any row, search “10/11” or “yesterday”, and reorder before merging or combining."
            )
            .into(),
            Try::Library,
        ),
        (
            tr!("回溯錄影，錄影優先", "Replay buffer; recording comes first").into(),
            trf!(
                "原本的「重播」改名「回溯錄影」：在背景一直錄、只留最近幾分鐘，需要時再存下{}，停止前會先問要不要存。錄完自動處理進行中也能直接開始錄影。",
                "“Replay” is now the replay buffer: it keeps the last few minutes so you can save them{}, and asks before stopping. You can start recording even while post-processing runs.",
                key(app, 6)
            ),
            Try::None,
        ),
        (
            tr!("儲存位置用選的，不用打字", "Pick the save folder instead of typing").into(),
            tr!(
                "「設定 → 儲存位置」或主畫面下方的資料夾按鈕可以直接選資料夾；全部錄影會顯示檔名，按每一列的「重新命名」或按兩下檔名就能改名。外觀可以選淺色或深色（設定 → 進階）。",
                "Choose the folder in Settings → Save location or from the folder button at the bottom of the main window. All recordings shows file names; rename with the rename button or by double-clicking. Pick a light or dark appearance in Settings → Advanced."
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
        Try::Library => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video)),
    }
}
