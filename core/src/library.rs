//! 錄影清單：加速版歸到原始錄影底下，支援搜尋、篩選、排序、分頁、改名。
//! 讀取影片資訊（長度、解析度、有無聲音）要執行 FFmpeg，只對需要的檔案做，並快取結果。

use crate::args::{measure_duration_args, parse_measured_duration, parse_media_info};
use crate::error::{Error, Result};
use crate::format::{check_recording_name, parse_export_name, strip_mp4};
use crate::paths::{created_ms, mtime_ms, set_created};
use crate::process::run;
use crate::types::{DayGroup, ExportFormat, ExportInfo, GridFit, LibraryEntry, LibraryFilter, LibraryPage, LibraryQuery, LibrarySort, MediaInfo, LIBRARY_ROW_MAIN_PX, LIBRARY_ROW_SUB_PX};
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
                    folder: String::new(),
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
/// 其他格式的影片（MOV、MKV…，見 import.rs）也列出來，可以播放、轉成 MP4
static VIDEO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(mp4|gif|webp|png|jpe?g|m4a|mov|mkv|avi|wmv|webm|m4v|flv|mpg|mpeg|ts|3gp)$").unwrap());
/// 只有聲音（「存成 M4A」做出來的）：清單上單獨一筆，標示「只有聲音」
static AUDIO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.m4a$").unwrap());
static IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(png|jpe?g)$").unwrap());

/// 截圖（PNG，或截圖編輯另存的 JPG）：清單上單獨一筆，沒有加速版
pub fn is_image_name(name: &str) -> bool {
    IMAGE_RE.is_match(name)
}

/// 去掉副檔名（.mp4 / .png / .m4a）
fn strip_ext(name: &str) -> String {
    if is_image_name(name) || is_audio_name(name) || crate::import::is_other_video(name) {
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
            // 已經轉成 MP4 的其他格式（同名的 .mp4 在旁邊）：只列 MP4
            _ if crate::import::is_other_video(&f.name) && by_name.contains_key(&format!("{}.mp4", strip_ext(&f.name)).to_lowercase()) => {}
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

/// 子資料夾最多往下讀幾層、最多幾個（儲存位置設在整顆磁碟或網路磁碟時不會讀很久）
pub const SUBDIR_DEPTH: usize = 3;
const SUBDIR_MAX: usize = 300;

/// 不列出的子資料夾：程式自己用的（.parts 錄影中的片段、markers 標記與游標紀錄）、系統的（資源回收筒等）
fn skip_subdir(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with('.') || n.starts_with('$') || n == "markers" || n == "system volume information" || n == "node_modules"
}

/// root 底下的子資料夾（不含 root；最多 SUBDIR_DEPTH 層、SUBDIR_MAX 個；不跟著捷徑 / 連結）：路徑與相對路徑（例如「教學\2026」）
pub fn subdirs_of(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = vec![];
    let mut level: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    for _ in 0..SUBDIR_DEPTH {
        let mut next = vec![];
        for (dir, rel) in &level {
            let Ok(rd) = std::fs::read_dir(dir) else { continue };
            let mut subs: Vec<(PathBuf, String)> = rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    (!skip_subdir(&name)).then(|| (e.path(), if rel.is_empty() { name.clone() } else { format!("{rel}{}{name}", std::path::MAIN_SEPARATOR) }))
                })
                .collect();
            subs.sort_by_key(|a| a.1.to_lowercase());
            for s in subs {
                if out.len() >= SUBDIR_MAX {
                    return out;
                }
                out.push(s.clone());
                next.push(s);
            }
        }
        level = next;
    }
    out
}

/// 這些資料夾，subdirs 時再加上各自的子資料夾（同一個資料夾只列一次）
pub async fn expand_dirs(dirs: Vec<String>, subdirs: bool) -> Vec<String> {
    if !subdirs {
        return dirs;
    }
    tokio::task::spawn_blocking(move || {
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for d in dirs {
            let subs = subdirs_of(Path::new(&d));
            for p in std::iter::once(d).chain(subs.into_iter().map(|(p, _)| p.display().to_string())) {
                if seen.insert(p.trim_end_matches(['\\', '/']).to_lowercase()) {
                    out.push(p);
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default()
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
    // 截圖只要寬高：讀檔頭就好，不必啟動 FFmpeg
    if is_image_name(&e.media.name) {
        let path = PathBuf::from(&e.media.path);
        let size = tokio::task::spawn_blocking(move || image::image_dimensions(path).ok()).await.ok().flatten();
        let mut e = e;
        if let Some((w, h)) = size {
            (e.media.width, e.media.height) = (Some(w), Some(h));
        }
        return e;
    }
    let Some(ff) = ffmpeg else { return e };
    let folder = e.media.folder.clone();
    let mut media = cache.probe(ff, &e.media.path).await.unwrap_or(e.media);
    media.folder = folder;
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
pub fn name_time_ms(name: &str) -> Option<f64> {
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

fn same_dir(a: &Path, b: &Path) -> bool {
    a.display().to_string().trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.display().to_string().trim_end_matches(['\\', '/']))
}

/// 記下在哪個子資料夾（加速版、GIF 也一樣）
fn in_folder(mut e: LibraryEntry, rel: &str) -> LibraryEntry {
    e.media.folder = rel.to_string();
    for x in &mut e.exports {
        x.media.folder = rel.to_string();
    }
    e
}

fn list_has(list: &[LibraryEntry], path: &str) -> bool {
    list.iter().any(|e| e.media.path.eq_ignore_ascii_case(path))
}

pub async fn list_library(cache: &Arc<MediaCache>, ffmpeg: Option<&Path>, dir: &Path, q: &LibraryQuery) -> LibraryPage {
    list_library_with(cache, ffmpeg, dir, q, None).await
}

/// 同上；shots：截圖的文字索引與分類資料（搜尋時也找圖裡的字、中英對照；瀏覽欄的條件與張數）
pub async fn list_library_with(cache: &Arc<MediaCache>, ffmpeg: Option<&Path>, dir: &Path, q: &LibraryQuery, shots: Option<&crate::facets::Sources<'_>>) -> LibraryPage {
    let ocr = shots.map(|s| s.ocr);
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
    // 讀過的資料夾（找字幕用）
    let mut scanned: Vec<PathBuf> = vec![dir.to_path_buf()];
    // 子資料夾：一起列出，記下在哪個子資料夾（讀不到的略過）
    if q.subdirs {
        let root = dir.to_path_buf();
        let subs = tokio::task::spawn_blocking(move || subdirs_of(&root)).await.unwrap_or_default();
        for (sub, rel) in subs {
            if let Ok(more) = scan_checked(cache, &sub).await {
                list.extend(more.into_iter().map(|e| in_folder(e, &rel)));
                scanned.push(sub);
            }
        }
    }
    // 截圖：也列出留在其他資料夾的舊截圖（讀不到的資料夾略過）
    if q.filter == LibraryFilter::Shot {
        for extra in q.extra_dirs.iter().map(Path::new).filter(|d| !same_dir(d, dir)) {
            let mut dirs: Vec<(PathBuf, String)> = vec![(extra.to_path_buf(), String::new())];
            if q.subdirs {
                let root = extra.to_path_buf();
                dirs.extend(tokio::task::spawn_blocking(move || subdirs_of(&root)).await.unwrap_or_default());
            }
            for (d, rel) in dirs {
                if let Ok(more) = scan_checked(cache, &d).await {
                    let add: Vec<LibraryEntry> = more.into_iter().filter(|e| is_image_name(&e.media.name) && !list_has(&list, &e.media.path)).map(|e| in_folder(e, &rel)).collect();
                    list.extend(add);
                }
            }
        }
    }

    if let Some(text) = q.q.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        // 也找字幕裡說過的話（影片旁邊的 SRT；子資料夾裡的也找）
        let mut srts: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for d in &scanned {
            for (k, v) in srt_files(d) {
                srts.entry(k).or_default().extend(v);
            }
        }
        let videos: std::collections::HashSet<String> = list.iter().map(|e| strip_ext(&e.media.name).to_lowercase()).collect();
        for e in list.iter_mut().filter(|e| !is_image_name(&e.media.name)) {
            e.hits = speech_hits(&srts, &videos, &e.media.name, &text);
        }
        // 也可以用日期找：「10/11」「10月11日」「今天」「昨天」
        let day = date_query(&text, chrono::Local::now().date_naive());
        // 截圖裡的字（文字索引）：符合時加一筆說明，清單上標出「圖裡有這段字」
        let norm = crate::ocr_index::normalize(&text);
        // 也找中英對照的詞（打「錯誤」也找得到 error）
        let alts: Vec<String> = std::iter::once(norm.clone()).chain(crate::facets::synonyms(&norm)).collect();
        if let Some(o) = ocr {
            for e in list.iter_mut().filter(|e| is_image_name(&e.media.name)) {
                if alts.iter().any(|a| o.matches(&e.media.path, a)) {
                    e.hits = vec![crate::types::SpeechHit { start: -1.0, text: text.clone() }];
                }
            }
        }
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

    // 瀏覽欄的條件（只有截圖）：篩選並算出每個選項的張數
    let facets = match (shots, &q.facet) {
        (Some(src), Some(fq)) if q.filter == LibraryFilter::Shot => {
            let prep = crate::facets::prepare(src);
            let items: Vec<crate::facets::Facets> = list
                .iter()
                .map(|e| crate::facets::Facets {
                    folder: e.media.folder.clone(),
                    ..crate::facets::describe(&prep, &e.media.path, &e.media.name, sort_time(e), e.media.width.unwrap_or(0), e.media.height.unwrap_or(0))
                })
                .collect();
            let (keep, counts) = crate::facets::filter_and_count(&items, fq, &prep);
            let mut it = keep.into_iter();
            list.retain(|_| it.next().unwrap_or(true));
            Some(counts)
        }
        _ => None,
    };

    if let Some(only) = &q.only {
        let set: std::collections::HashSet<String> = only.iter().map(|p| p.to_lowercase()).collect();
        list.retain(|e| set.contains(&e.media.path.to_lowercase()) || e.exports.iter().any(|x| set.contains(&x.media.path.to_lowercase())));
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
    let all_paths: Vec<String> = list.iter().flat_map(|e| std::iter::once(e.media.path.clone()).chain(e.exports.iter().map(|x| x.media.path.clone()))).collect();
    let clamp_page = |pages: usize| {
        let p = q.page.filter(|p| p.is_finite()).map(f64::floor).unwrap_or(1.0);
        (if p >= 1.0 { p as usize } else { 1 }).min(pages)
    };
    let days: Vec<String> = list.iter().map(|e| entry_day(e).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()).collect();
    let grid = q.grid.filter(|g| g.cols > 0 && g.height.is_finite() && g.row_h > 0.0);
    let start;
    let (pages, page, slice, page_size) = match (grid, q.fit_px.filter(|f| f.is_finite())) {
        (Some(g), _) => {
            let places = grid_layout(&days, g);
            let pages = places.last().map_or(1, |p| p.page + 1);
            let page = match q.anchor.filter(|a| *a < places.len()) {
                Some(a) => places[a].page + 1,
                None => clamp_page(pages),
            };
            let idx: Vec<usize> = places.iter().enumerate().filter(|(_, p)| p.page + 1 == page).map(|(i, _)| i).collect();
            start = idx.first().copied().unwrap_or(0);
            let slice: Vec<LibraryEntry> = idx.iter().map(|i| list[*i].clone()).collect();
            let n = slice.len();
            (pages, page, slice, n)
        }
        (None, Some(fit)) => {
            let starts = page_starts(&list, fit.max(LIBRARY_ROW_MAIN_PX));
            let pages = starts.len();
            let page = clamp_page(pages);
            let end = starts.get(page).copied().unwrap_or(list.len());
            start = starts[page - 1];
            let slice: Vec<LibraryEntry> = list[start..end].to_vec();
            let n = slice.len();
            (pages, page, slice, n)
        }
        (None, None) => {
            let ps = q.page_size.filter(|p| p.is_finite()).map(f64::floor).unwrap_or(30.0).clamp(1.0, 200.0) as usize;
            let pages = total.div_ceil(ps).max(1);
            let page = clamp_page(pages);
            start = (page - 1) * ps;
            let slice: Vec<LibraryEntry> = list.iter().skip(start).take(ps).cloned().collect();
            (pages, page, slice, ps)
        }
    };
    // 依日期分組：這一頁出現的每一天，連同那一天全部的檔案（選這一天用）
    let groups = if grid.is_some() {
        let mut out: Vec<DayGroup> = vec![];
        for i in 0..slice.len() {
            let day = &days[start + i];
            if i > 0 && &days[start + i - 1] == day {
                continue;
            }
            let all: Vec<usize> = (0..list.len()).filter(|j| &days[*j] == day).collect();
            let paths = all.iter().flat_map(|j| std::iter::once(list[*j].media.path.clone()).chain(list[*j].exports.iter().map(|x| x.media.path.clone()))).collect();
            out.push(DayGroup { day: day.clone(), first: i, total: all.len(), paths, continued: i == 0 && start > 0 && &days[start - 1] == day });
        }
        out
    } else {
        vec![]
    };
    let items = if need_all { slice } else { pool_with_info(cache, ffmpeg, slice, 6).await };
    LibraryPage { dir: dir.display().to_string(), total, page, pages, page_size, items, all_paths, start, groups, facets, error: None }
}

/// 清理舊截圖：這些資料夾裡在 cutoff_ms（毫秒）之前的截圖，由舊到新；同一個檔案只列一次
/// 這些資料夾裡全部的截圖與時間（檔名或修改時間；同一個檔案只算一次），由舊到新
pub async fn shots_with_time(cache: &MediaCache, dirs: &[String]) -> Vec<(MediaInfo, f64)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(MediaInfo, f64)> = vec![];
    for d in dirs {
        let Ok(list) = scan_checked(cache, Path::new(d)).await else { continue };
        for e in list {
            if is_image_name(&e.media.name) && seen.insert(e.media.path.to_lowercase()) {
                let t = sort_time(&e);
                out.push((e.media, t));
            }
        }
    }
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

pub async fn shots_before(cache: &MediaCache, dirs: &[String], cutoff_ms: f64) -> Vec<MediaInfo> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(f64, MediaInfo)> = vec![];
    for d in dirs {
        let Ok(list) = scan_checked(cache, Path::new(d)).await else { continue };
        for e in list {
            let t = sort_time(&e);
            if is_image_name(&e.media.name) && t < cutoff_ms && seen.insert(e.media.path.to_lowercase()) {
                out.push((t, e.media));
            }
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, m)| m).collect()
}

/// 截圖格的一格放在哪裡：第幾頁（從 0 算）、這一頁裡的 y（px，卡片上緣）、第幾欄、要不要在上面畫日期標題（標題列上緣的 y）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPlace {
    pub page: usize,
    pub y: f64,
    pub col: usize,
    pub head_y: Option<f64>,
}

/// 依日期分組排截圖格：每天從新的一列開始、上面有標題列；放不下就換頁（新的一頁開頭再畫一次那天的標題）。
/// days：每一筆的日期（同一天的要相鄰）。介面排版與分頁都用這個，兩邊才會一致
pub fn grid_layout(days: &[String], g: GridFit) -> Vec<GridPlace> {
    let cols = g.cols.max(1);
    let mut out = Vec::with_capacity(days.len());
    let (mut page, mut used, mut col, mut row_y) = (0usize, 0.0f64, cols, 0.0f64);
    for (i, d) in days.iter().enumerate() {
        let page_start = used == 0.0;
        let new_day = i == 0 || &days[i - 1] != d;
        // 這一筆需要：新的一天（或新的一頁）先放標題列＋新的一列；同一天放滿一列就換列
        let head = new_day || page_start;
        let new_row = head || col >= cols;
        let need = if head { g.head_h } else { 0.0 } + if new_row { g.row_h } else { 0.0 };
        if !page_start && used + need > g.height + 0.5 {
            page += 1;
            used = 0.0;
        }
        let page_start = used == 0.0;
        let head = new_day || page_start;
        let new_row = head || col >= cols;
        let mut head_y = None;
        if head {
            head_y = Some(used);
            used += g.head_h;
        }
        if new_row {
            row_y = used;
            used += g.row_h;
            col = 0;
        }
        out.push(GridPlace { page, y: row_y, col, head_y });
        col += 1;
    }
    out
}

/// 錄影改名：原片與底下的加速版 / GIF 一起改（Rec_X_4x.mp4 → 新名_4x.mp4），清單上才不會斷開。
/// 任何一個失敗就把已改的改回去。busy = 正在錄影 / 轉檔的檔案（小寫完整路徑）。回傳新的完整路徑。
/// 跟著錄影的檔案：字幕 stem.srt、stem_2.srt、stem.en.srt，只有聲音的 stem.m4a；回傳 (路徑, 名稱 stem 後面的部分)。
/// stem_2.srt 是另一支錄影 stem_2.mp4 的字幕時不算
fn sidecars(dir: &Path, stem: &str) -> Vec<(String, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^((_\d+)?(\.[a-z]{2}(-[a-z]{2,4})?)?\.srt|\.m4a)$").unwrap());
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let lower = stem.to_lowercase();
    let mut out = vec![];
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.to_lowercase().starts_with(&lower) || !name.is_char_boundary(stem.len()) {
            continue;
        }
        let rest = &name[stem.len()..];
        if !RE.is_match(rest) {
            continue;
        }
        if let Some(n) = rest.strip_prefix('_').and_then(|r| r.split('.').next()) {
            if dir.join(format!("{stem}_{n}.mp4")).exists() {
                continue;
            }
        }
        out.push((e.path().display().to_string(), rest.to_string()));
    }
    out.sort();
    out
}

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
    // 截圖、其他格式的影片保留原本的副檔名（png / jpg、mov / mkv…）
    let ext =
        if is_image_name(&entry.media.name) || crate::import::is_other_video(&entry.media.name) { entry.media.name.rsplit_once('.').map_or("png", |(_, e)| e).to_lowercase() } else { "mp4".into() };
    if base == old_base {
        return Ok(entry.media.path);
    }
    let old_len = old_base.chars().count();
    let mut plan: Vec<(String, PathBuf)> = vec![(entry.media.path.clone(), dir.join(format!("{base}.{ext}")))];
    for x in &entry.exports {
        let suffix: String = x.media.name.chars().skip(old_len).collect();
        plan.push((x.media.path.clone(), dir.join(format!("{base}{suffix}"))));
    }
    // 字幕（搜尋說過的話用它）、只有聲音的 M4A 一起改
    if !is_image_name(&entry.media.name) {
        for (from, suffix) in sidecars(&dir, &old_base) {
            plan.push((from, dir.join(format!("{base}{suffix}"))));
        }
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

// ───────────── 截圖搬到截圖資料夾 ─────────────

/// 資料夾裡（不含子資料夾）的截圖（PNG / JPG）
pub fn images_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<PathBuf> =
        rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(is_image_name)).collect();
    v.sort();
    v
}

/// 搬截圖的結果
#[derive(Debug, Default)]
pub struct MoveReport {
    /// 搬好的：(原本的路徑, 新的路徑)
    pub moved: Vec<(String, String)>,
    /// 沒搬成的：(檔名, 原因)；原檔都還在
    pub failed: Vec<(String, String)>,
}

/// 新資料夾裡沒人用的檔名：已經有同名的就加 _2、_3…
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = name.rsplit_once('.').map_or((name, ""), |(a, b)| (a, b));
    (2..).map(|i| dir.join(if ext.is_empty() { format!("{stem}_{i}") } else { format!("{stem}_{i}.{ext}") })).find(|p| !p.exists()).unwrap_or(first)
}

/// 搬一個檔案：同一顆磁碟直接改位置；跨磁碟時先複製（保留時間）再刪原檔，原檔刪不掉就把複製的刪掉、算失敗
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let meta = std::fs::metadata(from)?;
    std::fs::copy(from, to)?;
    let copied = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0);
    if copied != meta.len() {
        let _ = std::fs::remove_file(to);
        return Err(std::io::Error::other(tr!("複製後大小不符", "The copy doesn't match the original size")));
    }
    if let Ok(t) = meta.modified() {
        if let Ok(f) = std::fs::File::options().write(true).open(to) {
            let _ = f.set_modified(t);
        }
    }
    set_created(to, created_ms(&meta));
    if let Err(e) = std::fs::remove_file(from) {
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    Ok(())
}

/// 把 from 資料夾裡的截圖（PNG / JPG，不含子資料夾）搬到 to；同名的加 _2，不覆蓋。一張失敗不影響其他張
pub fn move_images(from: &Path, to: &Path) -> MoveReport {
    let mut r = MoveReport::default();
    if same_dir(from, to) {
        return r;
    }
    let files = images_in(from);
    if files.is_empty() {
        return r;
    }
    if let Err(e) = std::fs::create_dir_all(to) {
        r.failed = files.iter().map(|f| (f.file_name().unwrap_or_default().to_string_lossy().into_owned(), e.to_string())).collect();
        return r;
    }
    for f in files {
        let name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let dest = free_name(to, &name);
        match move_file(&f, &dest) {
            Ok(()) => r.moved.push((f.display().to_string(), dest.display().to_string())),
            Err(e) => r.failed.push((name, e.to_string())),
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sidecars_follow_the_recording_but_not_other_recordings() {
        let d = make(&["Rec_a.mp4", "Rec_a.srt", "Rec_a_2.srt", "Rec_a.en.srt", "Rec_a.m4a", "Rec_a_3.mp4", "Rec_a_3.srt", "Rec_ab.srt", "Rec_a.txt"]);
        let got: Vec<String> = sidecars(d.path(), "Rec_a").into_iter().map(|(_, rest)| rest).collect();
        assert_eq!(got, vec![".en.srt", ".m4a", ".srt", "_2.srt"]);
    }

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
    async fn subfolders_are_listed_with_their_folder() {
        let dir = make(&["Rec_A.mp4", "Rec_A.srt"]);
        let sub = dir.path().join("教學").join("2026");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("Rec_B.mp4"), "x").unwrap();
        fs::write(sub.join("Rec_B_4x.mp4"), "x").unwrap();
        fs::write(sub.join("Rec_B.srt"), "1\n00:00:01,000 --> 00:00:02,000\n報價單\n").unwrap();
        // 程式自己的資料夾、太深的資料夾不列
        for skip in [".parts", "markers", "$RECYCLE.BIN"] {
            fs::create_dir_all(dir.path().join(skip)).unwrap();
            fs::write(dir.path().join(skip).join("Rec_X.mp4"), "x").unwrap();
        }
        let deep = dir.path().join("a").join("b").join("c").join("d");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Rec_Deep.mp4"), "x").unwrap();
        let cache = Arc::new(MediaCache::default());
        // 沒開：只有最上層
        let page = list_library(&cache, None, dir.path(), &q(None, None, LibrarySort::Old)).await;
        assert_eq!(page.items.iter().map(|e| e.media.name.as_str()).collect::<Vec<_>>(), ["Rec_A.mp4"]);
        let page = list_library(&cache, None, dir.path(), &LibraryQuery { subdirs: true, ..q(None, None, LibrarySort::Old) }).await;
        let got: Vec<(&str, &str)> = page.items.iter().map(|e| (e.media.name.as_str(), e.media.folder.as_str())).collect();
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(got, [("Rec_A.mp4", ""), ("Rec_B.mp4", format!("教學{sep}2026").as_str())]);
        // 加速版跟著原片、也記下子資料夾
        assert_eq!(page.items[1].exports.len(), 1);
        assert_eq!(page.items[1].exports[0].media.folder, format!("教學{sep}2026"));
        // 子資料夾裡的字幕也找得到
        let found = list_library(&cache, None, dir.path(), &LibraryQuery { subdirs: true, q: Some("報價單".into()), ..q(None, None, LibrarySort::Old) }).await;
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.items[0].media.name, "Rec_B.mp4");
        // 展開資料夾：最上層、子資料夾（最多 3 層），不含程式自己的資料夾
        // 比對相對於最上層的路徑：暫存資料夾的名稱是隨機的（可能剛好以 d 結尾）
        let all = expand_dirs(vec![dir.path().display().to_string()], true).await;
        let rel: Vec<String> = all.iter().map(|d| Path::new(d).strip_prefix(dir.path()).unwrap().display().to_string()).collect();
        let abc = ["a", "b", "c"].join(&sep.to_string());
        assert!(rel.contains(&String::new()));
        assert!(rel.contains(&format!("教學{sep}2026")));
        assert!(rel.contains(&abc));
        assert!(!rel.iter().any(|d| d.ends_with(&format!("{sep}d")) || d.contains(".parts") || d.contains("markers") || d.contains("RECYCLE")));
        assert_eq!(expand_dirs(vec!["x".into()], false).await, vec!["x".to_string()]);
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

    /// 「選取全部」：all_paths 是每一頁的全部（含加速版）；「只看已選取」：only 只列那些檔案
    #[tokio::test]
    async fn all_paths_cover_every_page_and_only_filters() {
        let dir = make(&["Rec_A.mp4", "Rec_A_4x.mp4", "Rec_B.mp4", "Rec_C.mp4"]);
        let cache = Arc::new(MediaCache::default());
        let page = list_library(&cache, None, dir.path(), &LibraryQuery { page_size: Some(1.0), ..q(None, None, LibrarySort::Old) }).await;
        assert_eq!((page.items.len(), page.total, page.pages), (1, 3, 3));
        let names: Vec<String> = page.all_paths.iter().map(|p| file_name(p)).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"Rec_A_4x.mp4".to_string()) && names.contains(&"Rec_C.mp4".to_string()), "{names:?}");
        // 只看已選取：選了加速版也列出原片
        let only = vec![dir.path().join("Rec_A_4x.mp4").display().to_string(), dir.path().join("Rec_C.mp4").display().to_string()];
        let picked = list_library(&cache, None, dir.path(), &LibraryQuery { only: Some(only), ..q(None, None, LibrarySort::Old) }).await;
        let mut got: Vec<&str> = picked.items.iter().map(|e| e.media.name.as_str()).collect();
        got.sort();
        assert_eq!(got, vec!["Rec_A.mp4", "Rec_C.mp4"]);
    }

    #[test]
    fn grid_layout_groups_days_and_pages() {
        let d = |s: &str, n: usize| vec![s.to_string(); n];
        let days: Vec<String> = [d("07-27", 5), d("07-26", 2), d("07-25", 4)].concat();
        // 每列 3 格；標題 20、列高 100、可用 360：一頁放「標題＋3 列」（20+300=320）或「標題＋列＋標題＋列」…
        let g = GridFit { cols: 3, height: 360.0, row_h: 100.0, head_h: 20.0 };
        let p = grid_layout(&days, g);
        // 07-27：標題 0，第 1 列 y=20（3 格）、第 2 列 y=120（2 格）
        assert_eq!((p[0].page, p[0].y, p[0].col, p[0].head_y), (0, 20.0, 0, Some(0.0)));
        assert_eq!((p[3].y, p[3].col, p[3].head_y), (120.0, 0, None));
        // 07-26：新的一天，標題 220、列 240；放得下（340 ≤ 360）
        assert_eq!((p[5].page, p[5].y, p[5].head_y), (0, 240.0, Some(220.0)));
        // 07-25：再放標題＋列要到 460，換頁；新的一頁開頭重畫標題
        assert_eq!((p[7].page, p[7].y, p[7].head_y), (1, 20.0, Some(0.0)));
        assert_eq!((p[10].page, p[10].y, p[10].col), (1, 120.0, 0));
    }

    #[tokio::test]
    async fn old_shots_by_name_time() {
        let dir = make(&["Shot_2020-01-02_10-00-00.png", "Shot_2020-01-01_10-00-00.png", "Shot_2099-01-01_10-00-00.png", "Rec_2020-01-01_10-00-00.mp4"]);
        let cache = MediaCache::default();
        let cutoff = chrono::NaiveDate::from_ymd_opt(2021, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis() as f64;
        let d = dir.path().display().to_string();
        let old = shots_before(&cache, &[d.clone(), d], cutoff).await;
        let names: Vec<&str> = old.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["Shot_2020-01-01_10-00-00.png", "Shot_2020-01-02_10-00-00.png"], "由舊到新、不含錄影與新的、資料夾重複只算一次");
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
    #[test]
    fn move_images_keeps_names_and_skips_videos() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for n in ["Shot_2026-10-14_10-00-00.png", "Shot_2026-10-14_10-00-00_編輯.png", "photo.JPG", "Rec_2026-10-14_10-00-00.mp4"] {
            std::fs::write(a.path().join(n), n).unwrap();
        }
        // 新資料夾已有同名的：加 _2，不覆蓋
        std::fs::write(b.path().join("photo.JPG"), "old").unwrap();
        std::fs::create_dir(a.path().join("教學_2026")).unwrap();
        let r = move_images(a.path(), b.path());
        assert_eq!(r.moved.len(), 3);
        assert!(r.failed.is_empty());
        assert!(b.path().join("Shot_2026-10-14_10-00-00.png").is_file());
        assert!(b.path().join("Shot_2026-10-14_10-00-00_編輯.png").is_file());
        assert_eq!(std::fs::read_to_string(b.path().join("photo.JPG")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(b.path().join("photo_2.JPG")).unwrap(), "photo.JPG");
        // 影片與子資料夾不動
        assert!(a.path().join("Rec_2026-10-14_10-00-00.mp4").is_file());
        assert!(a.path().join("教學_2026").is_dir());
        assert!(images_in(a.path()).is_empty());
        // 同一個資料夾：什麼都不做
        assert!(move_images(b.path(), b.path()).moved.is_empty());
    }
}
