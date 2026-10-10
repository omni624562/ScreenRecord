//! 長截圖（捲動截圖）：一邊捲動一邊截取同一個範圍，找出前後兩張重疊的地方接起來。
//! 比對以「每一列的雜湊」進行：右邊的捲軸（每次位置不同）不算；整列同色的列不拿來判斷；
//! 底部固定不動的列（固定的頁尾）只在最後接一次。

/// 最高多少像素（太長的圖很難用，也很佔記憶體）
pub const MAX_HEIGHT: u32 = 30_000;

/// 每一列的雜湊（不含右邊捲軸的寬度）；整列同色時為 None
fn row_hashes(rgba: &[u8], w: u32, h: u32) -> Vec<Option<u64>> {
    let skip = (w / 30).clamp(16, 40).min(w / 2);
    let used = (w - skip) as usize * 4;
    (0..h as usize)
        .map(|y| {
            let row = &rgba[y * w as usize * 4..][..used];
            let first = &row[..4];
            if row.as_chunks::<4>().0.iter().all(|p| p[..3] == first[..3]) {
                return None;
            }
            // FNV-1a
            let mut x: u64 = 0xcbf2_9ce4_8422_2325;
            for b in row {
                x ^= *b as u64;
                x = x.wrapping_mul(0x100_0000_01b3);
            }
            Some(x)
        })
        .collect()
}

/// 底部有幾列和上一張完全一樣（固定的頁尾）；最多算到 1/3 高
fn footer(prev: &[Option<u64>], next: &[Option<u64>]) -> usize {
    let h = prev.len().min(next.len());
    (0..h / 3).take_while(|&i| prev[h - 1 - i] == next[h - 1 - i]).count()
}

/// 下一張比上一張往下捲了幾列（只比對 0..body 列）；完全一樣時 Some(0)，對不起來時 None
fn find_shift(prev: &[Option<u64>], next: &[Option<u64>], body: usize, hint: Option<usize>) -> Option<usize> {
    if prev[..body] == next[..body] {
        return Some(0);
    }
    let min_overlap = (body / 6).max(8);
    let mut best: Option<(usize, f64, usize)> = None;
    for s in 1..body.saturating_sub(min_overlap) {
        let (mut seen, mut same) = (0usize, 0usize);
        for r in 0..body - s {
            if let (Some(a), b) = (next[r], prev[r + s]) {
                seen += 1;
                if Some(a) == b {
                    same += 1;
                }
            }
        }
        if seen < 8 {
            continue;
        }
        let ratio = same as f64 / seen as f64;
        if ratio < 0.9 {
            continue;
        }
        // 比對得一樣好時，選和上次捲動距離接近的
        let dist = hint.map(|h| h.abs_diff(s)).unwrap_or(s);
        let better = match best {
            None => true,
            Some((_, r, d)) => ratio > r + 1e-9 || ((ratio - r).abs() <= 1e-9 && dist < d),
        };
        if better {
            best = Some((s, ratio, dist));
        }
    }
    best.map(|b| b.0)
}

/// 一張一張加進來接成長圖
pub struct Stitcher {
    w: u32,
    h: u32,
    /// 接好的內容（不含固定的頁尾）
    out: Vec<u8>,
    out_h: u32,
    prev: Vec<Option<u64>>,
    last: Vec<u8>,
    footer: Option<usize>,
    last_shift: Option<usize>,
}

/// 加進一張的結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// 往下接了幾列
    Added(usize),
    /// 和上一張一樣（捲到底了）
    Same,
    /// 對不起來（畫面變了、捲太多）
    Lost,
    /// 已經到最高的高度
    Full,
}

impl Stitcher {
    pub fn new(first: Vec<u8>, w: u32, h: u32) -> Stitcher {
        let prev = row_hashes(&first, w, h);
        Stitcher { w, h, out: first.clone(), out_h: h, prev, last: first, footer: None, last_shift: None }
    }

    pub fn add(&mut self, next: Vec<u8>) -> Step {
        let (w, h) = (self.w as usize, self.h as usize);
        if next.len() != w * h * 4 {
            return Step::Lost;
        }
        let hashes = row_hashes(&next, self.w, self.h);
        // 固定的頁尾：第一次捲動時決定，先從接好的內容拿掉，最後再接一次
        let f = match self.footer {
            Some(f) => f,
            None => {
                if self.prev == hashes {
                    return Step::Same;
                }
                let f = footer(&self.prev, &hashes);
                self.out.truncate((self.out_h as usize - f) * w * 4);
                self.out_h -= f as u32;
                self.footer = Some(f);
                f
            }
        };
        let body = h - f;
        let step = match find_shift(&self.prev, &hashes, body, self.last_shift) {
            Some(0) => Step::Same,
            None => Step::Lost,
            Some(s) => {
                if self.out_h as usize + s > MAX_HEIGHT as usize {
                    return Step::Full;
                }
                self.out.extend_from_slice(&next[(body - s) * w * 4..body * w * 4]);
                self.out_h += s as u32;
                self.last_shift = Some(s);
                Step::Added(s)
            }
        };
        if matches!(step, Step::Added(_)) {
            self.prev = hashes;
            self.last = next;
        }
        step
    }

    /// 接好的長圖（RGBA）與寬高
    pub fn finish(mut self) -> (Vec<u8>, u32, u32) {
        let f = self.footer.unwrap_or(0);
        let (w, h) = (self.w as usize, self.h as usize);
        self.out.extend_from_slice(&self.last[(h - f) * w * 4..]);
        (self.out, self.w, self.out_h + f as u32)
    }

    /// 目前的高度
    pub fn height(&self) -> u32 {
        self.out_h + self.footer.unwrap_or(0) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一張很長的「網頁」：每一列有不同的花紋，偶爾有空白列；右邊有會移動的捲軸
    fn page(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            if y % 37 == 5 {
                continue; // 空白列
            }
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let c = ((x * 7 + y * 13 + (y * y) % 251) % 256) as u8;
                v[i..i + 3].copy_from_slice(&[c, c.wrapping_mul(3), 255 - c]);
            }
        }
        v
    }

    /// 從長頁的 top 開始看到的畫面（高 h）；有固定頁尾、捲軸的位置跟著 top 變
    fn view(p: &[u8], w: u32, top: u32, h: u32, foot: u32) -> Vec<u8> {
        let mut v = p[(top * w * 4) as usize..((top + h) * w * 4) as usize].to_vec();
        for y in h - foot..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                v[i..i + 4].copy_from_slice(&[20, 40, (x % 200) as u8, 255]);
            }
        }
        let thumb = top / 10;
        for y in 0..h {
            for x in w - 12..w {
                let i = ((y * w + x) * 4) as usize;
                let c = if y >= thumb && y < thumb + 30 { 90 } else { 230 };
                v[i..i + 4].copy_from_slice(&[c, c, c, 255]);
            }
        }
        v
    }

    #[test]
    fn stitches_scrolled_views_with_fixed_footer() {
        let (w, h, foot) = (200u32, 120u32, 15u32);
        let long = page(w, 600);
        let mut s = Stitcher::new(view(&long, w, 0, h, foot), w, h);
        let mut top = 0;
        let mut steps = vec![];
        for d in [40, 40, 55, 30, 40, 40, 40, 40, 40, 40, 40, 40, 40] {
            top = (top + d).min(600 - h);
            steps.push(s.add(view(&long, w, top, h, foot)));
        }
        // 捲到底後畫面不變
        assert_eq!(steps.last(), Some(&Step::Same));
        assert_eq!(steps[0], Step::Added(40));
        assert_eq!(steps[2], Step::Added(55));
        let (img, iw, ih) = s.finish();
        assert_eq!((iw, ih), (w, 600 - h + h));
        // 內容（不含捲軸）和原本的長頁一樣，最下面是頁尾
        let body = (600 - foot) as usize;
        for y in (0..body).step_by(7) {
            let a = &img[y * w as usize * 4..][..(w as usize - 40) * 4];
            let b = &long[y * w as usize * 4..][..(w as usize - 40) * 4];
            assert_eq!(a, b, "第 {y} 列");
        }
        assert_eq!(&img[(ih as usize - 1) * w as usize * 4..][..4], &[20, 40, 0, 255]);
    }

    #[test]
    fn unrelated_view_is_lost() {
        let (w, h) = (100u32, 80u32);
        let long = page(w, 400);
        let mut s = Stitcher::new(view(&long, w, 0, h, 0), w, h);
        // 換成完全不同的內容（例如跳到別頁）
        let other: Vec<u8> = view(&page(w, 400), w, 300, h, 0).iter().map(|b| b.wrapping_add(77)).collect();
        assert_eq!(s.add(other), Step::Lost);
        assert_eq!(s.height(), h);
    }
}
