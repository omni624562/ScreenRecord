//! 錄影時顯示滑鼠點擊與按鍵：畫波紋與按鍵提示（各平台共用的部分，Windows 的視窗在 input_overlay_win）。

use crate::annotate::{self, Ann, AnnKind};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// 波紋擴散的時間（毫秒）
pub const RIPPLE_MS: u64 = 450;
/// 按鍵提示顯示多久（毫秒，之後淡出）
pub const KEY_SHOW_MS: u64 = 1400;
pub const KEY_FADE_MS: u64 = 300;

/// 波紋視窗的邊長（96 DPI 時的像素）
pub const RIPPLE_SIZE: f32 = 64.0;

/// 點擊的波紋：progress 0～1（由小變大、漸漸變淡）；右鍵用藍色
pub fn ripple(size: u32, progress: f32, right: bool) -> Option<Pixmap> {
    let mut pm = Pixmap::new(size, size)?;
    let t = progress.clamp(0.0, 1.0);
    // 先快後慢
    let ease = 1.0 - (1.0 - t) * (1.0 - t);
    let c = size as f32 / 2.0;
    let line = (size as f32 / 21.0).max(2.0);
    let r = (c - line) * (0.3 + 0.7 * ease);
    let fade = 1.0 - t;
    let (cr, cg, cb) = if right { (0x00, 0x90, 0xff) } else { (0xff, 0xc4, 0x00) };
    let path = PathBuilder::from_circle(c, c, r)?;
    let mut paint = Paint { anti_alias: true, ..Default::default() };
    paint.set_color(Color::from_rgba8(cr, cg, cb, (fade * 90.0) as u8));
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    paint.set_color(Color::from_rgba8(cr, cg, cb, (fade * 235.0) as u8));
    pm.stroke_path(&path, &paint, &Stroke { width: line, ..Default::default() }, Transform::identity(), None);
    Some(pm)
}

/// 游標光暈的大小（96 DPI 時的直徑）
pub const HALO_SIZE: f32 = 56.0;

/// 游標光暈：半透明黃色的圓，邊緣柔和
pub fn halo(size: u32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(size, size)?;
    let c = size as f32 / 2.0;
    // 由外往內疊幾圈，越裡面越不透明
    for (k, a) in [(1.0, 30u8), (0.85, 40), (0.7, 45)] {
        let path = PathBuilder::from_circle(c, c, (c - 1.0) * k)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(Color::from_rgba8(255, 214, 0, a));
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
    Some(pm)
}

/// 按鍵提示：深色圓角底、白字（例如「Ctrl + C」）；scale = DPI / 96
pub fn key_pill(text: &str, scale: f32) -> Option<Pixmap> {
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
        size: (22.0 * scale as f64).round(),
        text: Some(text.to_string()),
        bg: true,
        n: None,
        shape: None,
        invert: false,
        rot: 0.0,
        pts: vec![],
    };
    annotate::measure(&mut a);
    let mut pm = Pixmap::new(a.w.ceil().max(1.0) as u32, a.h.ceil().max(1.0) as u32)?;
    annotate::draw(&mut pm, &a, Transform::identity());
    Some(pm)
}

/// 連續按同一組按鍵時加上次數：「Ctrl + Z ×3」
pub fn key_text(label: &str, count: u32) -> String {
    if count > 1 {
        format!("{label} ×{count}")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_ripple_and_key_pill() {
        let start = ripple(64, 0.0, false).unwrap();
        let end = ripple(64, 0.95, true).unwrap();
        // 一開始中間有顏色、角落透明；快結束時幾乎看不到
        assert!(start.pixel(32, 32).unwrap().alpha() > 0);
        assert_eq!(start.pixel(0, 0).unwrap().alpha(), 0);
        assert!(end.pixels().iter().map(|p| p.alpha() as u32).max().unwrap() < 40);
        let pill = key_pill("Ctrl + C", 1.0).unwrap();
        assert!(pill.width() > pill.height() && pill.height() >= 22);
        // 深色底
        let c = pill.pixel(2, pill.height() / 2).unwrap();
        assert!(c.alpha() > 100 && c.red() < 80);
        assert!(key_pill("Ctrl + C", 2.0).unwrap().width() > pill.width() * 3 / 2);
        assert_eq!(key_text("Ctrl + Z", 3), "Ctrl + Z ×3");
        assert_eq!(key_text("Enter", 1), "Enter");
        let h = halo(56).unwrap();
        assert!(h.pixel(28, 28).unwrap().alpha() > 80 && h.pixel(0, 0).unwrap().alpha() == 0);
    }
}
