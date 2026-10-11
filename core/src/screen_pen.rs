//! 螢幕畫筆：在螢幕上直接畫線（講解時圈重點），錄影時會一起錄進去。
//! 這裡是筆畫與按鍵的邏輯、畫法（可以在任何平台測試）；Windows 的視窗見 screen_pen_win.rs。

use crate::annotate;
use crate::{tr, trf};
use tiny_skia::{Color, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut, Stroke, Transform};

/// 可選的顏色（按 1～4 切換）：紅、黃、綠、藍
/// (中文名稱, RGB, 英文名稱)
pub const COLORS: [(&str, [u8; 3], &str); 4] = [("紅", [239, 68, 68], "Red"), ("黃", [250, 204, 21], "Yellow"), ("綠", [34, 197, 94], "Green"), ("藍", [59, 130, 246], "Blue")];

/// 一筆
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub color: [u8; 3],
    pub width: f32,
    pub pts: Vec<(f32, f32)>,
}

/// 按鍵的結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyResult {
    /// 沒有用到這個鍵
    None,
    /// 畫面要重畫
    Redraw,
    /// 結束畫筆
    Exit,
}

#[derive(Debug, Clone)]
pub struct Board {
    pub lines: Vec<Line>,
    pub color: usize,
    /// 線寬（像素，已依螢幕縮放換算）
    pub width: f32,
    /// 正在畫的那一筆（lines 的最後一筆）
    drawing: bool,
}

impl Board {
    pub fn new(scale: f32) -> Board {
        Board { lines: vec![], color: 0, width: (4.0 * scale).round().max(2.0), drawing: false }
    }

    pub fn begin(&mut self, x: f32, y: f32) {
        self.lines.push(Line { color: COLORS[self.color].1, width: self.width, pts: vec![(x, y)] });
        self.drawing = true;
    }

    /// 畫筆移動：移動超過一點點才加點；回傳要不要重畫
    pub fn move_to(&mut self, x: f32, y: f32) -> bool {
        if !self.drawing {
            return false;
        }
        let Some(l) = self.lines.last_mut() else { return false };
        if l.pts.last().is_some_and(|&(px, py)| (x - px).hypot(y - py) < 1.5) {
            return false;
        }
        l.pts.push((x, y));
        true
    }

    pub fn end(&mut self) {
        self.drawing = false;
    }

    /// 按鍵：1～4 換顏色、Ctrl+Z 復原一筆、Delete / Backspace 全部清掉、Esc 結束
    pub fn key(&mut self, vk: u32, ctrl: bool) -> KeyResult {
        match vk {
            0x1B => KeyResult::Exit,
            0x31..=0x34 => {
                self.color = (vk - 0x31) as usize;
                KeyResult::Redraw
            }
            0x5A if ctrl => {
                self.drawing = false;
                if self.lines.pop().is_some() {
                    KeyResult::Redraw
                } else {
                    KeyResult::None
                }
            }
            0x2E | 0x08 => {
                self.drawing = false;
                self.lines.clear();
                KeyResult::Redraw
            }
            _ => KeyResult::None,
        }
    }

    /// 畫出所有筆畫（off：螢幕左上角在畫布上的位置，畫布座標 = 螢幕座標 − off）
    pub fn render(&self, pm: &mut Pixmap, off: (f32, f32)) {
        self.render_to(&mut pm.as_mut(), off, false);
    }

    /// 畫到任何像素緩衝；bgr = 緩衝是 Windows 點陣圖的 BGRA 順序（直接畫進去，不用再轉一次）
    pub fn render_to(&self, pm: &mut PixmapMut, off: (f32, f32), bgr: bool) {
        let t = Transform::from_translate(-off.0, -off.1);
        for l in &self.lines {
            draw_line(pm, l, t, bgr);
        }
    }

    /// 正在畫的那一筆目前有幾個點
    pub fn drawing_len(&self) -> Option<usize> {
        self.drawing.then(|| self.lines.last().map(|l| l.pts.len())).flatten()
    }

    /// 只畫正在畫的那一筆從第 from 個點之後的新線段（拖曳中每次只畫新的一小段，不重畫整個螢幕）。
    /// 先用直線、不加外框；放開滑鼠後再整個重畫成平滑、有外框的線
    pub fn render_tail(&self, pm: &mut PixmapMut, off: (f32, f32), bgr: bool, from: usize) {
        let Some(l) = self.lines.last() else { return };
        if l.pts.len() < 2 || from + 1 >= l.pts.len() {
            return;
        }
        let mut pb = PathBuilder::new();
        let (x0, y0) = l.pts[from.min(l.pts.len() - 1)];
        pb.move_to(x0, y0);
        for &(x, y) in &l.pts[from + 1..] {
            pb.line_to(x, y);
        }
        let Some(path) = pb.finish() else { return };
        let stroke = Stroke { width: l.width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &paint(color_of(l.color, bgr)), &stroke, Transform::from_translate(-off.0, -off.1), None);
    }
}

fn color_of(c: [u8; 3], bgr: bool) -> Color {
    if bgr {
        Color::from_rgba8(c[2], c[1], c[0], 255)
    } else {
        Color::from_rgba8(c[0], c[1], c[2], 255)
    }
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

/// 一筆：深色的外框讓線在任何背景都看得清楚；用相鄰兩點的中點畫二次曲線，比較平滑
fn draw_line(pm: &mut PixmapMut, l: &Line, t: Transform, bgr: bool) {
    let Some(&(x0, y0)) = l.pts.first() else { return };
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    if l.pts.len() == 1 {
        pb.line_to(x0 + 0.01, y0);
    }
    for i in 1..l.pts.len() {
        let (px, py) = l.pts[i - 1];
        let (qx, qy) = l.pts[i];
        if i == 1 {
            pb.line_to((px + qx) / 2.0, (py + qy) / 2.0);
        } else {
            pb.quad_to(px, py, (px + qx) / 2.0, (py + qy) / 2.0);
        }
    }
    if let Some(&(lx, ly)) = l.pts.last().filter(|_| l.pts.len() > 1) {
        pb.line_to(lx, ly);
    }
    let Some(path) = pb.finish() else { return };
    let stroke = |w: f32| Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    pm.stroke_path(&path, &paint(Color::from_rgba8(0, 0, 0, 90)), &stroke(l.width + 2.0), t, None);
    pm.stroke_path(&path, &paint(color_of(l.color, bgr)), &stroke(l.width), t, None);
}

/// 操作說明（畫在不會被錄進去的小視窗）
pub fn help_text(color: usize) -> String {
    let (zh, _, en) = COLORS[color.min(COLORS.len() - 1)];
    trf!("螢幕畫筆（{}）　拖曳畫線　1～4 換顏色　Ctrl+Z 復原　Delete 清除　Esc 結束", "Screen pen ({})   Drag to draw   1–4: color   Ctrl+Z: undo   Delete: clear   Esc: exit", tr!(zh, en))
}

/// 說明列的圖（不透明的深色圓角底、白字）；回傳預乘 RGBA
pub fn render_help(text: &str, scale: f32) -> Option<Pixmap> {
    let size = 14.0 * scale as f64;
    let mut a = annotate::plain_text(text, size, "#ffffff");
    let (pad_x, pad_y) = (16.0 * scale as f64, 8.0 * scale as f64);
    let (w, h) = ((a.w + pad_x * 2.0).ceil() as u32, (a.h + pad_y * 2.0).ceil() as u32);
    let mut pm = Pixmap::new(w, h)?;
    if let Some(p) = annotate::round_rect(0.0, 0.0, w as f32, h as f32, h as f32 / 2.0) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(24, 24, 27, 235));
        paint.anti_alias = true;
        pm.fill_path(&p, &paint, tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
    (a.x, a.y) = (pad_x, pad_y);
    annotate::draw(&mut pm, &a, Transform::identity());
    Some(pm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_and_keys() {
        let mut b = Board::new(1.5);
        assert_eq!(b.width, 6.0);
        // 沒按下時移動不畫
        assert!(!b.move_to(5.0, 5.0));
        b.begin(10.0, 10.0);
        assert!(!b.move_to(10.5, 10.5), "移動太少不加點");
        assert!(b.move_to(40.0, 10.0));
        b.end();
        assert!(!b.move_to(80.0, 10.0));
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0].pts, vec![(10.0, 10.0), (40.0, 10.0)]);
        assert_eq!(b.lines[0].color, COLORS[0].1);
        // 換成綠色，第二筆用綠色
        assert_eq!(b.key(0x33, false), KeyResult::Redraw);
        b.begin(0.0, 0.0);
        b.end();
        assert_eq!(b.lines[1].color, COLORS[2].1);
        // Z 沒有 Ctrl 不算；Ctrl+Z 復原一筆
        assert_eq!(b.key(0x5A, false), KeyResult::None);
        assert_eq!(b.key(0x5A, true), KeyResult::Redraw);
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.key(0x2E, false), KeyResult::Redraw);
        assert!(b.lines.is_empty());
        assert_eq!(b.key(0x5A, true), KeyResult::None);
        assert_eq!(b.key(0x1B, false), KeyResult::Exit);
        assert!(help_text(2).contains("（綠）"));
    }

    #[test]
    fn renders_lines_offset_to_the_monitor() {
        let mut b = Board::new(1.0);
        // 第二個螢幕在 (1920, 0)：畫布的 (100, 50) 是螢幕的 (2020, 50)
        b.begin(2020.0, 50.0);
        b.move_to(2120.0, 50.0);
        b.end();
        let mut pm = Pixmap::new(300, 100).unwrap();
        b.render(&mut pm, (1920.0, 0.0));
        let c = pm.pixel(150, 50).unwrap();
        assert_eq!((c.red(), c.alpha()), (239, 255));
        assert_eq!(pm.pixel(150, 80).unwrap().alpha(), 0);
        let help = render_help(&help_text(0), 1.0).unwrap();
        assert!(help.width() > 300 && help.height() > 20);
    }

    #[test]
    fn bgr_and_tail_drawing() {
        let mut b = Board::new(1.0);
        b.begin(10.0, 50.0);
        b.move_to(60.0, 50.0);
        assert_eq!(b.drawing_len(), Some(2));
        // 只畫新的一段：BGRA 順序時紅色寫在第三個位元組
        let mut pm = Pixmap::new(200, 100).unwrap();
        b.render_tail(&mut pm.as_mut(), (0.0, 0.0), true, 0);
        let px = pm.pixel(35, 50).unwrap();
        assert_eq!((px.red(), px.blue(), px.alpha()), (68, 239, 255));
        // 繼續畫：只畫第 2 點之後
        b.move_to(110.0, 50.0);
        let mut pm2 = Pixmap::new(200, 100).unwrap();
        b.render_tail(&mut pm2.as_mut(), (0.0, 0.0), false, 1);
        assert_eq!(pm2.pixel(35, 50).unwrap().alpha(), 0, "舊的那段不重畫");
        assert_eq!(pm2.pixel(85, 50).unwrap().red(), 239);
        b.end();
        assert_eq!(b.drawing_len(), None);
    }
}
