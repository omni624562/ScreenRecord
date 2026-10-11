//! 讀取 QR 碼：從截圖（或框選的範圍）找出 QR 碼並解出內容。

/// 圖裡所有 QR 碼的內容（找不到時是空的）。大圖先縮小再找，小圖放大一點比較認得出來
pub fn decode(rgba: &[u8], w: u32, h: u32) -> Vec<String> {
    let try_at = |k: f64| -> Vec<String> {
        let (sw, sh) = (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1));
        let luma = |x: usize, y: usize| -> u8 {
            let (ox, oy) = (((x as f64 / k) as u32).min(w - 1), ((y as f64 / k) as u32).min(h - 1));
            let i = ((oy * w + ox) * 4) as usize;
            let (r, g, b) = (rgba[i] as u32, rgba[i + 1] as u32, rgba[i + 2] as u32);
            ((r * 299 + g * 587 + b * 114) / 1000) as u8
        };
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(sw, sh, luma);
        img.detect_grids().into_iter().filter_map(|g| g.decode().ok().map(|(_, text)| text)).collect()
    };
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return vec![];
    }
    let big = w.max(h);
    let scales: &[f64] = if big > 2400 {
        &[1600.0 / big as f64, 1.0]
    } else if big < 300 {
        &[2.0, 1.0]
    } else {
        &[1.0, 0.5]
    };
    for &k in scales {
        let mut found = try_at(k);
        if !found.is_empty() {
            found.dedup();
            return found;
        }
    }
    vec![]
}

/// 內容是網址（可以直接開啟）
pub fn is_url(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白底黑碼的 RGBA 圖（每格 scale 像素，四周留白）
    fn qr_image(text: &str, scale: u32) -> (Vec<u8>, u32, u32) {
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let n = code.width() as u32;
        let size = (n + 8) * scale;
        let mut img = vec![255u8; (size * size * 4) as usize];
        for y in 0..n {
            for x in 0..n {
                if code[(x as usize, y as usize)] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (px, py) = ((x + 4) * scale + dx, (y + 4) * scale + dy);
                            let i = ((py * size + px) * 4) as usize;
                            img[i..i + 3].copy_from_slice(&[0, 0, 0]);
                        }
                    }
                }
            }
        }
        (img, size, size)
    }

    #[test]
    fn reads_qr_codes() {
        let (img, w, h) = qr_image("https://example.com/登入?x=1", 6);
        assert_eq!(decode(&img, w, h), vec!["https://example.com/登入?x=1".to_string()]);
        // 很小的碼也讀得到（放大後再找）
        let (img, w, h) = qr_image("LINE ID: test", 2);
        assert_eq!(decode(&img, w, h), vec!["LINE ID: test".to_string()]);
        // 沒有 QR 碼
        assert!(decode(&vec![255u8; 100 * 100 * 4], 100, 100).is_empty());
        assert!(is_url("https://a.tw/x") && !is_url("hello https://a.tw") && !is_url("ftp://x"));
    }
}
