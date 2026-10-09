// 開發用：把各種標註畫成一張圖（cargo run -p screenrecorder-core --example ann_sheet -- out.png）
use screenrecorder_core::annotate::*;
use tiny_skia::{Color, Pixmap, Transform};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "ann_sheet.png".into());
    let mut pm = Pixmap::new(960, 540).unwrap();
    pm.fill(Color::from_rgba8(70, 110, 160, 255));
    let base = Ann { id: 1, kind: AnnKind::Text, x: 40.0, y: 30.0, w: 0.0, h: 0.0, start: 0.0, end: 3.0, color: "#ffffff".into(), size: 48.0, text: Some("按這裡登入 Login\n第二行 👉✅⚠️🔥".into()), bg: true, n: None, shape: None, invert: false };
    let mut list = vec![base.clone()];
    list.push(Ann { y: 200.0, bg: false, color: "#f5b301".into(), text: Some("沒有底色的文字 😀".into()), ..base.clone() });
    list.push(Ann { kind: AnnKind::Arrow, x: 500.0, y: 300.0, w: 200.0, h: -120.0, size: 8.0, color: "#e5484d".into(), text: None, ..base.clone() });
    list.push(Ann { kind: AnnKind::Rect, x: 60.0, y: 320.0, w: 220.0, h: 140.0, size: 6.0, color: "#30a46c".into(), text: None, ..base.clone() });
    list.push(Ann { kind: AnnKind::Ellipse, x: 320.0, y: 330.0, w: 160.0, h: 110.0, size: 6.0, color: "#0090ff".into(), text: None, ..base.clone() });
    list.push(Ann { kind: AnnKind::Highlight, x: 740.0, y: 40.0, w: 180.0, h: 60.0, color: "#f5b301".into(), text: None, ..base.clone() });
    for (i, c) in ["#e5484d", "#ffffff", "#f5b301"].iter().enumerate() {
        list.push(Ann { kind: AnnKind::Step, x: 760.0 + i as f64 * 70.0, y: 420.0, size: 56.0, n: Some(i as u32 + 1), color: c.to_string(), text: None, ..base.clone() });
    }
    for a in &mut list {
        measure(a);
        draw(&mut pm, a, Transform::identity());
    }
    pm.save_png(&out).unwrap();
    println!("{out}");
}
