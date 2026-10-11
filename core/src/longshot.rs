//! 長截圖（捲動截圖）：一邊捲動一邊擷取同一個範圍，找出前後兩張重疊的地方接起來。
//! 比對以「格子」進行：每一列切成幾段（格子），每格只記幾組平均亮度（容許一點點差異），只看前後兩張之間有變動的欄：
//! - 右邊的捲軸（每次位置不同）不算；固定不動的側欄、整欄同色的邊也不算
//! - 整格同色的格子不拿來判斷
//! - 每一段各自比對，大部分對得上的段才算數：不會跟著捲動、但內容會變的部分（側邊目錄標出目前的段落、
//!   輪播廣告）只影響自己那幾段；捲動後才出現的變化（標題列加陰影、滑鼠停的地方反白）只扣一點分數
//! - 頂端固定不動的列（瀏覽器的分頁列與網址列、網頁固定的標題列）不拿來比對，只出現一次
//! - 底部固定不動的列（固定的頁尾）只在最後接一次

/// 最高多少像素（太長的圖很難用，也很佔記憶體）
pub const MAX_HEIGHT: u32 = 30_000;

/// 右邊不比對的寬度（捲軸）：放大 150%～200% 時捲軸約 25～35 像素，點選視窗時還包含視窗的邊框，
/// 所以留寬一點；右邊少比對一點內容不影響找重疊
fn scrollbar_skip(w: usize) -> usize {
    (w / 10).clamp(32, 96).min(w / 2)
}

/// 兩張之間有變動的欄（不含右邊捲軸的寬度），與有變動的列數
fn moving_columns(a: &[u8], b: &[u8], w: usize, h: usize) -> (Vec<usize>, usize) {
    let used = w - scrollbar_skip(w);
    let mut moving = vec![false; used];
    let mut rows = 0;
    for y in 0..h {
        let (ra, rb) = (&a[y * w * 4..][..used * 4], &b[y * w * 4..][..used * 4]);
        if ra == rb {
            continue;
        }
        rows += 1;
        for (x, m) in moving.iter_mut().enumerate() {
            if !*m && ra[x * 4..x * 4 + 3] != rb[x * 4..x * 4 + 3] {
                *m = true;
            }
        }
    }
    (moving.iter().enumerate().filter(|(_, m)| **m).map(|(x, _)| x).collect(), rows)
}

/// 每一列切成幾段（每段約 48 欄以上，最多 12 段）
fn strips(cols: &[usize]) -> Vec<&[usize]> {
    let n = (cols.len() / 48).clamp(1, 12);
    cols.chunks(cols.len().div_ceil(n).max(1)).collect()
}

/// 每一格的摘要：這一格的欄平均分成幾組，每組的平均亮度
const BINS: usize = 16;

/// 兩格的每組亮度都差不到這麼多就算一樣：同一張圖、漸層重新繪製時常有 ±1～2 的差異，
/// 文字的反鋸齒也可能有一點不同；不同的內容差很多
const TOL: u8 = 4;

/// 亮度差不到這麼多的格子算整格同色（不拿來判斷）
const FLAT: u8 = 12;

/// 每一列每一段（格子）的摘要；整格（幾乎）同色時為 None
struct Grid {
    strips: usize,
    cells: Vec<Option<[u8; BINS]>>,
}

/// 兩格是否一樣（都同色也算一樣）
fn same_cell(a: &Option<[u8; BINS]>, b: &Option<[u8; BINS]>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= TOL),
        _ => false,
    }
}

fn same_row(a: &[Option<[u8; BINS]>], b: &[Option<[u8; BINS]>]) -> bool {
    a.iter().zip(b).all(|(a, b)| same_cell(a, b))
}

impl Grid {
    fn new(rgba: &[u8], w: usize, h: usize, strips: &[&[usize]]) -> Grid {
        let mut cells = Vec::with_capacity(h * strips.len());
        let mut lum = Vec::new();
        for y in 0..h {
            let row = &rgba[y * w * 4..][..w * 4];
            for cols in strips {
                lum.clear();
                lum.extend(cols.iter().map(|&x| ((row[x * 4] as u32 * 77 + row[x * 4 + 1] as u32 * 150 + row[x * 4 + 2] as u32 * 29) >> 8) as u8));
                let (lo, hi) = lum.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
                if hi - lo < FLAT {
                    cells.push(None);
                    continue;
                }
                let mut sig = [0u8; BINS];
                let n = lum.len();
                for (i, s) in sig.iter_mut().enumerate() {
                    let part = &lum[i * n / BINS..((i + 1) * n / BINS).max(i * n / BINS + 1).min(n)];
                    *s = (part.iter().map(|&v| v as u32).sum::<u32>() / part.len() as u32) as u8;
                }
                cells.push(Some(sig));
            }
        }
        Grid { strips: strips.len(), cells }
    }

    fn row(&self, y: usize) -> &[Option<[u8; BINS]>] {
        &self.cells[y * self.strips..][..self.strips]
    }

    fn rows(&self) -> usize {
        self.cells.len() / self.strips.max(1)
    }

    /// 0..body 列都一樣
    fn same_as(&self, other: &Grid, body: usize) -> bool {
        (0..body).all(|r| same_row(self.row(r), other.row(r)))
    }
}

/// 底部有幾列和上一張完全一樣（固定的頁尾）；最多算到 1/3 高
fn footer(prev: &Grid, next: &Grid) -> usize {
    let h = prev.rows().min(next.rows());
    (0..h / 3).take_while(|&i| same_row(prev.row(h - 1 - i), next.row(h - 1 - i))).count()
}

/// 頂端有幾列和上一張完全一樣（固定的標題列、工具列）；最多算到一半
fn header(prev: &Grid, next: &Grid, body: usize) -> usize {
    (0..body / 2).take_while(|&i| same_row(prev.row(i), next.row(i))).count()
}

/// 至少要有這麼多（有內容的格子裡）對得上，才算找到捲動的距離
const MIN_SCORE: f64 = 0.35;

/// 一段裡至少要有這麼多格對得上，這一段才算數：滑鼠停的地方反白、標題列加陰影會讓一部分對不上；
/// 錯開的位置通常只有一兩成對得上
const STRIP_MIN: f64 = 0.35;

/// 往下捲 s 列時對得上的程度（0～1）：只算對得上的段（見 STRIP_MIN）
fn score(prev: &Grid, next: &Grid, top: usize, body: usize, s: usize) -> f64 {
    let n = prev.strips;
    let (mut seen, mut same) = (vec![0usize; n], vec![0usize; n]);
    for r in top..body - s {
        for (k, (a, b)) in next.row(r).iter().zip(prev.row(r + s)).enumerate() {
            if a.is_some() {
                seen[k] += 1;
                if same_cell(a, b) {
                    same[k] += 1;
                }
            }
        }
    }
    let valid = |k: &usize| seen[*k] >= 8;
    let total: usize = (0..n).filter(valid).map(|k| seen[k]).sum();
    if total < 8 {
        return 0.0;
    }
    let agree: usize = (0..n).filter(valid).filter(|&k| same[k] as f64 >= seen[k] as f64 * STRIP_MIN).map(|k| same[k]).sum();
    agree as f64 / total as f64
}

/// 下一張比上一張往下捲了幾列（只比對 top..body 列）；沒有捲動時 Some(0)，找不到時 None
fn find_shift(prev: &Grid, next: &Grid, top: usize, body: usize, hint: Option<usize>) -> Option<usize> {
    if prev.same_as(next, body) {
        return Some(0);
    }
    let span = body.saturating_sub(top);
    let min_overlap = (span / 6).max(8);
    let mut best: Option<(usize, f64, usize)> = None;
    for s in 1..span.saturating_sub(min_overlap) {
        let r = score(prev, next, top, body, s);
        if r < MIN_SCORE {
            continue;
        }
        // 對得差不多好時，選和上次捲動距離接近的（第一次選捲得少的）
        let dist = hint.map(|h| h.abs_diff(s)).unwrap_or(s);
        let better = match best {
            None => true,
            Some((_, br, bd)) => r > br + 0.01 || ((r - br).abs() <= 0.01 && dist < bd),
        };
        if better {
            best = Some((s, r, dist));
        }
    }
    best.map(|b| b.0)
}

/// 每一段的大小：夠大才會直接向系統要記憶體、放掉時馬上還給系統（小塊會留在程式裡，接著配剪貼簿的記憶體時兩份疊在一起）
const PART_BYTES: usize = 8 << 20;

/// 接好的長圖：分段存放（每段是完整的幾列）。長圖可能上百 MB，
/// 一整塊連續的記憶體每次變大都要整個搬一次（搬的時候新舊兩份同時佔著）；分段就不用
pub struct LongImage {
    pub w: u32,
    pub h: u32,
    parts: Vec<Vec<u8>>,
}

impl LongImage {
    /// 一整塊 RGBA 當成一段
    pub fn from_rgba(w: u32, h: u32, rgba: Vec<u8>) -> LongImage {
        LongImage { w, h, parts: vec![rgba] }
    }

    /// 由上而下的每一列（RGBA）
    pub fn rows(&self) -> impl Iterator<Item = &[u8]> {
        let row = self.w as usize * 4;
        self.parts.iter().flat_map(move |p| p.chunks_exact(row))
    }

    /// 最上面 n 列（連續的 RGBA）
    pub fn top(&self, n: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(n as usize * self.w as usize * 4);
        for r in self.rows().take(n as usize) {
            v.extend_from_slice(r);
        }
        v
    }

    /// 整張接成一塊（測試用）
    pub fn to_vec(&self) -> Vec<u8> {
        self.parts.concat()
    }

    /// 壓成 PNG 寫進 out：一列一列寫，不用先接成一整塊（壓縮方式與一般截圖相同）
    pub fn write_png<W: std::io::Write>(&self, out: W) -> Result<(), String> {
        use std::io::Write;
        let mut enc = png::Encoder::new(out, self.w, self.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        enc.set_filter(png::Filter::Adaptive);
        let mut wr = enc.write_header().map_err(|e| e.to_string())?;
        let mut s = wr.stream_writer().map_err(|e| e.to_string())?;
        for r in self.rows() {
            s.write_all(r).map_err(|e| e.to_string())?;
        }
        s.finish().map_err(|e| e.to_string())?;
        wr.finish().map_err(|e| e.to_string())
    }
}

/// 把幾列接到最後一段；放不下時開新的一段（每段約 PART_BYTES，事先配好不再變大）
fn append_rows(parts: &mut Vec<Vec<u8>>, mut rows: &[u8], row: usize) {
    while !rows.is_empty() {
        if parts.last().is_none_or(|p| p.capacity() - p.len() < row) {
            parts.push(Vec::with_capacity((PART_BYTES / row).max(1) * row));
        }
        let Some(p) = parts.last_mut() else { return };
        let n = ((p.capacity() - p.len()) / row * row).min(rows.len());
        p.extend_from_slice(&rows[..n]);
        rows = &rows[n..];
    }
}

/// 一張一張加進來接成長圖
pub struct Stitcher {
    w: u32,
    h: u32,
    /// 接好的內容（不含最下面先留著的列），一次捲動一段
    parts: Vec<Vec<u8>>,
    out_h: u32,
    /// 上一張接上去的畫面
    last: Vec<u8>,
    /// 第一次捲動時決定：(固定的頁尾, 頁尾上面先留著的列)。
    /// 每次只接最下面一段上面的新內容，最後一張才接到底：貼在底部的東西（右下角的聊天按鈕、
    /// 浮動的「回到頂端」）不會每接一段就出現一次
    hold: Option<(usize, usize)>,
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
        Stitcher { w, h, parts: vec![first.clone()], out_h: h, last: first, hold: None, last_shift: None }
    }

    pub fn add(&mut self, next: Vec<u8>) -> Step {
        let (w, h) = (self.w as usize, self.h as usize);
        if next.len() != w * h * 4 {
            return Step::Lost;
        }
        // 只比對有變動的欄：固定的側欄、整欄同色的邊不影響比對
        let (cols, changed) = moving_columns(&self.last, &next, w, h);
        if cols.is_empty() {
            return Step::Same;
        }
        let strips = strips(&cols);
        let prev = Grid::new(&self.last, w, h, &strips);
        let cells = Grid::new(&next, w, h, &strips);
        // 固定的頁尾與先留著的列：第一次捲動時決定，先從接好的內容拿掉，最後再從最後一張接上
        let (f, e) = match self.hold {
            Some(fe) => fe,
            None => {
                if prev.same_as(&cells, h) {
                    return Step::Same;
                }
                let f = footer(&prev, &cells);
                let e = (h - f) / 8;
                if let Some(p) = self.parts.last_mut() {
                    p.truncate(p.len().saturating_sub((f + e) * w * 4));
                }
                self.out_h -= (f + e) as u32;
                self.hold = Some((f, e));
                (f, e)
            }
        };
        let body = h - f;
        let top = header(&prev, &cells, body);
        let step = match find_shift(&prev, &cells, top, body, self.last_shift) {
            Some(0) => Step::Same,
            // 只有一小部分變了（滑鼠移過去的按鈕變色、輪播廣告換圖）而內容沒有捲動：捲到底了
            None if changed * 10 <= h * 3 => Step::Same,
            None => Step::Lost,
            Some(s) => {
                if self.out_h as usize + s > MAX_HEIGHT as usize {
                    return Step::Full;
                }
                // 對得上時 s 不超過 body - top - 重疊的最少列數（≥ body / 6），所以 body - e - s ≥ 0
                append_rows(&mut self.parts, &next[(body - e - s) * w * 4..(body - e) * w * 4], w * 4);
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

    /// 接好的長圖（最後接上固定的頁尾）
    pub fn finish(mut self) -> LongImage {
        let (f, e) = self.hold.unwrap_or((0, 0));
        let (w, h) = (self.w as usize, self.h as usize);
        append_rows(&mut self.parts, &self.last[(h - f - e) * w * 4..], w * 4);
        LongImage { w: self.w, h: self.out_h + (f + e) as u32, parts: self.parts }
    }

    /// 目前的高度
    pub fn height(&self) -> u32 {
        self.out_h + self.hold.map_or(0, |(f, e)| f + e) as u32
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
        let long_img = s.finish();
        let (img, iw, ih) = (long_img.to_vec(), long_img.w, long_img.h);
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
        let long_img = s.finish();
        let (img, iw, ih) = (long_img.to_vec(), long_img.w, long_img.h);
        // 工具列只出現一次，下面是完整的長頁
        assert_eq!((iw, ih), (w, head + long_h));
        for y in (head..ih).step_by(5) {
            let a = &img[(y * w + side) as usize * 4..][..(w - side - 40) as usize * 4];
            let b = &long[((y - head) * w + side) as usize * 4..][..(w - side - 40) as usize * 4];
            assert_eq!(a, b, "第 {y} 列");
        }
    }

    /// 像真的網頁：固定的側欄會標出目前的段落、捲動後標題列下面多一道陰影、滑鼠停的地方反白、
    /// 每次重新繪製顏色有 ±1 的差異、右下角有固定的按鈕
    fn live_view(p: &[u8], w: u32, top: u32, h: u32, frame: u32) -> Vec<u8> {
        let (head, side) = (30u32, 40u32);
        let mut v = window_view(p, w, top, h, head, side);
        let mut px = |x: u32, y: u32, f: &dyn Fn([u8; 4]) -> [u8; 4]| {
            let i = ((y * w + x) * 4) as usize;
            let c = f([v[i], v[i + 1], v[i + 2], v[i + 3]]);
            v[i..i + 4].copy_from_slice(&c);
        };
        for y in head..h {
            for x in side..w - 12 {
                let n = ((x * 31 + y * 17 + frame * 7) % 3) as i16 - 1;
                px(x, y, &|c| [(c[0] as i16 + n).clamp(0, 255) as u8, (c[1] as i16 + n).clamp(0, 255) as u8, (c[2] as i16 + n).clamp(0, 255) as u8, 255]);
            }
        }
        let mark = 40 + (top / 40 % 5) * 20;
        for y in mark..mark + 12 {
            for x in 4..side - 4 {
                px(x, y, &|_| [200, 220, 255, 255]);
            }
        }
        if top > 0 {
            for y in head..head + 4 {
                for x in side..w - 12 {
                    px(x, y, &|c| [c[0] / 2, c[1] / 2, c[2] / 2, 255]);
                }
            }
        }
        if frame > 0 {
            for y in 50..62 {
                for x in side..w - 12 {
                    px(x, y, &|c| [c[0].saturating_sub(12), c[1].saturating_sub(12), c[2].saturating_sub(12), 255]);
                }
            }
        }
        for y in h - 18..h - 6 {
            for x in w - 60..w - 44 {
                px(x, y, &|_| [37, 99, 235, 255]);
            }
        }
        v
    }

    #[test]
    fn stitches_live_page_with_changing_parts() {
        let (w, h, head, side, long_h) = (240u32, 160u32, 30u32, 40u32, 700u32);
        let long = page(w, long_h);
        let max_top = long_h - (h - head);
        let mut s = Stitcher::new(live_view(&long, w, 0, h, 0), w, h);
        let (mut top, mut steps, mut want) = (0, vec![], vec![]);
        for frame in 1..40 {
            let next = (top + 40).min(max_top);
            want.push(if next == top { Step::Same } else { Step::Added((next - top) as usize) });
            top = next;
            steps.push(s.add(live_view(&long, w, top, h, frame)));
            if steps.last() == Some(&Step::Same) {
                break;
            }
        }
        assert_eq!(steps, want);
        let long_img = s.finish();
        let (img, ih) = (long_img.to_vec(), long_img.h);
        assert_eq!(ih, head + long_h);
        // 內容和原本的長頁一樣（容許 ±1）；右下角的按鈕只在最下面出現一次
        let row = |y: u32| &img[(y * w * 4) as usize..][..(w * 4) as usize];
        for y in (head..ih - 20).step_by(3) {
            let (a, b) = (row(y), &long[((y - head) * w * 4) as usize..][..(w * 4) as usize]);
            for x in side as usize..(w - 60) as usize {
                for c in 0..3 {
                    assert!(a[x * 4 + c].abs_diff(b[x * 4 + c]) <= 1, "第 {y} 列第 {x} 欄");
                }
            }
        }
        let bubble = (0..ih).filter(|&y| row(y)[((w - 52) * 4) as usize..][..3] == [37, 99, 235]).count();
        assert_eq!(bubble, 12);
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
    fn rows_go_into_big_parts() {
        // 每段事先配好、不再變大；放不下的列開新的一段，列不會被切開
        let row = 1000 * 4;
        let mut parts = vec![vec![1u8; row * 3]];
        append_rows(&mut parts, &vec![2u8; row * 5], row);
        let per = PART_BYTES / row * row;
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].capacity(), per);
        assert_eq!(parts[1].len(), row * 5);
        let big = vec![3u8; per];
        append_rows(&mut parts, &big, row);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1].len(), per);
        assert!(parts.iter().all(|p| p.len() % row == 0 && p.len() <= per.max(row * 3)));
        assert_eq!(parts.iter().map(Vec::len).sum::<usize>(), row * 8 + per);
    }

    #[test]
    fn long_image_writes_png_row_by_row() {
        // 分好幾段的長圖：寫出的 PNG 讀回來和接成一整塊的一樣
        let (w, h) = (64u32, 90u32);
        let long = page(w, h);
        let row = (w * 4) as usize;
        let img = LongImage { w, h, parts: vec![long[..row * 40].to_vec(), long[row * 40..row * 41].to_vec(), long[row * 41..].to_vec()] };
        assert_eq!(img.rows().count(), h as usize);
        assert_eq!(img.top(3), long[..row * 3]);
        assert_eq!(img.to_vec(), long);
        let mut png = Vec::new();
        img.write_png(&mut png).unwrap();
        let back = tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert_eq!((back.width(), back.height()), (w, h));
        assert_eq!(back.data(), &long[..]);
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
