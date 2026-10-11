//! 新功能介紹：更新到新的次版號（例如 3.1）後第一次開啟時顯示一次，列出重點功能，可以直接「試試看」。

use super::library_dialog::{Kind, LibraryDialog};
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
    EditLastShot,
    Videos,
    Shots,
}

/// 新功能清單（標題、說明、試試看）：依介面語言
fn items() -> [(&'static str, &'static str, Try); 14] {
    [
        (
            "語言 Language",
            tr!(
                "介面可以換成英文：「設定 → 進階 → 語言 Language」選繁體中文、English，或跟著 Windows。",
                "The interface can be shown in Traditional Chinese or English: Settings → Advanced → 語言 Language (or follow Windows)."
            ),
            Try::Settings(Page::Advanced),
        ),
        (
            tr!("截圖旁的箭頭", "Screenshot arrow menu"),
            tr!(
                "「截圖」旁的小箭頭：框選、延遲截圖、長截圖（自動捲動接成一張）、讀取 QR 碼、步驟截圖（做成教學文件）、取色器、尺規。框選時游標旁有放大鏡，按 C 複製色碼。",
                "The small arrow next to “Screenshot”: select area, delayed screenshot, scrolling screenshot (scrolls automatically and stitches everything into one image), read QR codes, step capture (makes a step-by-step guide), color picker, ruler. While selecting, a magnifier follows the cursor; press C to copy the color code."
            ),
            Try::None,
        ),
        (
            tr!("截圖編輯", "Screenshot editing"),
            tr!(
                "標註、自動遮個資、聚光燈、放大鏡、加上圖片、背景與圓角、旋轉；截完右下角的小縮圖可以直接編輯、複製、釘選。",
                "Annotations, auto-redact personal info, spotlight, magnifier, images, background and rounded corners, rotate. Right after a screenshot, use the thumbnail in the bottom-right corner to edit, copy or pin it."
            ),
            Try::EditLastShot,
        ),
        (
            tr!("錄影效果", "Recording effects"),
            tr!(
                "錄影時顯示滑鼠點擊、按下的快速鍵、游標光暈，也可以在螢幕上放攝影機小視窗（錄影前就顯示，可拖曳移動、調整大小）、隱藏桌面圖示。",
                "Show mouse clicks, pressed shortcuts and a cursor highlight while recording. You can also put a camera bubble on the screen (shown before recording starts; drag to move or resize it) and hide desktop icons."
            ),
            Try::Settings(Page::Record),
        ),
        (
            tr!("排程錄影", "Scheduled recording"),
            tr!(
                "右上角「排程」：指定時間自動開始錄影，也可以錄幾分鐘後自動停止；等待與錄影時電腦不會睡眠。",
                "“Schedule” at the top right: start recording automatically at a set time and optionally stop after a set length. The PC stays awake while waiting and recording."
            ),
            Try::None,
        ),
        (
            tr!("講稿小視窗", "Script window"),
            tr!(
                "右上角「講稿」：浮在最上層、不會被錄進影片；「提詞」用大字自動往上捲，開始錄影時自動開始、暫停時停下。",
                "“Script” at the top right: a window that stays on top and isn't recorded. “Prompt” shows your script in large text that scrolls up automatically, starting when you record and stopping when you pause."
            ),
            Try::None,
        ),
        (
            tr!("設定組合", "Recording presets"),
            tr!(
                "把範圍、畫面、聲音、儲存位置存成組合（例如「教學影片」「線上會議」），在主畫面下方「設定」旁或系統匣選單一次切換。",
                "Save the area, video, audio and save location as a preset (e.g. “Tutorial” or “Meeting”) and switch in one click next to “Settings” at the bottom of the main window or from the tray menu."
            ),
            Try::None,
        ),
        (
            tr!("螢幕畫筆", "Screen pen"),
            tr!(
                "按 Ctrl+Alt+D 直接在螢幕上畫線圈重點，錄影時會一起錄進去；1～4 換顏色，Esc 結束。",
                "Press Ctrl+Alt+D to draw on the screen and circle key points; it is recorded along with the video. 1–4 change the color, Esc ends."
            ),
            Try::None,
        ),
        (
            tr!("只錄聲音", "Audio only"),
            tr!(
                "範圍選「只錄聲音」，錄會議或口述；一樣可以剪輯、降噪，在「更多」選單存成 M4A。",
                "Choose “Audio only” as the capture area to record meetings or narration. You can still edit it and apply noise reduction, and save it as M4A from the “More” menu."
            ),
            Try::None,
        ),
        (
            tr!("只錄某個視窗", "Record a single window"),
            tr!("「自訂範圍 → 選擇視窗」，視窗移到哪裡就錄到哪裡。", "“Custom area → Choose window”: the recording follows the window wherever it moves."),
            Try::None,
        ),
        (
            tr!("錄影中加標記", "Markers while recording"),
            tr!(
                "錄影時按 Ctrl+Alt+M 加標記，剪輯時直接跳過去；存檔時變成影片章節，可以複製貼到 YouTube。",
                "Press Ctrl+Alt+M while recording to add a marker, then jump straight to it in the editor. Markers become video chapters you can copy to YouTube."
            ),
            Try::None,
        ),
        (
            tr!("剪輯", "Editing"),
            tr!(
                "局部加速、自動剪掉沒動靜的片段、跟著點擊放大、背景與圓角、自動字幕、加上 Logo、降噪與音量平衡。",
                "Speed up a section, auto-cut idle parts, zoom on clicks, background and rounded corners, automatic subtitles, add a logo, noise reduction and loudness normalization."
            ),
            Try::Videos,
        ),
        (
            tr!("整理與分享", "Organize and share"),
            tr!(
                "做成 WebP 動圖（比 GIF 小很多）、多張截圖拼成一張、多支錄影合併成一支、壓縮到指定大小、複製成檔案貼到 LINE。",
                "Make WebP animations (much smaller than GIF), combine several screenshots into one image, merge several recordings into one, compress to a target size, copy as a file to paste into LINE."
            ),
            Try::Shots,
        ),
        (
            tr!("錄影前測試音量", "Test levels before recording"),
            tr!(
                "主畫面「聲音」旁的「測試音量」：講幾句話，確定麥克風收得到。",
                "Click “Test levels” next to “Audio” on the main window and say a few words to make sure the microphone picks you up."
            ),
            Try::None,
        ),
    ]
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let mut close = false;
    let mut act = Try::None;
    let modal = egui::Modal::new(Id::new("whats-new")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width((ctx.content_rect().width() - 80.0).min(620.0));
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("{} 版的新功能", "What's new in {}", minor_version())).font(theme::font_bold(18.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(theme::Icon::Close).ghost().small().tooltip(tr!("關閉", "Close")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 190.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (title, desc, t) in items() {
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
        Try::EditLastShot => match app.core.last_shot() {
            Some(s) => super::editor::shot::open(app, s.path),
            None => {
                app.library = Some(LibraryDialog::new(Kind::Shot));
                app.toast(tr!("先截一張圖，或從這裡選一張按「編輯」", "Take a screenshot first, or pick one here and click “Edit”"), false);
            }
        },
        Try::Videos => app.library = Some(LibraryDialog::new(Kind::Video)),
        Try::Shots => app.library = Some(LibraryDialog::new(Kind::Shot)),
    }
}
