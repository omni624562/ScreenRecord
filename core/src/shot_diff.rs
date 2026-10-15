//! 兩張截圖比對差異：找出不一樣的地方（改版前後、測試前後），在後面那張上用紅框標出來。
//! 以 8×8 的小格比較（忽略壓縮、反鋸齒造成的細微差異），相鄰的小格連成一塊，再合併重疊或很近的框。

use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const CELL: u32 = 8;
/// 一個像素算「不一樣」：任一色版差超過這麼多
const PIXEL_DIFF: u8 = 40;
/// 一格裡有這麼多不一樣的像素才算這格不一樣
const CELL_PIXELS: u32 = 3;

/// 不一樣的範圍（x, y, 寬, 高；像素）
pub type Box4 = (u32, u32, u32, u32);

/// b 縮放（最近鄰）成 w × h，大小一樣時原樣
fn resize(b: &[u8], bw: u32, bh: u32, w: u32, h: u32) -> Vec<u8> {
    if (bw, bh) == (w, h) {
        return b.to_vec();
    }
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let sy = (y as u64 * bh as u64 / h as u64) as u32;
        for x in 0..w {
            let sx = (x as u64 * bw as u64 / w as u64) as u32;
            let si = ((sy * bw + sx) * 4) as usize;
            let di = ((y * w + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&b[si..si + 4]);
        }
    }
    out
}

/// 找出不一樣的範圍（a 的大小；b 大小不同時先縮放成一樣）
pub fn find(a: &[u8], w: u32, h: u32, b: &[u8], bw: u32, bh: u32) -> Vec<Box4> {
    let b = resize(b, bw, bh, w, h);
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells = vec![false; (cw * ch) as usize];
    for cy in 0..ch {
        for cx in 0..cw {
            let mut n = 0;
            'cell: for y in cy * CELL..((cy + 1) * CELL).min(h) {
                for x in cx * CELL..((cx + 1) * CELL).min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    if (0..3).any(|k| a[i + k].abs_diff(b[i + k]) > PIXEL_DIFF) {
                        n += 1;
                        if n >= CELL_PIXELS {
                            break 'cell;
                        }
                    }
                }
            }
            cells[(cy * cw + cx) as usize] = n >= CELL_PIXELS;
        }
    }
    // 相鄰（含隔一格）的不一樣小格連成一塊
    let mut seen = vec![false; cells.len()];
    let mut boxes: Vec<(u32, u32, u32, u32)> = vec![];
    for start in 0..cells.len() {
        if !cells[start] || seen[start] {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (cx, cy) = (i as u32 % cw, i as u32 / cw);
            (x0, y0, x1, y1) = (x0.min(cx), y0.min(cy), x1.max(cx), y1.max(cy));
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= cw as i32 || ny >= ch as i32 {
                        continue;
                    }
                    let j = (ny as u32 * cw + nx as u32) as usize;
                    if cells[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        boxes.push((x0, y0, x1, y1));
    }
    // 小格 → 像素，四周多留一點；重疊的合併
    let pad = 4;
    let mut px: Vec<Box4> = boxes
        .into_iter()
        .map(|(x0, y0, x1, y1)| {
            let l = (x0 * CELL).saturating_sub(pad);
            let t = (y0 * CELL).saturating_sub(pad);
            let r = ((x1 + 1) * CELL + pad).min(w);
            let b = ((y1 + 1) * CELL + pad).min(h);
            (l, t, r - l, b - t)
        })
        .collect();
    loop {
        let mut merged = false;
        'outer: for i in 0..px.len() {
            for j in i + 1..px.len() {
                let (a, b) = (px[i], px[j]);
                if a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3 {
                    let l = a.0.min(b.0);
                    let t = a.1.min(b.1);
                    let r = (a.0 + a.2).max(b.0 + b.2);
                    let bt = (a.1 + a.3).max(b.1 + b.3);
                    px[i] = (l, t, r - l, bt - t);
                    px.remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    px.sort_by_key(|b| (b.1, b.0));
    px
}

/// 在後面那張（after，已預乘的 Pixmap）上畫出紅框與淡淡的紅底
pub fn mark(after: &mut Pixmap, boxes: &[Box4]) {
    let w = after.width().max(after.height()) as f32;
    let stroke = Stroke { width: (w / 400.0).clamp(3.0, 8.0), ..Default::default() };
    let mut line = Paint { anti_alias: true, ..Default::default() };
    line.set_color(Color::from_rgba8(229, 57, 53, 255));
    let mut fill = Paint::default();
    fill.set_color(Color::from_rgba8(229, 57, 53, 40));
    for &(x, y, bw, bh) in boxes {
        let Some(r) = Rect::from_xywh(x as f32, y as f32, bw as f32, bh as f32) else { continue };
        after.fill_rect(r, &fill, Transform::identity(), None);
        let path = PathBuilder::from_rect(r);
        after.stroke_path(&path, &line, &stroke, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&f(x, y));
            }
        }
        v
    }

    #[test]
    fn finds_two_separate_changes() {
        let a = img(200, 100, |_, _| [255, 255, 255, 255]);
        let b = img(200, 100, |x, y| if (10..30).contains(&x) && (10..20).contains(&y) || (150..190).contains(&x) && (60..90).contains(&y) { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let boxes = find(&a, 200, 100, &b, 200, 100);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        let (x, y, w, h) = boxes[0];
        assert!(x <= 10 && y <= 10 && x + w >= 30 && y + h >= 20, "{boxes:?}");
        let (x, y, w, h) = boxes[1];
        assert!(x <= 150 && y <= 60 && x + w >= 190 && y + h >= 90, "{boxes:?}");
    }

    #[test]
    fn tiny_noise_and_same_images_find_nothing() {
        let a = img(64, 64, |x, y| [(x * 4) as u8, (y * 4) as u8, 100, 255]);
        // 色差很小（壓縮）與單一像素不算
        let b = img(64, 64, |x, y| if (x, y) == (30, 30) { [0, 0, 0, 255] } else { [(x * 4) as u8 + 3, (y * 4) as u8, 100, 255] });
        assert!(find(&a, 64, 64, &b, 64, 64).is_empty());
        assert!(find(&a, 64, 64, &a, 64, 64).is_empty());
    }

    #[test]
    fn different_sizes_are_scaled_and_marks_are_drawn() {
        let a = img(100, 100, |_, _| [255, 255, 255, 255]);
        let b = img(200, 200, |x, y| if x < 40 && y < 40 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let boxes = find(&a, 100, 100, &b, 200, 200);
        assert_eq!(boxes.len(), 1);
        let mut pm = Pixmap::new(100, 100).unwrap();
        pm.fill(Color::WHITE);
        mark(&mut pm, &boxes);
        let edge = pm.pixel(boxes[0].0 + 1, boxes[0].1 + 10).unwrap();
        assert!(edge.red() > 200 && edge.green() < 160, "{edge:?}");
    }
}
