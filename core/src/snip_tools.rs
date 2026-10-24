//! 框選畫面上的小工具：放大鏡（顯示色碼與座標），以及框選後調整範圍（把手）與工具列的位置。
//! 這裡只有計算與繪製（可以在任何平台測試），Windows 的框選視窗（snip_win.rs）與介面的框選視窗（shell 的 snip.rs）負責顯示與操作。

use crate::annotate;
use crate::tr;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};

/// 放大鏡顯示游標周圍幾格（奇數，正中間是游標所在的像素）
pub const LOUPE_PIXELS: i32 = 15;
/// 每格放大幾倍（100% 縮放時）
pub const LOUPE_ZOOM: i32 = 8;

/// 色碼：#1D4ED8
pub fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// 放大鏡的大小（像素）：(寬, 高)；下方有一條顯示色碼與座標的資訊列
pub fn loupe_size(scale: f32) -> (i32, i32) {
    let side = (LOUPE_PIXELS * LOUPE_ZOOM) as f32 * scale;
    (side.round() as i32, (side + 52.0 * scale).round() as i32)
}

/// 放大鏡放在哪裡（左上角）：游標右下方，太靠近螢幕邊緣時換到另一邊
pub fn loupe_pos((cx, cy): (i32, i32), (lw, lh): (i32, i32), (x0, y0, x1, y1): (i32, i32, i32, i32), gap: i32) -> (i32, i32) {
    let x = if cx + gap + lw <= x1 { cx + gap } else { cx - gap - lw };
    let y = if cy + gap + lh <= y1 { cy + gap } else { cy - gap - lh };
    (x.clamp(x0, (x1 - lw).max(x0)), y.clamp(y0, (y1 - lh).max(y0)))
}

/// 畫出放大鏡（不透明）：px(x, y) 取得畫面上的顏色（超出範圍為 None）；(x, y) 是游標所在的像素
pub fn render_loupe(px: impl Fn(i32, i32) -> Option<[u8; 3]>, x: i32, y: i32, scale: f32, hint: &str) -> Option<Pixmap> {
    let (w, h) = loupe_size(scale);
    let mut pm = Pixmap::new(w as u32, h as u32)?;
    pm.fill(Color::from_rgba8(24, 24, 27, 255));
    let cell = w as f32 / LOUPE_PIXELS as f32;
    let half = LOUPE_PIXELS / 2;
    let fill = |pm: &mut Pixmap, r: Option<Rect>, c: Color| {
        if let Some(r) = r {
            let mut p = Paint::default();
            p.set_color(c);
            pm.fill_rect(r, &p, Transform::identity(), None);
        }
    };
    for j in 0..LOUPE_PIXELS {
        for i in 0..LOUPE_PIXELS {
            let c = match px(x + i - half, y + j - half) {
                Some([r, g, b]) => Color::from_rgba8(r, g, b, 255),
                // 螢幕外：深灰
                None => Color::from_rgba8(40, 40, 44, 255),
            };
            fill(&mut pm, Rect::from_xywh(i as f32 * cell, j as f32 * cell, cell + 0.5, cell + 0.5), c);
        }
    }
    // 十字線（半透明）與中間那格的外框
    let line = Color::from_rgba8(255, 255, 255, 70);
    let mid = half as f32 * cell;
    // 十字線不蓋到中間那格（看得到真正的顏色）
    let side = w as f32;
    fill(&mut pm, Rect::from_xywh(0.0, mid, mid, cell), line);
    fill(&mut pm, Rect::from_xywh(mid + cell, mid, side - mid - cell, cell), line);
    fill(&mut pm, Rect::from_xywh(mid, 0.0, cell, mid), line);
    fill(&mut pm, Rect::from_xywh(mid, mid + cell, cell, side - mid - cell), line);
    let center = px(x, y);
    let t = (1.5 * scale).max(1.0);
    for (dx, dy, ww, hh, c) in [
        (-t, -t, cell + 2.0 * t, t, Color::BLACK),
        (-t, cell, cell + 2.0 * t, t, Color::BLACK),
        (-t, 0.0, t, cell, Color::BLACK),
        (cell, 0.0, t, cell, Color::BLACK),
        (-2.0 * t, -2.0 * t, cell + 4.0 * t, t, Color::WHITE),
        (-2.0 * t, cell + t, cell + 4.0 * t, t, Color::WHITE),
        (-2.0 * t, -t, t, cell + 2.0 * t, Color::WHITE),
        (cell + t, -t, t, cell + 2.0 * t, Color::WHITE),
    ] {
        fill(&mut pm, Rect::from_xywh(mid + dx, mid + dy, ww, hh), c);
    }
    // 外框
    let border = Color::from_rgba8(255, 255, 255, 200);
    for r in
        [Rect::from_xywh(0.0, 0.0, w as f32, 1.0), Rect::from_xywh(0.0, h as f32 - 1.0, w as f32, 1.0), Rect::from_xywh(0.0, 0.0, 1.0, h as f32), Rect::from_xywh(w as f32 - 1.0, 0.0, 1.0, h as f32)]
    {
        fill(&mut pm, r, border);
    }
    fill(&mut pm, Rect::from_xywh(0.0, w as f32, w as f32, 1.0), border);
    // 資訊列：顏色方塊、色碼、座標或說明
    let top = w as f32 + 6.0 * scale;
    if let Some(c) = center {
        let sw = 16.0 * scale;
        fill(&mut pm, Rect::from_xywh(8.0 * scale - 1.0, top + 2.0 * scale - 1.0, sw + 2.0, sw + 2.0), Color::WHITE);
        fill(&mut pm, Rect::from_xywh(8.0 * scale, top + 2.0 * scale, sw, sw), Color::from_rgba8(c[0], c[1], c[2], 255));
        text(&mut pm, &hex(c), 30.0 * scale, top + 10.0 * scale, 14.0 * scale, false);
    }
    text(&mut pm, hint, w as f32 / 2.0, top + 32.0 * scale, 11.5 * scale, true);
    Some(pm)
}

/// 白字（x 為左邊或中間）
fn text(pm: &mut Pixmap, s: &str, x: f32, cy: f32, size: f32, centered: bool) {
    let mut a = annotate::plain_text(s, size as f64, "#ffffff");
    a.x = if centered { x as f64 - a.w / 2.0 } else { x as f64 };
    a.y = cy as f64 - a.h / 2.0;
    annotate::draw(pm, &a, Transform::identity());
}

// ───────────── 框選後調整範圍、工具列 ─────────────

/// 框選好之後要做什麼（工具列的按鈕）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnipAction {
    /// 完成：存檔並複製到剪貼簿
    Save,
    /// 存檔後開啟編輯
    Edit,
    /// 只複製到剪貼簿（不存檔）
    Copy,
    /// 釘在桌面（不存檔）
    Pin,
}

/// 工具列的按鈕
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// 改成整個螢幕
    Full,
    Edit,
    Copy,
    Pin,
    Cancel,
    Done,
}

impl Tool {
    /// 按鈕上的字；record = 只選範圍（例如錄影範圍）
    pub fn label(self, record: bool) -> &'static str {
        match self {
            Tool::Full => tr!("整個螢幕", "Full screen"),
            Tool::Edit => tr!("編輯", "Edit"),
            Tool::Copy => tr!("複製", "Copy"),
            Tool::Pin => tr!("釘在桌面", "Pin"),
            Tool::Cancel => tr!("取消（Esc）", "Cancel (Esc)"),
            Tool::Done if record => tr!("確定（Enter）", "OK (Enter)"),
            Tool::Done => tr!("完成（Enter）", "Done (Enter)"),
        }
    }

    /// 滑鼠移上去時的說明
    pub fn tip(self, record: bool) -> &'static str {
        match self {
            Tool::Full => tr!("改成整個螢幕（游標所在的螢幕）", "Use the whole screen (the one under the cursor)"),
            Tool::Edit => tr!("存檔後開啟編輯，加上標註", "Save, then open it in the editor to annotate"),
            Tool::Copy => tr!("只複製到剪貼簿，不存檔（Ctrl+C）", "Copy to the clipboard without saving (Ctrl+C)"),
            Tool::Pin => tr!("變成浮在最上層的小視窗，不存檔", "Show it in a small always-on-top window without saving"),
            Tool::Cancel => tr!("不截圖（Esc 或右鍵）", "Don't capture (Esc or right-click)"),
            Tool::Done if record => tr!("用這個範圍（Enter）", "Use this area (Enter)"),
            Tool::Done => tr!("存檔並複製到剪貼簿（Enter 或在範圍裡按兩下）", "Save and copy to the clipboard (Enter, or double-click inside the area)"),
        }
    }

    /// 按下去的動作（整個螢幕、取消另外處理）
    pub fn action(self) -> Option<SnipAction> {
        match self {
            Tool::Edit => Some(SnipAction::Edit),
            Tool::Copy => Some(SnipAction::Copy),
            Tool::Pin => Some(SnipAction::Pin),
            Tool::Done => Some(SnipAction::Save),
            Tool::Full | Tool::Cancel => None,
        }
    }
}

/// 工具列的按鈕（由左到右）：截圖有編輯、複製、釘在桌面；只選範圍時只有整個螢幕、取消、確定
pub fn tools(record: bool) -> &'static [Tool] {
    if record {
        &[Tool::Full, Tool::Cancel, Tool::Done]
    } else {
        &[Tool::Full, Tool::Edit, Tool::Copy, Tool::Pin, Tool::Cancel, Tool::Done]
    }
}

/// 工具列放在哪裡（左上角）：範圍下方、靠右對齊；下方放不下時放上方，都放不下時放在範圍裡面的下緣；不超出螢幕。
/// sel、bounds：(左, 上, 右, 下)
pub fn toolbar_pos(sel: (i32, i32, i32, i32), (w, h): (i32, i32), bounds: (i32, i32, i32, i32), gap: i32) -> (i32, i32) {
    let ((l, t, r, b), (x0, y0, x1, y1)) = (sel, bounds);
    let x = (r - w).max(l.min(x1 - w)).clamp(x0, (x1 - w).max(x0));
    let y = if b + gap + h <= y1 {
        b + gap
    } else if t - gap - h >= y0 {
        t - gap - h
    } else {
        (b - gap - h).clamp(y0, (y1 - h).max(y0))
    };
    (x, y)
}

/// 在選取範圍上按到哪裡：移動（裡面）或拉哪個邊、角
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

/// 選取範圍 (左, 上, 右, 下) 上的 (x, y) 按到哪裡；tol = 邊與把手可以按到的距離。範圍外為 None
pub fn grab_at((l, t, r, b): (i32, i32, i32, i32), (x, y): (i32, i32), tol: i32) -> Option<Grab> {
    if x < l - tol || x > r + tol || y < t - tol || y > b + tol {
        return None;
    }
    let near = |a: i32, v: i32| (a - v).abs() <= tol;
    // 範圍很小時優先拉右下（才拉得大）
    let (east, west) = (near(x, r), near(x, l) && !near(x, r));
    let (south, north) = (near(y, b), near(y, t) && !near(y, b));
    Some(match (north, south, west, east) {
        (true, _, true, _) => Grab::NW,
        (true, _, _, true) => Grab::NE,
        (_, true, true, _) => Grab::SW,
        (_, true, _, true) => Grab::SE,
        (true, ..) => Grab::N,
        (_, true, ..) => Grab::S,
        (_, _, true, _) => Grab::W,
        (_, _, _, true) => Grab::E,
        _ => Grab::Move,
    })
}

/// 拖曳範圍的邊、角或整個範圍 (dx, dy)：限制在 bounds 裡、至少 1×1，拉過另一邊時翻過去
pub fn drag_grab(orig: (i32, i32, i32, i32), g: Grab, (dx, dy): (i32, i32), bounds: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let ((l, t, r, b), (x0, y0, x1, y1)) = (orig, bounds);
    if g == Grab::Move {
        let (w, h) = ((r - l).min(x1 - x0), (b - t).min(y1 - y0));
        let nl = (l + dx).clamp(x0, x1 - w);
        let nt = (t + dy).clamp(y0, y1 - h);
        return (nl, nt, nl + w, nt + h);
    }
    let (mut nl, mut nt, mut nr, mut nb) = (l, t, r, b);
    if matches!(g, Grab::W | Grab::NW | Grab::SW) {
        nl += dx;
    }
    if matches!(g, Grab::E | Grab::NE | Grab::SE) {
        nr += dx;
    }
    if matches!(g, Grab::N | Grab::NE | Grab::NW) {
        nt += dy;
    }
    if matches!(g, Grab::S | Grab::SE | Grab::SW) {
        nb += dy;
    }
    let (l, r) = (nl.min(nr).clamp(x0, x1), nl.max(nr).clamp(x0, x1));
    let (t, b) = (nt.min(nb).clamp(y0, y1), nt.max(nb).clamp(y0, y1));
    (l, t, r.max(l + 1).min(x1.max(l + 1)), b.max(t + 1).min(y1.max(t + 1)))
}

/// 選取範圍的八個把手（四個角、四邊的中間）
pub fn handles((l, t, r, b): (i32, i32, i32, i32)) -> [(i32, i32); 8] {
    let (cx, cy) = ((l + r) / 2, (t + b) / 2);
    [(l, t), (cx, t), (r, t), (r, cy), (r, b), (cx, b), (l, b), (l, cy)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_codes() {
        assert_eq!(hex([29, 78, 216]), "#1D4ED8");
        assert_eq!(hex([0, 0, 0]), "#000000");
    }

    #[test]
    fn loupe_stays_on_screen() {
        let area = (0, 0, 1920, 1080);
        let size = loupe_size(1.0);
        assert_eq!(size, (120, 172));
        // 一般：游標右下方
        assert_eq!(loupe_pos((100, 100), size, area, 20), (120, 120));
        // 靠右下角：換到左上方
        assert_eq!(loupe_pos((1900, 1070), size, area, 20), (1760, 878));
        // 第二個螢幕（負座標）
        assert_eq!(loupe_pos((-1900, 10), size, (-1920, 0, 0, 1080), 20), (-1880, 30));
    }

    #[test]
    fn loupe_shows_the_pixel_under_the_cursor() {
        // 畫面：x < 50 紅色，其他藍色；游標在 (50, 3)
        let px = |x: i32, y: i32| (x >= 0 && y >= 0 && x < 100 && y < 100).then_some(if x < 50 { [255, 0, 0] } else { [0, 0, 255] });
        let pm = render_loupe(px, 50, 3, 1.0, "C 複製色碼").unwrap();
        assert_eq!((pm.width(), pm.height()), (120, 172));
        // 左邊幾格是紅色、右邊是藍色
        let c = pm.pixel(4, 4).unwrap();
        assert_eq!((c.red(), c.blue()), (40, 44), "最上面幾列在畫面外（y < 0）");
        let c = pm.pixel(20, 100).unwrap();
        assert_eq!((c.red(), c.blue()), (255, 0));
        let c = pm.pixel(100, 100).unwrap();
        assert_eq!((c.red(), c.blue()), (0, 255));
        // 中間那格不被十字線蓋到
        let c = pm.pixel(60, 60).unwrap();
        assert_eq!((c.red(), c.green(), c.blue()), (0, 0, 255));
        // 資訊列的顏色方塊是游標下的顏色（藍）
        let c = pm.pixel(15, 135).unwrap();
        assert_eq!((c.red(), c.blue()), (0, 255));
    }

    #[test]
    fn adjust_selection_and_place_toolbar() {
        let sel = (100, 100, 300, 200);
        let screen = (0, 0, 1920, 1080);
        assert_eq!(grab_at(sel, (100, 100), 6), Some(Grab::NW));
        assert_eq!(grab_at(sel, (303, 198), 6), Some(Grab::SE));
        assert_eq!(grab_at(sel, (200, 101), 6), Some(Grab::N));
        assert_eq!(grab_at(sel, (98, 150), 6), Some(Grab::W));
        assert_eq!(grab_at(sel, (200, 150), 6), Some(Grab::Move));
        assert_eq!(grab_at(sel, (90, 150), 6), None);
        // 很小的範圍：拉右下
        assert_eq!(grab_at((10, 10, 13, 13), (12, 12), 6), Some(Grab::SE));
        // 拉角、拉邊、移動（不超出螢幕）、拉過頭翻過去
        assert_eq!(drag_grab(sel, Grab::SE, (50, 20), screen), (100, 100, 350, 220));
        assert_eq!(drag_grab(sel, Grab::N, (0, -30), screen), (100, 70, 300, 200));
        assert_eq!(drag_grab(sel, Grab::Move, (-500, 10), screen), (0, 110, 200, 210));
        assert_eq!(drag_grab(sel, Grab::E, (-260, 0), screen), (40, 100, 100, 200));
        assert_eq!(drag_grab(sel, Grab::W, (300, 0), screen), (300, 100, 400, 200));
        assert_eq!(handles(sel)[4], (300, 200));
        // 工具列：下方靠右；下方放不下放上方；都放不下放範圍裡
        // 比範圍寬：從範圍左邊開始
        assert_eq!(toolbar_pos(sel, (400, 40), screen, 8), (100, 208));
        assert_eq!(toolbar_pos((1000, 900, 1900, 1060), (400, 40), screen, 8), (1500, 852));
        assert_eq!(toolbar_pos((0, 0, 1920, 1080), (400, 40), screen, 8), (1520, 1032));
        assert_eq!(tools(true), &[Tool::Full, Tool::Cancel, Tool::Done]);
        assert_eq!(Tool::Done.action(), Some(SnipAction::Save));
        assert_eq!(Tool::Full.action(), None);
    }
}
