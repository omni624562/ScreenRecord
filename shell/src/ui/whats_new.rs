//! 新功能介紹：更新到新的次版號（例如 3.1）後第一次開啟時顯示一次，列出重點功能，可以直接「試試看」。

use super::library_dialog::{Kind, LibraryDialog};
use super::settings_dialog::{self, Page};
use super::theme::{self, Btn};
use super::UiApp;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::version::APP_VERSION;

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

const ITEMS: [(&str, &str, Try); 10] = [
    ("截圖旁的箭頭", "「截圖」旁的小箭頭：框選、延遲截圖、長截圖（自動捲動接成一張）、讀取 QR 碼、步驟截圖（做成教學文件）、取色器、尺規。框選時游標旁有放大鏡，按 C 複製色碼。", Try::None),
    ("截圖編輯", "標註、自動遮個資、聚光燈、放大鏡、加上圖片、背景與圓角、旋轉；截完右下角的小縮圖可以直接編輯、複製、釘選。", Try::EditLastShot),
    ("錄影效果", "錄影時顯示滑鼠點擊、按下的快捷鍵、游標光暈，也可以在螢幕上放攝影機小窗（錄影前就顯示，可拖曳移動、調整大小）、隱藏桌面圖示。", Try::Settings(Page::Record)),
    ("螢幕畫筆", "按 Ctrl+Alt+D 直接在螢幕上畫線圈重點，錄影時會一起錄進去；1～4 換顏色，Esc 結束。", Try::None),
    ("只錄聲音", "範圍選「只錄聲音」，錄會議或口述；一樣可以剪輯、降噪，在「更多」選單存成 M4A。", Try::None),
    ("只錄某個視窗", "「自訂範圍 → 選擇視窗」，視窗移到哪裡就錄到哪裡。", Try::None),
    ("錄影中打點", "錄影時按 Ctrl+Alt+M 做記號，剪輯時直接跳過去。", Try::None),
    ("剪輯", "局部加速、自動剪掉沒動靜的片段、跟著點擊放大、背景與圓角、自動字幕、加上 Logo、降噪與音量平衡。", Try::Videos),
    ("整理與分享", "多張截圖拼成一張、多支錄影合併成一支、壓縮到指定大小、複製成檔案貼到 LINE。", Try::Shots),
    ("錄影前測試音量", "主畫面「聲音」旁的「測試音量」：講幾句話，確定麥克風收得到。", Try::None),
];

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let mut close = false;
    let mut act = Try::None;
    let modal = egui::Modal::new(Id::new("whats-new")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width((ctx.content_rect().width() - 80.0).min(620.0));
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} 版的新功能", minor_version())).font(theme::font_bold(18.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(theme::Icon::Close).ghost().small().tooltip("關閉").show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 190.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (title, desc, t) in ITEMS {
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
                            if Btn::new("試試看").small().show(ui).clicked() {
                                act = t;
                            }
                        });
                    }
                });
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if Btn::new("完整的更新說明").ghost().small().show(ui).clicked() {
                act = Try::None;
                close = true;
                app.changelog_open = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new("知道了").primary().show(ui).clicked() {
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
                app.toast("先截一張圖，或從這裡選一張按「編輯」", false);
            }
        },
        Try::Videos => app.library = Some(LibraryDialog::new(Kind::Video)),
        Try::Shots => app.library = Some(LibraryDialog::new(Kind::Shot)),
    }
}
