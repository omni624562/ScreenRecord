//! 只錄聲音時的畫面：一張程式畫的卡片（麥克風圖示、「只錄聲音」與開始時間）。
//! 錄影流程照舊（分段、暫停、意外中斷續錄、聲音對齊都一樣），只是畫面來源換成這張圖，
//! 清單上的縮圖也一眼看得出是錄音。畫面不會動，影片檔幾乎只有聲音的大小。

use crate::annotate;
use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Rect, SpreadMode, Stroke, Transform};

/// 卡片大小（也是影片的大小）
pub const SIZE: (u32, u32) = (640, 360);
/// 每秒幾張（畫面不會動，越少越省）
pub const FPS: f64 = 5.0;

/// 畫出卡片；when：開始時間（例如 2026-10-10 14:30）
pub fn render(when: &str) -> Option<Pixmap> {
    let (w, h) = (SIZE.0 as f32, SIZE.1 as f32);
    let mut pm = Pixmap::new(SIZE.0, SIZE.1)?;
    let shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(w, h),
        vec![GradientStop::new(0.0, Color::from_rgba8(15, 23, 42, 255)), GradientStop::new(1.0, Color::from_rgba8(30, 58, 138, 255))],
        SpreadMode::Pad,
        Transform::identity(),
    )?;
    let bg = Paint { shader, ..Default::default() };
    pm.fill_rect(Rect::from_xywh(0.0, 0.0, w, h)?, &bg, Transform::identity(), None);

    // 麥克風：圓角的頭、U 形的架子、底下的柱子與底座
    let (cx, top) = (w / 2.0, 70.0);
    let mut white = Paint::default();
    white.set_color(Color::WHITE);
    white.anti_alias = true;
    if let Some(head) = annotate::round_rect(cx - 26.0, top, 52.0, 92.0, 26.0) {
        pm.fill_path(&head, &white, FillRule::Winding, Transform::identity(), None);
    }
    let stroke = Stroke { width: 8.0, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
    let mut pb = PathBuilder::new();
    pb.move_to(cx - 46.0, top + 52.0);
    pb.cubic_to(cx - 46.0, top + 130.0, cx + 46.0, top + 130.0, cx + 46.0, top + 52.0);
    pb.move_to(cx, top + 111.0);
    pb.line_to(cx, top + 140.0);
    pb.move_to(cx - 28.0, top + 140.0);
    pb.line_to(cx + 28.0, top + 140.0);
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &white, &stroke, Transform::identity(), None);
    }

    for (s, size, color, y) in [("只錄聲音", 34.0, "#ffffff", 252.0), (when, 20.0, "#cbd5e1", 298.0)] {
        if s.is_empty() {
            continue;
        }
        let mut a = annotate::plain_text(s, size, color);
        a.x = (w as f64 - a.w) / 2.0;
        a.y = y - a.h / 2.0;
        annotate::draw(&mut pm, &a, Transform::identity());
    }
    Some(pm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_has_icon_and_text() {
        let pm = render("2026-10-10 14:30").unwrap();
        assert_eq!((pm.width(), pm.height()), SIZE);
        // 背景不透明、麥克風頭是白色
        assert_eq!(pm.pixel(5, 5).unwrap().alpha(), 255);
        let c = pm.pixel(320, 110).unwrap();
        assert_eq!((c.red(), c.green(), c.blue()), (255, 255, 255));
        // 標題附近有白色的字
        let lit = (200..440).flat_map(|x| (236..268).map(move |y| (x, y))).filter(|&(x, y)| pm.pixel(x, y).unwrap().red() > 200).count();
        assert!(lit > 200, "{lit}");
    }
}
