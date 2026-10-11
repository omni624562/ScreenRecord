//! 影片背景與圓角：輸出大小不變，影片縮小放在漸層 / 單色背景中間，四角變圓、下方有柔和的陰影
//! （和截圖的「背景與圓角」同一套樣式）。
//!
//! FFmpeg 不擅長畫圓角，所以先畫好兩張圖：
//! - 底圖：背景加上陰影，影片疊在上面；
//! - 遮罩：底圖挖掉圓角矩形（裡面透明），蓋在影片上，把影片的四個角蓋成背景。

use crate::annotate;
use serde::{Deserialize, Serialize};
use tiny_skia::{FillRule, Mask, Pixmap, PixmapPaint, Transform};

/// 圓角大小（以 720 像素高為準，依畫面縮放）
pub const RADII: [u32; 4] = [0, 8, 16, 28];
/// 留白（畫面短邊的百分比）
pub const PADDINGS: [u32; 3] = [4, 7, 10];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrame {
    /// 「#rrggbb」單色或「#a,#b」漸層（同 shot_edit::BACKGROUNDS）
    pub background: String,
    pub radius: u32,
    pub padding: u32,
}

impl Default for VideoFrame {
    fn default() -> Self {
        Self { background: crate::shot_edit::BACKGROUNDS[0].into(), radius: 16, padding: 7 }
    }
}

/// 影片在輸出畫面裡的位置與大小（偶數，4:2:0 需要）、圓角半徑
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub radius: f32,
}

pub fn layout(f: &VideoFrame, out_w: i32, out_h: i32) -> Layout {
    let short = out_w.min(out_h).max(2) as f64;
    let m = (short * f.padding.min(30) as f64 / 100.0).round();
    // 保持影片比例，縮到放得進留白裡面
    let k = ((out_w as f64 - m * 2.0) / out_w as f64).min((out_h as f64 - m * 2.0) / out_h as f64).max(0.1);
    let even = |v: f64| ((v / 2.0).round() as i32 * 2).max(2);
    let (w, h) = (even(out_w as f64 * k), even(out_h as f64 * k));
    let (x, y) = ((out_w - w) / 2 / 2 * 2, (out_h - h) / 2 / 2 * 2);
    let radius = (f.radius.min(200) as f64 * short / 720.0) as f32;
    Layout { x, y, w, h, radius: radius.min(w.min(h) as f32 / 2.0) }
}

/// 畫出底圖（背景 + 陰影）與遮罩（底圖挖掉影片的圓角範圍）
pub fn render(f: &VideoFrame, out_w: i32, out_h: i32) -> Option<(Pixmap, Pixmap)> {
    let l = layout(f, out_w, out_h);
    let mut base = Pixmap::new(out_w as u32, out_h as u32)?;
    crate::shot_edit::fill_background(&mut base, &f.background);
    // 陰影：圓角矩形往下偏一點，模糊透明度
    let (w, h) = (out_w as usize, out_h as usize);
    let blur = ((l.w.min(l.h) as f64 * 0.03).round() as usize).clamp(6, 40);
    let off = (blur as f32 * 0.4).round();
    let mut sh = Pixmap::new(out_w as u32, out_h as u32)?;
    if let Some(p) = annotate::round_rect(l.x as f32, l.y as f32 + off, l.w as f32, l.h as f32, l.radius) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(tiny_skia::Color::from_rgba8(0, 0, 0, 120));
        paint.anti_alias = true;
        sh.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }
    let mut a: Vec<u8> = sh.data().to_vec();
    crate::effects::box_blur(&mut a, w, h, (blur / 3).max(1));
    for (d, s) in sh.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(a.as_chunks::<4>().0) {
        *d = [0, 0, 0, s[3]];
    }
    base.draw_pixmap(0, 0, sh.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    // 遮罩：圓角矩形以外留著底圖，裡面透明
    let mut cover = base.clone();
    let mut mask = Mask::new(out_w as u32, out_h as u32)?;
    mask.fill_path(&annotate::round_rect(l.x as f32, l.y as f32, l.w as f32, l.h as f32, l.radius)?, FillRule::Winding, true, Transform::identity());
    for v in mask.data_mut() {
        *v = 255 - *v;
    }
    cover.apply_mask(&mask);
    Some((base, cover))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_keeps_ratio_and_even_sizes() {
        let f = VideoFrame::default();
        let l = layout(&f, 1920, 1080);
        // 留白 7%（76 px）：高度 1080 − 152 = 928，寬度依比例
        assert_eq!((l.w, l.h), (1650, 928));
        assert_eq!((l.x, l.y), (134, 76));
        assert!((l.radius - 24.0).abs() < 0.01);
        let l = layout(&VideoFrame { padding: 4, radius: 0, ..f }, 1001, 601);
        assert!(l.w % 2 == 0 && l.h % 2 == 0 && l.x % 2 == 0 && l.y % 2 == 0);
        assert_eq!(l.radius, 0.0);
    }

    #[test]
    fn cover_hides_only_the_corners() {
        let f = VideoFrame { background: "#ff0000".into(), radius: 28, padding: 10 };
        let (base, cover) = render(&f, 640, 360).unwrap();
        let l = layout(&f, 640, 360);
        // 底圖：背景是紅色；影片範圍下方有陰影（比較暗）
        let c = base.pixel(2, 2).unwrap();
        assert_eq!((c.red(), c.alpha()), (255, 255));
        let below = base.pixel((l.x + l.w / 2) as u32, (l.y + l.h + 3) as u32).unwrap();
        assert!(below.red() < 250, "{}", below.red());
        // 遮罩：中間透明、背景不透明、影片的角落不透明（蓋成背景）
        assert_eq!(cover.pixel(320, 180).unwrap().alpha(), 0);
        assert_eq!(cover.pixel(2, 2).unwrap().alpha(), 255);
        assert_eq!(cover.pixel(l.x as u32 + 1, l.y as u32 + 1).unwrap().alpha(), 255);
        assert_eq!(cover.pixel((l.x + l.w / 2) as u32, l.y as u32 + 1).unwrap().alpha(), 0);
    }
}
