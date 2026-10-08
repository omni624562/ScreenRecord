//! 程式圖示（以程式繪製，不需要圖檔；對應 src/icon.ts）：
//! - Idle：深色圓角方塊 + 紅點（與網頁圖示一致）
//! - Recording：紅色方塊 + 白點，一眼看出正在錄影
//! - Paused：琥珀色方塊 + 兩條白槓
//! - Countdown：深色方塊 + 紅色圓環（即將開始）
//!
//! 產生 Windows 圖示格式（BMP 型 ICO 資源），供系統匣與 exe 圖示使用。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconState {
    Idle,
    Recording,
    Paused,
    Countdown,
}

type Rgba = [f64; 4];

fn colors(state: IconState) -> (Rgba, Rgba) {
    match state {
        IconState::Idle | IconState::Countdown => ([43.0, 43.0, 43.0, 255.0], [229.0, 72.0, 77.0, 255.0]),
        IconState::Recording => ([229.0, 72.0, 77.0, 255.0], [255.0, 255.0, 255.0, 255.0]),
        IconState::Paused => ([214.0, 140.0, 18.0, 255.0], [255.0, 255.0, 255.0, 255.0]),
    }
}

/// 與 JS 的 Uint8ClampedArray 相同：四捨五入（.5 取偶數）並限制在 0～255
fn clamp_u8(v: f64) -> u8 {
    v.clamp(0.0, 255.0).round_ties_even() as u8
}

/// 每個像素 4×4 超取樣做反鋸齒；回傳由上而下的 RGBA
pub fn render_icon(size: usize, state: IconState) -> Vec<u8> {
    let (bg, fg) = colors(state);
    let s = size as f64;
    let mut px = vec![0u8; size * size * 4];
    let pad = s * 0.06;
    let radius = s * 0.24;
    let c = s / 2.0;
    let in_round_rect = |x: f64, y: f64| {
        let (l, t, r, b) = (pad, pad, s - pad, s - pad);
        if x < l || x > r || y < t || y > b {
            return false;
        }
        let cx = x.max(l + radius).min(r - radius);
        let cy = y.max(t + radius).min(b - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius.powi(2)
    };
    let in_fg = |x: f64, y: f64| {
        if state == IconState::Paused {
            let (w, h, gap) = (s * 0.13, s * 0.44, s * 0.09);
            let top = c - h / 2.0;
            return y >= top && y <= top + h && ((x >= c - gap - w && x <= c - gap) || (x >= c + gap && x <= c + gap + w));
        }
        let d2 = (x - c).powi(2) + (y - c).powi(2);
        if state == IconState::Countdown {
            return d2 <= (s * 0.26).powi(2) && d2 >= (s * 0.16).powi(2);
        }
        d2 <= (s * if state == IconState::Idle { 0.2 } else { 0.18 }).powi(2)
    };
    const SS: usize = 4;
    for y in 0..size {
        for x in 0..size {
            let (mut nb, mut nf) = (0usize, 0usize);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f64 + (sx as f64 + 0.5) / SS as f64;
                    let fy = y as f64 + (sy as f64 + 0.5) / SS as f64;
                    if in_round_rect(fx, fy) {
                        if in_fg(fx, fy) {
                            nf += 1;
                        } else {
                            nb += 1;
                        }
                    }
                }
            }
            let a = (nb + nf) as f64 / (SS * SS) as f64;
            if a == 0.0 {
                continue;
            }
            let i = (y * size + x) * 4;
            let t = nf as f64 / (nb + nf) as f64;
            for k in 0..3 {
                px[i + k] = clamp_u8(bg[k] * (1.0 - t) + fg[k] * t);
            }
            px[i + 3] = clamp_u8(255.0 * a);
        }
    }
    px
}

/// 單一尺寸的 BMP 型圖示資源（BITMAPINFOHEADER + BGRA 由下而上 + AND 遮罩）
pub fn icon_resource(size: usize, state: IconState) -> Vec<u8> {
    let rgba = render_icon(size, state);
    let mask_stride = size.div_ceil(32) * 4;
    let mut out = vec![0u8; 40 + size * size * 4 + mask_stride * size];
    out[0..4].copy_from_slice(&40u32.to_le_bytes());
    out[4..8].copy_from_slice(&(size as i32).to_le_bytes());
    out[8..12].copy_from_slice(&(size as i32 * 2).to_le_bytes()); // 高度含 AND 遮罩
    out[12..14].copy_from_slice(&1u16.to_le_bytes());
    out[14..16].copy_from_slice(&32u16.to_le_bytes());
    out[20..24].copy_from_slice(&((size * size * 4 + mask_stride * size) as u32).to_le_bytes());
    for y in 0..size {
        for x in 0..size {
            let s = (y * size + x) * 4;
            let d = 40 + ((size - 1 - y) * size + x) * 4;
            out[d] = rgba[s + 2];
            out[d + 1] = rgba[s + 1];
            out[d + 2] = rgba[s];
            out[d + 3] = rgba[s + 3];
        }
    }
    out // AND 遮罩全為 0（透明度由 alpha 決定）
}

/// 多尺寸 .ico 檔（exe 圖示用）
pub fn ico_file(state: IconState, sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|s| icon_resource(*s, state)).collect();
    let mut out = vec![0u8; 6 + 16 * sizes.len()];
    out[2..4].copy_from_slice(&1u16.to_le_bytes());
    out[4..6].copy_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = out.len();
    for (i, (s, img)) in sizes.iter().zip(&images).enumerate() {
        let e = 6 + i * 16;
        out[e] = if *s >= 256 { 0 } else { *s as u8 };
        out[e + 1] = if *s >= 256 { 0 } else { *s as u8 };
        out[e + 4..e + 6].copy_from_slice(&1u16.to_le_bytes());
        out[e + 6..e + 8].copy_from_slice(&32u16.to_le_bytes());
        out[e + 8..e + 12].copy_from_slice(&(img.len() as u32).to_le_bytes());
        out[e + 12..e + 16].copy_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        out.extend(img);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_shapes() {
        let px = render_icon(32, IconState::Idle);
        let at = |x: usize, y: usize| &px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0); // 角落透明
        assert_eq!(at(16, 16), &[229, 72, 77, 255]); // 中心紅點
        assert_eq!(at(16, 5), &[43, 43, 43, 255]); // 深色底
        let rec = render_icon(32, IconState::Recording);
        assert_eq!(&rec[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn ico_layout() {
        let ico = ico_file(IconState::Idle, &[16, 256]);
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 2, 0]);
        let first = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(first, 6 + 32);
        assert_eq!(ico[6 + 16], 0); // 256 記為 0
        assert_eq!(ico.len(), 6 + 32 + icon_resource(16, IconState::Idle).len() + icon_resource(256, IconState::Idle).len());
    }
}
