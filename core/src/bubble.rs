//! 浮在畫面上的小提示：延遲截圖的倒數（「3」秒後框選）、長截圖進行中（已接 N 段）、步驟截圖的小工具列（完成、取消）。
//! 這裡只有畫法與按鈕的位置（任何平台都能測試）；Windows 的視窗（不搶焦點、不被截圖錄影拍進去）在 bubble_win.rs。

use crate::annotate;
use tiny_skia::{Color, FillRule, Paint, Pixmap, Transform};

/// 要顯示的內容：最左邊一個大字（倒數的秒數，可以沒有）、一到兩行字、右邊的按鈕
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Content {
    pub big: Option<String>,
    pub lines: Vec<String>,
    /// (編號, 字, 主要按鈕)
    pub buttons: Vec<(u32, String, bool)>,
}

impl Content {
    pub fn text(line: impl Into<String>) -> Content {
        Content { lines: vec![line.into()], ..Default::default() }
    }
}

/// 版面（像素）：大小與每個按鈕的範圍 (編號, (x, y, 寬, 高))
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub w: u32,
    pub h: u32,
    pub buttons: Vec<(u32, (f32, f32, f32, f32))>,
}

/// 點到哪個按鈕
pub fn hit(l: &Layout, x: f32, y: f32) -> Option<u32> {
    l.buttons.iter().find(|(_, (bx, by, bw, bh))| x >= *bx && x < bx + bw && y >= *by && y < by + bh).map(|(id, _)| *id)
}

fn round(pm: &mut Pixmap, (x, y, w, h): (f32, f32, f32, f32), r: f32, c: Color) {
    if let Some(p) = annotate::round_rect(x, y, w, h, r) {
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(c);
        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// 畫出整個提示（預乘 RGBA，深色圓角底、白字）；scale = DPI / 96，hover = 滑鼠在哪個按鈕上
pub fn render(c: &Content, scale: f32, hover: Option<u32>) -> Option<(Pixmap, Layout)> {
    let s = scale.max(1.0) as f64;
    let (pad_x, pad_y, gap) = (16.0 * s, 9.0 * s, 10.0 * s);
    let big = c.big.as_ref().map(|t| annotate::plain_text(t, 30.0 * s, "#ffffff"));
    let lines: Vec<_> = c.lines.iter().enumerate().map(|(i, t)| annotate::plain_text(t, if i == 0 { 14.0 } else { 12.0 } * s, if i == 0 { "#ffffff" } else { "#c8c8cc" })).collect();
    let buttons: Vec<_> = c.buttons.iter().map(|(id, t, primary)| (*id, annotate::plain_text(t, 13.0 * s, "#ffffff"), *primary)).collect();
    let (bpad, bh) = (12.0 * s, 28.0 * s);
    let text_w = lines.iter().map(|a| a.w).fold(0.0, f64::max);
    let text_h: f64 = lines.iter().map(|a| a.h).sum::<f64>() + 2.0 * s * (lines.len().saturating_sub(1)) as f64;
    let big_w = big.as_ref().map(|a| a.w + gap).unwrap_or(0.0);
    let btn_w: f64 = buttons.iter().map(|(_, a, _)| a.w + bpad * 2.0 + 6.0 * s).sum();
    let inner_h = text_h.max(big.as_ref().map(|a| a.h).unwrap_or(0.0)).max(if buttons.is_empty() { 0.0 } else { bh });
    let (w, h) = ((pad_x * 2.0 + big_w + text_w + if buttons.is_empty() { 0.0 } else { gap + btn_w }).ceil(), (pad_y * 2.0 + inner_h).ceil());
    let mut pm = Pixmap::new(w as u32, h as u32)?;
    round(&mut pm, (0.0, 0.0, w as f32, h as f32), (h / 2.0).min(18.0 * s) as f32, Color::from_rgba8(24, 24, 27, 238));
    let cy = h / 2.0;
    let mut x = pad_x;
    if let Some(mut a) = big {
        (a.x, a.y) = (x, cy - a.h / 2.0);
        annotate::draw(&mut pm, &a, Transform::identity());
        x += a.w + gap;
    }
    let mut y = cy - text_h / 2.0;
    for mut a in lines {
        (a.x, a.y) = (x, y);
        y += a.h + 2.0 * s;
        annotate::draw(&mut pm, &a, Transform::identity());
    }
    x += text_w + gap;
    let mut rects = vec![];
    for (id, mut a, primary) in buttons {
        let r = (x as f32, (cy - bh / 2.0) as f32, (a.w + bpad * 2.0) as f32, bh as f32);
        let fill = match (primary, hover == Some(id)) {
            (true, false) => Some(Color::from_rgba8(37, 99, 235, 255)),
            (true, true) => Some(Color::from_rgba8(59, 130, 246, 255)),
            (false, true) => Some(Color::from_rgba8(72, 72, 78, 255)),
            (false, false) => Some(Color::from_rgba8(52, 52, 58, 255)),
        };
        if let Some(f) = fill {
            round(&mut pm, r, 7.0 * s as f32, f);
        }
        (a.x, a.y) = (x + bpad, cy - a.h / 2.0);
        annotate::draw(&mut pm, &a, Transform::identity());
        rects.push((id, r));
        x += a.w + bpad * 2.0 + 6.0 * s;
    }
    Some((pm, Layout { w: w as u32, h: h as u32, buttons: rects }))
}

/// 放在哪裡（左上角，桌面座標）：near 的下方，放不下時上方，都放不下時放在 near 裡面的下緣；
/// 沒有 near 時放在 work（工作區）上方或下方的中間
pub fn place((w, h): (i32, i32), work: (i32, i32, i32, i32), near: Option<(i32, i32, i32, i32)>, bottom: bool, gap: i32) -> (i32, i32) {
    let (x0, y0, x1, y1) = work;
    let clamp_x = |x: i32| x.clamp(x0, (x1 - w).max(x0));
    match near {
        Some((l, t, r, b)) => {
            let x = clamp_x((l + r - w) / 2);
            let y = if b + gap + h <= y1 {
                b + gap
            } else if t - gap - h >= y0 {
                t - gap - h
            } else {
                (b - gap - h).clamp(y0, (y1 - h).max(y0))
            };
            (x, y)
        }
        None => (clamp_x((x0 + x1 - w) / 2), if bottom { y1 - gap - h } else { y0 + gap }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_text_big_number_and_buttons() {
        let plain = render(&Content::text("長截圖中・已接 3 段・按 Esc 停止"), 1.0, None).unwrap().0;
        assert!(plain.width() > 200 && plain.height() >= 30, "{}×{}", plain.width(), plain.height());
        // 圓角外透明、中間是深色底
        assert_eq!(plain.pixel(0, 0).unwrap().alpha(), 0);
        let c = Content { big: Some("3".into()), lines: vec!["秒後框選".into(), "按 Esc 或點一下取消".into()], buttons: vec![] };
        let (pm, l) = render(&c, 1.5, None).unwrap();
        assert!(l.buttons.is_empty() && pm.height() > plain.height());
        let c = Content { big: None, lines: vec!["步驟截圖中・已截 3 步".into()], buttons: vec![(1, "完成".into(), true), (2, "取消".into(), false)] };
        let (pm, l) = render(&c, 1.0, Some(1)).unwrap();
        assert_eq!(l.buttons.len(), 2);
        let (bx, by, bw, bh) = l.buttons[1].1;
        assert_eq!(hit(&l, bx + bw / 2.0, by + bh / 2.0), Some(2));
        assert_eq!(hit(&l, 2.0, 2.0), None);
        assert!(bx + bw <= pm.width() as f32);
    }

    #[test]
    fn placement() {
        let work = (0, 0, 1920, 1040);
        // 範圍下方置中；下方放不下放上方；都放不下放在範圍裡
        assert_eq!(place((200, 40), work, Some((100, 100, 500, 400)), false, 12), (200, 412));
        assert_eq!(place((200, 40), work, Some((100, 600, 500, 1030)), false, 12), (200, 548));
        assert_eq!(place((200, 40), work, Some((0, 0, 1920, 1040)), false, 12), (860, 988));
        // 沒有範圍：工作區上方或下方的中間
        assert_eq!(place((200, 40), work, None, false, 12), (860, 12));
        assert_eq!(place((200, 40), work, None, true, 12), (860, 988));
    }
}
