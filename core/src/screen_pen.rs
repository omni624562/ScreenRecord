//! 螢幕畫筆：在螢幕上直接畫線（講解時圈重點），錄影時會一起錄進去。
//! 這裡是筆畫與按鍵的邏輯、畫法（可以在任何平台測試）；Windows 的視窗見 screen_pen_win.rs。

use crate::annotate;
use crate::tr;
use tiny_skia::{Color, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut, Stroke, Transform};

/// 可選的顏色（按 1～4 切換）：紅、黃、綠、藍
/// (中文名稱, RGB, 英文名稱)
pub const COLORS: [(&str, [u8; 3], &str); 4] = [("紅", [239, 68, 68], "Red"), ("黃", [250, 204, 21], "Yellow"), ("綠", [34, 197, 94], "Green"), ("藍", [59, 130, 246], "Blue")];

/// 線的粗細（100% 縮放時的像素）：(中文, 像素, 英文)
pub const WIDTHS: [(&str, f32, &str); 3] = [("細", 2.5, "Thin"), ("中", 4.0, "Medium"), ("粗", 8.0, "Thick")];

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
    /// 線寬（像素，已依螢幕縮放換算）與選的粗細（WIDTHS 的位置）
    pub width: f32,
    pub thick: usize,
    scale: f32,
    /// 正在畫的那一筆（lines 的最後一筆）
    drawing: bool,
}

impl Board {
    pub fn new(scale: f32) -> Board {
        let mut b = Board { lines: vec![], color: 0, width: 0.0, thick: 1, scale, drawing: false };
        b.set_thick(1);
        b
    }

    /// 換粗細（之後畫的線）
    pub fn set_thick(&mut self, i: usize) {
        self.thick = i.min(WIDTHS.len() - 1);
        self.width = (WIDTHS[self.thick].1 * self.scale).round().max(2.0);
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

/// 說明列上點得到的地方：顏色、粗細
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpHit {
    Color(usize),
    Thick(usize),
}

/// 說明列的大小與點得到的地方 (哪個, (x, y, 寬, 高))
#[derive(Debug, Clone, PartialEq)]
pub struct HelpLayout {
    pub w: u32,
    pub h: u32,
    pub hits: Vec<(HelpHit, (f32, f32, f32, f32))>,
}

/// 說明列上 (x, y) 點到哪裡
pub fn help_hit(l: &HelpLayout, x: f32, y: f32) -> Option<HelpHit> {
    l.hits.iter().find(|(_, (hx, hy, hw, hh))| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh).map(|(h, _)| *h)
}

/// 操作說明（畫在不會被錄進去的小視窗）
pub fn help_text() -> &'static str {
    tr!("拖曳畫線・1～4 換顏色・Ctrl+Z 復原・Delete 清除・Esc 結束", "Drag to draw · 1–4: color · Ctrl+Z: undo · Delete: clear · Esc: exit")
}

fn fill_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Color) {
    if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
        let mut paint = paint(c);
        paint.anti_alias = true;
        pm.fill_path(&p, &paint, tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
}

fn fill_round(pm: &mut Pixmap, (x, y, w, h): (f32, f32, f32, f32), r: f32, c: Color) {
    if let Some(p) = annotate::round_rect(x, y, w, h, r) {
        pm.fill_path(&p, &paint(c), tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
}

/// 說明列（不透明的深色圓角底、白字）：「螢幕畫筆」、四個顏色（按一下換）、粗細（細、中、粗）、操作說明；回傳預乘 RGBA 與點得到的地方
pub fn render_help(color: usize, thick: usize, scale: f32) -> Option<(Pixmap, HelpLayout)> {
    let s = scale.max(1.0);
    let size = 14.0 * s as f64;
    let title = annotate::plain_text(tr!("螢幕畫筆", "Screen pen"), size, "#ffffff");
    let label = annotate::plain_text(tr!("粗細", "Width"), 12.0 * s as f64, "#c8c8cc");
    let help = annotate::plain_text(help_text(), 12.5 * s as f64, "#c8c8cc");
    let (pad_x, pad_y, gap) = (16.0 * s, 8.0 * s, 14.0 * s);
    let dot = 22.0 * s;
    let seg = 30.0 * s;
    let h = ((title.h.max(help.h) as f32).max(dot) + pad_y * 2.0).ceil();
    let w = (pad_x * 2.0 + title.w as f32 + gap + dot * COLORS.len() as f32 + gap + label.w as f32 + 6.0 * s + seg * WIDTHS.len() as f32 + gap + help.w as f32).ceil();
    let mut pm = Pixmap::new(w as u32, h as u32)?;
    fill_round(&mut pm, (0.0, 0.0, w, h), h / 2.0, Color::from_rgba8(24, 24, 27, 235));
    let cy = h / 2.0;
    let mut x = pad_x;
    let mut hits = vec![];
    let draw_text = |pm: &mut Pixmap, mut a: crate::annotate::Ann, x: f32| {
        (a.x, a.y) = (x as f64, (cy as f64 - a.h / 2.0).round());
        annotate::draw(pm, &a, Transform::identity());
    };
    let tw = title.w as f32;
    draw_text(&mut pm, title, x);
    x += tw + gap;
    // 顏色：選的那個有白色外圈
    for (i, (_, rgb, _)) in COLORS.iter().enumerate() {
        let c = x + dot / 2.0;
        if i == color {
            fill_circle(&mut pm, c, cy, dot / 2.0, Color::WHITE);
        }
        fill_circle(&mut pm, c, cy, dot / 2.0 - 3.0 * s, Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255));
        hits.push((HelpHit::Color(i), (x, 0.0, dot, h)));
        x += dot;
    }
    x += gap;
    let lw = label.w as f32;
    draw_text(&mut pm, label, x);
    x += lw + 6.0 * s;
    // 粗細：一段示範的線，選的那個有底色
    for (i, (_, px, _)) in WIDTHS.iter().enumerate() {
        let r = (x + 1.0 * s, cy - 11.0 * s, seg - 2.0 * s, 22.0 * s);
        if i == thick {
            fill_round(&mut pm, r, 6.0 * s, Color::from_rgba8(72, 72, 80, 255));
        }
        let lh = (px * s).max(1.5);
        fill_round(&mut pm, (x + 7.0 * s, cy - lh / 2.0, seg - 14.0 * s, lh), lh / 2.0, Color::WHITE);
        hits.push((HelpHit::Thick(i), (x, 0.0, seg, h)));
        x += seg;
    }
    x += gap;
    draw_text(&mut pm, help, x);
    Some((pm, HelpLayout { w: w as u32, h: h as u32, hits }))
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
        // 換粗細
        b.set_thick(2);
        assert_eq!((b.thick, b.width), (2, 12.0));
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
        let (help, l) = render_help(0, 1, 1.0).unwrap();
        assert!(help.width() > 300 && help.height() > 20);
        // 點顏色、粗細
        let (x, y, w, h) = l.hits[2].1;
        assert_eq!(help_hit(&l, x + w / 2.0, y + h / 2.0), Some(HelpHit::Color(2)));
        let (x, y, w, h) = l.hits[4 + 2].1;
        assert_eq!(help_hit(&l, x + w / 2.0, y + h / 2.0), Some(HelpHit::Thick(2)));
        assert_eq!(help_hit(&l, 2.0, 2.0), None);
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
