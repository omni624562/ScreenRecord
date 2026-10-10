//! 剪輯視窗與截圖編輯的標註：文字（含表情符號）、箭頭、方框、圓框、螢光筆、步驟編號、畫筆、馬賽克、模糊、放大鏡（只限截圖）。
//! 座標與大小一律用原影片的像素，時間用原影片的秒數（剪輯前）。
//!
//! 繪製用 tiny-skia（向量）與系統字型（ttf-parser 取字形，彩色表情支援 COLR 與點陣字形）：
//! 編輯時的預覽與匯出的 PNG 用同一套，看到的就是輸出的樣子。
//! 匯出時，馬賽克 / 模糊交給 FFmpeg（圓角、橢圓附上遮罩）；其他標註畫成透明 PNG，由 FFmpeg 疊上。
//!
//! 欄位名稱與 2.1 版（網頁介面）存的剪輯專案相同，舊的專案可以直接開啟。

use crate::edit::{Overlay, OverlayKind};
use crate::fonts::{invisible, pick, text_fonts, FontFile};
use serde::{Deserialize, Serialize};
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnKind {
    Text,
    Arrow,
    Rect,
    Ellipse,
    Highlight,
    Step,
    Mosaic,
    Blur,
    /// 手繪的線（點存在 pts，0～1 相對於 x, y, w, h）
    Pen,
    /// 放大鏡：把圓裡中心附近的畫面放大顯示（size = 倍率 × 100）；只用在截圖（影片匯出不支援）
    Magnify,
}

impl AnnKind {
    pub const ALL: [AnnKind; 10] = [AnnKind::Text, AnnKind::Arrow, AnnKind::Rect, AnnKind::Ellipse, AnnKind::Highlight, AnnKind::Step, AnnKind::Pen, AnnKind::Mosaic, AnnKind::Blur, AnnKind::Magnify];

    pub fn label(self) -> &'static str {
        match self {
            AnnKind::Text => "文字",
            AnnKind::Arrow => "箭頭",
            AnnKind::Rect => "方框",
            AnnKind::Ellipse => "圓框",
            AnnKind::Highlight => "螢光筆",
            AnnKind::Step => "編號",
            AnnKind::Mosaic => "馬賽克",
            AnnKind::Blur => "模糊",
            AnnKind::Pen => "畫筆",
            AnnKind::Magnify => "放大鏡",
        }
    }

    /// 馬賽克 / 模糊 / 放大鏡：處理畫面本身（影片由 FFmpeg 處理，截圖由 effects.rs），不是畫上去的
    pub fn is_effect(self) -> bool {
        matches!(self, AnnKind::Mosaic | AnnKind::Blur | AnnKind::Magnify)
    }

    /// 只能用在截圖（影片匯出不支援）
    pub fn image_only(self) -> bool {
        self == AnnKind::Magnify
    }

    /// 用拖曳框出範圍的標註
    pub fn is_box(self) -> bool {
        matches!(self, AnnKind::Rect | AnnKind::Ellipse | AnnKind::Highlight | AnnKind::Mosaic | AnnKind::Blur | AnnKind::Magnify)
    }
}

/// 馬賽克 / 模糊的形狀
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Rect,
    Round,
    Ellipse,
}

impl Shape {
    pub const ALL: [Shape; 3] = [Shape::Rect, Shape::Round, Shape::Ellipse];

    pub fn label(self) -> &'static str {
        match self {
            Shape::Rect => "方形",
            Shape::Round => "圓角",
            Shape::Ellipse => "橢圓",
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ann {
    #[serde(default)]
    pub id: u64,
    pub kind: AnnKind,
    /// 左上角；箭頭為起點
    pub x: f64,
    pub y: f64,
    /// 寬高；箭頭為終點減起點（可為負）
    pub w: f64,
    pub h: f64,
    pub start: f64,
    pub end: f64,
    /// #rrggbb
    pub color: String,
    /// 文字：字級；箭頭 / 框線：線寬；編號：圓的直徑
    pub size: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 文字加半透明深色底
    #[serde(default, skip_serializing_if = "is_false")]
    pub bg: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    /// 範圍外模糊（或馬賽克），範圍內清楚
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
    /// 旋轉角度（度，順時針，以中心為軸）；箭頭不用
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rot: f64,
    /// 畫筆的點（0～1，相對於 x, y, w, h；調整大小時跟著縮放）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pts: Vec<[f32; 2]>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl AnnKind {
    /// 可以旋轉（箭頭的兩端本來就能指向任何方向）
    pub fn rotatable(self) -> bool {
        !matches!(self, AnnKind::Arrow | AnnKind::Magnify)
    }
}

/// 實際使用的旋轉角度（度）
pub fn rotation(a: &Ann) -> f64 {
    if a.kind.rotatable() && a.rot.is_finite() {
        a.rot
    } else {
        0.0
    }
}

/// 旋轉的中心（標註的外框中心）
pub fn center(a: &Ann) -> (f64, f64) {
    (a.x + a.w / 2.0, a.y + a.h / 2.0)
}

/// 點 (px, py) 繞 (cx, cy) 轉 deg 度
pub fn rotate_point(px: f64, py: f64, cx: f64, cy: f64, deg: f64) -> (f64, f64) {
    let (s, c) = deg.to_radians().sin_cos();
    let (dx, dy) = (px - cx, py - cy);
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

/// 影片座標 → 標註未旋轉時的座標
pub fn to_local(a: &Ann, px: f64, py: f64) -> (f64, f64) {
    let r = rotation(a);
    if r == 0.0 {
        return (px, py);
    }
    let (cx, cy) = center(a);
    rotate_point(px, py, cx, cy, -r)
}

/// 標註未旋轉時的座標 → 影片座標
pub fn to_world(a: &Ann, lx: f64, ly: f64) -> (f64, f64) {
    let r = rotation(a);
    if r == 0.0 {
        return (lx, ly);
    }
    let (cx, cy) = center(a);
    rotate_point(lx, ly, cx, cy, r)
}

/// 未旋轉的外框四個角轉到影片座標（左上、右上、右下、左下）
pub fn corners(a: &Ann, (x, y, w, h): (f64, f64, f64, f64)) -> [(f64, f64); 4] {
    [to_world(a, x, y), to_world(a, x + w, y), to_world(a, x + w, y + h), to_world(a, x, y + h)]
}

/// 旋轉的把手位置（未旋轉時在外框上方正中間 dist 處，影片像素）
pub fn rotate_handle(a: &Ann, dist: f64) -> (f64, f64) {
    let (bx, by, bw, _) = local_bbox(a);
    to_world(a, bx + bw / 2.0, by - dist)
}

/// 點 (px, py) 是否在旋轉後的形狀內（馬賽克 / 模糊）
pub fn ann_contains(a: &Ann, px: f64, py: f64) -> bool {
    let (lx, ly) = to_local(a, px, py);
    shape_contains(a.shape, a.x, a.y, a.w, a.h, lx, ly)
}

/// 旋轉後的外接框（x, y, w, h）
pub fn rotated_rect(a: &Ann, r: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    if rotation(a) == 0.0 {
        return r;
    }
    let c = corners(a, r);
    let x0 = c.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let y0 = c.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let x1 = c.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y1 = c.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    (x0, y0, x1 - x0, y1 - y0)
}

pub const COLORS: [&str; 7] = ["#e5484d", "#f5b301", "#30a46c", "#0090ff", "#8e4ec6", "#ffffff", "#111111"];
pub const EMOJIS: [&str; 12] = ["👉", "👆", "✅", "❌", "⚠️", "💡", "⭐", "❓", "👍", "😀", "😮", "🔥"];

/// #rrggbb → (r, g, b)
pub fn parse_color(s: &str) -> (u8, u8, u8) {
    let h = s.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0);
    if h.len() >= 6 {
        (p(0), p(2), p(4))
    } else {
        (0, 0, 0)
    }
}

fn color(s: &str, alpha: f32) -> Color {
    let (r, g, b) = parse_color(s);
    Color::from_rgba8(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

/// 依影片高度換算的預設大小（以 1080p 為準）
pub fn default_size(kind: AnnKind, vh: f64) -> f64 {
    let k = vh / 1080.0;
    match kind {
        AnnKind::Text => (48.0 * k).round(),
        AnnKind::Step => (64.0 * k).round(),
        AnnKind::Magnify => 200.0,
        _ => (8.0 * k).round().max(2.0),
    }
}

/// 清單與時間軸上顯示的名稱
pub fn label(a: &Ann) -> String {
    match a.kind {
        AnnKind::Text => {
            let t: String = a.text.as_deref().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
            if t.is_empty() {
                "文字".into()
            } else if t.chars().count() > 12 {
                format!("「{}…」", t.chars().take(12).collect::<String>())
            } else {
                format!("「{t}」")
            }
        }
        AnnKind::Step => format!("編號 {}", a.n.unwrap_or(1)),
        AnnKind::Magnify => format!("放大鏡 {}×", crate::format::num((a.size / 100.0 * 10.0).round() / 10.0)),
        AnnKind::Mosaic | AnnKind::Blur => {
            let mut parts = vec![];
            if let Some(s) = a.shape.filter(|s| *s != Shape::Rect) {
                parts.push(s.label());
            }
            if a.invert {
                parts.push("範圍外");
            }
            if parts.is_empty() {
                a.kind.label().into()
            } else {
                format!("{}（{}）", a.kind.label(), parts.join("・"))
            }
        }
        k => k.label().into(),
    }
}

// ───────────── 文字排版 ─────────────

struct Glyph<'a> {
    font: &'a FontFile,
    id: ttf_parser::GlyphId,
    /// 相對行首的位置（影片像素）
    x: f64,
    emoji: bool,
}

struct Line<'a> {
    glyphs: Vec<Glyph<'a>>,
    width: f64,
}

/// 單行排版（簡單排版：依字寬往右排，不做連字）
fn layout_line(text: &str, size: f64) -> Line<'static> {
    let fonts = text_fonts();
    let chars: Vec<char> = text.chars().collect();
    let mut glyphs = Vec::new();
    let mut x = 0.0;
    for (i, &c) in chars.iter().enumerate() {
        if invisible(c) {
            continue;
        }
        let next = chars.get(i + 1).copied();
        let Some((font, id, emoji)) = pick(fonts, c, next) else {
            x += size * 0.5;
            continue;
        };
        let Some(face) = font.face() else { continue };
        let scale = size / face.units_per_em() as f64;
        let adv = face.glyph_hor_advance(id).unwrap_or(0) as f64 * scale;
        glyphs.push(Glyph { font, id, x, emoji });
        x += adv;
    }
    Line { glyphs, width: x }
}

/// 字型的「上緣」：canvas 的 textBaseline = "top"（em 方塊頂端到基線的距離）
fn em_top(size: f64) -> f64 {
    text_fonts()
        .text
        .first()
        .and_then(|f| f.face())
        .map(|f| {
            let (asc, desc) = (f.ascender() as f64, f.descender() as f64);
            size * asc / (asc - desc).max(1.0)
        })
        .unwrap_or(size * 0.8)
}

fn font_text_lines(a: &Ann) -> Vec<String> {
    a.text.as_deref().unwrap_or(" ").split('\n').map(|l| if l.is_empty() { " ".to_string() } else { l.to_string() }).collect()
}

fn text_pad(a: &Ann) -> f64 {
    if a.bg {
        a.size * 0.35
    } else {
        a.size * 0.1
    }
}

/// 文字與編號的寬高由內容決定
pub fn measure(a: &mut Ann) {
    match a.kind {
        AnnKind::Step => {
            a.w = a.size;
            a.h = a.size;
        }
        AnnKind::Text => {
            let lines = font_text_lines(a);
            let pad = text_pad(a);
            let widest = lines.iter().map(|l| layout_line(l, a.size).width).fold(0.0, f64::max);
            a.w = (widest + pad * 2.0).ceil();
            a.h = (lines.len() as f64 * a.size * 1.25 + pad * 2.0 - a.size * 0.25).ceil();
        }
        _ => {}
    }
}

// ───────────── 幾何 ─────────────

/// 標註實際佔的範圍（含線寬、箭頭頭部；旋轉後的外接框），用於匯出圖片：(x, y, w, h)
pub fn bbox(a: &Ann) -> (f64, f64, f64, f64) {
    rotated_rect(a, local_bbox(a))
}

/// 未旋轉時佔的範圍（含線寬、箭頭頭部）
pub fn local_bbox(a: &Ann) -> (f64, f64, f64, f64) {
    if a.kind == AnnKind::Arrow {
        let pad = a.size * 2.5;
        let x0 = a.x.min(a.x + a.w);
        let y0 = a.y.min(a.y + a.h);
        return (x0 - pad, y0 - pad, a.w.abs() + pad * 2.0, a.h.abs() + pad * 2.0);
    }
    let pad = match a.kind {
        AnnKind::Rect | AnnKind::Ellipse => a.size,
        AnnKind::Pen => a.size / 2.0 + 1.0,
        // 外圈的白邊
        AnnKind::Magnify => magnify_ring(a.w.abs().min(a.h.abs())) + 1.0,
        _ => 0.0,
    };
    (a.x - pad, a.y - pad, a.w + pad * 2.0, a.h + pad * 2.0)
}

/// 點 (x, y)（影片像素）是否點到這個標註
pub fn hit(a: &Ann, x: f64, y: f64, tolerance: f64) -> bool {
    if a.kind == AnnKind::Arrow {
        let (x2, y2) = (a.x + a.w, a.y + a.h);
        let len2 = (a.w * a.w + a.h * a.h).max(1.0);
        let t = (((x - a.x) * a.w + (y - a.y) * a.h) / len2).clamp(0.0, 1.0);
        let (dx, dy) = (x - (a.x + t * a.w), y - (a.y + t * a.h));
        return dx.hypot(dy) <= a.size * 2.0 + tolerance || (x - x2).hypot(y - y2) <= a.size * 3.0 + tolerance;
    }
    let (x, y) = to_local(a, x, y);
    if a.kind == AnnKind::Pen && a.pts.len() > 1 {
        // 點到線附近才算（框裡的空白處可以點到後面的東西）
        let pts = pen_points(a);
        let near = |(x0, y0): (f64, f64), (x1, y1): (f64, f64)| {
            let (dx, dy) = (x1 - x0, y1 - y0);
            let t = (((x - x0) * dx + (y - y0) * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
            (x - (x0 + t * dx)).hypot(y - (y0 + t * dy)) <= a.size / 2.0 + tolerance + 2.0
        };
        return pts.windows(2).any(|p| near(p[0], p[1]));
    }
    let (bx, by, bw, bh) = local_bbox(a);
    x >= bx - tolerance && x <= bx + bw + tolerance && y >= by - tolerance && y <= by + bh + tolerance
}

/// 調整大小的把手位置（影片像素）：箭頭為兩端，其他為右下角（跟著旋轉）
pub fn handles(a: &Ann) -> Vec<(f64, f64)> {
    if a.kind == AnnKind::Arrow {
        vec![(a.x, a.y), (a.x + a.w, a.y + a.h)]
    } else {
        vec![to_world(a, a.x + a.w, a.y + a.h)]
    }
}

/// 圓角的半徑（影片像素）
pub fn round_radius(w: f64, h: f64) -> f64 {
    w.abs().min(h.abs()) * 0.2
}

/// 馬賽克的格子大小 / 模糊的半徑（影片像素），與匯出時 FFmpeg 用的一致（args.rs 的 blur_effect）。框外時以整個畫面為準。
pub fn effect_size(a: &Ann, vw: f64, vh: f64) -> f64 {
    let short = if a.invert { vw.min(vh) } else { a.w.abs().min(a.h.abs()) } as i64;
    if a.kind == AnnKind::Mosaic {
        return (short / if a.invert { 45 } else { 6 }).max(12) as f64;
    }
    if a.invert {
        (short / 40).clamp(4, 30) as f64
    } else {
        (short / 4 - 1).clamp(1, 30) as f64
    }
}

/// 點 (px, py) 是否在形狀內（形狀的外接框為 x, y, w, h）
pub fn shape_contains(shape: Option<Shape>, x: f64, y: f64, w: f64, h: f64, px: f64, py: f64) -> bool {
    if px < x || py < y || px > x + w || py > y + h {
        return false;
    }
    match shape.unwrap_or_default() {
        Shape::Rect => true,
        Shape::Ellipse => {
            let (rx, ry) = (w / 2.0, h / 2.0);
            let (dx, dy) = ((px - x - rx) / rx.max(0.5), (py - y - ry) / ry.max(0.5));
            dx * dx + dy * dy <= 1.0
        }
        Shape::Round => {
            let r = round_radius(w, h);
            let cx = px.clamp(x + r, x + w - r);
            let cy = py.clamp(y + r, y + h - r);
            (px - cx).hypot(py - cy) <= r
        }
    }
}

/// 形狀的路徑（影片像素座標）
pub fn shape_path(shape: Option<Shape>, x: f64, y: f64, w: f64, h: f64) -> Option<Path> {
    let (x, y, w, h) = (x as f32, y as f32, w.max(0.5) as f32, h.max(0.5) as f32);
    match shape.unwrap_or_default() {
        Shape::Rect => Some(PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?)),
        Shape::Ellipse => PathBuilder::from_oval(Rect::from_xywh(x, y, w, h)?),
        Shape::Round => round_rect(x, y, w, h, round_radius(w as f64, h as f64) as f32),
    }
}

/// 圓角矩形（半徑不超過短邊的一半）
pub(crate) fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w.abs() / 2.0).min(h.abs() / 2.0).max(0.0);
    let k = 0.552_284_8 * r;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    p.close();
    p.finish()
}

// ───────────── 繪製 ─────────────

/// ttf-parser 的字形外框 → tiny-skia 路徑
struct Outline {
    pb: PathBuilder,
}

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

fn glyph_path(face: &ttf_parser::Face, id: ttf_parser::GlyphId) -> Option<Path> {
    let mut o = Outline { pb: PathBuilder::new() };
    face.outline_glyph(id, &mut o)?;
    o.pb.finish()
}

/// 彩色字形（COLR）：每一層是「字形外框裁切 + 填色」
struct ColorPainter<'a, 'p> {
    face: &'a ttf_parser::Face<'a>,
    pixmap: &'p mut Pixmap,
    transforms: Vec<Transform>,
    /// outline_glyph 設定、push_clip 使用的外框（已轉成畫面座標）
    path: Option<Path>,
    clips: Vec<Option<Mask>>,
}

impl ColorPainter<'_, '_> {
    fn current(&self) -> Transform {
        *self.transforms.last().unwrap_or(&Transform::identity())
    }

    fn mask_of(&self, path: &Path) -> Option<Mask> {
        let mut m = match self.clips.last() {
            Some(Some(top)) => top.clone(),
            _ => {
                let mut m = Mask::new(self.pixmap.width(), self.pixmap.height())?;
                m.fill_path(path, FillRule::Winding, true, Transform::identity());
                return Some(m);
            }
        };
        m.intersect_path(path, FillRule::Winding, true, Transform::identity());
        Some(m)
    }
}

fn rgba(c: ttf_parser::RgbaColor) -> Color {
    Color::from_rgba8(c.red, c.green, c.blue, c.alpha)
}

fn stops(list: impl Iterator<Item = ttf_parser::colr::ColorStop>) -> Vec<tiny_skia::GradientStop> {
    list.map(|s| tiny_skia::GradientStop::new(s.stop_offset, rgba(s.color))).collect()
}

fn spread(e: ttf_parser::colr::GradientExtend) -> tiny_skia::SpreadMode {
    match e {
        ttf_parser::colr::GradientExtend::Pad => tiny_skia::SpreadMode::Pad,
        ttf_parser::colr::GradientExtend::Repeat => tiny_skia::SpreadMode::Repeat,
        ttf_parser::colr::GradientExtend::Reflect => tiny_skia::SpreadMode::Reflect,
    }
}

impl<'a> ttf_parser::colr::Painter<'a> for ColorPainter<'a, '_> {
    fn outline_glyph(&mut self, glyph_id: ttf_parser::GlyphId) {
        self.path = glyph_path(self.face, glyph_id).and_then(|p| p.transform(self.current()));
    }

    fn paint(&mut self, p: ttf_parser::colr::Paint<'a>) {
        let Some(Some(mask)) = self.clips.last() else { return };
        let t = self.current();
        let shader = match p {
            ttf_parser::colr::Paint::Solid(c) => tiny_skia::Shader::SolidColor(rgba(c)),
            ttf_parser::colr::Paint::LinearGradient(g) => {
                let s = stops(g.stops(0, &[]));
                tiny_skia::LinearGradient::new((g.x0, g.y0).into(), (g.x1, g.y1).into(), s.clone(), spread(g.extend), t)
                    .unwrap_or(tiny_skia::Shader::SolidColor(s.first().map(|_| Color::BLACK).unwrap_or(Color::TRANSPARENT)))
            }
            ttf_parser::colr::Paint::RadialGradient(g) => {
                let s = stops(g.stops(0, &[]));
                tiny_skia::RadialGradient::new((g.x0, g.y0).into(), g.r0.max(0.0), (g.x1, g.y1).into(), g.r1.max(0.01), s, spread(g.extend), t).unwrap_or(tiny_skia::Shader::SolidColor(Color::BLACK))
            }
            ttf_parser::colr::Paint::SweepGradient(g) => {
                // 少見：用第一個顏色近似
                let c = g.stops(0, &[]).next().map(|s| rgba(s.color)).unwrap_or(Color::BLACK);
                tiny_skia::Shader::SolidColor(c)
            }
        };
        let mut pt = Paint { shader, ..Default::default() };
        pt.anti_alias = true;
        let full = Rect::from_xywh(0.0, 0.0, self.pixmap.width() as f32, self.pixmap.height() as f32).unwrap();
        self.pixmap.fill_rect(full, &pt, Transform::identity(), Some(mask));
    }

    fn push_clip(&mut self) {
        let m = self.path.take().and_then(|p| self.mask_of(&p));
        self.clips.push(m);
    }

    fn push_clip_box(&mut self, b: ttf_parser::colr::ClipBox) {
        let m = Rect::from_ltrb(b.x_min, b.y_min, b.x_max, b.y_max).map(PathBuilder::from_rect).and_then(|p| p.transform(self.current())).and_then(|p| self.mask_of(&p));
        self.clips.push(m);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_layer(&mut self, _mode: ttf_parser::colr::CompositeMode) {}

    fn pop_layer(&mut self) {}

    fn push_transform(&mut self, t: ttf_parser::Transform) {
        let next = self.current().pre_concat(Transform::from_row(t.a, t.b, t.c, t.d, t.e, t.f));
        self.transforms.push(next);
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
}

/// 畫一行文字：基線在 (x, baseline)（影片像素），t = 影片像素 → 畫布像素。
/// outline = 外框（顏色, 寬度），畫在填色之前
#[allow(clippy::too_many_arguments)]
fn draw_line(pixmap: &mut Pixmap, line: &Line, x: f64, baseline: f64, size: f64, fill: Color, outline: Option<(Color, f64)>, t: Transform) {
    for g in &line.glyphs {
        let Some(face) = g.font.face() else { continue };
        let scale = (size / face.units_per_em() as f64) as f32;
        let gx = (x + g.x) as f32;
        let place = t.pre_translate(gx, baseline as f32).pre_scale(scale, -scale);
        if g.emoji {
            if face.is_color_glyph(g.id) {
                let mut painter = ColorPainter { face: &face, pixmap, transforms: vec![place], path: None, clips: vec![] };
                if face.paint_color_glyph(g.id, 0, ttf_parser::RgbaColor::new(0, 0, 0, 255), &mut painter).is_some() {
                    continue;
                }
            }
            // 點陣表情（CBDT / sbix）
            let ppem = (size * t.sy as f64).round().clamp(8.0, 256.0) as u16;
            if let Some(img) = face.glyph_raster_image(g.id, ppem) {
                if let Ok(bmp) = Pixmap::decode_png(img.data) {
                    let k = (size / img.pixels_per_em as f64) as f32;
                    let top = baseline as f32 - (img.y as f32 + img.height as f32) * k;
                    let pt = t.pre_translate(gx + img.x as f32 * k, top).pre_scale(k, k);
                    let pp = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
                    pixmap.draw_pixmap(0, 0, bmp.as_ref(), &pp, pt, None);
                    continue;
                }
            }
        }
        let Some(path) = glyph_path(&face, g.id) else { continue };
        if let Some((oc, ow)) = outline {
            let stroke = Stroke { width: (ow / scale as f64) as f32, line_join: LineJoin::Round, line_cap: LineCap::Round, ..Default::default() };
            pixmap.stroke_path(&path, &paint(oc), &stroke, place, None);
        }
        pixmap.fill_path(&path, &paint(fill), FillRule::Winding, place, None);
    }
}

fn stroke(width: f64) -> Stroke {
    Stroke { width: width as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

/// 畫一個標註（t = 影片像素 → 畫布像素）；馬賽克 / 模糊不在這裡畫
pub fn draw(pixmap: &mut Pixmap, a: &Ann, t: Transform) {
    let r = rotation(a);
    let t = if r != 0.0 {
        let (cx, cy) = center(a);
        t.pre_concat(Transform::from_rotate_at(r as f32, cx as f32, cy as f32))
    } else {
        t
    };
    let (x, y, w, h, size) = (a.x as f32, a.y as f32, a.w as f32, a.h as f32, a.size as f32);
    match a.kind {
        AnnKind::Text => {
            let lines = font_text_lines(a);
            let pad = text_pad(a);
            if a.bg {
                if let Some(p) = round_rect(x, y, w, h, size * 0.3) {
                    pixmap.fill_path(&p, &paint(Color::from_rgba8(0, 0, 0, 158)), FillRule::Winding, t, None);
                }
            }
            let fill = color(&a.color, 1.0);
            // 沒有底色時加上外框，在任何背景上都看得清楚
            let outline = (!a.bg).then(|| (if a.color == "#111111" { Color::from_rgba8(255, 255, 255, 230) } else { Color::from_rgba8(0, 0, 0, 191) }, (a.size / 8.0).max(2.0)));
            let top = em_top(a.size);
            for (i, l) in lines.iter().enumerate() {
                let line = layout_line(l, a.size);
                let line_top = a.y + pad + i as f64 * a.size * 1.25;
                draw_line(pixmap, &line, a.x + pad, line_top + top, a.size, fill, outline, t);
            }
        }
        AnnKind::Arrow => {
            let (x2, y2) = (x + w, y + h);
            let ang = h.atan2(w);
            let head = size * 3.2;
            let mut pb = PathBuilder::new();
            pb.move_to(x, y);
            pb.line_to(x2 - ang.cos() * head * 0.6, y2 - ang.sin() * head * 0.6);
            if let Some(shaft) = pb.finish() {
                pixmap.stroke_path(&shaft, &paint(Color::from_rgba8(0, 0, 0, 89)), &stroke(a.size + 3.0), t, None);
                pixmap.stroke_path(&shaft, &paint(color(&a.color, 1.0)), &stroke(a.size), t, None);
            }
            let mut pb = PathBuilder::new();
            pb.move_to(x2, y2);
            pb.line_to(x2 - (ang - 0.45).cos() * head, y2 - (ang - 0.45).sin() * head);
            pb.line_to(x2 - (ang + 0.45).cos() * head, y2 - (ang + 0.45).sin() * head);
            pb.close();
            if let Some(tip) = pb.finish() {
                pixmap.fill_path(&tip, &paint(color(&a.color, 1.0)), FillRule::Winding, t, None);
            }
        }
        AnnKind::Rect | AnnKind::Ellipse => {
            let path = if a.kind == AnnKind::Rect {
                round_rect(x.min(x + w), y.min(y + h), w.abs(), h.abs(), size)
            } else {
                Rect::from_xywh(x.min(x + w), y.min(y + h), w.abs().max(0.5), h.abs().max(0.5)).and_then(PathBuilder::from_oval)
            };
            if let Some(p) = path {
                pixmap.stroke_path(&p, &paint(color(&a.color, 1.0)), &stroke(a.size), t, None);
            }
        }
        AnnKind::Highlight => {
            if let Some(r) = Rect::from_xywh(x.min(x + w), y.min(y + h), w.abs().max(0.5), h.abs().max(0.5)) {
                pixmap.fill_rect(r, &paint(color(&a.color, 0.35)), t, None);
            }
        }
        AnnKind::Step => {
            let r = size / 2.0;
            if let Some(c) = PathBuilder::from_circle(x + r, y + r, r) {
                pixmap.fill_path(&c, &paint(color(&a.color, 1.0)), FillRule::Winding, t, None);
                pixmap.stroke_path(&c, &paint(Color::WHITE), &stroke((a.size / 16.0).max(2.0)), t, None);
            }
            let ink = if a.color == "#ffffff" || a.color == "#f5b301" { Color::from_rgba8(17, 17, 17, 255) } else { Color::WHITE };
            let fs = (a.size * 0.56).round();
            let line = layout_line(&a.n.unwrap_or(1).to_string(), fs);
            // 數字置中：以 em 方塊的中線對齊圓心
            let baseline = a.y + a.size / 2.0 + a.size * 0.03 - fs / 2.0 + em_top(fs);
            draw_line(pixmap, &line, a.x + a.size / 2.0 - line.width / 2.0, baseline, fs, ink, None, t);
        }
        AnnKind::Pen => {
            let pts = pen_points(a);
            let Some(&(x0, y0)) = pts.first() else { return };
            let mut pb = PathBuilder::new();
            pb.move_to(x0 as f32, y0 as f32);
            if pts.len() == 1 {
                pb.line_to(x0 as f32 + 0.01, y0 as f32);
            }
            // 用相鄰兩點的中點畫二次曲線，手繪的線比較平滑
            for i in 1..pts.len() {
                let (px, py) = pts[i - 1];
                let (qx, qy) = pts[i];
                if i == 1 {
                    pb.line_to(((px + qx) / 2.0) as f32, ((py + qy) / 2.0) as f32);
                } else {
                    pb.quad_to(px as f32, py as f32, ((px + qx) / 2.0) as f32, ((py + qy) / 2.0) as f32);
                }
            }
            if let Some(&(lx, ly)) = pts.last() {
                pb.line_to(lx as f32, ly as f32);
            }
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint(Color::from_rgba8(0, 0, 0, 70)), &stroke(a.size + 2.0), t, None);
                pixmap.stroke_path(&path, &paint(color(&a.color, 1.0)), &stroke(a.size), t, None);
            }
        }
        AnnKind::Mosaic | AnnKind::Blur | AnnKind::Magnify => {}
    }
}

/// 畫筆的點（影片像素，未旋轉）
pub fn pen_points(a: &Ann) -> Vec<(f64, f64)> {
    a.pts.iter().map(|p| (a.x + p[0] as f64 * a.w, a.y + p[1] as f64 * a.h)).collect()
}

/// 畫筆：把畫好的點（影片像素）轉成外框與 0～1 的相對位置
pub fn set_pen_points(a: &mut Ann, pts: &[(f64, f64)]) {
    if pts.is_empty() {
        return;
    }
    let x0 = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let y0 = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let x1 = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y1 = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    // 直線（寬或高為 0）也能縮放：至少 1 像素
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    a.x = x0;
    a.y = y0;
    a.w = w;
    a.h = h;
    a.pts = pts.iter().map(|p| [((p.0 - x0) / w) as f32, ((p.1 - y0) / h) as f32]).collect();
}

/// 放大鏡外圈白邊的寬度（影片像素）
pub fn magnify_ring(d: f64) -> f64 {
    (d * 0.025).clamp(2.0, 8.0)
}

/// 以不透明度畫一個標註（預覽中選取、但目前時間看不到的標註畫淡一點）
pub fn draw_with_opacity(pixmap: &mut Pixmap, a: &Ann, t: Transform, opacity: f32) {
    if opacity >= 0.999 {
        return draw(pixmap, a, t);
    }
    let Some(mut layer) = Pixmap::new(pixmap.width(), pixmap.height()) else { return };
    draw(&mut layer, a, t);
    let pp = PixmapPaint { opacity, ..Default::default() };
    pixmap.draw_pixmap(0, 0, layer.as_ref(), &pp, Transform::identity(), None);
}

// ───────────── 匯出 ─────────────

/// 匯出：馬賽克 / 模糊交給 FFmpeg（圓角、橢圓附上遮罩）；其他畫成剛好包住標註的透明 PNG
pub fn to_overlay(a: &Ann, vw: f64, vh: f64) -> Option<Overlay> {
    if a.kind.image_only() {
        return None;
    }
    if a.kind.is_effect() {
        let kind = if a.kind == AnnKind::Mosaic { OverlayKind::Mosaic } else { OverlayKind::Blur };
        let r = rotation(a);
        if r == 0.0 {
            let (x, y, w, h) = (a.x.round(), a.y.round(), a.w.round(), a.h.round());
            let mut o = Overlay { kind, x, y, w, h, start: a.start, end: a.end, png: None, mask: None, invert: a.invert };
            if a.shape.is_some_and(|s| s != Shape::Rect) && w >= 2.0 && h >= 2.0 {
                let mut m = Pixmap::new(w as u32, h as u32)?;
                m.fill(Color::BLACK);
                if let Some(p) = shape_path(a.shape, 0.0, 0.0, w, h) {
                    m.fill_path(&p, &paint(Color::WHITE), FillRule::Winding, Transform::identity(), None);
                }
                o.mask = m.encode_png().ok();
            }
            return Some(o);
        }
        // 旋轉：範圍取旋轉後的外接框（限制在畫面內），遮罩畫成旋轉後的形狀
        let (bx, by, bw, bh) = rotated_rect(a, (a.x, a.y, a.w, a.h));
        let x0 = bx.floor().max(0.0);
        let y0 = by.floor().max(0.0);
        let x1 = (bx + bw).ceil().min(vw);
        let y1 = (by + bh).ceil().min(vh);
        if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
            return None;
        }
        let mut m = Pixmap::new((x1 - x0) as u32, (y1 - y0) as u32)?;
        m.fill(Color::BLACK);
        let (cx, cy) = center(a);
        let t = Transform::from_translate(-x0 as f32, -y0 as f32).pre_concat(Transform::from_rotate_at(r as f32, cx as f32, cy as f32));
        if let Some(p) = shape_path(a.shape, a.x, a.y, a.w, a.h) {
            m.fill_path(&p, &paint(Color::WHITE), FillRule::Winding, t, None);
        }
        return Some(Overlay { kind, x: x0, y: y0, w: x1 - x0, h: y1 - y0, start: a.start, end: a.end, png: None, mask: m.encode_png().ok(), invert: a.invert });
    }
    let (bx, by, bw, bh) = bbox(a);
    // 只保留畫面內的部分
    let x0 = bx.floor().max(0.0);
    let y0 = by.floor().max(0.0);
    let x1 = (bx + bw).ceil().min(vw);
    let y1 = (by + bh).ceil().min(vh);
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
        return None;
    }
    let mut pm = Pixmap::new((x1 - x0) as u32, (y1 - y0) as u32)?;
    draw(&mut pm, a, Transform::from_translate(-x0 as f32, -y0 as f32));
    Some(Overlay { kind: OverlayKind::Image, x: x0, y: y0, w: x1 - x0, h: y1 - y0, start: a.start, end: a.end, png: pm.encode_png().ok(), mask: None, invert: false })
}

/// 存起來的剪輯設定與標註（之後可以再修改）；與 2.1 版的格式相同
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectData {
    pub v: u32,
    pub duration: f64,
    pub spec: ProjectSpec,
    #[serde(default)]
    pub crop_on: bool,
    #[serde(default)]
    pub anns: Vec<Ann>,
    /// 聲音處理（降噪、音量平衡、靜音）
    #[serde(default, skip_serializing_if = "crate::edit::AudioFx::is_default")]
    pub audio: crate::edit::AudioFx,
    /// 局部加速的片段
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fast: Vec<crate::edit::FastRange>,
    /// 跟著點擊放大的倍率（0 = 不放大）
    #[serde(default, skip_serializing_if = "is_zero")]
    pub zoom: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSpec {
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub removed: Vec<(f64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<crate::edit::CropInput>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(kind: AnnKind) -> Ann {
        Ann {
            id: 1,
            kind,
            x: 100.0,
            y: 80.0,
            w: 200.0,
            h: 120.0,
            start: 0.0,
            end: 3.0,
            color: "#e5484d".into(),
            size: 8.0,
            text: None,
            bg: false,
            n: None,
            shape: None,
            invert: false,
            rot: 0.0,
            pts: vec![],
        }
    }

    #[test]
    fn rotation_geometry() {
        // 200×120 的方框轉 90 度：外接框變成 120×200，中心不變
        let a = Ann { rot: 90.0, ..ann(AnnKind::Rect) };
        let (cx, cy) = center(&a);
        let (x, y, w, h) = rotated_rect(&a, (a.x, a.y, a.w, a.h));
        assert!((w - 120.0).abs() < 1e-6 && (h - 200.0).abs() < 1e-6, "{w}×{h}");
        assert!((x + w / 2.0 - cx).abs() < 1e-6 && (y + h / 2.0 - cy).abs() < 1e-6);
        // 原本在框外（右邊）的點，轉 90 度後在框內（上下方向）
        assert!(hit(&a, cx, cy - 90.0, 0.0), "轉 90 度後上下延伸 100");
        assert!(!hit(&a, cx + 90.0, cy, 0.0), "轉 90 度後左右只延伸 60＋線寬");
        // 世界 ↔ 本地座標互轉
        let (lx, ly) = to_local(&a, 10.0, 20.0);
        let (wx, wy) = to_world(&a, lx, ly);
        assert!((wx - 10.0).abs() < 1e-9 && (wy - 20.0).abs() < 1e-9);
        // 右下角的把手跟著轉
        let (hx, hy) = handles(&a)[0];
        assert!((hx - (cx - 60.0)).abs() < 1e-6 && (hy - (cy + 100.0)).abs() < 1e-6, "{hx},{hy}");
        // 箭頭不旋轉
        assert_eq!(rotation(&Ann { rot: 45.0, ..ann(AnnKind::Arrow) }), 0.0);
    }

    #[test]
    fn rotated_effect_exports_with_mask() {
        let a = Ann { rot: 45.0, ..ann(AnnKind::Blur) };
        let o = to_overlay(&a, 1920.0, 1080.0).unwrap();
        let (_, _, w, h) = rotated_rect(&a, (a.x, a.y, a.w, a.h));
        assert!((o.w - w.ceil()).abs() <= 2.0 && (o.h - h.ceil()).abs() <= 2.0, "{}×{}", o.w, o.h);
        let m = Pixmap::decode_png(o.mask.as_ref().expect("旋轉時要有遮罩")).unwrap();
        // 外接框的角落在形狀外（黑），中心在形狀內（白）
        let px = |x: u32, y: u32| m.pixel(x, y).unwrap().red();
        assert_eq!(px(0, 0), 0);
        assert_eq!(px(m.width() / 2, m.height() / 2), 255);
        // 沒有旋轉：與之前相同（方形不需要遮罩）
        let o = to_overlay(&ann(AnnKind::Blur), 1920.0, 1080.0).unwrap();
        assert!(o.mask.is_none() && o.w == 200.0);
    }

    #[test]
    fn rotation_is_optional_in_saved_projects() {
        let a = ann(AnnKind::Text);
        assert!(!serde_json::to_string(&a).unwrap().contains("rot"));
        let b = Ann { rot: 30.0, ..a };
        let back: Ann = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(back.rot, 30.0);
    }

    #[test]
    fn reads_projects_saved_by_version_2_1() {
        // 2.1 版（網頁介面）存的格式
        let json = r##"{"v":1,"duration":30,"spec":{"start":1,"end":20,"removed":[[5,6]],"crop":{"x":0,"y":0,"width":640,"height":360}},"cropOn":true,
            "anns":[{"id":3,"kind":"text","x":10,"y":20,"w":100,"h":40,"start":0,"end":3,"color":"#ffffff","size":48,"text":"按這裡","bg":true},
                    {"id":4,"kind":"blur","x":1,"y":2,"w":30,"h":40,"start":1,"end":2,"color":"#e5484d","size":8,"shape":"ellipse","invert":true},
                    {"id":5,"kind":"step","x":1,"y":2,"w":64,"h":64,"start":1,"end":2,"color":"#e5484d","size":64,"n":2}]}"##;
        let p: ProjectData = serde_json::from_str(json).unwrap();
        assert_eq!(p.spec.removed, vec![(5.0, 6.0)]);
        assert!(p.crop_on);
        assert_eq!(p.anns[0].text.as_deref(), Some("按這裡"));
        assert!(p.anns[0].bg);
        assert_eq!(p.anns[1].shape, Some(Shape::Ellipse));
        assert!(p.anns[1].invert);
        assert_eq!(p.anns[2].n, Some(2));
        // 存回去再讀，內容不變
        let again: ProjectData = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(again, p);
    }

    #[test]
    fn labels_and_geometry() {
        let mut t = ann(AnnKind::Text);
        t.text = Some("  很長很長很長很長很長很長的說明  ".into());
        assert_eq!(label(&t), "「很長很長很長很長很長很長…」");
        let mut b = ann(AnnKind::Blur);
        b.shape = Some(Shape::Ellipse);
        b.invert = true;
        assert_eq!(label(&b), "模糊（橢圓・範圍外）");
        assert_eq!(label(&ann(AnnKind::Mosaic)), "馬賽克");
        // 方框的範圍含線寬
        assert_eq!(bbox(&ann(AnnKind::Rect)), (92.0, 72.0, 216.0, 136.0));
        // 箭頭：點在線上或終點附近
        let mut a = ann(AnnKind::Arrow);
        a.w = 100.0;
        a.h = 0.0;
        assert!(hit(&a, 150.0, 82.0, 0.0));
        assert!(!hit(&a, 150.0, 120.0, 0.0));
        // 與 FFmpeg 相同的強度
        assert_eq!(effect_size(&ann(AnnKind::Blur), 1920.0, 1080.0), 29.0);
        assert_eq!(effect_size(&ann(AnnKind::Mosaic), 1920.0, 1080.0), 20.0);
        let mut inv = ann(AnnKind::Blur);
        inv.invert = true;
        assert_eq!(effect_size(&inv, 1920.0, 1080.0), 27.0);
        // 形狀
        assert!(shape_contains(Some(Shape::Ellipse), 0.0, 0.0, 100.0, 50.0, 50.0, 25.0));
        assert!(!shape_contains(Some(Shape::Ellipse), 0.0, 0.0, 100.0, 50.0, 2.0, 2.0));
        assert!(!shape_contains(Some(Shape::Round), 0.0, 0.0, 100.0, 50.0, 0.5, 0.5));
        assert!(shape_contains(Some(Shape::Round), 0.0, 0.0, 100.0, 50.0, 50.0, 1.0));
        assert!(shape_contains(None, 0.0, 0.0, 100.0, 50.0, 0.5, 0.5));
    }

    fn alpha_at(png: &[u8], x: u32, y: u32) -> u8 {
        let p = Pixmap::decode_png(png).unwrap();
        p.pixel(x, y).unwrap().alpha()
    }

    #[test]
    fn overlays_are_png_cropped_to_the_annotation() {
        let r = ann(AnnKind::Rect);
        let o = to_overlay(&r, 1920.0, 1080.0).unwrap();
        assert_eq!((o.kind, o.x, o.y, o.w, o.h), (OverlayKind::Image, 92.0, 72.0, 216.0, 136.0));
        let png = o.png.unwrap();
        // 框線上不透明、框內透明
        assert!(alpha_at(&png, 8, 60) > 200);
        assert_eq!(alpha_at(&png, 108, 68), 0);
        // 完全在畫面外：不輸出
        let mut out = ann(AnnKind::Rect);
        out.x = 5000.0;
        assert!(to_overlay(&out, 1920.0, 1080.0).is_none());
        // 圓角 / 橢圓的馬賽克附上遮罩
        let mut m = ann(AnnKind::Mosaic);
        assert!(to_overlay(&m, 1920.0, 1080.0).unwrap().mask.is_none());
        m.shape = Some(Shape::Ellipse);
        let mask = to_overlay(&m, 1920.0, 1080.0).unwrap().mask.unwrap();
        let mp = Pixmap::decode_png(&mask).unwrap();
        assert_eq!((mp.width(), mp.height()), (200, 120));
        assert_eq!(mp.pixel(100, 60).unwrap().red(), 255);
        assert_eq!(mp.pixel(1, 1).unwrap().red(), 0);
    }

    #[test]
    fn text_is_drawn_with_system_fonts() {
        if text_fonts().text.is_empty() {
            return; // 沒有中文字型的環境
        }
        let mut t = ann(AnnKind::Text);
        t.text = Some("按這裡\n第二行".into());
        t.size = 48.0;
        t.bg = true;
        t.color = "#ffffff".into();
        measure(&mut t);
        assert!(t.w > 48.0 * 3.0 && t.w < 48.0 * 4.5, "{}", t.w);
        assert!((t.h - (2.0_f64 * 48.0 * 1.25 + 2.0 * 48.0 * 0.35 - 12.0).ceil()).abs() < 1.0);
        let o = to_overlay(&t, 1920.0, 1080.0).unwrap();
        let p = Pixmap::decode_png(&o.png.unwrap()).unwrap();
        // 有白色的字（不只是半透明的底）
        let white = p.pixels().iter().filter(|c| c.red() > 240 && c.green() > 240 && c.alpha() > 240).count();
        assert!(white > 200, "{white}");
        // 編號
        let mut s = ann(AnnKind::Step);
        s.size = 64.0;
        s.n = Some(3);
        measure(&mut s);
        assert_eq!((s.w, s.h), (64.0, 64.0));
        let o = to_overlay(&s, 1920.0, 1080.0).unwrap();
        let p = Pixmap::decode_png(&o.png.unwrap()).unwrap();
        assert!(p.pixel(32, 32).unwrap().alpha() == 255);
    }

    #[test]
    fn emoji_are_drawn_in_color() {
        if text_fonts().emoji.is_empty() {
            return;
        }
        let mut t = ann(AnnKind::Text);
        t.text = Some("🔥".into());
        t.size = 80.0;
        measure(&mut t);
        let o = to_overlay(&t, 1920.0, 1080.0).unwrap();
        let p = Pixmap::decode_png(&o.png.unwrap()).unwrap();
        // 火焰是橘黃色：有彩色（非灰階）的像素
        let colorful = p.pixels().iter().filter(|c| c.alpha() > 200 && (c.red() as i32 - c.blue() as i32) > 80).count();
        assert!(colorful > 100, "{colorful}");
    }
}
