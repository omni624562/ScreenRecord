//! 框選畫面上的小工具：放大鏡與取色（色碼）、尺規（量距離）。
//! 這裡只有計算與繪製（可以在任何平台測試），Windows 的框選視窗（snip_win.rs）負責顯示與操作。

use crate::annotate;
use crate::trf;
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

/// 尺規：按住 Shift 時鎖定成水平、垂直或 45 度
pub fn snap((ax, ay): (i32, i32), (bx, by): (i32, i32)) -> (i32, i32) {
    let (dx, dy) = (bx - ax, by - ay);
    let (adx, ady) = (dx.abs(), dy.abs());
    if adx * 2 < ady {
        (ax, by)
    } else if ady * 2 < adx {
        (bx, ay)
    } else {
        let d = adx.max(ady);
        (ax + d * dx.signum(), ay + d * dy.signum())
    }
}

/// 尺規的說明：320 px（寬 300、高 110）・20°
pub fn ruler_text((ax, ay): (i32, i32), (bx, by): (i32, i32)) -> String {
    let (dx, dy) = (bx - ax, by - ay);
    let len = ((dx * dx + dy * dy) as f64).sqrt();
    // 角度：向右為 0、逆時針為正（和量角器一樣）
    let deg = (-(dy as f64)).atan2(dx as f64).to_degrees();
    let mut s = format!("{} px", len.round() as i64);
    if dx != 0 && dy != 0 {
        s.push_str(&trf!("（寬 {}、高 {}）", " (W {}, H {})", dx.abs(), dy.abs()));
    }
    s.push_str(&trf!("・{}°", " · {}°", deg.round() as i64));
    s
}

/// 量好的距離畫成線的兩端小橫槓（垂直於線）：回傳兩條短線的端點
pub fn ruler_ticks((ax, ay): (i32, i32), (bx, by): (i32, i32), len: f64) -> [((i32, i32), (i32, i32)); 2] {
    let (dx, dy) = ((bx - ax) as f64, (by - ay) as f64);
    let d = dx.hypot(dy).max(1e-9);
    let (nx, ny) = (-dy / d * len, dx / d * len);
    let tick = |x: i32, y: i32| (((x as f64 - nx).round() as i32, (y as f64 - ny).round() as i32), ((x as f64 + nx).round() as i32, (y as f64 + ny).round() as i32));
    [tick(ax, ay), tick(bx, by)]
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
    fn ruler() {
        assert_eq!(snap((0, 0), (100, 10)), (100, 0));
        assert_eq!(snap((0, 0), (-8, 90)), (0, 90));
        assert_eq!(snap((0, 0), (40, -50)), (50, -50));
        assert_eq!(ruler_text((0, 0), (300, 0)), "300 px・0°");
        assert_eq!(ruler_text((0, 0), (0, -120)), "120 px・90°");
        assert_eq!(ruler_text((10, 10), (310, 120)), "320 px（寬 300、高 110）・-20°");
        let [(a, b), _] = ruler_ticks((0, 0), (100, 0), 6.0);
        assert_eq!((a, b), ((0, -6), (0, 6)));
    }
}
