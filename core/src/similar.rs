//! 找出重複、相似的截圖：每張圖算一個「長相指紋」（縮成 17×16 灰階後，左右相鄰像素誰比較亮，共 256 位元），
//! 指紋差幾位元以內、而且寬高相同的算同一組。
//!
//! 指紋存在 shot_hashes.json（小寫路徑 → 大小、修改時間、指紋）；檔案沒改過就不用重新讀圖。

use crate::paths::mtime_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
struct Entry {
    bytes: u64,
    mtime: f64,
    w: u32,
    h: u32,
    hash: [u64; 4],
    /// 主要顏色（COLOR_* 位元）；舊版沒有，要重算
    #[serde(default)]
    colors: Option<u8>,
}

pub const COLOR_DARK: u8 = 1;
pub const COLOR_LIGHT: u8 = 2;
pub const COLOR_RED: u8 = 4;
pub const COLOR_YELLOW: u8 = 8;
pub const COLOR_GREEN: u8 = 16;
pub const COLOR_BLUE: u8 = 32;
pub const COLOR_PURPLE: u8 = 64;

/// 寬、高、指紋
pub type Print = (u32, u32, [u64; 4]);

/// 讀一張圖算出來的：指紋與主要顏色
type Analyzed = (Print, u8);

pub struct HashStore {
    file: PathBuf,
    map: Mutex<HashMap<String, Entry>>,
    /// 正在算指紋：(做完, 總共)，介面顯示進度
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

/// 靈敏度：指紋最多可以差幾位元（共 256）
pub fn max_diff(level: u8) -> u32 {
    match level {
        0 => 0,  // 完全一樣
        1 => 6,  // 幾乎一樣（游標、時鐘不同）
        _ => 20, // 相似（一小塊內容不同）
    }
}

/// 算一張圖的指紋：縮成 17×16 灰階，每列 16 個「右邊比左邊亮」的位元
pub fn fingerprint(img: &image::DynamicImage) -> [u64; 4] {
    let g = image::imageops::resize(&img.to_luma8(), 17, 16, image::imageops::FilterType::Triangle);
    let mut out = [0u64; 4];
    let mut bit = 0;
    for y in 0..16 {
        for x in 0..16 {
            if g.get_pixel(x + 1, y)[0] > g.get_pixel(x, y)[0] {
                out[bit / 64] |= 1 << (bit % 64);
            }
            bit += 1;
        }
    }
    out
}

/// 主要顏色：整體偏暗、偏亮，以及占一定面積的鮮明顏色（紅、黃、綠、藍、紫）
pub fn colors(img: &image::DynamicImage) -> u8 {
    let small = img.resize_exact(48, 48, image::imageops::FilterType::Triangle).to_rgb8();
    let n = (small.width() * small.height()) as f32;
    let mut luma = 0.0f32;
    let mut hues = [0f32; 5];
    for p in small.pixels() {
        let [r, g, b] = p.0.map(|c| c as f32 / 255.0);
        luma += 0.299 * r + 0.587 * g + 0.114 * b;
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        if sat < 0.45 || max < 0.35 {
            continue;
        }
        let d = max - min;
        let h = if max == r {
            60.0 * (((g - b) / d).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        let bucket = match h {
            h if !(20.0..330.0).contains(&h) => 0,
            h if h < 70.0 => 1,
            h if h < 170.0 => 2,
            h if h < 260.0 => 3,
            _ => 4,
        };
        hues[bucket] += 1.0;
    }
    let luma = luma / n;
    let mut out = 0u8;
    if luma < 0.32 {
        out |= COLOR_DARK;
    } else if luma > 0.75 {
        out |= COLOR_LIGHT;
    }
    for (i, bit) in [COLOR_RED, COLOR_YELLOW, COLOR_GREEN, COLOR_BLUE, COLOR_PURPLE].into_iter().enumerate() {
        if hues[i] / n > 0.06 {
            out |= bit;
        }
    }
    out
}

/// 存的鍵：Windows 不分大小寫，用小寫；其他平台照原樣（檢查檔案還在不在時才找得到）
fn key(p: &str) -> String {
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_string()
    }
}

fn diff(a: &[u64; 4], b: &[u64; 4]) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

/// 依指紋分組：寬高相同且差 max 位元以內的連在一起（A 像 B、B 像 C 就三張一組）。回傳每組的索引（至少兩張）
pub fn group(items: &[(u32, u32, [u64; 4])], max: u32) -> Vec<Vec<usize>> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    // 先依寬高分桶，只比同樣大小的
    let mut buckets: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        buckets.entry((it.0, it.1)).or_default().push(i);
    }
    for idx in buckets.values() {
        for (k, &i) in idx.iter().enumerate() {
            for &j in &idx[k + 1..] {
                if diff(&items[i].2, &items[j].2) <= max {
                    let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                    if a != b {
                        parent[a] = b;
                    }
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    out.sort_by_key(|g| g[0]);
    out
}

impl HashStore {
    pub fn new(file: PathBuf) -> Self {
        let map = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { file, map: Mutex::new(map), done: AtomicUsize::new(0), total: AtomicUsize::new(0) }
    }

    fn save(&self) {
        let data = serde_json::to_vec(&*self.map.lock().unwrap_or_else(|e| e.into_inner())).unwrap_or_default();
        let _ = crate::paths::write_atomic(&self.file, &data);
    }

    /// 算過的全部：路徑鍵 → (寬, 高, 指紋, 主要顏色)
    pub fn snapshot(&self) -> HashMap<String, (Print, u8)> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, e)| (k.clone(), ((e.w, e.h, e.hash), e.colors.unwrap_or(0)))).collect()
    }

    /// 查表用的鍵（和 snapshot() 的鍵一樣）
    pub fn key(path: &str) -> String {
        key(path)
    }

    /// 這些圖的 (寬, 高, 指紋)；沒算過或改過的才讀圖（用幾條執行緒一起算）。讀不到的圖是 None
    pub fn fingerprints(&self, paths: &[String]) -> Vec<Option<Print>> {
        let stamp = |p: &str| std::fs::metadata(p).ok().map(|m| (m.len(), mtime_ms(&m)));
        let stamps: Vec<Option<(u64, f64)>> = paths.iter().map(|p| stamp(p)).collect();
        let mut out: Vec<Option<Print>> = vec![None; paths.len()];
        let mut todo = vec![];
        {
            let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            for (i, p) in paths.iter().enumerate() {
                let Some((bytes, mtime)) = stamps[i] else { continue };
                match map.get(&key(p)) {
                    Some(e) if e.colors.is_some() && e.bytes == bytes && crate::paths::same_mtime(e.mtime, mtime) => out[i] = Some((e.w, e.h, e.hash)),
                    _ => todo.push(i),
                }
            }
        }
        self.total.store(todo.len(), Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        if todo.is_empty() {
            return out;
        }
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<(usize, Option<Analyzed>)>> = Mutex::new(vec![]);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = todo.get(k) else { break };
                    let r = image::open(&paths[i]).ok().map(|img| ((img.width(), img.height(), fingerprint(&img)), colors(&img)));
                    results.lock().unwrap_or_else(|e| e.into_inner()).push((i, r));
                    self.done.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        for (i, r) in results.into_inner().unwrap() {
            if let (Some(((w, h, hash), c)), Some((bytes, mtime))) = (r, stamps[i]) {
                map.insert(key(&paths[i]), Entry { bytes, mtime, w, h, hash, colors: Some(c) });
            }
            out[i] = r.map(|(p, _)| p);
        }
        // 刪掉已經不存在的檔案的紀錄
        map.retain(|p, _| std::path::Path::new(p).exists());
        drop(map);
        self.save();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn screen(seed: u32, dot: Option<(u32, u32)>) -> image::DynamicImage {
        // 像畫面的圖：幾塊不同亮度的區塊
        let mut img = RgbaImage::from_fn(320, 200, |x, y| {
            let v = ((x / 40 * 37 + y / 25 * 53 + seed * 71) % 200) as u8 + 30;
            Rgba([v, v, v, 255])
        });
        if let Some((cx, cy)) = dot {
            for y in cy..cy + 6 {
                for x in cx..cx + 4 {
                    img.put_pixel(x, y, Rgba([255, 0, 0, 255]));
                }
            }
        }
        image::DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn near_identical_screens_group_and_different_ones_do_not() {
        let a = fingerprint(&screen(1, None));
        let a2 = fingerprint(&screen(1, Some((100, 60)))); // 多了一個小游標
        let b = fingerprint(&screen(2, None));
        assert!(diff(&a, &a2) <= max_diff(1), "小差異：{}", diff(&a, &a2));
        assert!(diff(&a, &b) > max_diff(2), "不同畫面：{}", diff(&a, &b));
        let items = vec![(320, 200, a), (320, 200, b), (320, 200, a2), (640, 400, a)];
        assert_eq!(group(&items, max_diff(1)), vec![vec![0, 2]], "大小不同的不算");
    }

    #[test]
    fn fingerprints_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        screen(3, None).save(&p).unwrap();
        let file = dir.path().join("h.json");
        let store = HashStore::new(file.clone());
        let paths = vec![p.display().to_string(), dir.path().join("missing.png").display().to_string()];
        let r = store.fingerprints(&paths);
        assert!(r[0].is_some() && r[1].is_none());
        assert_eq!(store.total.load(Ordering::Relaxed), 1);
        let again = HashStore::new(file);
        assert_eq!(again.fingerprints(&paths)[0], r[0]);
        assert_eq!(again.total.load(Ordering::Relaxed), 0, "讀檔就好，不用重算");
    }

    #[test]
    fn colors_find_dark_light_and_red() {
        let solid = |c: [u8; 4]| image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, Rgba(c)));
        assert_eq!(colors(&solid([20, 20, 30, 255])), COLOR_DARK);
        assert_eq!(colors(&solid([250, 250, 250, 255])), COLOR_LIGHT);
        // 白底上一大塊紅
        let mut img = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 255]));
        for y in 0..20 {
            for x in 0..64 {
                img.put_pixel(x, y, Rgba([220, 30, 30, 255]));
            }
        }
        let c = colors(&image::DynamicImage::ImageRgba8(img));
        assert!(c & COLOR_RED != 0 && c & COLOR_BLUE == 0, "{c}");
    }
}
