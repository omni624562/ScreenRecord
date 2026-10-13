//! 錄影清單：加速版歸到原始錄影底下，支援搜尋、篩選、排序、分頁、改名。
//! 讀取影片資訊（長度、解析度、有無聲音）要執行 FFmpeg，只對需要的檔案做，並快取結果。

use crate::args::{measure_duration_args, parse_measured_duration, parse_media_info};
use crate::error::{Error, Result};
use crate::format::{check_recording_name, parse_export_name, strip_mp4};
use crate::paths::{created_ms, mtime_ms, set_created};
use crate::process::run;
use crate::types::{ExportFormat, ExportInfo, LibraryEntry, LibraryFilter, LibraryPage, LibraryQuery, LibrarySort, MediaInfo, LIBRARY_ROW_MAIN_PX, LIBRARY_ROW_SUB_PX};
use crate::{tr, trf};
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
                    return Err(Error::other(tr!("無法讀取影片資訊", "Couldn't read the video info")));
                }
                let mut p = parse_media_info(&r.stderr);
                if p.duration_sec.is_none() && p.width.is_some() {
                    let m = run(ffmpeg, &measure_duration_args(path), Duration::from_secs(15)).await;
                    p.duration_sec = parse_measured_duration(&m.stderr);
                }
                Ok::<_, Error>(MediaInfo {
                    path: path.to_string(),
                    name: file_name(path),
                    bytes: meta.len(),
                    mtime: mtime_ms(&meta),
                    created: created_ms(&meta),
                    duration_sec: p.duration_sec,
                    width: p.width,
                    height: p.height,
                    fps: p.fps,
                    has_audio: Some(p.has_audio),
                    chapters: p.chapters,
                    audio_tracks: p.audio_tracks,
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
static VIDEO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(mp4|gif|webp|png|jpe?g|m4a)$").unwrap());
/// 只有聲音（「存成 M4A」做出來的）：清單上單獨一筆，標示「只有聲音」
static AUDIO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.m4a$").unwrap());
static IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(png|jpe?g)$").unwrap());

/// 截圖（PNG，或截圖編輯另存的 JPG）：清單上單獨一筆，沒有加速版
pub fn is_image_name(name: &str) -> bool {
    IMAGE_RE.is_match(name)
}

/// 去掉副檔名（.mp4 / .png / .m4a）
fn strip_ext(name: &str) -> String {
    if is_image_name(name) || is_audio_name(name) {
        name.rsplit_once('.').map_or(name, |(b, _)| b).to_string()
    } else {
        strip_mp4(name)
    }
}
/// 動圖（GIF、WebP）
static GIF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(gif|webp)$").unwrap());

pub fn is_cut_name(name: &str) -> bool {
    CUT_RE.is_match(name)
}

pub fn is_audio_name(name: &str) -> bool {
    AUDIO_RE.is_match(name)
}

/// 掃描資料夾：原始錄影（含剪輯版）各自一筆，加速版放在對應的原檔底下。
/// 非同步讀取：儲存位置在網路磁碟且回應很慢時，不會卡住錄影與狀態更新。
pub async fn scan(cache: &MediaCache, dir: &Path) -> Vec<LibraryEntry> {
    scan_checked(cache, dir).await.unwrap_or_default()
}

/// 同 scan，但資料夾讀不到（不存在、隨身碟或網路磁碟沒接上、沒有權限）時回傳錯誤
pub async fn scan_checked(cache: &MediaCache, dir: &Path) -> std::io::Result<Vec<LibraryEntry>> {
    let mut names = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
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
            files.push(MediaInfo { name: name.clone(), path: dir.join(name).display().to_string(), bytes: st.len(), mtime: mtime_ms(&st), created: created_ms(&st), ..Default::default() });
        }
    }
    cache.prune(dir, &files.iter().map(|f| f.path.to_lowercase()).collect::<Vec<_>>());
    let by_name: HashMap<String, usize> = files.iter().enumerate().map(|(i, f)| (f.name.to_lowercase(), i)).collect();
    let mut entries: Vec<LibraryEntry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut derived: Vec<(String, ExportInfo)> = Vec::new();
    for f in &files {
        let mut e = parse_export_name(&f.name);
        // 原速動圖：先找同名的 MP4（Rec_X_cut_2.gif → Rec_X_cut_2.mp4），找不到才用去掉 _N 的名稱
        if let Some(p) = e.as_mut().filter(|p| p.format.is_animation() && p.speed == 1.0) {
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
                entries.push(LibraryEntry { media: f.clone(), exports: vec![], hits: vec![] });
            }
            _ => {} // 找不到原片的動圖不列出
        }
    }
    for (base, x) in derived {
        if let Some(&i) = index.get(&base) {
            entries[i].exports.push(x);
        }
    }
    for e in &mut entries {
        e.exports.sort_by(|p, q| {
            // 同一個倍率：MP4、GIF、WebP
            let rank = |f: Option<ExportFormat>| match f {
                Some(ExportFormat::Gif) => 1u8,
                Some(ExportFormat::Webp) => 2,
                _ => 0,
            };
            p.speed.total_cmp(&q.speed).then(rank(p.format).cmp(&rank(q.format))).then(p.media.mtime.total_cmp(&q.media.mtime))
        });
    }
    Ok(entries)
}

/// 資料夾讀不到時給使用者看的說明；None = 當作空的資料夾（還沒錄過影，預設的資料夾還沒建立）
fn dir_error(dir: &Path, e: &std::io::Error) -> Option<String> {
    let shown = dir.display().to_string();
    match e.kind() {
        std::io::ErrorKind::NotFound if dir.parent().is_some_and(Path::is_dir) => None,
        std::io::ErrorKind::NotFound => Some(trf!("找不到資料夾「{shown}」（隨身碟或網路磁碟可能沒接上）", "Folder “{shown}” not found (a USB drive or network drive may be disconnected)")),
        std::io::ErrorKind::PermissionDenied => Some(trf!("沒有權限讀取資料夾「{shown}」", "No permission to read the folder “{shown}”")),
        _ => Some(trf!("無法讀取資料夾「{shown}」（隨身碟或網路磁碟可能沒接上）", "Couldn't read the folder “{shown}” (a USB drive or network drive may be disconnected)")),
    }
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
    LibraryEntry { media, exports, hits: e.hits }
}

/// 資料夾裡的字幕檔：檔名（不含 .srt，小寫）→ 路徑
fn srt_files(dir: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if let Some(stem) = name.to_lowercase().strip_suffix(".srt") {
            map.entry(stem.to_string()).or_default().push(p.clone());
        }
    }
    map
}

/// 這支影片的字幕裡說過的話：同名的 SRT（另存時可能是「名稱_2.srt」；有同名的另一支影片時那是它的）
fn speech_hits(srts: &HashMap<String, Vec<PathBuf>>, videos: &std::collections::HashSet<String>, video_name: &str, query: &str) -> Vec<crate::types::SpeechHit> {
    static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^_\d+$").unwrap());
    let stem = strip_ext(video_name).to_lowercase();
    let mut hits = vec![];
    for (s, paths) in srts {
        let Some(rest) = s.strip_prefix(&stem) else { continue };
        if !rest.is_empty() && (!NUM.is_match(rest) || videos.contains(s)) {
            continue;
        }
        for p in paths {
            if let Ok(text) = std::fs::read_to_string(p) {
                hits.extend(crate::subtitles::search(&text, query));
            }
        }
    }
    hits.sort_by(|a, b| a.start.total_cmp(&b.start));
    hits.dedup_by(|a, b| a.start == b.start && a.text == b.text);
    hits
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
        let h = LIBRARY_ROW_MAIN_PX + (e.exports.len() + e.hits.len().min(crate::types::LIBRARY_HITS_SHOWN)) as f64 * LIBRARY_ROW_SUB_PX;
        if used > 0.0 && used + h > fit_px {
            starts.push(i);
            used = 0.0;
        }
        used += h;
    }
    starts
}

/// 搜尋的字是日期時要找的那一天（或那個月）：年份 None = 每一年都算
#[derive(Debug, Clone, Copy, PartialEq)]
struct DateQuery {
    year: Option<i32>,
    month: u32,
    day: Option<u32>,
}

/// 「10/11」「10-11」「10月11日」「2026/10/11」「2026年10月11日」「10月」「今天」「昨天」「前天」→ 要找的日期
fn date_query(q: &str, today: chrono::NaiveDate) -> Option<DateQuery> {
    use chrono::Datelike;
    static MD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:(\d{4})\s*[/\-.年]\s*)?(\d{1,2})\s*[/\-.月]\s*(\d{1,2})\s*[日號号]?$").unwrap());
    static M: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:(\d{4})\s*年\s*)?(\d{1,2})\s*月(?:份)?$").unwrap());
    let q = q.trim();
    let back = |days: u64| today.checked_sub_days(chrono::Days::new(days)).map(|d| DateQuery { year: Some(d.year()), month: d.month(), day: Some(d.day()) });
    match q {
        "今天" | "today" => return back(0),
        "昨天" | "yesterday" => return back(1),
        "前天" => return back(2),
        _ => {}
    }
    let (year, month, day) = match MD.captures(q) {
        Some(m) => (m.get(1).and_then(|y| y.as_str().parse().ok()), m[2].parse().ok()?, Some(m[3].parse().ok()?)),
        None => {
            let m = M.captures(q)?;
            (m.get(1).and_then(|y| y.as_str().parse().ok()), m[2].parse().ok()?, None)
        }
    };
    ((1..=12).contains(&month) && day.is_none_or(|d| (1..=31).contains(&d))).then_some(DateQuery { year, month, day })
}

/// 錄影的那一天（與清單上顯示的日期相同：檔名裡的時間，沒有時用檔案的時間）
fn entry_day(e: &LibraryEntry) -> Option<chrono::NaiveDate> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(sort_time(e) as i64).single().map(|d| d.date_naive())
}

impl DateQuery {
    fn matches(&self, d: chrono::NaiveDate) -> bool {
        use chrono::Datelike;
        self.year.is_none_or(|y| y == d.year()) && self.month == d.month() && self.day.is_none_or(|x| x == d.day())
    }
}

fn local_date(mtime_ms: f64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(mtime_ms as i64).single().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// 檔名裡的錄影時間（毫秒）：Rec_2026-10-06_08-17-18…、Shot_…；沒有時 None
fn name_time_ms(name: &str) -> Option<f64> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{4}-\d{2}-\d{2})_(\d{2})-(\d{2})-(\d{2})").unwrap());
    use chrono::TimeZone;
    RE.captures(name)
        .and_then(|m| chrono::NaiveDateTime::parse_from_str(&format!("{} {}:{}:{}", &m[1], &m[2], &m[3], &m[4]), "%Y-%m-%d %H:%M:%S").ok())
        .and_then(|t| chrono::Local.from_local_datetime(&t).earliest())
        .map(|t| t.timestamp_millis() as f64)
}

/// 排序用的時間（毫秒）：檔名裡的錄影時間（Rec_2026-10-06_08-17-18…，剪輯版沿用原片的時間），
/// 不是預設檔名時用建立或修改時間（改名後不會跳到錄完的時間）。介面依日期分組時也用這個時間，才不會同一天被拆開
fn sort_time(e: &LibraryEntry) -> f64 {
    name_time_ms(&e.media.name)
        // 同一秒的：剪輯版、_2 排在後面（修改時間較新）
        .map(|t| t + (e.media.mtime / 1e13).fract())
        .unwrap_or(e.media.time_ms())
}

pub async fn list_library(cache: &Arc<MediaCache>, ffmpeg: Option<&Path>, dir: &Path, q: &LibraryQuery) -> LibraryPage {
    let mut list = match scan_checked(cache, dir).await {
        Ok(list) => list,
        Err(e) => {
            let error = dir_error(dir, &e);
            if error.is_some() {
                crate::info!("[清單] 無法讀取資料夾 {}：{e}", dir.display());
            }
            return LibraryPage { dir: dir.display().to_string(), page: 1, pages: 1, error, ..Default::default() };
        }
    };

    if let Some(text) = q.q.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        // 也找字幕裡說過的話（影片旁邊的 SRT）
        let srts = srt_files(dir);
        let videos: std::collections::HashSet<String> = list.iter().map(|e| strip_ext(&e.media.name).to_lowercase()).collect();
        for e in list.iter_mut().filter(|e| !is_image_name(&e.media.name)) {
            e.hits = speech_hits(&srts, &videos, &e.media.name, &text);
        }
        // 也可以用日期找：「10/11」「10月11日」「今天」「昨天」
        let day = date_query(&text, chrono::Local::now().date_naive());
        list.retain(|e| {
            e.media.name.to_lowercase().contains(&text)
                || local_date(e.media.time_ms()).contains(&text)
                || day.is_some_and(|q| entry_day(e).is_some_and(|d| q.matches(d)))
                || e.exports.iter().any(|x| x.media.name.to_lowercase().contains(&text))
                || !e.hits.is_empty()
        });
    }
    if !q.audio {
        list.retain(|e| !is_audio_name(&e.media.name));
    }
    // 錄影與截圖分開列：除了「截圖」，其他篩選都只列影片
    match q.filter {
        LibraryFilter::All | LibraryFilter::Audio => list.retain(|e| !is_image_name(&e.media.name)),
        LibraryFilter::Original => list.retain(|e| !is_cut_name(&e.media.name) && !is_image_name(&e.media.name) && !is_audio_name(&e.media.name)),
        LibraryFilter::Shot => list.retain(|e| is_image_name(&e.media.name)),
        LibraryFilter::Cut => list.retain(|e| is_cut_name(&e.media.name)),
        LibraryFilter::Speed => list.retain(|e| !e.exports.is_empty()),
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
        LibrarySort::New => list.sort_by(|a, b| sort_time(b).total_cmp(&sort_time(a))),
        LibrarySort::Old => list.sort_by(|a, b| sort_time(a).total_cmp(&sort_time(b))),
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
    LibraryPage { dir: dir.display().to_string(), total, page, pages, page_size, items, error: None }
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
        return Err(Error::config(tr!("找不到這個錄影，可能已被移動或刪除", "This recording wasn't found. It may have been moved or deleted")));
    };
    let old_base = strip_ext(&entry.media.name);
    // 截圖保留原本的副檔名（png / jpg）
    let ext = if is_image_name(&entry.media.name) { entry.media.name.rsplit_once('.').map_or("png", |(_, e)| e).to_lowercase() } else { "mp4".into() };
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
            return Err(Error::config(tr!("檔案正在錄影或轉檔中，完成後才能改名", "The file is being recorded or converted. Rename it after that finishes")));
        }
        // 只改大小寫時目標就是自己（Windows 不分大小寫），不算衝突
        if to.exists() && to.display().to_string().to_lowercase() != from.to_lowercase() {
            return Err(Error::config(trf!("已有同名的檔案：{}", "A file with this name already exists: {}", file_name(&to.display().to_string()))));
        }
    }
    let mut done: Vec<(&String, &PathBuf)> = Vec::new();
    for (from, to) in &plan {
        if let Err(e) = tokio::fs::rename(from, to).await {
            for (f, t) in done.iter().rev() {
                let _ = tokio::fs::rename(t, f).await;
            }
            let busy = e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32) | Some(5));
            return Err(Error::config(if busy {
                tr!("檔案正在使用中（例如正在播放或剪輯），關閉後再試一次", "The file is in use (e.g. playing or open in the editor). Close it and try again").to_string()
            } else {
                trf!("無法改名：{e}", "Couldn't rename: {e}")
            }));
        }
        done.push((from, to));
    }
    // 檔名裡的錄影時間寫進建立時間：改名後清單仍顯示錄影的時間，不會跳到錄完的時間（MediaInfo::time_ms）
    if let Some(t) = name_time_ms(&entry.media.name) {
        set_created(&plan[0].1, t);
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
        let dir = make(&["Rec_X.mp4", "Rec_X_4x.mp4", "Rec_X_2.gif", "Rec_X_4x.webp", "Rec_X_cut.mp4", "Rec_X_cut_2.mp4", "Rec_X_cut_2.gif", "Rec_X_cut_2.webp", "Orphan.gif", "Orphan.webp"]);
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
                ("Rec_X.mp4".into(), vec!["Rec_X_2.gif:gif:1".into(), "Rec_X_4x.mp4:mp4:4".into(), "Rec_X_4x.webp:webp:4".into()]),
                ("Rec_X_cut.mp4".into(), vec![]),
                ("Rec_X_cut_2.mp4".into(), vec!["Rec_X_cut_2.gif:gif:1".into(), "Rec_X_cut_2.webp:webp:1".into()]),
            ]
        );
    }

    #[tokio::test]
    async fn search_finds_words_in_subtitles() {
        let dir = make(&["Rec_A.mp4", "Rec_A_cut.mp4", "Rec_B.mp4", "Rec_B_2.mp4", "會議.mp4"]);
        let srt = |cues: &[(f64, &str)]| {
            "\u{feff}".to_string() + &crate::subtitles::to_srt(&cues.iter().map(|(s, t)| crate::subtitles::Cue { start: *s, end: s + 2.0, text: t.to_string() }).collect::<Vec<_>>())
        };
        fs::write(dir.path().join("Rec_A.srt"), srt(&[(1.0, "今天示範怎麼剪輯"), (5.0, "接著存檔")])).unwrap();
        // 另存的第二份字幕（Rec_A 已經有 Rec_A.srt）
        fs::write(dir.path().join("Rec_A_2.srt"), srt(&[(9.0, "最後再剪輯一次")])).unwrap();
        fs::write(dir.path().join("Rec_A_cut.srt"), srt(&[(0.5, "剪輯版\n的字幕")])).unwrap();
        // Rec_B_2.srt 是 Rec_B_2.mp4 的，不是 Rec_B 的
        fs::write(dir.path().join("Rec_B_2.srt"), srt(&[(3.0, "Editing in English")])).unwrap();
        let cache = Arc::new(MediaCache::default());
        let find = |text: &str| LibraryQuery { q: Some(text.into()), ..q(None, None, LibrarySort::Old) };
        let page = list_library(&cache, None, dir.path(), &find("剪輯")).await;
        let got: Vec<(String, Vec<(f64, String)>)> = page.items.iter().map(|e| (e.media.name.clone(), e.hits.iter().map(|h| (h.start, h.text.clone())).collect())).collect();
        assert_eq!(got, vec![("Rec_A.mp4".into(), vec![(1.0, "今天示範怎麼剪輯".into()), (9.0, "最後再剪輯一次".into())]), ("Rec_A_cut.mp4".into(), vec![(0.5, "剪輯版 的字幕".into())]),]);
        // 不分大小寫；檔名符合的也列出（沒有說過的話）
        let page = list_library(&cache, None, dir.path(), &find("editing")).await;
        assert_eq!(page.items.iter().map(|e| (e.media.name.as_str(), e.hits.len())).collect::<Vec<_>>(), vec![("Rec_B_2.mp4", 1)]);
        let page = list_library(&cache, None, dir.path(), &find("會議")).await;
        assert_eq!(page.items.iter().map(|e| (e.media.name.as_str(), e.hits.len())).collect::<Vec<_>>(), vec![("會議.mp4", 0)]);
        // 沒有搜尋：不讀字幕
        assert!(list_library(&cache, None, dir.path(), &q(None, None, LibrarySort::Old)).await.items.iter().all(|e| e.hits.is_empty()));
    }

    #[test]
    fn dates_in_the_search_box() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let q = |t: &str| date_query(t, today);
        let md = |m, d| Some(DateQuery { year: None, month: m, day: Some(d) });
        assert_eq!(q("10/11"), md(10, 11));
        assert_eq!(q("10-11"), md(10, 11));
        assert_eq!(q("10月11日"), md(10, 11));
        assert_eq!(q("10 月 11 號"), md(10, 11));
        assert_eq!(q("2026/10/11"), Some(DateQuery { year: Some(2026), month: 10, day: Some(11) }));
        assert_eq!(q("2026年10月11日"), Some(DateQuery { year: Some(2026), month: 10, day: Some(11) }));
        assert_eq!(q("10月"), Some(DateQuery { year: None, month: 10, day: None }));
        assert_eq!(q("今天"), Some(DateQuery { year: Some(2026), month: 10, day: Some(13) }));
        assert_eq!(q("昨天"), Some(DateQuery { year: Some(2026), month: 10, day: Some(12) }));
        assert_eq!(q("13/40"), None);
        assert_eq!(q("會議"), None);
        assert!(q("10/11").unwrap().matches(chrono::NaiveDate::from_ymd_opt(2025, 10, 11).unwrap()));
        assert!(!q("2026/10/11").unwrap().matches(chrono::NaiveDate::from_ymd_opt(2025, 10, 11).unwrap()));
    }

    #[tokio::test]
    async fn search_by_date() {
        let dir = make(&["Rec_2026-10-11_09-00-00.mp4", "Rec_2026-10-12_09-00-00.mp4", "Shot_2026-10-11_10-00-00.png"]);
        let cache = Arc::new(MediaCache::default());
        let find = |text: &str| LibraryQuery { q: Some(text.into()), ..q(None, None, LibrarySort::Old) };
        for t in ["10/11", "10月11日", "2026/10/11"] {
            let page = list_library(&cache, None, dir.path(), &find(t)).await;
            assert_eq!(page.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_2026-10-11_09-00-00.mp4"], "{t}");
        }
        let shots = list_library(&cache, None, dir.path(), &LibraryQuery { filter: LibraryFilter::Shot, ..find("10/11") }).await;
        assert_eq!(shots.items.len(), 1);
    }

    #[tokio::test]
    async fn audio_files_are_listed_on_their_own() {
        let dir = make(&["Rec_X.mp4", "Rec_X.m4a", "Shot_2026-10-09_14-30-00.png"]);
        let cache = Arc::new(MediaCache::default());
        let all = list_library(&cache, None, dir.path(), &LibraryQuery { audio: true, ..q(None, None, LibrarySort::Old) }).await;
        let mut names: Vec<(&str, usize)> = all.items.iter().map(|e| (e.media.name.as_str(), e.exports.len())).collect();
        names.sort();
        assert_eq!(names, vec![("Rec_X.m4a", 0), ("Rec_X.mp4", 0)]);
        // 主畫面的最近錄影不列聲音檔
        let recent = list_library(&cache, None, dir.path(), &q(None, None, LibrarySort::Old)).await;
        assert_eq!(recent.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_X.mp4"]);
        // 「原始錄影」不含聲音檔
        let originals = list_library(&cache, None, dir.path(), &LibraryQuery { filter: LibraryFilter::Original, ..q(None, None, LibrarySort::Old) }).await;
        assert_eq!(originals.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_X.mp4"]);
    }

    #[tokio::test]
    async fn unreachable_folder_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(MediaCache::default());
        // 還沒建立的資料夾（預設的儲存位置還沒錄過影）：當作空的
        let page = list_library(&cache, None, &dir.path().join("Timelapse"), &q(None, None, LibrarySort::New)).await;
        assert_eq!((page.total, page.error), (0, None));
        // 上層也不在（隨身碟、網路磁碟沒接上）：說明找不到
        let page = list_library(&cache, None, &dir.path().join("USB").join("Timelapse"), &q(None, None, LibrarySort::New)).await;
        assert_eq!(page.total, 0);
        assert!(page.error.unwrap().contains("找不到資料夾"));
    }

    fn entry(n: usize) -> LibraryEntry {
        LibraryEntry { media: MediaInfo { name: "a.mp4".into(), ..Default::default() }, exports: vec![ExportInfo { speed: 4.0, ..Default::default() }; n], hits: vec![] }
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
        let cache = Arc::new(cache);
        let videos = list_library(&cache, None, dir.path(), &q(None, None, LibrarySort::New)).await;
        assert_eq!(videos.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_X.mp4"]);
        let shots = list_library(&cache, None, dir.path(), &LibraryQuery { filter: LibraryFilter::Shot, ..q(None, None, LibrarySort::New) }).await;
        assert_eq!(shots.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Shot_2026-10-09_14-30-00.png"]);
        // 改名保留 .png
        let src = dir.path().join("Shot_2026-10-09_14-30-00.png").display().to_string();
        let out = rename_recording(&MediaCache::default(), &src, "登入畫面", &[]).await.unwrap();
        assert!(out.ends_with("登入畫面.png"), "{out}");
    }

    /// 截圖編輯另存的 JPG 也列在全部截圖，改名保留 .jpg
    #[tokio::test]
    async fn jpg_screenshots_are_listed_and_keep_extension() {
        let dir = make(&["Shot_2026-10-09_14-30-00.png", "登入畫面_編輯.jpg", "Rec_X.mp4"]);
        let cache = Arc::new(MediaCache::default());
        let shots = list_library(&cache, None, dir.path(), &LibraryQuery { filter: LibraryFilter::Shot, ..q(None, None, LibrarySort::Old) }).await;
        let mut names: Vec<&str> = shots.items.iter().map(|e| e.media.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["Shot_2026-10-09_14-30-00.png", "登入畫面_編輯.jpg"]);
        let videos = list_library(&cache, None, dir.path(), &q(None, None, LibrarySort::Old)).await;
        assert_eq!(videos.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), vec!["Rec_X.mp4"]);
        let src = dir.path().join("登入畫面_編輯.jpg").display().to_string();
        let out = rename_recording(&MediaCache::default(), &src, "登入步驟", &[]).await.unwrap();
        assert!(out.ends_with("登入步驟.jpg"), "{out}");
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

    #[test]
    fn time_of_undated_names() {
        let m = |mtime: f64, created: f64| MediaInfo { mtime, created, ..Default::default() };
        // 改過名的錄影：建立時間（錄影時間）比修改時間（錄完）早
        assert_eq!(m(2000.0, 1000.0).time_ms(), 1000.0);
        // 從別處複製來的：建立時間是複製的時候，用修改時間
        assert_eq!(m(1000.0, 5000.0).time_ms(), 1000.0);
        assert_eq!(m(1000.0, 0.0).time_ms(), 1000.0);
        assert!(name_time_ms("Rec_2026-10-11_16-20-00_cut.mp4").is_some());
        assert_eq!(name_time_ms("操作示範.mp4"), None);
    }

    /// 改名後清單仍顯示錄影的時間（檔名裡的時間寫進建立時間），不會跳到錄完的時間
    #[tokio::test]
    async fn rename_keeps_recording_time() {
        let dir = make(&["Rec_2026-10-11_16-20-00.mp4"]);
        let src = dir.path().join("Rec_2026-10-11_16-20-00.mp4");
        let start = name_time_ms("Rec_2026-10-11_16-20-00.mp4").unwrap();
        // 錄了 14 分鐘：修改時間是錄完的時間
        let end = std::time::UNIX_EPOCH + Duration::from_millis(start as u64 + 14 * 60_000);
        fs::File::options().write(true).open(&src).unwrap().set_modified(end).unwrap();
        let cache = MediaCache::default();
        rename_recording(&cache, &src.display().to_string(), "會議", &[]).await.unwrap();
        let list = scan(&cache, dir.path()).await;
        assert_eq!(list[0].media.name, "會議.mp4");
        // 只有 Windows 能改建立時間
        if cfg!(windows) {
            assert!((sort_time(&list[0]) - start).abs() < 1.0, "{} != {start}", sort_time(&list[0]));
        }
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
