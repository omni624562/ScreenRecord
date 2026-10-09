//! 錄影清單：加速版歸到原始錄影底下，支援搜尋、篩選、排序、分頁、改名。
//! 讀取影片資訊（長度、解析度、有無聲音）要執行 FFmpeg，只對需要的檔案做，並快取結果。

use crate::args::parse_media_info;
use crate::error::{Error, Result};
use crate::format::{check_recording_name, parse_export_name, strip_mp4};
use crate::paths::mtime_ms;
use crate::process::run;
use crate::types::{ExportFormat, ExportInfo, LibraryEntry, LibraryFilter, LibraryPage, LibraryQuery, LibrarySort, MediaInfo, LIBRARY_ROW_MAIN_PX, LIBRARY_ROW_SUB_PX};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::{OnceCell, Semaphore};

type Slot = Arc<OnceCell<MediaInfo>>;

/// 影片資訊快取：同一個檔案同時被要求（搜尋、換頁、轉檔完成重新整理）只會執行一次 FFmpeg
#[derive(Default)]
pub struct MediaCache {
    map: Mutex<HashMap<String, (String, Slot)>>,
}

impl MediaCache {
    pub async fn probe(&self, ffmpeg: &Path, path: &str) -> Result<MediaInfo> {
        let meta = tokio::fs::metadata(path).await?;
        let key = format!("{}:{}", meta.len(), mtime_ms(&meta));
        let slot = {
            let mut m = self.map.lock().unwrap();
            match m.get(path) {
                Some((k, s)) if *k == key => s.clone(),
                _ => {
                    let s: Slot = Arc::default();
                    m.insert(path.to_string(), (key, s.clone()));
                    s
                }
            }
        };
        let info = slot
            .get_or_try_init(|| async {
                // 沒指定輸出時 ffmpeg 會以代碼 1 結束，但 stderr 已含完整的串流資訊
                let r = run(ffmpeg, &["-hide_banner", "-i", path], Duration::from_secs(15)).await;
                if r.timed_out || (r.code == -1 && r.stderr.is_empty()) {
                    return Err(Error::other("無法讀取影片資訊"));
                }
                let p = parse_media_info(&r.stderr);
                Ok::<_, Error>(MediaInfo {
                    path: path.to_string(),
                    name: file_name(path),
                    bytes: meta.len(),
                    mtime: mtime_ms(&meta),
                    duration_sec: p.duration_sec,
                    width: p.width,
                    height: p.height,
                    fps: p.fps,
                    has_audio: Some(p.has_audio),
                })
            })
            .await;
        match info {
            Ok(i) => Ok(i.clone()),
            Err(e) => {
                // 失敗不快取，下次再試
                let mut m = self.map.lock().unwrap();
                if m.get(path).is_some_and(|(_, s)| Arc::ptr_eq(s, &slot)) {
                    m.remove(path);
                }
                Err(e)
            }
        }
    }

    /// 移除已不存在的檔案的快取（每次掃描資料夾後呼叫）
    fn prune(&self, dir: &Path, present: &[String]) {
        let prefix = dir.join("_").display().to_string();
        let prefix = prefix[..prefix.len() - 1].to_lowercase(); // 與 join() 產生的路徑同樣格式，結尾為分隔符
        let mut m = self.map.lock().unwrap();
        m.retain(|path, _| {
            let lower = path.to_lowercase();
            !(lower.starts_with(&prefix) && !lower[prefix.len()..].contains(['\\', '/']) && !present.contains(&lower))
        });
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

static CUT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)_cut(_\d+)?\.mp4$").unwrap());
static VIDEO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(mp4|gif|png)$").unwrap());
static IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.png$").unwrap());

/// 截圖（PNG）：清單上單獨一筆，沒有加速版
pub fn is_image_name(name: &str) -> bool {
    IMAGE_RE.is_match(name)
}

/// 去掉副檔名（.mp4 / .png）
fn strip_ext(name: &str) -> String {
    if is_image_name(name) {
        name[..name.len() - 4].to_string()
    } else {
        strip_mp4(name)
    }
}
static GIF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.gif$").unwrap());

pub fn is_cut_name(name: &str) -> bool {
    CUT_RE.is_match(name)
}

/// 掃描資料夾：原始錄影（含剪輯版）各自一筆，加速版放在對應的原檔底下。
/// 非同步讀取：儲存位置在網路磁碟且回應很慢時，不會卡住錄影與狀態更新。
pub async fn scan(cache: &MediaCache, dir: &Path) -> Vec<LibraryEntry> {
    let mut names = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else { return vec![] };
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        if VIDEO_RE.is_match(&name) {
            names.push(name);
        }
    }
    // 依檔名排序，結果穩定（與讀取順序無關）
    names.sort();
    let mut tasks = tokio::task::JoinSet::new();
    for (i, name) in names.iter().enumerate() {
        let p = dir.join(name);
        tasks.spawn(async move { (i, tokio::fs::metadata(p).await.ok()) });
    }
    let mut stats = vec![None; names.len()];
    while let Some(Ok((i, m))) = tasks.join_next().await {
        stats[i] = m;
    }
    let mut files: Vec<MediaInfo> = Vec::new();
    for (name, st) in names.iter().zip(stats) {
        // 檔案剛好被刪除或鎖住
        if let Some(st) = st.filter(|s| s.is_file()) {
            files.push(MediaInfo { name: name.clone(), path: dir.join(name).display().to_string(), bytes: st.len(), mtime: mtime_ms(&st), ..Default::default() });
        }
    }
    cache.prune(dir, &files.iter().map(|f| f.path.to_lowercase()).collect::<Vec<_>>());
    let by_name: HashMap<String, usize> = files.iter().enumerate().map(|(i, f)| (f.name.to_lowercase(), i)).collect();
    let mut entries: Vec<LibraryEntry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut derived: Vec<(String, ExportInfo)> = Vec::new();
    for f in &files {
        let mut e = parse_export_name(&f.name);
        // 原速 GIF：先找同名的 MP4（Rec_X_cut_2.gif → Rec_X_cut_2.mp4），找不到才用去掉 _N 的名稱
        if let Some(p) = e.as_mut().filter(|p| p.format == ExportFormat::Gif && p.speed == 1.0) {
            let exact = GIF_RE.replace(&f.name, ".mp4").into_owned();
            if by_name.contains_key(&exact.to_lowercase()) {
                p.base = exact;
            }
        }
        match e {
            Some(p) if by_name.contains_key(&p.base.to_lowercase()) => {
                derived.push((p.base.to_lowercase(), ExportInfo { media: f.clone(), speed: p.speed, format: Some(p.format) }));
            }
            _ if !GIF_RE.is_match(&f.name) => {
                index.insert(f.name.to_lowercase(), entries.len());
                entries.push(LibraryEntry { media: f.clone(), exports: vec![] });
            }
            _ => {} // 找不到原片的 GIF 不列出
        }
    }
    for (base, x) in derived {
        if let Some(&i) = index.get(&base) {
            entries[i].exports.push(x);
        }
    }
    for e in &mut entries {
        e.exports.sort_by(|p, q| {
            p.speed.total_cmp(&q.speed).then(((p.format == Some(ExportFormat::Gif)) as u8).cmp(&((q.format == Some(ExportFormat::Gif)) as u8))).then(p.media.mtime.total_cmp(&q.media.mtime))
        });
    }
    entries
}

/// 已讀過的資訊直接帶入，沒讀過的才執行 FFmpeg
async fn with_info(cache: &MediaCache, ffmpeg: Option<&Path>, e: LibraryEntry) -> LibraryEntry {
    let Some(ff) = ffmpeg else { return e };
    let media = cache.probe(ff, &e.media.path).await.unwrap_or(e.media);
    let mut exports = Vec::with_capacity(e.exports.len());
    for x in e.exports {
        let m = cache.probe(ff, &x.media.path).await.unwrap_or(x.media);
        exports.push(ExportInfo { media: m, ..x });
    }
    LibraryEntry { media, exports }
}

/// 同時最多 limit 個讀取影片資訊的工作
async fn pool_with_info(cache: &Arc<MediaCache>, ffmpeg: Option<&Path>, list: Vec<LibraryEntry>, limit: usize) -> Vec<LibraryEntry> {
    let sem = Arc::new(Semaphore::new(limit));
    let mut tasks = tokio::task::JoinSet::new();
    let n = list.len();
    for (i, e) in list.into_iter().enumerate() {
        let (cache, sem, ff) = (cache.clone(), sem.clone(), ffmpeg.map(Path::to_path_buf));
        tasks.spawn(async move {
            let _permit = sem.acquire_owned().await;
            (i, with_info(&cache, ff.as_deref(), e).await)
        });
    }
    let mut out: Vec<Option<LibraryEntry>> = (0..n).map(|_| None).collect();
    while let Some(Ok((i, e))) = tasks.join_next().await {
        out[i] = Some(e);
    }
    out.into_iter().flatten().collect()
}

/// 依高度分頁：回傳每頁第一筆的索引。一筆（含子列）放不下一整頁時自己一頁
pub fn page_starts(list: &[LibraryEntry], fit_px: f64) -> Vec<usize> {
    let mut starts = vec![0];
    let mut used = 0.0;
    for (i, e) in list.iter().enumerate() {
        let h = LIBRARY_ROW_MAIN_PX + e.exports.len() as f64 * LIBRARY_ROW_SUB_PX;
        if used > 0.0 && used + h > fit_px {
            starts.push(i);
            used = 0.0;
        }
        used += h;
    }
    starts
}

fn local_date(mtime_ms: f64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(mtime_ms as i64).single().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

pub async fn list_library(cache: &Arc<MediaCache>, ffmpeg: Option<&Path>, dir: &Path, q: &LibraryQuery) -> LibraryPage {
    let mut list = scan(cache, dir).await;

    if let Some(text) = q.q.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        list.retain(|e| e.media.name.to_lowercase().contains(&text) || local_date(e.media.mtime).contains(&text) || e.exports.iter().any(|x| x.media.name.to_lowercase().contains(&text)));
    }
    match q.filter {
        LibraryFilter::Original => list.retain(|e| !is_cut_name(&e.media.name) && !is_image_name(&e.media.name)),
        LibraryFilter::Shot => list.retain(|e| is_image_name(&e.media.name)),
        LibraryFilter::Cut => list.retain(|e| is_cut_name(&e.media.name)),
        LibraryFilter::Speed => list.retain(|e| !e.exports.is_empty()),
        _ => {}
    }

    // 依聲音篩選、依長度排序需要每個檔案的資訊（有快取，第一次較慢）
    let need_all = q.filter == LibraryFilter::Audio || q.sort == LibrarySort::Duration;
    if need_all && ffmpeg.is_some() {
        list = pool_with_info(cache, ffmpeg, list, 6).await;
    }
    if q.filter == LibraryFilter::Audio {
        list.retain(|e| e.media.has_audio == Some(true));
    }
    match q.sort {
        LibrarySort::New => list.sort_by(|a, b| b.media.mtime.total_cmp(&a.media.mtime)),
        LibrarySort::Old => list.sort_by(|a, b| a.media.mtime.total_cmp(&b.media.mtime)),
        LibrarySort::Size => list.sort_by_key(|e| std::cmp::Reverse(e.media.bytes)),
        LibrarySort::Duration => list.sort_by(|a, b| b.media.duration_sec.unwrap_or(0.0).total_cmp(&a.media.duration_sec.unwrap_or(0.0))),
    }

    let total = list.len();
    let clamp_page = |pages: usize| {
        let p = q.page.filter(|p| p.is_finite()).map(f64::floor).unwrap_or(1.0);
        (if p >= 1.0 { p as usize } else { 1 }).min(pages)
    };
    let (pages, page, slice, page_size) = match q.fit_px.filter(|f| f.is_finite()) {
        Some(fit) => {
            let starts = page_starts(&list, fit.max(LIBRARY_ROW_MAIN_PX));
            let pages = starts.len();
            let page = clamp_page(pages);
            let end = starts.get(page).copied().unwrap_or(list.len());
            let slice: Vec<LibraryEntry> = list[starts[page - 1]..end].to_vec();
            let n = slice.len();
            (pages, page, slice, n)
        }
        None => {
            let ps = q.page_size.filter(|p| p.is_finite()).map(f64::floor).unwrap_or(30.0).clamp(1.0, 200.0) as usize;
            let pages = total.div_ceil(ps).max(1);
            let page = clamp_page(pages);
            let slice: Vec<LibraryEntry> = list.iter().skip((page - 1) * ps).take(ps).cloned().collect();
            (pages, page, slice, ps)
        }
    };
    let items = if need_all { slice } else { pool_with_info(cache, ffmpeg, slice, 6).await };
    LibraryPage { dir: dir.display().to_string(), total, page, pages, page_size, items }
}

/// 錄影改名：原片與底下的加速版 / GIF 一起改（Rec_X_4x.mp4 → 新名_4x.mp4），清單上才不會斷開。
/// 任何一個失敗就把已改的改回去。busy = 正在錄影 / 轉檔的檔案（小寫完整路徑）。回傳新的完整路徑。
pub async fn rename_recording(cache: &MediaCache, path: &str, new_name: &str, busy: &[String]) -> Result<String> {
    let base = strip_ext(new_name.trim()).trim().to_string();
    if let Some(bad) = check_recording_name(&base) {
        return Err(Error::config(bad));
    }
    let dir = Path::new(path).parent().map(Path::to_path_buf).unwrap_or_default();
    let entries = scan(cache, &dir).await;
    let Some(entry) = entries.into_iter().find(|e| e.media.path.to_lowercase() == path.to_lowercase()) else {
        return Err(Error::config("找不到這個錄影，可能已被移動或刪除"));
    };
    let old_base = strip_ext(&entry.media.name);
    let ext = if is_image_name(&entry.media.name) { "png" } else { "mp4" };
    if base == old_base {
        return Ok(entry.media.path);
    }
    let old_len = old_base.chars().count();
    let mut plan: Vec<(String, PathBuf)> = vec![(entry.media.path.clone(), dir.join(format!("{base}.{ext}")))];
    for x in &entry.exports {
        let suffix: String = x.media.name.chars().skip(old_len).collect();
        plan.push((x.media.path.clone(), dir.join(format!("{base}{suffix}"))));
    }
    for (from, to) in &plan {
        if busy.contains(&from.to_lowercase()) {
            return Err(Error::config("檔案正在錄影或轉檔中，完成後才能改名"));
        }
        // 只改大小寫時目標就是自己（Windows 不分大小寫），不算衝突
        if to.exists() && to.display().to_string().to_lowercase() != from.to_lowercase() {
            return Err(Error::config(format!("已有同名的檔案：{}", file_name(&to.display().to_string()))));
        }
    }
    let mut done: Vec<(&String, &PathBuf)> = Vec::new();
    for (from, to) in &plan {
        if let Err(e) = tokio::fs::rename(from, to).await {
            for (f, t) in done.iter().rev() {
                let _ = tokio::fs::rename(t, f).await;
            }
            let busy = e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32) | Some(5));
            return Err(Error::config(if busy { "檔案正在使用中（例如正在播放或剪輯），關閉後再試一次".to_string() } else { format!("無法改名：{e}") }));
        }
        done.push((from, to));
    }
    Ok(plan[0].1.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for n in names {
            fs::write(dir.path().join(n), "x").unwrap();
        }
        dir
    }

    fn q(fit_px: Option<f64>, page: Option<f64>, sort: LibrarySort) -> LibraryQuery {
        LibraryQuery { page_size: Some(50.0), fit_px, page, sort, ..Default::default() }
    }

    #[tokio::test]
    async fn exports_group_under_the_right_original() {
        let dir = make(&["Rec_X.mp4", "Rec_X_4x.mp4", "Rec_X_2.gif", "Rec_X_cut.mp4", "Rec_X_cut_2.mp4", "Rec_X_cut_2.gif", "Orphan.gif"]);
        let cache = Arc::new(MediaCache::default());
        let page = list_library(&cache, None, dir.path(), &q(None, None, LibrarySort::New)).await;
        let mut by: Vec<(String, Vec<String>)> = page
            .items
            .iter()
            .map(|e| {
                let xs = e.exports.iter().map(|x| format!("{}:{}:{}", x.media.name, x.format.unwrap().ext(), crate::format::num(x.speed))).collect();
                (e.media.name.clone(), xs)
            })
            .collect();
        by.sort();
        assert_eq!(
            by,
            vec![
                ("Rec_X.mp4".into(), vec!["Rec_X_2.gif:gif:1".into(), "Rec_X_4x.mp4:mp4:4".into()]),
                ("Rec_X_cut.mp4".into(), vec![]),
                ("Rec_X_cut_2.mp4".into(), vec!["Rec_X_cut_2.gif:gif:1".into()]),
            ]
        );
    }

    fn entry(n: usize) -> LibraryEntry {
        LibraryEntry { media: MediaInfo { name: "a.mp4".into(), ..Default::default() }, exports: vec![ExportInfo { speed: 4.0, ..Default::default() }; n] }
    }

    #[test]
    fn pages_by_height() {
        // 原片 41px、子列 33px；高度 200：41+41+(41+33*2=107) = 189 放得下，下一筆換頁
        assert_eq!(page_starts(&[entry(0), entry(0), entry(2), entry(0)], 200.0), vec![0, 3]);
        assert_eq!(page_starts(&[entry(0), entry(10), entry(0)], 200.0), vec![0, 1, 2]);
        assert_eq!(page_starts(&[], 200.0), vec![0]);
    }

    #[tokio::test]
    async fn fit_px_pagination() {
        let dir = make(&[]);
        for (i, n) in ["Rec_A.mp4", "Rec_A_4x.mp4", "Rec_A_8x.mp4", "Rec_B.mp4", "Rec_C.mp4"].iter().enumerate() {
            let p = dir.path().join(n);
            fs::write(&p, "x").unwrap();
            let t = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000 + i as u64);
            fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        let cache = Arc::new(MediaCache::default());
        let fit = 41.0 * 2.0 + 33.0 * 2.0;
        let p1 = list_library(&cache, None, dir.path(), &q(Some(fit), None, LibrarySort::Old)).await;
        assert_eq!((p1.total, p1.pages), (3, 2));
        assert_eq!(p1.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_A.mp4", "Rec_B.mp4"]);
        let p2 = list_library(&cache, None, dir.path(), &q(Some(fit), Some(2.0), LibrarySort::Old)).await;
        assert_eq!(p2.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_C.mp4"]);
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn screenshots_are_standalone_entries() {
        let dir = make(&["Rec_X.mp4", "Rec_X_4x.mp4", "Shot_2026-10-09_14-30-00.png"]);
        let cache = MediaCache::default();
        let list = scan(&cache, dir.path()).await;
        let names: Vec<(String, usize)> = list.iter().map(|e| (e.media.name.clone(), e.exports.len())).collect();
        assert_eq!(names, vec![("Rec_X.mp4".into(), 1), ("Shot_2026-10-09_14-30-00.png".into(), 0)]);
        let shots = list_library(&Arc::new(cache), None, dir.path(), &LibraryQuery { filter: LibraryFilter::Shot, ..q(None, None, LibrarySort::New) }).await;
        assert_eq!(shots.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Shot_2026-10-09_14-30-00.png"]);
        // 改名保留 .png
        let src = dir.path().join("Shot_2026-10-09_14-30-00.png").display().to_string();
        let out = rename_recording(&MediaCache::default(), &src, "登入畫面", &[]).await.unwrap();
        assert!(out.ends_with("登入畫面.png"), "{out}");
    }

    #[tokio::test]
    async fn rename_with_exports() {
        let dir = make(&["Rec_X.mp4", "Rec_X_4x.mp4", "Rec_X_2x.gif", "Rec_X.gif", "Rec_X_cut.mp4"]);
        let cache = MediaCache::default();
        let src = dir.path().join("Rec_X.mp4").display().to_string();
        let out = rename_recording(&cache, &src, " 操作示範.mp4 ", &[]).await.unwrap();
        assert_eq!(out, dir.path().join("操作示範.mp4").display().to_string());
        let mut expect: Vec<String> = ["Rec_X_cut.mp4", "操作示範.gif", "操作示範.mp4", "操作示範_2x.gif", "操作示範_4x.mp4"].iter().map(|s| s.to_string()).collect();
        expect.sort();
        assert_eq!(listing(dir.path()), expect);
    }

    #[tokio::test]
    async fn rename_refuses_bad_names_conflicts_and_busy_files() {
        let dir = make(&["Rec_A.mp4", "Rec_A_4x.mp4", "B_4x.mp4"]);
        let cache = MediaCache::default();
        let src = dir.path().join("Rec_A.mp4").display().to_string();
        let err = |r: Result<String>| r.unwrap_err().message().to_string();
        assert!(err(rename_recording(&cache, &src, "a:b", &[]).await).contains("不能包含"));
        assert!(err(rename_recording(&cache, &src, "Demo_4x", &[]).await).contains("加速版"));
        assert!(err(rename_recording(&cache, &src, "B", &[]).await).contains("同名"));
        let busy = vec![dir.path().join("Rec_A_4x.mp4").display().to_string().to_lowercase()];
        assert!(err(rename_recording(&cache, &src, "C", &busy).await).contains("轉檔中"));
        assert_eq!(listing(dir.path()), vec!["B_4x.mp4", "Rec_A.mp4", "Rec_A_4x.mp4"]);
    }
}
