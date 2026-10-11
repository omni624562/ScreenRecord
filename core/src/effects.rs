//! 剪輯預覽的馬賽克 / 模糊：在 CPU 上直接處理影片畫面（RGBA），強度與匯出時 FFmpeg 相同：
//! - 模糊：boxblur 半徑 r、套用兩次（= FFmpeg 的 boxblur=r:2）
//! - 馬賽克：縮小（取平均）再以最近鄰放大
//! - 框內：只處理範圍，範圍邊緣以範圍內的像素延伸（與 FFmpeg 先 crop 再處理相同）
//! - 框外：整個畫面處理後，把形狀內的原始畫面留下
//!
//! - 放大鏡（只限截圖）：圓裡顯示中心附近放大後的畫面，外圈白邊與細的深色外框
//!
//! 畫面可以比原影片小（預覽解析度）：位置與強度依比例換算。

use crate::annotate::{ann_contains, effect_size, magnify_ring, rotated_rect, rotation, shape_contains, Ann, AnnKind};

/// 水平或垂直方向的方塊模糊（邊緣延伸），src → dst
fn box_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if horizontal { (line * w + i) * 4 } else { (i * w + line) * 4 };
    let div = (2 * r + 1) as u32;
    for line in 0..lines {
        let mut sum = [0u32; 4];
        // 視窗 [-r, r]，邊緣延伸
        for k in 0..=2 * r {
            let i = (k as isize - r as isize).clamp(0, n as isize - 1) as usize;
            let p = idx(line, i);
            for c in 0..4 {
                sum[c] += src[p + c] as u32;
            }
        }
        for i in 0..n {
            let o = idx(line, i);
            for c in 0..4 {
                dst[o + c] = ((sum[c] + div / 2) / div) as u8;
            }
            let out = idx(line, (i as isize - r as isize).clamp(0, n as isize - 1) as usize);
            let inn = idx(line, (i + r + 1).min(n - 1));
            for c in 0..4 {
                sum[c] = sum[c] + src[inn + c] as u32 - src[out + c] as u32;
            }
        }
    }
}

/// boxblur=r:2（水平、垂直各兩次）
pub fn box_blur(px: &mut [u8], w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0u8; px.len()];
    for _ in 0..2 {
        box_pass(px, &mut tmp, w, h, r.min(w.saturating_sub(1)).max(1), true);
        box_pass(&tmp, px, w, h, r.min(h.saturating_sub(1)).max(1), false);
    }
}

/// 縮小成 cols × rows 格（取平均）再放大回原尺寸
pub fn pixelate(px: &mut [u8], w: usize, h: usize, cols: usize, rows: usize) {
    let (cols, rows) = (cols.clamp(1, w.max(1)), rows.clamp(1, h.max(1)));
    for gy in 0..rows {
        let (y0, y1) = (gy * h / rows, ((gy + 1) * h / rows).max(gy * h / rows + 1).min(h));
        for gx in 0..cols {
            let (x0, x1) = (gx * w / cols, ((gx + 1) * w / cols).max(gx * w / cols + 1).min(w));
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = (y * w + x) * 4;
                    for c in 0..4 {
                        sum[c] += px[p + c] as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as u64;
            let avg = sum.map(|s| (s / n) as u8);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = (y * w + x) * 4;
                    px[p..p + 4].copy_from_slice(&avg);
                }
            }
        }
    }
}

/// 對畫面（fw × fh，RGBA）套用一個馬賽克 / 模糊；vw × vh 為原影片尺寸
pub fn apply(frame: &mut [u8], fw: usize, fh: usize, a: &Ann, vw: f64, vh: f64) {
    if !a.kind.is_effect() || fw == 0 || fh == 0 || vw <= 0.0 || vh <= 0.0 {
        return;
    }
    let (sx, sy) = (fw as f64 / vw, fh as f64 / vh);
    if a.kind == AnnKind::Magnify {
        return magnify(frame, fw, fh, a, sx, sy);
    }
    let size = effect_size(a, vw, vh);
    // 範圍（畫面像素，限制在畫面內；旋轉時取旋轉後的外接框）
    let (bx, by, bw, bh) = rotated_rect(a, (a.x, a.y, a.w, a.h));
    let rx0 = (bx * sx).floor().clamp(0.0, fw as f64) as usize;
    let ry0 = (by * sy).floor().clamp(0.0, fh as f64) as usize;
    let rx1 = ((bx + bw) * sx).ceil().clamp(0.0, fw as f64) as usize;
    let ry1 = ((by + bh) * sy).ceil().clamp(0.0, fh as f64) as usize;
    let rotated = rotation(a) != 0.0;
    let inside = |x: usize, y: usize| {
        if rotated {
            ann_contains(a, (x as f64 + 0.5) / sx, (y as f64 + 0.5) / sy)
        } else {
            shape_contains(a.shape, a.x * sx, a.y * sy, a.w * sx, a.h * sy, x as f64 + 0.5, y as f64 + 0.5)
        }
    };
    if a.invert {
        let mut fx = frame.to_vec();
        if a.kind == AnnKind::Blur {
            box_blur(&mut fx, fw, fh, (size * sx).round().max(1.0) as usize);
        } else {
            let cell = (size * sx).max(1.0);
            pixelate(&mut fx, fw, fh, (fw as f64 / cell).round() as usize, (fh as f64 / cell).round() as usize);
        }
        for y in 0..fh {
            for x in 0..fw {
                if !inside(x, y) {
                    let p = (y * fw + x) * 4;
                    frame[p..p + 4].copy_from_slice(&fx[p..p + 4]);
                }
            }
        }
        return;
    }
    let (rw, rh) = (rx1.saturating_sub(rx0), ry1.saturating_sub(ry0));
    if rw == 0 || rh == 0 {
        return;
    }
    let mut region = vec![0u8; rw * rh * 4];
    for y in 0..rh {
        let s = ((ry0 + y) * fw + rx0) * 4;
        region[y * rw * 4..(y + 1) * rw * 4].copy_from_slice(&frame[s..s + rw * 4]);
    }
    if a.kind == AnnKind::Blur {
        box_blur(&mut region, rw, rh, (size * sx).round().max(1.0) as usize);
    } else {
        let cell = (size * sx).max(1.0);
        pixelate(&mut region, rw, rh, (rw as f64 / cell).round() as usize, (rh as f64 / cell).round() as usize);
    }
    for y in 0..rh {
        for x in 0..rw {
            if inside(rx0 + x, ry0 + y) {
                let p = ((ry0 + y) * fw + rx0 + x) * 4;
                let q = (y * rw + x) * 4;
                frame[p..p + 4].copy_from_slice(&region[q..q + 4]);
            }
        }
    }
}

/// 放大鏡：橢圓（通常是正圓）裡顯示以中心為準、放大 size / 100 倍的畫面（雙線性取樣），外圈白邊與深色外框
fn magnify(frame: &mut [u8], fw: usize, fh: usize, a: &Ann, sx: f64, sy: f64) {
    let zoom = (a.size / 100.0).clamp(1.1, 8.0);
    let (x0, y0, w, h) = (a.x.min(a.x + a.w) * sx, a.y.min(a.y + a.h) * sy, a.w.abs() * sx, a.h.abs() * sy);
    if w < 2.0 || h < 2.0 {
        return;
    }
    let (cx, cy, rx, ry) = (x0 + w / 2.0, y0 + h / 2.0, w / 2.0, h / 2.0);
    let ring = magnify_ring(a.w.abs().min(a.h.abs())) * sx;
    let src = frame.to_vec();
    let sample = |x: f64, y: f64| -> [u8; 4] {
        let x = x.clamp(0.0, fw as f64 - 1.0);
        let y = y.clamp(0.0, fh as f64 - 1.0);
        let (ix, iy) = (x.floor() as usize, y.floor() as usize);
        let (jx, jy) = ((ix + 1).min(fw - 1), (iy + 1).min(fh - 1));
        let (fx, fy) = (x - ix as f64, y - iy as f64);
        let px = |x: usize, y: usize, c: usize| src[(y * fw + x) * 4 + c] as f64;
        let mut out = [0u8; 4];
        for (c, o) in out.iter_mut().enumerate() {
            let top = px(ix, iy, c) * (1.0 - fx) + px(jx, iy, c) * fx;
            let bot = px(ix, jy, c) * (1.0 - fx) + px(jx, jy, c) * fx;
            *o = (top * (1.0 - fy) + bot * fy).round() as u8;
        }
        out
    };
    let outline = 1.5 * sx.max(0.5);
    let ys = (y0 - outline).floor().max(0.0) as usize..((y0 + h + outline).ceil() as usize).min(fh);
    for y in ys {
        for x in (x0 - outline).floor().max(0.0) as usize..((x0 + w + outline).ceil() as usize).min(fw) {
            let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
            // 到橢圓邊的距離（像素，近似）：裡面為負
            let n = ((dx / rx).powi(2) + (dy / ry).powi(2)).sqrt();
            let d = (n - 1.0) * rx.min(ry);
            let p = (y * fw + x) * 4;
            let mix = |frame: &mut [u8], c: [u8; 4], k: f64| {
                for i in 0..3 {
                    frame[p + i] = (frame[p + i] as f64 * (1.0 - k) + c[i] as f64 * k).round() as u8;
                }
                frame[p + 3] = frame[p + 3].max((255.0 * k) as u8);
            };
            if d > outline {
                continue;
            }
            if d > 0.0 {
                // 外框：深色細線（邊緣漸淡）
                mix(frame, [0, 0, 0, 255], 0.55 * (1.0 - d / outline));
            } else if d > -ring {
                mix(frame, [255, 255, 255, 255], 1.0);
            } else {
                let v = sample(cx + dx / zoom - 0.5, cy + dy / zoom - 0.5);
                frame[p..p + 4].copy_from_slice(&v);
                // 白邊內側一圈淡淡的陰影
                let k = (1.0 - (-d - ring) / (2.0 * sx.max(0.5))).clamp(0.0, 1.0) * 0.25;
                if k > 0.0 {
                    mix(frame, [0, 0, 0, 255], k);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::Shape;

    /// 黑白相間的直條紋（每 2 像素）
    fn stripes(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = if (x / 2) % 2 == 0 { 255 } else { 0 };
                v[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c, c, c, 255]);
            }
        }
        v
    }

    fn ann(kind: AnnKind, shape: Option<Shape>, invert: bool) -> Ann {
        Ann { id: 1, kind, x: 20.0, y: 20.0, w: 40.0, h: 40.0, start: 0.0, end: 1.0, color: "#000000".into(), size: 8.0, text: None, bg: false, n: None, shape, invert, rot: 0.0, pts: vec![] }
    }

    fn at(v: &[u8], w: usize, x: usize, y: usize) -> u8 {
        v[(y * w + x) * 4]
    }

    #[test]
    fn magnify_zooms_the_center() {
        // 左半黑、右半白（分界 x = 40）；放大鏡中心 (40, 40)、半徑 20、放大 4 倍
        let (w, h) = (100, 80);
        let half = |w: usize, h: usize| {
            let mut v = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let c = if x < 40 { 0 } else { 255 };
                    v[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c, c, c, 255]);
                }
            }
            v
        };
        let mut f = half(w, h);
        let mut a = ann(AnnKind::Magnify, None, false);
        a.size = 400.0;
        apply(&mut f, w, h, &a, w as f64, h as f64);
        // 放大後分界線兩側各 12 像素看到的仍是原本分界旁 3 像素：左黑右白
        assert_eq!(at(&f, w, 28, 40), 0);
        assert_eq!(at(&f, w, 52, 40), 255);
        // 原本在 x = 30 的黑色區域，放大後變成白邊與分界附近的漸層之外：x = 36 取樣自 x ≈ 39 → 黑
        assert!(at(&f, w, 36, 40) < 40);
        // 上緣是白邊、外面不變
        assert_eq!(at(&f, w, 40, 21), 255);
        assert_eq!(at(&f, w, 5, 5), 0);
        assert_eq!(at(&f, w, 95, 5), 255);
    }

    #[test]
    fn blur_inside_and_outside() {
        let (w, h) = (100, 80);
        // 框內模糊：範圍內變灰、範圍外不變
        let mut f = stripes(w, h);
        apply(&mut f, w, h, &ann(AnnKind::Blur, None, false), w as f64, h as f64);
        assert!((100..156).contains(&at(&f, w, 40, 40)), "{}", at(&f, w, 40, 40));
        assert_eq!(at(&f, w, 5, 5), stripes(w, h)[(5 * w + 5) * 4]);
        // 框外模糊：範圍內不變、範圍外變灰
        let mut f = stripes(w, h);
        apply(&mut f, w, h, &ann(AnnKind::Blur, None, true), w as f64, h as f64);
        assert_eq!(at(&f, w, 40, 40), stripes(w, h)[(40 * w + 40) * 4]);
        assert!((60..196).contains(&at(&f, w, 5, 5)), "{}", at(&f, w, 5, 5));
        // 橢圓：外接框的角落不處理
        let mut f = stripes(w, h);
        apply(&mut f, w, h, &ann(AnnKind::Blur, Some(Shape::Ellipse), false), w as f64, h as f64);
        assert_eq!(at(&f, w, 21, 21), stripes(w, h)[(21 * w + 21) * 4]);
        assert!((100..156).contains(&at(&f, w, 40, 40)));
    }

    #[test]
    fn mosaic_makes_uniform_cells() {
        let (w, h) = (100, 80);
        let mut f = stripes(w, h);
        // 預覽畫面是原影片的一半：格子大小也減半（原影片 12px → 6px）
        apply(&mut f, w, h, &Ann { x: 40.0, y: 40.0, w: 80.0, h: 80.0, ..ann(AnnKind::Mosaic, None, false) }, 200.0, 160.0);
        let cell = at(&f, w, 20, 20);
        assert!((cell as i32 - 128).abs() < 50, "{cell}");
        assert_eq!(at(&f, w, 21, 21), cell);
        assert_eq!(at(&f, w, 5, 5), 255);
    }

    #[test]
    fn box_blur_keeps_flat_color() {
        let mut v = vec![200u8; 16 * 9 * 4];
        box_blur(&mut v, 16, 9, 3);
        assert!(v.iter().all(|&c| c == 200));
        let mut one = vec![10u8; 4];
        box_blur(&mut one, 1, 1, 5);
        assert_eq!(one, vec![10; 4]);
    }
}
