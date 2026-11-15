//! 檔案庫三個分頁（錄影、截圖、Jellyfin）共用的查詢列：輸入框、下拉、分段選擇、按鈕都一樣高（H）、一樣的灰框樣式，
//! 同一組裡間距 6、組和組之間 16。順序都是：搜尋 → 篩選 → 排序 → 縮圖 / 清單 → 這個分頁才有的功能。
//! 搜尋、篩選、排序靠左，縮圖和這個分頁的功能靠右（寬螢幕時左右平衡，不會全擠在左邊）

use super::theme::{self, Btn, Icon};
use eframe::egui::{self, Id, Ui};

/// 查詢列每個東西的高度
pub const H: f32 = 32.0;
const GAP: f32 = 6.0;
const GROUP_GAP: f32 = 16.0;
const SEARCH_MIN: f32 = 200.0;
/// 放不下時搜尋框最短縮到這麼寬
const SEARCH_SMALL: f32 = 130.0;

/// 查詢列的哪一段
pub enum Part {
    /// 搜尋框（給它的寬度）
    Search(f32),
    /// 篩選、排序
    Left,
    /// 縮圖 / 清單、這個分頁才有的功能（放不下時 true：按鈕只留圖示、「縮圖」兩字移除）
    Right(bool),
}

/// 畫查詢列：draw 依序被叫三次（搜尋、左、右）。寬度用上一次畫的量，算搜尋框能多寬、要不要縮。
/// 一定待在這一排的寬度裡（不會把整個檔案庫撐得比視窗寬）：放不下時右邊那組先只留圖示、搜尋框縮短，
/// 還是放不下就把右邊那組換到第二排
pub fn bar(ui: &mut Ui, id: Id, mut draw: impl FnMut(&mut Ui, Part)) {
    let row_w = ui.available_width();
    // 上一次量的：左邊那組、右邊那組（完整的、只留圖示的）
    let (left_w, right_w, right_c): (f32, f32, f32) = ui.data(|d| d.get_temp(id)).unwrap_or((0.0, 0.0, 0.0));
    let gaps = GROUP_GAP * 2.0 + GAP;
    let compact = row_w - (left_w + right_w + gaps) < SEARCH_MIN;
    let right_now = if compact && right_c > 0.0 { right_c } else { right_w };
    // 只留圖示、搜尋框縮到最短還是放不下：右邊那組換到第二排（完整的）
    let two_rows = compact && right_c > 0.0 && row_w - (left_w + right_c + gaps) < SEARCH_SMALL;
    let style = |ui: &mut Ui| {
        ui.set_height(H);
        theme::field_style(ui);
        ui.spacing_mut().interact_size.y = H;
        ui.spacing_mut().item_spacing.x = GAP;
    };
    let (mut lw, mut rw) = (left_w, 0.0);
    row(ui, row_w, |ui| {
        style(ui);
        let rest = if two_rows { left_w + GROUP_GAP } else { left_w + right_now + gaps };
        draw(ui, Part::Search((row_w - rest).clamp(if compact { SEARCH_SMALL } else { SEARCH_MIN }, 420.0)));
        ui.add_space(GROUP_GAP - GAP);
        let x = ui.min_rect().right();
        draw(ui, Part::Left);
        lw = ui.min_rect().right() - x;
        if two_rows {
            return;
        }
        ui.add_space(GROUP_GAP - GAP);
        // 右邊那組靠右（用上一次量的寬度）
        let pad = ui.available_width() - right_now - GAP;
        if pad > 0.0 {
            ui.add_space(pad);
        }
        let x = ui.min_rect().right();
        draw(ui, Part::Right(compact));
        rw = ui.min_rect().right() - x;
    });
    if two_rows {
        ui.add_space(GAP);
        row(ui, row_w, |ui| {
            style(ui);
            let pad = ui.available_width() - right_w;
            if pad > 0.0 {
                ui.add_space(pad);
            }
            let x = ui.min_rect().right();
            draw(ui, Part::Right(false));
            rw = ui.min_rect().right() - x;
        });
    }
    let stored = if compact && !two_rows { (lw, right_w, rw) } else { (lw, rw, right_c) };
    ui.data_mut(|d| d.insert_temp(id, stored));
    // 這一格才第一次量到只留圖示時多寬：再畫一格，用量到的寬度決定要不要換成兩排
    if compact && right_c <= 0.0 {
        ui.ctx().request_repaint();
    }
}

/// 固定寬度的一排：裡面的東西比這一排寬時只是被裁掉，不會把外面撐寬
fn row(ui: &mut Ui, w: f32, add: impl FnOnce(&mut Ui)) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, H), egui::Sense::hover());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()).expand2(egui::vec2(0.0, 4.0)));
    add(&mut child);
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

/// 搜尋框（或其他右邊留了約 26 像素的輸入框）有字時，右邊一個 ×：按一下清掉打的字（回傳 true）
pub fn clear_x(ui: &mut Ui, field: &egui::Response, text: &mut String) -> bool {
    if text.is_empty() {
        return false;
    }
    let p = theme::pal(ui);
    let x = egui::Rect::from_center_size(egui::pos2(field.rect.max.x - 14.0, field.rect.center().y), egui::vec2(20.0, 20.0));
    let xr = ui.interact(x, field.id.with("clear"), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(screenrecorder_core::tr!("清除", "Clear"));
    if xr.hovered() {
        ui.painter().circle_filled(x.center(), 10.0, p.surface2);
    }
    theme::paint_icon(ui.painter(), egui::Rect::from_center_size(x.center(), egui::vec2(11.0, 11.0)), Icon::Close, if xr.hovered() { p.text } else { p.muted });
    if xr.clicked() {
        text.clear();
        // 清掉後游標留在輸入框，可以直接打新的
        field.request_focus();
        return true;
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 在寬 w 的面板裡畫幾格查詢列（左邊那組寬 left、右邊那組完整 right、只留圖示 right_c）：回傳面板最後用了多寬、查詢列多高
    fn run(w: f32, left: f32, right: f32, right_c: f32) -> (f32, f32) {
        let ctx = egui::Context::default();
        let mut used = (0.0, 0.0);
        for _ in 0..6 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 400.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                ui.set_width(w);
                let top = ui.cursor().top();
                bar(ui, Id::new("test-bar"), |ui, part| match part {
                    Part::Search(sw) => drop(ui.allocate_space(egui::vec2(sw, H))),
                    Part::Left => drop(ui.allocate_space(egui::vec2(left, H))),
                    Part::Right(compact) => drop(ui.allocate_space(egui::vec2(if compact { right_c } else { right }, H))),
                });
                used = (ui.min_rect().width(), ui.cursor().top() - top);
            });
            out.textures_delta.clear();
        }
        used
    }

    #[test]
    fn bar_never_wider_than_its_row() {
        // 夠寬：一排
        let (w, h) = run(1200.0, 400.0, 300.0, 120.0);
        assert!(w <= 1200.5 && h < H * 1.5, "{w} {h}");
        // 窄一點：右邊只留圖示，還是一排
        let (w, h) = run(760.0, 400.0, 300.0, 120.0);
        assert!(w <= 760.5 && h < H * 1.5, "{w} {h}");
        // 更窄：右邊那組換到第二排，面板不會被撐寬
        let (w, h) = run(640.0, 400.0, 300.0, 120.0);
        assert!(w <= 640.5 && h > H * 1.5, "{w} {h}");
        // 連左邊那組都放不下：裁掉，也不撐寬
        let (w, _) = run(300.0, 400.0, 300.0, 120.0);
        assert!(w <= 300.5, "{w}");
    }
}
