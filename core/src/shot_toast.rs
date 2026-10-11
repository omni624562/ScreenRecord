//! 截圖後的小縮圖（Windows 視窗在 shot_toast_win）：右下角出現截到的圖，下面有「編輯、複製、釘選、刪除」，
//! 幾秒後自動消失（滑鼠移上去時不會）。這裡只負責畫與判斷點到哪裡，各平台共用。

use crate::annotate::{self, Ann, AnnKind};
use crate::tr;
use tiny_skia::{Color, FillRule, FilterQuality, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

/// 下面的按鈕
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Edit,
    Copy,
    Pin,
    Delete,
    Close,
}

/// (按鈕, 中文, 英文)
pub const BUTTONS: [(Button, &str, &str); 4] = [(Button::Edit, "編輯", "Edit"), (Button::Copy, "複製", "Copy"), (Button::Pin, "釘選", "Pin"), (Button::Delete, "刪除", "Delete")];

/// 點到哪裡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Image,
    Button(Button),
}

/// 版面（像素，scale = DPI / 96）
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub w: u32,
    pub h: u32,
    pub image: (f32, f32, f32, f32),
    pub close: (f32, f32, f32),
    pub buttons: Vec<(Button, (f32, f32, f32, f32))>,
}

pub fn layout(scale: f32) -> Layout {
    let s = scale.max(1.0);
    let (w, img_h, bar_h, pad) = (280.0 * s, 158.0 * s, 36.0 * s, 6.0 * s);
    let h = pad + img_h + bar_h;
    let bw = (w - pad * 2.0) / BUTTONS.len() as f32;
    let buttons = BUTTONS.iter().enumerate().map(|(i, (b, _, _))| (*b, (pad + i as f32 * bw, pad + img_h + 2.0 * s, bw, bar_h - 6.0 * s))).collect();
    let r = 11.0 * s;
    Layout { w: w.round() as u32, h: h.round() as u32, image: (pad, pad, w - pad * 2.0, img_h), close: (w - pad - r - 4.0 * s, pad + r + 4.0 * s, r), buttons }
}

/// 點 (x, y) 是哪裡
pub fn hit(l: &Layout, x: f32, y: f32) -> Option<Hit> {
    let (cx, cy, r) = l.close;
    if (x - cx).hypot(y - cy) <= r + 2.0 {
        return Some(Hit::Button(Button::Close));
    }
    for (b, (bx, by, bw, bh)) in &l.buttons {
        if x >= *bx && x < bx + bw && y >= *by && y < by + bh {
            return Some(Hit::Button(*b));
        }
    }
    let (ix, iy, iw, ih) = l.image;
    (x >= ix && x < ix + iw && y >= iy && y < iy + ih).then_some(Hit::Image)
}

fn fill_round(pm: &mut Pixmap, (x, y, w, h): (f32, f32, f32, f32), r: f32, c: Color) {
    if let Some(p) = annotate::round_rect(x, y, w, h, r) {
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(c);
        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn text(pm: &mut Pixmap, s: &str, cx: f32, cy: f32, size: f32) {
    let mut a = Ann {
        id: 0,
        kind: AnnKind::Text,
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
        start: 0.0,
        end: 1.0,
        color: "#ffffff".into(),
        size: size as f64,
        text: Some(s.to_string()),
        bg: false,
        n: None,
        shape: None,
        invert: false,
        rot: 0.0,
        pts: vec![],
    };
    annotate::measure(&mut a);
    a.x = cx as f64 - a.w / 2.0;
    a.y = cy as f64 - a.h / 2.0;
    annotate::draw(pm, &a, Transform::identity());
}

/// 畫出整個小視窗（預乘 RGBA）；thumb：截圖（預乘），hover：滑鼠在哪裡
pub fn render(thumb: &Pixmap, scale: f32, hover: Option<Hit>) -> Option<(Pixmap, Layout)> {
    let l = layout(scale);
    let s = scale.max(1.0);
    let mut pm = Pixmap::new(l.w, l.h)?;
    fill_round(&mut pm, (0.0, 0.0, l.w as f32, l.h as f32), 10.0 * s, Color::from_rgba8(32, 33, 36, 245));
    // 截圖：等比縮小放在中間
    let (ix, iy, iw, ih) = l.image;
    fill_round(&mut pm, l.image, 6.0 * s, Color::from_rgba8(18, 18, 20, 255));
    let k = (iw / thumb.width() as f32).min(ih / thumb.height() as f32);
    let (tw, th) = (thumb.width() as f32 * k, thumb.height() as f32 * k);
    let (tx, ty) = (ix + (iw - tw) / 2.0, iy + (ih - th) / 2.0);
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, opacity: if hover == Some(Hit::Image) { 0.85 } else { 1.0 }, ..Default::default() };
    let mask = annotate::round_rect(ix, iy, iw, ih, 6.0 * s).and_then(|p| {
        let mut m = tiny_skia::Mask::new(l.w, l.h)?;
        m.fill_path(&p, FillRule::Winding, true, Transform::identity());
        Some(m)
    });
    pm.draw_pixmap(0, 0, thumb.as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, tx, ty), mask.as_ref());
    if hover == Some(Hit::Image) {
        let (w, h) = (100.0 * s, 26.0 * s);
        fill_round(&mut pm, (ix + (iw - w) / 2.0, iy + (ih - h) / 2.0, w, h), 13.0 * s, Color::from_rgba8(0, 0, 0, 170));
        text(&mut pm, tr!("點一下編輯", "Click to edit"), ix + iw / 2.0, iy + ih / 2.0, 13.0 * s);
    }
    // 關閉
    let (cx, cy, r) = l.close;
    let hot = hover == Some(Hit::Button(Button::Close));
    if let Some(c) = PathBuilder::from_circle(cx, cy, r) {
        let mut p = Paint { anti_alias: true, ..Default::default() };
        p.set_color(if hot { Color::from_rgba8(229, 72, 77, 255) } else { Color::from_rgba8(0, 0, 0, 160) });
        pm.fill_path(&c, &p, FillRule::Winding, Transform::identity(), None);
    }
    // ✕ 用兩條線畫（字型不一定有這個符號）
    let d = r * 0.38;
    let mut pb = PathBuilder::new();
    pb.move_to(cx - d, cy - d);
    pb.line_to(cx + d, cy + d);
    pb.move_to(cx + d, cy - d);
    pb.line_to(cx - d, cy + d);
    if let Some(x) = pb.finish() {
        let mut p = Paint { anti_alias: true, ..Default::default() };
        p.set_color(Color::WHITE);
        pm.stroke_path(&x, &p, &tiny_skia::Stroke { width: 1.8 * s, line_cap: tiny_skia::LineCap::Round, ..Default::default() }, Transform::identity(), None);
    }
    // 按鈕
    for ((b, zh, en), (_, rect)) in BUTTONS.iter().zip(&l.buttons) {
        if hover == Some(Hit::Button(*b)) {
            let c = if *b == Button::Delete { Color::from_rgba8(229, 72, 77, 200) } else { Color::from_rgba8(60, 64, 67, 255) };
            fill_round(&mut pm, *rect, 6.0 * s, c);
        }
        let (x, y, w, h) = *rect;
        text(&mut pm, tr!(zh, en), x + w / 2.0, y + h / 2.0, 13.0 * s);
    }
    Some((pm, l))
}

/// 縮圖：長邊縮到 max 像素以內（預乘 RGBA）
pub fn thumbnail(src: &Pixmap, max: u32) -> Option<Pixmap> {
    let k = (max as f32 / src.width().max(src.height()) as f32).min(1.0);
    let (w, h) = (((src.width() as f32 * k).round() as u32).max(1), ((src.height() as f32 * k).round() as u32).max(1));
    let mut out = Pixmap::new(w, h)?;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
    out.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_scale(w as f32 / src.width() as f32, h as f32 / src.height() as f32), None);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_hits() {
        let l = layout(1.0);
        assert_eq!((l.w, l.h), (280, 200));
        assert_eq!(hit(&l, 140.0, 80.0), Some(Hit::Image));
        assert_eq!(hit(&l, l.close.0, l.close.1), Some(Hit::Button(Button::Close)));
        let (bx, by, bw, bh) = l.buttons[3].1;
        assert_eq!(hit(&l, bx + bw / 2.0, by + bh / 2.0), Some(Hit::Button(Button::Delete)));
        assert_eq!(hit(&l, 300.0, 300.0), None);
        // 高 DPI 時整個放大
        assert_eq!(layout(1.5).w, 420);
        let mut src = Pixmap::new(1920, 1080).unwrap();
        src.fill(Color::from_rgba8(0, 120, 255, 255));
        let t = thumbnail(&src, 560).unwrap();
        assert_eq!((t.width(), t.height()), (560, 315));
        let (pm, _) = render(&t, 1.0, Some(Hit::Button(Button::Edit))).unwrap();
        // 圓角外透明、中間是截圖的顏色
        assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
        let c = pm.pixel(140, 80).unwrap();
        assert!(c.blue() > 200 && c.red() < 30);
    }
}
