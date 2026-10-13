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
    /// 範圍切到「只錄聲音」
    AudioOnly,
    /// 開啟排程錄影
    Schedule,
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
            tr!("框選截圖更好找", "Easier area screenshots").into(),
            trf!(
                "「開始錄影」下面就是「框選截圖」{}與「整個範圍」；小箭頭選單和系統匣一樣分成「截圖方式」「小工具」（QR 碼、取色器、尺規、螢幕畫筆）。",
                "“Select area”{} and “Whole area” are right below “Start recording”. The small arrow menu matches the tray: “Screenshot” and “Tools” (QR code, color picker, ruler, screen pen).",
                key(app, 3)
            ),
            Try::Snip,
        ),
        (
            tr!("錄影優先", "Recording comes first").into(),
            tr!(
                "錄完自動處理（剪掉沒動靜的片段、字幕、壓縮）進行中也能直接開始錄影：處理會先暫停，錄完再繼續。設定裡獨立成「錄完處理」一頁。",
                "You can start recording even while post-processing (cutting idle parts, subtitles, compression) is running: it pauses and continues afterwards. It now has its own “After recording” page in Settings."
            )
            .into(),
            Try::Settings(Page::After),
        ),
        (
            tr!("回溯錄影", "Replay buffer").into(),
            trf!(
                "原本的「重播」改名「回溯錄影」：在背景一直錄、只留最近幾分鐘，需要時再存下{}；停止前會先問要不要存。",
                "Keeps recording in the background, holding only the last few minutes; save them when you need to{}. Before stopping, it asks whether to save.",
                key(app, 6)
            ),
            Try::None,
        ),
        (
            tr!("只錄聲音存成 M4A", "Audio only, saved as M4A").into(),
            tr!("「只錄聲音」時按「開始錄音」，可以選存成 M4A（手機、電腦都能直接播放）或 MP4。", "In “Audio only”, click “Start audio recording” and choose to save as M4A (plays on any phone or computer) or MP4.")
                .into(),
            Try::AudioOnly,
        ),
        (
            tr!("排程錄影可以選日期", "Schedule on any date").into(),
            tr!(
                "排程可以選今天、明天或其他日期；程式重新開啟後也會繼續等。",
                "Schedule a recording for today, tomorrow or another date; it keeps waiting even if you reopen the app."
            )
            .into(),
            Try::Schedule,
        ),
        (
            tr!("外觀與開機啟動", "Appearance and startup").into(),
            tr!(
                "「設定 → 進階」可以選淺色或深色外觀、開機時自動啟動，也可以在這裡結束程式。",
                "In Settings → Advanced you can choose a light or dark appearance, start with Windows, and quit the app."
            )
            .into(),
            Try::Settings(Page::Advanced),
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
        Try::AudioOnly => {
            app.s.source_type = super::settings::SourceType::Audio;
            app.save_settings();
        }
        Try::Schedule => super::schedule::open(app),
    }
}
