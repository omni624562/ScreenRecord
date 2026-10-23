//! 找出 Jellyfin 上重複的影片：長度幾乎一樣的才拿來比（加速版、剪輯版長度不同，不會被當成重複），
//! 再比影片裡幾個時間點的畫面：縮成 17×16 灰階的「長相指紋」（同 similar.rs 的截圖），加上 4×4 格的平均顏色
//! （畫面構圖像、顏色或角落的字不同的不算，例如同一個片頭、不同場次的比賽）。
//! 大小、長度都一樣的是「完全一樣」；畫面一樣但檔案不同（重新壓縮過、手機傳了兩次畫質不同）是「內容一樣」。
//!
//! 畫面直接從 Jellyfin 串流讀（FFmpeg 跳到那個時間只讀那附近一小段），不用下載整支。
//! 讀過的指紋記在設定資料夾的 jellyfin-prints.json（影片編號 → 大小、長度、指紋），下次只讀新的影片。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// 比對畫面的位置（長度的幾成）
const SPOTS: [f64; 3] = [0.25, 0.5, 0.75];
/// 每張畫面的指紋最多差幾位元（共 256）還算同一個畫面：重新壓縮、畫質不同只差一點點
const MAX_DIFF: u32 = 24;
/// 每一格的平均顏色最多差多少（0–255）：壓得很爛差 4 左右、調過亮度飽和度差 10 左右；角落的字不同就差 30
const MAX_COLOR: u8 = 20;

/// 一張畫面的長相
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Print {
    /// 每列 16 個「右邊比左邊亮」的位元
    pub hash: [u64; 4],
    /// 4×4 格的平均 RGB（48 個數）
    pub color: Vec<u8>,
}

/// Jellyfin 上的一支影片（找重複用）
#[derive(Debug, Clone, PartialEq)]
pub struct JfVideo {
    pub id: String,
    pub name: String,
    /// 伺服器上的路徑（看在哪個資料夾）
    pub path: String,
    pub size: u64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub bitrate: u64,
    /// YYYY-MM-DD（加入的日期）
    pub added: String,
    /// 瀏覽的使用者看過了
    pub played: bool,
    pub has_image: bool,
}

impl JfVideo {
    /// 畫質排名：解析度、位元率、大小，大的在前（預設留這一份）
    pub fn quality(&self) -> (u64, u64, u64) {
        (self.width as u64 * self.height as u64, self.bitrate, self.size)
    }
}

/// 一組重複的影片
#[derive(Debug, Clone, PartialEq)]
pub struct DupGroup {
    /// 畫質好的在前
    pub items: Vec<JfVideo>,
    /// 檔案完全一樣（大小、長度一樣）；不然是內容一樣、檔案不同
    pub exact: bool,
}

/// /Items 的回應（fields=Path,MediaSources,DateCreated）裡的影片
pub fn parse_videos(json: &str) -> (Vec<JfVideo>, usize) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (vec![], 0) };
    let total = v.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or(0) as usize;
    let list = v
        .get("Items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let src = i.pointer("/MediaSources/0");
                    let streams = src.and_then(|s| s.get("MediaStreams")).or_else(|| i.get("MediaStreams")).and_then(Value::as_array).cloned().unwrap_or_default();
                    let video = streams.iter().find(|s| s.get("Type").and_then(Value::as_str) == Some("Video"));
                    let num = |s: Option<&Value>, k: &str| s.and_then(|s| s.get(k)).and_then(Value::as_u64).unwrap_or(0);
                    let duration = i.get("RunTimeTicks").and_then(Value::as_f64).unwrap_or(0.0) / 1e7;
                    Some(JfVideo {
                        id: i.get("Id")?.as_str()?.to_string(),
                        name: i.get("Name").and_then(Value::as_str).unwrap_or("?").to_string(),
                        path: i.get("Path").and_then(Value::as_str).unwrap_or("").to_string(),
                        size: num(src, "Size"),
                        duration,
                        width: num(video, "Width") as u32,
                        height: num(video, "Height") as u32,
                        bitrate: num(src, "Bitrate").max(num(video, "BitRate")),
                        added: i.get("DateCreated").and_then(Value::as_str).and_then(|d| d.get(..10)).unwrap_or("").to_string(),
                        played: i.pointer("/UserData/Played").and_then(Value::as_bool) == Some(true),
                        has_image: i.pointer("/ImageTags/Primary").is_some(),
                    })
                })
                .filter(|v| v.duration > 0.0)
                .collect()
        })
        .unwrap_or_default();
    (list, total)
}

/// 長度差多少以內才拿來比（1 秒或 0.5%，取大的）：轉檔、重新壓縮後長度會差一點點
fn same_length(a: f64, b: f64) -> bool {
    (a - b).abs() <= (a.max(b) * 0.005).max(1.0)
}

/// 長度幾乎一樣的分在一起（依長度排序後相鄰的串起來）；只回傳至少兩支的組
pub fn by_length(videos: &[JfVideo]) -> Vec<Vec<usize>> {
    let mut idx: Vec<usize> = (0..videos.len()).collect();
    idx.sort_by(|&a, &b| videos[a].duration.total_cmp(&videos[b].duration));
    let mut out: Vec<Vec<usize>> = vec![];
    let mut cur: Vec<usize> = vec![];
    for i in idx {
        match cur.last() {
            Some(&j) if same_length(videos[j].duration, videos[i].duration) => cur.push(i),
            _ => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur = vec![i];
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// 一張畫面（17×16 的 RGBA）的長相
pub fn frame_print(rgba: &[u8]) -> Print {
    let px = |x: usize, y: usize| {
        let i = (y * 17 + x) * 4;
        rgba.get(i..i + 3).map_or([0; 3], |p| [p[0] as u32, p[1] as u32, p[2] as u32])
    };
    let luma = |x: usize, y: usize| {
        let [r, g, b] = px(x, y);
        r * 299 + g * 587 + b * 114
    };
    let mut hash = [0u64; 4];
    for y in 0..16 {
        for x in 0..16 {
            if luma(x + 1, y) > luma(x, y) {
                let bit = y * 16 + x;
                hash[bit / 64] |= 1 << (bit % 64);
            }
        }
    }
    let mut color = Vec::with_capacity(48);
    for by in 0..4 {
        for bx in 0..4 {
            let mut sum = [0u32; 3];
            for y in by * 4..by * 4 + 4 {
                for x in bx * 4..bx * 4 + 4 {
                    let p = px(x, y);
                    (0..3).for_each(|c| sum[c] += p[c]);
                }
            }
            color.extend(sum.map(|s| (s / 16) as u8));
        }
    }
    Print { hash, color }
}

fn same_frame(a: &Print, b: &Print) -> bool {
    let bits: u32 = a.hash.iter().zip(&b.hash).map(|(x, y)| (x ^ y).count_ones()).sum();
    bits <= MAX_DIFF && a.color.len() == b.color.len() && a.color.iter().zip(&b.color).all(|(x, y)| x.abs_diff(*y) <= MAX_COLOR)
}

/// 每個時間點的畫面都像，才算同一支（一個時間點讀不到時不算）
fn same_content(a: &[Print], b: &[Print]) -> bool {
    a.len() == SPOTS.len() && b.len() == SPOTS.len() && a.iter().zip(b).all(|(x, y)| same_frame(x, y))
}

/// 有指紋的影片依內容分組（A 像 B、B 像 C 就三支一組）：回傳每組（畫質好的在前）
pub fn group(videos: &[JfVideo], prints: &HashMap<String, Vec<Print>>) -> Vec<DupGroup> {
    let mut out = vec![];
    for cand in by_length(videos) {
        let mut parent: Vec<usize> = (0..cand.len()).collect();
        fn root(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for a in 0..cand.len() {
            for b in a + 1..cand.len() {
                let (va, vb) = (&videos[cand[a]], &videos[cand[b]]);
                let exact = va.size > 0 && va.size == vb.size && (va.duration - vb.duration).abs() < 0.05;
                let alike = matches!((prints.get(&va.id), prints.get(&vb.id)), (Some(x), Some(y)) if same_content(x, y));
                if exact || alike {
                    let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                    if ra != rb {
                        parent[ra] = rb;
                    }
                }
            }
        }
        let mut groups: HashMap<usize, Vec<JfVideo>> = HashMap::new();
        for i in 0..cand.len() {
            let r = root(&mut parent, i);
            groups.entry(r).or_default().push(videos[cand[i]].clone());
        }
        for (_, mut items) in groups.into_iter().filter(|(_, g)| g.len() > 1) {
            items.sort_by(|a, b| b.quality().cmp(&a.quality()).then_with(|| a.added.cmp(&b.added)));
            let exact = items.windows(2).all(|w| w[0].size > 0 && w[0].size == w[1].size);
            out.push(DupGroup { items, exact });
        }
    }
    // 完全一樣的在前，再依大小（省最多空間的在前）
    out.sort_by(|a, b| b.exact.cmp(&a.exact).then_with(|| b.items[0].size.cmp(&a.items[0].size)));
    out
}

/// 記下來的指紋：影片編號 → (大小, 長度, 指紋)
#[derive(Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    prints: HashMap<String, (u64, f64, Vec<Print>)>,
}

fn cache_file() -> PathBuf {
    crate::paths::data_dir().join("jellyfin-prints.json")
}

/// 掃描的進度（介面顯示「正在比對第 N / M 支」）與取消
#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub cancel: AtomicBool,
}

/// 找重複：videos 是 Jellyfin 上全部的影片；只讀長度和別支差不多的影片的畫面（stream_url 給串流網址）。
/// 取消時回傳 None
pub fn find(ffmpeg: &Path, videos: &[JfVideo], stream_url: impl Fn(&str) -> String + Sync, progress: &Progress) -> Option<Vec<DupGroup>> {
    let file = cache_file();
    let mut cache: Cache = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let need: Vec<&JfVideo> = by_length(videos).into_iter().flatten().map(|i| &videos[i]).collect();
    // 大小、長度沒變的用記下來的
    let todo: Vec<&JfVideo> = need.iter().copied().filter(|v| !cache.prints.get(&v.id).is_some_and(|(s, d, p)| *s == v.size && (d - v.duration).abs() < 0.01 && p.len() == SPOTS.len())).collect();
    progress.total.store(todo.len(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let got = std::sync::Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    // 同時讀 3 支（NAS 在網路上，一次讀太多反而慢）
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| loop {
                if progress.cancel.load(Ordering::Relaxed) {
                    return;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(v) = todo.get(i) else { return };
                let url = stream_url(&v.id);
                let prints: Vec<Print> = SPOTS.iter().filter_map(|f| crate::player::decode_frame(ffmpeg, &url, v.duration * f, 17, 16, false)).map(|f| frame_print(&f)).collect();
                if prints.len() == SPOTS.len() {
                    got.lock().unwrap_or_else(|e| e.into_inner()).push((v.id.clone(), (v.size, v.duration, prints)));
                } else {
                    crate::info!("[Jellyfin] 找重複：讀不到「{}」的畫面", v.name);
                }
                progress.done.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    for (id, e) in got.into_inner().unwrap() {
        cache.prints.insert(id, e);
    }
    // 伺服器上已經沒有的拿掉
    let ids: std::collections::HashSet<&str> = videos.iter().map(|v| v.id.as_str()).collect();
    cache.prints.retain(|id, _| ids.contains(id.as_str()));
    if let Ok(t) = serde_json::to_string(&cache) {
        let _ = crate::paths::write_atomic(&file, t.as_bytes());
    }
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }
    let prints: HashMap<String, Vec<Print>> = cache.prints.into_iter().map(|(id, (_, _, p))| (id, p)).collect();
    Some(group(videos, &prints))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: &str, dur: f64, size: u64, w: u32) -> JfVideo {
        JfVideo { id: id.into(), name: id.into(), path: String::new(), size, duration: dur, width: w, height: w * 9 / 16, bitrate: 0, added: String::new(), played: false, has_image: false }
    }

    fn p(hash: [u64; 4], gray: u8) -> Print {
        Print { hash, color: vec![gray; 48] }
    }

    #[test]
    fn only_similar_lengths_are_compared() {
        // 60 秒和 60.4 秒一組；15 秒（加速版）、100 秒自己一支
        let list = vec![v("a", 60.0, 1, 1920), v("b", 15.0, 1, 1920), v("c", 60.4, 1, 1280), v("d", 100.0, 1, 1920)];
        assert_eq!(by_length(&list), vec![vec![0, 2]]);
    }

    #[test]
    fn groups_exact_copies_and_reencoded_ones() {
        let list = vec![v("a", 60.0, 500, 1280), v("b", 60.0, 500, 1280), v("c", 60.3, 200, 1920), v("d", 60.2, 300, 640), v("e", 300.0, 9, 1920)];
        let same = vec![p([1, 2, 3, 4], 100); 3];
        let other = vec![p([!0, 0, !0, 0], 100); 3];
        let mut prints = HashMap::new();
        // c 和 a 畫面一樣（重新壓縮過、畫質比較好）；d 長度差不多但畫面不同
        prints.insert("c".to_string(), same.clone());
        prints.insert("a".to_string(), same);
        prints.insert("d".to_string(), other);
        let g = group(&list, &prints);
        assert_eq!(g.len(), 1);
        let ids: Vec<&str> = g[0].items.iter().map(|x| x.id.as_str()).collect();
        // 畫質好的（1920）在前；a、b 大小一樣、c 不一樣：不是「完全一樣」
        assert_eq!(ids[0], "c");
        assert_eq!(ids.len(), 3);
        assert!(!g[0].exact);
        // 只有完全一樣的兩支
        let g = group(&list[..2], &HashMap::new());
        assert!(g[0].exact && g[0].items.len() == 2);
    }

    #[test]
    fn frame_print_reads_brightness_steps() {
        // 由左到右越來越亮：每個位元都是 1
        let mut rgba = vec![0u8; 17 * 16 * 4];
        for y in 0..16 {
            for x in 0..17 {
                let i = (y * 17 + x) * 4;
                rgba[i..i + 3].fill((x * 15) as u8);
            }
        }
        let f = frame_print(&rgba);
        assert_eq!(f.hash, [!0u64; 4]);
        // 左上那格是 x 0–3 的平均：(0+15+30+45)/4
        assert_eq!(&f.color[..3], &[22, 22, 22]);
        assert_eq!(f.color.len(), 48);
        let a = vec![p([0; 4], 100); 3];
        assert!(same_content(&a, &vec![p([0b111; 4], 100); 3]));
        assert!(!same_content(&a, &a[..2]));
        // 構圖一樣、顏色差很多（例如另一場比賽的同一個片頭換了隊伍顏色）
        assert!(!same_content(&a, &vec![p([0; 4], 140); 3]));
        assert!(same_content(&a, &vec![p([0; 4], 110); 3]));
    }

    #[test]
    fn parses_videos_with_media_sources() {
        let json = r#"{"Items":[{"Id":"x","Name":"生日","Path":"/v/a.mp4","RunTimeTicks":600000000,"DateCreated":"2026-10-01T00:00:00Z","UserData":{"Played":true},
            "MediaSources":[{"Size":12345,"Bitrate":800000,"MediaStreams":[{"Type":"Video","Width":1920,"Height":1080}]}]},{"Id":"y","Name":"沒長度"}],"TotalRecordCount":2}"#;
        let (list, total) = parse_videos(json);
        assert_eq!(total, 2);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].size, list[0].width, list[0].bitrate, list[0].duration, list[0].played), (12345, 1920, 800000, 60.0, true));
        assert_eq!(list[0].added, "2026-10-01");
    }
}
