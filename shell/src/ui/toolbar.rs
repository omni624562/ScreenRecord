//! 檔案庫三個分頁（錄影、截圖、Jellyfin）共用的查詢列：輸入框、下拉、分段選擇、按鈕都一樣高（H）、一樣的灰框樣式，
//! 同一組裡間距 6、組和組之間 16。順序都是：搜尋 → 篩選 → 排序 → 縮圖 / 清單 → 這個分頁才有的功能。
//! 排法（示意，選好後只留一種）：A 搜尋框隨寬度變長，其他接在後面；B 搜尋、篩選、排序靠左，其他靠右

use super::theme::{self, Btn, Icon};
use eframe::egui::{self, Id, Ui};

/// 查詢列每個東西的高度
pub const H: f32 = 32.0;
const GAP: f32 = 6.0;
const GROUP_GAP: f32 = 16.0;
const SEARCH_MIN: f32 = 200.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    A,
    B,
}

/// 示意用：SCREENRECORDER_LIBBAR=b 看 B 排法
pub fn mode() -> Mode {
    if std::env::var("SCREENRECORDER_LIBBAR").as_deref() == Ok("b") {
        Mode::B
    } else {
        Mode::A
    }
}

/// 查詢列的哪一段
pub enum Part {
    /// 搜尋框（給它的寬度）
    Search(f32),
    /// 篩選、排序
    Left,
    /// 縮圖 / 清單、這個分頁才有的功能（放不下時 true：按鈕只留圖示、「縮圖」兩字拿掉）
    Right(bool),
}

/// 畫一排查詢列：draw 依序被叫三次（搜尋、左、右）。寬度用上一次畫的量，算搜尋框能多寬、要不要縮
pub fn bar(ui: &mut Ui, id: Id, mut draw: impl FnMut(&mut Ui, Part)) {
    // 這一排的高度先固定好，每個東西都對齊同一條中線（不然先畫的會比後畫的高一點）
    let w = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(w, H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_height(H);
        theme::field_style(ui);
        ui.spacing_mut().interact_size.y = H;
        ui.spacing_mut().item_spacing.x = GAP;
        let (left_w, right_w): (f32, f32) = ui.data(|d| d.get_temp(id)).unwrap_or((0.0, 0.0));
        let mode = mode();
        let row_w = ui.available_width();
        let rest = left_w + right_w + GROUP_GAP * 2.0 + GAP;
        let compact = row_w - rest < SEARCH_MIN;
        let max = if mode == Mode::A { 560.0 } else { 420.0 };
        draw(ui, Part::Search((row_w - rest).clamp(SEARCH_MIN, max)));
        ui.add_space(GROUP_GAP - GAP);
        let x = ui.min_rect().right();
        draw(ui, Part::Left);
        let lw = ui.min_rect().right() - x;
        ui.add_space(GROUP_GAP - GAP);
        // B：右邊那組靠右（用上一次量的寬度）
        if mode == Mode::B {
            let pad = ui.available_width() - right_w - GAP;
            if pad > 0.0 {
                ui.add_space(pad);
            }
        }
        let x = ui.min_rect().right();
        draw(ui, Part::Right(compact));
        let rw = ui.min_rect().right() - x;
        if !compact {
            ui.data_mut(|d| d.insert_temp(id, (lw, rw)));
        }
    });
}

/// 查詢列的按鈕：和下拉一樣的灰框、一樣高；放不下時只留圖示（名稱放在滑鼠提示）
pub fn button(text: &str, icon: Icon, compact: bool, tip: &str) -> Btn {
    let b = if compact { Btn::icon_only(icon).tooltip(format!("{text}：{tip}")) } else { Btn::new(text).icon(icon).tooltip(tip) };
    b.height(H)
}

/// 搜尋框（和查詢列一樣高；右邊留位置給清除鈕）
pub fn search_box<'a>(text: &'a mut String, hint: &str, width: f32) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .hint_text(hint)
        .desired_width(width)
        .min_size(egui::vec2(0.0, H))
        .margin(egui::Margin { left: 8, right: 28, top: 2, bottom: 2 })
        .vertical_align(egui::Align::Center)
}

/// 「縮圖」兩字 + 分段選擇（和查詢列一樣高）
pub fn view_switch<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, items: &[(T, &str)], compact: bool, tip: &str) -> bool {
    if !compact {
        ui.label(theme::muted(ui, egui_label())).on_hover_text(tip);
    }
    theme::segmented_h(ui, value, items, true, H - 8.0)
}

fn egui_label() -> &'static str {
    screenrecorder_core::tr!("縮圖", "Thumbnails")
}
