//! 長截圖（捲動截圖）：一邊捲動一邊截取同一個範圍，找出前後兩張重疊的地方接起來。
//! 比對以「每一列的雜湊」進行，只看前後兩張之間有變動的欄：
//! - 右邊的捲軸（每次位置不同）不算；固定不動的側欄、整欄同色的邊也不算
//! - 整列同色的列不拿來判斷
//! - 頂端固定不動的列（瀏覽器的分頁列與網址列、網頁固定的標題列）不拿來比對，只出現一次
//! - 底部固定不動的列（固定的頁尾）只在最後接一次

/// 最高多少像素（太長的圖很難用，也很佔記憶體）
pub const MAX_HEIGHT: u32 = 30_000;

/// 右邊不比對的寬度（捲軸）：放大 150%～200% 時捲軸約 25～35 像素，點選視窗時還包含視窗的邊框，
/// 所以留寬一點；右邊少比對一點內容不影響找重疊
fn scrollbar_skip(w: usize) -> usize {
    (w / 10).clamp(32, 96).min(w / 2)
}

/// 兩張之間有變動的欄（不含右邊捲軸的寬度）
fn moving_columns(a: &[u8], b: &[u8], w: usize, h: usize) -> Vec<usize> {
    let used = w - scrollbar_skip(w);
    let mut moving = vec![false; used];
    for y in 0..h {
        let (ra, rb) = (&a[y * w * 4..][..used * 4], &b[y * w * 4..][..used * 4]);
        if ra == rb {
            continue;
        }
        for (x, m) in moving.iter_mut().enumerate() {
            if !*m && ra[x * 4..x * 4 + 3] != rb[x * 4..x * 4 + 3] {
                *m = true;
            }
        }
    }
    moving.iter().enumerate().filter(|(_, m)| **m).map(|(x, _)| x).collect()
}

/// 每一列的雜湊（只算 cols 這些欄）；這些欄整列同色時為 None
fn row_hashes(rgba: &[u8], w: usize, h: usize, cols: &[usize]) -> Vec<Option<u64>> {
    (0..h)
        .map(|y| {
            let row = &rgba[y * w * 4..][..w * 4];
            let px = |x: usize| &row[x * 4..x * 4 + 3];
            let first = px(*cols.first()?);
            if cols.iter().all(|&x| px(x) == first) {
                return None;
            }
            // FNV-1a
            let mut v: u64 = 0xcbf2_9ce4_8422_2325;
            for &x in cols {
                for b in px(x) {
                    v ^= *b as u64;
                    v = v.wrapping_mul(0x100_0000_01b3);
                }
            }
            Some(v)
        })
        .collect()
}

/// 底部有幾列和上一張完全一樣（固定的頁尾）；最多算到 1/3 高
fn footer(prev: &[Option<u64>], next: &[Option<u64>]) -> usize {
    let h = prev.len().min(next.len());
    (0..h / 3).take_while(|&i| prev[h - 1 - i] == next[h - 1 - i]).count()
}

/// 頂端有幾列和上一張完全一樣（固定的標題列、工具列）；最多算到一半
fn header(prev: &[Option<u64>], next: &[Option<u64>], body: usize) -> usize {
    (0..body / 2).take_while(|&i| prev[i] == next[i]).count()
}

/// 下一張比上一張往下捲了幾列（只比對 top..body 列）；沒有捲動時 Some(0)，對不起來時 None
fn find_shift(prev: &[Option<u64>], next: &[Option<u64>], top: usize, body: usize, hint: Option<usize>) -> Option<usize> {
    if prev[..body] == next[..body] {
        return Some(0);
    }
    let span = body.saturating_sub(top);
    let min_overlap = (span / 6).max(8);
    let mut best: Option<(usize, f64, usize)> = None;
    // 也試 0：只有一小塊變了（滑鼠移過去的按鈕變色、游標閃爍）而內容沒有捲動
    for s in 0..span.saturating_sub(min_overlap) {
        let (mut seen, mut same) = (0usize, 0usize);
        for r in top..body - s {
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
        // 比對得一樣好時，選沒捲動或和上次捲動距離接近的
        let dist = if s == 0 { 0 } else { hint.map(|h| h.abs_diff(s)).unwrap_or(s) };
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
    /// 上一張接上去的畫面
    last: Vec<u8>,
    footer: Option<usize>,
    last_shift: Option<usize>,
}

/// 加進一張的結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// 往下接了幾列
    Added(usize),
    /// 和上一張一樣（沒有捲動，或捲到底了）
    Same,
    /// 對不起來（畫面變了、捲太多）
    Lost,
    /// 已經到最高的高度
    Full,
}

impl Stitcher {
    pub fn new(first: Vec<u8>, w: u32, h: u32) -> Stitcher {
        Stitcher { w, h, out: first.clone(), out_h: h, last: first, footer: None, last_shift: None }
    }

    pub fn add(&mut self, next: Vec<u8>) -> Step {
        let (w, h) = (self.w as usize, self.h as usize);
        if next.len() != w * h * 4 {
            return Step::Lost;
        }
        // 只比對有變動的欄：固定的側欄、整欄同色的邊不影響比對
        let cols = moving_columns(&self.last, &next, w, h);
        if cols.is_empty() {
            return Step::Same;
        }
        let prev = row_hashes(&self.last, w, h, &cols);
        let hashes = row_hashes(&next, w, h, &cols);
        // 固定的頁尾：第一次捲動時決定，先從接好的內容拿掉，最後再接一次
        let f = match self.footer {
            Some(f) => f,
            None => {
                if prev == hashes {
                    return Step::Same;
                }
                let f = footer(&prev, &hashes);
                self.out.truncate((self.out_h as usize - f) * w * 4);
                self.out_h -= f as u32;
                self.footer = Some(f);
                f
            }
        };
        let body = h - f;
        let top = header(&prev, &hashes, body);
        let step = match find_shift(&prev, &hashes, top, body, self.last_shift) {
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

    /// 有沒有接上任何一段（false = 只有第一張）
    pub fn grew(&self) -> bool {
        self.last_shift.is_some()
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

    /// 像瀏覽器視窗的畫面：上面 head 列是固定的工具列、左邊 side 欄是固定的側欄（都有內容，每列不同），
    /// 其餘是長頁從 top 開始的內容，右邊有會移動的捲軸
    fn window_view(p: &[u8], w: u32, top: u32, h: u32, head: u32, side: u32) -> Vec<u8> {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let px = if y < head {
                    [((x * 3 + y * 11) % 200) as u8 + 20, 60, 90, 255]
                } else if x < side {
                    [30, ((x * 5 + y * 17) % 180) as u8 + 40, 50, 255]
                } else {
                    let j = (((top + y - head) * w + x) * 4) as usize;
                    [p[j], p[j + 1], p[j + 2], 255]
                };
                v[i..i + 4].copy_from_slice(&px);
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
    fn stitches_window_with_fixed_toolbar_and_sidebar() {
        // 點一下選整個瀏覽器視窗：上面的分頁列與網址列、左邊的側欄都不會動
        let (w, h, head, side, long_h) = (240u32, 160u32, 30u32, 40u32, 700u32);
        let long = page(w, long_h);
        let max_top = long_h - (h - head);
        let mut s = Stitcher::new(window_view(&long, w, 0, h, head, side), w, h);
        let mut top = 0;
        let mut steps = vec![];
        while steps.len() < 30 {
            top = (top + 40).min(max_top);
            steps.push(s.add(window_view(&long, w, top, h, head, side)));
            if steps.last() == Some(&Step::Same) {
                break;
            }
        }
        assert_eq!(steps[0], Step::Added(40));
        assert_eq!(steps.last(), Some(&Step::Same));
        assert!(!steps.contains(&Step::Lost), "{steps:?}");
        assert!(s.grew());
        let (img, iw, ih) = s.finish();
        // 工具列只出現一次，下面是完整的長頁
        assert_eq!((iw, ih), (w, head + long_h));
        for y in (head..ih).step_by(5) {
            let a = &img[(y * w + side) as usize * 4..][..(w - side - 40) as usize * 4];
            let b = &long[((y - head) * w + side) as usize * 4..][..(w - side - 40) as usize * 4];
            assert_eq!(a, b, "第 {y} 列");
        }
    }

    #[test]
    fn hover_change_is_not_a_scroll() {
        // 滑鼠停在按鈕上，按鈕變色但內容沒有捲動：不能當成捲動接上去
        let (w, h) = (200u32, 120u32);
        let long = page(w, 600);
        let first = view(&long, w, 0, h, 0);
        let mut s = Stitcher::new(first.clone(), w, h);
        let mut hover = first;
        for y in 50..65 {
            for x in 60..120 {
                let i = ((y * w + x) * 4) as usize;
                hover[i..i + 4].copy_from_slice(&[0, 120, 215, 255]);
            }
        }
        assert_eq!(s.add(hover), Step::Same);
        assert!(!s.grew());
        // 之後真的捲動了照樣接得上
        assert_eq!(s.add(view(&long, w, 40, h, 0)), Step::Added(40));
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
