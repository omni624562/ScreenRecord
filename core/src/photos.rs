//! 相片：NAS（或任何資料夾）上的相片，在檔案庫的「相片」分頁瀏覽、整理。
//!
//! - 掃描：一層層讀相片資料夾（NAS 自己的資料夾 #recycle、@eaDir 等不進去，資料夾連結不跟進去），找出相片檔
//! - 拍攝日期：相片裡的 EXIF（DateTimeOriginal）→ 檔名裡的日期（IMG_20240512_143022.jpg）→ 檔案的修改時間。
//!   EXIF 要讀檔案開頭一小段，NAS 上的相片多時很花時間：掃描時先用檔名或修改時間，之後在背景一批批補上
//! - 索引存在程式資料夾的 photo-index.json：下次開啟馬上列出，再在背景重新掃描
//! - 縮圖：JPG、PNG、WebP、BMP 在程式裡解碼縮小（照 EXIF 的方向轉正）；HEIC、HEIF、AVIF、GIF、TIFF 用 FFmpeg。
//!   存在 %LOCALAPPDATA%\ScreenRecorder\photo-thumbs，以「路徑 + 大小 + 修改時間」為鍵，檔案改了就重做
//! - 星號另外存在 photo-stars.json（不和錄影、截圖的放在一起：清單開啟時會檢查那些檔案在不在，NAS 連不到時會卡住）

use crate::paths::{data_dir, mtime_ms, same_mtime};
use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// 認得的相片副檔名
pub const EXTS: [&str; 11] = ["jpg", "jpeg", "png", "webp", "bmp", "heic", "heif", "avif", "gif", "tif", "tiff"];
/// 程式裡解不了、要用 FFmpeg 的
const FFMPEG_EXTS: [&str; 6] = ["heic", "heif", "avif", "gif", "tif", "tiff"];
/// 不會有 EXIF 的（不用讀）
const NO_EXIF: [&str; 2] = ["bmp", "gif"];
/// 縮圖最長的一邊（縮圖格「大」在高解析度螢幕上也夠清楚）
pub const THUMB_MAX: u32 = 480;
/// 縮圖快取最多幾張（超過時刪掉最舊的）
const THUMB_FILES: usize = 40_000;
/// 讀拍攝日期時讀檔案開頭多少（JPEG 的 EXIF 在最前面；HEIC 的通常也在前面一段）
const EXIF_HEAD: u64 = 256 * 1024;
/// 最多讀幾個資料夾、幾張相片（NAS 很大時不會一直讀下去）
const MAX_DIRS: usize = 50_000;
const MAX_PHOTOS: usize = 500_000;

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// 檔名是相片（看副檔名）；NAS、macOS 的隱藏檔（._IMG_0001.JPG）不算
pub fn is_photo_name(name: &str) -> bool {
    !name.starts_with('.') && EXTS.contains(&ext_of(name).as_str())
}

/// 要用 FFmpeg 才看得到（HEIC、GIF…）
pub fn needs_ffmpeg(name: &str) -> bool {
    FFMPEG_EXTS.contains(&ext_of(name).as_str())
}

/// NAS 自己的資料夾（資源回收筒、縮圖快取…）、隱藏資料夾：不進去找相片
pub fn is_system_dir(name: &str) -> bool {
    name.starts_with(['#', '.', '@', '$']) || name.eq_ignore_ascii_case("System Volume Information")
}

/// 拍攝日期從哪裡來
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DateFrom {
    /// 檔案的修改時間（傳到 NAS 時可能變成傳的那天）
    #[default]
    File,
    /// 檔名裡的日期
    Name,
    /// 相片裡記的拍攝時間
    Exif,
}

/// 一張相片（索引裡的一筆）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Photo {
    #[serde(rename = "p")]
    pub path: String,
    #[serde(rename = "s")]
    pub size: u64,
    /// 修改時間（毫秒）
    #[serde(rename = "m")]
    pub mtime: f64,
    /// 拍攝時間：當地的「年月日時分秒」照 UTC 換成毫秒（只用來排序、分組、顯示，不換時區）
    #[serde(rename = "w")]
    pub when: i64,
    #[serde(rename = "f", default)]
    pub from: DateFrom,
    /// 讀過 EXIF 了（讀不到日期的也不再讀）
    #[serde(rename = "x", default)]
    pub exif_read: bool,
}

impl Photo {
    /// 新找到的相片：日期先用檔名裡的，沒有就用修改時間
    pub fn new(path: String, size: u64, mtime: f64) -> Photo {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        let (when, from) = match date_from_name(name) {
            Some(t) => (t.and_utc().timestamp_millis(), DateFrom::Name),
            None => (local_ms(mtime), DateFrom::File),
        };
        let exif_read = NO_EXIF.contains(&ext_of(name).as_str());
        Photo { path, size, mtime, when, from, exif_read }
    }

    pub fn name(&self) -> &str {
        self.path.rsplit(['\\', '/']).next().unwrap_or(&self.path)
    }

    /// 拍攝時間（當地時間）
    pub fn taken(&self) -> NaiveDateTime {
        chrono::DateTime::from_timestamp_millis(self.when).map(|t| t.naive_utc()).unwrap_or_default()
    }
}

/// 修改時間（UTC 毫秒）→ 當地的年月日時分秒當成 UTC 的毫秒
fn local_ms(mtime: f64) -> i64 {
    chrono::Local.timestamp_millis_opt(mtime as i64).single().map(|t| t.naive_local().and_utc().timestamp_millis()).unwrap_or(mtime as i64)
}

/// 檔名裡的日期時間：IMG_20240512_143022.jpg、PXL_20240512_063022123.jpg、2024-05-12 14.30.22.jpg、
/// Screenshot_2024-05-12-14-30-22.png、20240512.jpg；沒有時間時是中午 12 點（排序時排在同一天中間）
pub fn date_from_name(name: &str) -> Option<NaiveDateTime> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:^|[^0-9])((?:19|20)\d{2})[-_.]?(\d{2})[-_.]?(\d{2})(?:[ _\-T.]?(\d{2})[-_.:]?(\d{2})[-_.:]?(\d{2}))?(?:[^0-9]|$|\d{3}[^0-9]|\d{3}$)").expect("日期")
    });
    for c in RE.captures_iter(name) {
        let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
        let (Some(y), Some(m), Some(d)) = (n(1), n(2), n(3)) else { continue };
        let Some(date) = NaiveDate::from_ymd_opt(y as i32, m, d) else { continue };
        let time = match (n(4), n(5), n(6)) {
            (Some(h), Some(mi), Some(s)) => date.and_hms_opt(h, mi, s),
            _ => None,
        };
        return time.or_else(|| date.and_hms_opt(12, 0, 0));
    }
    None
}

// ───────────── EXIF ─────────────

/// 找出 EXIF 的 TIFF 資料：JPEG 的 APP1 區段；其他格式（HEIC、TIFF、WebP、PNG）找「Exif\0\0」或 TIFF 的開頭
fn find_tiff(b: &[u8]) -> Option<&[u8]> {
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 4 <= b.len() && b[i] == 0xFF {
            let marker = b[i + 1];
            // 影像資料開始了：後面沒有 EXIF
            if marker == 0xDA || marker == 0xD9 {
                return None;
            }
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            let seg = b.get(i + 4..(i + 2 + len).min(b.len()))?;
            if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
                return Some(&seg[6..]);
            }
            i += 2 + len;
        }
        return None;
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some(b);
    }
    let at = b.windows(10).position(|w| w.starts_with(b"Exif\0\0") && (&w[6..] == b"II*\0" || &w[6..] == b"MM\0*"))?;
    Some(&b[at + 6..])
}

/// 讀 TIFF 資料（EXIF）：位元組順序、讀數字、一個 IFD 裡有哪些標籤
struct Tiff<'a> {
    t: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(t: &'a [u8]) -> Option<Self> {
        let le = match t.get(..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Tiff { t, le })
    }

    fn u16_at(&self, o: usize) -> Option<u16> {
        self.t.get(o..o + 2).map(|s| if self.le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    }

    fn u32_at(&self, o: usize) -> Option<u32> {
        self.t.get(o..o + 4).map(|s| if self.le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
    }

    /// 一個 IFD 裡的（標籤, 那一筆的位置）
    fn entries(&self, ifd: usize) -> Vec<(u16, usize)> {
        let n = self.u16_at(ifd).unwrap_or(0) as usize;
        (0..n.min(500)).filter_map(|i| self.u16_at(ifd + 2 + i * 12).map(|tag| (tag, ifd + 2 + i * 12))).collect()
    }

    /// 下一個 IFD 的位置（IFD0 之後是放縮圖的 IFD1）
    fn next_ifd(&self, ifd: usize) -> Option<usize> {
        let n = self.u16_at(ifd)? as usize;
        let next = self.u32_at(ifd + 2 + n * 12)? as usize;
        (next > 0).then_some(next)
    }

    /// 那一筆的數值（SHORT 或 LONG）
    fn value(&self, e: usize) -> Option<u32> {
        match self.u16_at(e + 2)? {
            3 => self.u16_at(e + 8).map(u32::from),
            _ => self.u32_at(e + 8),
        }
    }
}

fn find_tag(list: &[(u16, usize)], tag: u16) -> Option<usize> {
    list.iter().find(|(t, _)| *t == tag).map(|(_, e)| *e)
}

/// TIFF 資料裡的拍攝時間：Exif IFD 的 DateTimeOriginal（0x9003）→ DateTimeDigitized（0x9004）→ IFD0 的 DateTime（0x0132）
fn tiff_date(t: &[u8]) -> Option<NaiveDateTime> {
    let x = Tiff::new(t)?;
    let ascii = |e: usize| -> Option<NaiveDateTime> {
        let count = x.u32_at(e + 4)? as usize;
        let off = if count <= 4 { e + 8 } else { x.u32_at(e + 8)? as usize };
        let s = std::str::from_utf8(t.get(off..off + count.min(19))?).ok()?;
        let d = NaiveDateTime::parse_from_str(s.trim_end_matches('\0').trim(), "%Y:%m:%d %H:%M:%S").ok()?;
        // 相機沒設時間時常是 0000:00:00 或 1970 年：不算
        (d.year() >= 1980).then_some(d)
    };
    let ifd0 = x.entries(x.u32_at(4)? as usize);
    if let Some(exif) = find_tag(&ifd0, 0x8769).and_then(|e| x.u32_at(e + 8)) {
        let list = x.entries(exif as usize);
        for tag in [0x9003, 0x9004] {
            if let Some(d) = find_tag(&list, tag).and_then(ascii) {
                return Some(d);
            }
        }
    }
    find_tag(&ifd0, 0x0132).and_then(ascii)
}

/// EXIF 裡附的小縮圖（IFD1 的 JPEG，多半 160×120）與相片的方向（IFD0 的 0x0112）
fn tiff_thumb(t: &[u8]) -> Option<(&[u8], u8)> {
    let x = Tiff::new(t)?;
    let ifd0 = x.u32_at(4)? as usize;
    let orientation = find_tag(&x.entries(ifd0), 0x0112).and_then(|e| x.value(e)).unwrap_or(1) as u8;
    let ifd1 = x.entries(x.next_ifd(ifd0)?);
    let off = find_tag(&ifd1, 0x0201).and_then(|e| x.value(e))? as usize;
    let len = find_tag(&ifd1, 0x0202).and_then(|e| x.value(e))? as usize;
    let jpeg = t.get(off..off.checked_add(len)?)?;
    jpeg.starts_with(&[0xFF, 0xD8]).then_some((jpeg, orientation))
}

/// 相片裡附的小縮圖（JPG 才有；只讀檔案開頭一小段，NAS 上也很快）：先顯示這張，清楚的縮圖做好再換掉。
/// 轉正好的 RGBA：（寬, 高, 像素）
pub fn quick_thumb(path: &str) -> Option<(u32, u32, Vec<u8>)> {
    use std::io::Read;
    if !matches!(ext_of(path).as_str(), "jpg" | "jpeg") {
        return None;
    }
    let mut head = Vec::with_capacity(64 * 1024);
    std::fs::File::open(path).ok()?.take(128 * 1024).read_to_end(&mut head).ok()?;
    let (jpeg, orientation) = tiff_thumb(find_tiff(&head)?)?;
    let mut img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
    if let Some(o) = image::metadata::Orientation::from_exif(orientation) {
        img.apply_orientation(o);
    }
    let img = img.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// 檔案開頭這一段裡的拍攝時間（EXIF）
pub fn exif_date(head: &[u8]) -> Option<NaiveDateTime> {
    tiff_date(find_tiff(head)?)
}

/// 讀這張相片的拍攝時間（只讀檔案開頭一小段）；檔案打不開時 Err（NAS 斷線，之後再讀）
pub fn read_date(path: &str) -> std::io::Result<Option<NaiveDateTime>> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut head = Vec::with_capacity(64 * 1024);
    f.take(EXIF_HEAD).read_to_end(&mut head)?;
    Ok(exif_date(&head))
}

/// 補上這些相片的拍攝時間（還沒讀過 EXIF 的；沒有記拍攝時間的照舊用檔名或修改時間）。
/// 打不開的檔案（NAS 斷線）不算讀過，下次再讀；stop 時停在讀到的那裡
pub fn fill_dates(photos: &mut [Photo], stop: &AtomicBool) {
    for p in photos.iter_mut().filter(|p| !p.exif_read) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(date) = read_date(&p.path) else { continue };
        if let Some(d) = date {
            p.when = d.and_utc().timestamp_millis();
            p.from = DateFrom::Exif;
        }
        p.exif_read = true;
    }
}

// ───────────── 掃描與索引 ─────────────

/// 掃描的結果：找到的相片（依路徑排序）、讀不到的相片資料夾（資料夾, 原因）
#[derive(Debug, Default)]
pub struct Scan {
    pub photos: Vec<Photo>,
    pub errors: Vec<(String, String)>,
}

/// 同時讀幾個資料夾：NAS 上每讀一個資料夾都要等網路來回，一起讀快很多
const SCAN_THREADS: usize = 8;

/// 一個資料夾裡的東西：相片、子資料夾（NAS 的系統資料夾、資料夾連結不算）
fn read_one(dir: &Path, old: &HashMap<String, &Photo>, photos: &mut Vec<Photo>, subdirs: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)?.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if !is_system_dir(&name) {
                subdirs.push(e.path());
            }
            continue;
        }
        if !is_photo_name(&name) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let path = e.path().display().to_string();
        let (size, mtime) = (meta.len(), mtime_ms(&meta));
        photos.push(match old.get(&path.to_lowercase()) {
            Some(p) if p.size == size && same_mtime(p.mtime, mtime) => Photo { path, ..(*p).clone() },
            _ => Photo::new(path, size, mtime),
        });
    }
    Ok(())
}

/// 讀這些相片資料夾（含子資料夾）裡的相片，同時讀好幾個資料夾。known：上次的索引（同一個檔案沒改過時沿用讀過的拍攝時間）；
/// found：已經找到幾張（畫面上顯示進度）；stop：停止
pub fn scan(roots: &[String], known: &[Photo], found: &AtomicUsize, stop: &AtomicBool) -> Scan {
    let old: HashMap<String, &Photo> = known.iter().map(|p| (p.path.to_lowercase(), p)).collect();
    let mut out = Scan::default();
    let mut starts = vec![];
    for root in roots {
        let root = root.trim();
        if root.is_empty() {
            continue;
        }
        match std::fs::read_dir(root) {
            Ok(_) => starts.push(PathBuf::from(root)),
            Err(e) => out.errors.push((root.to_string(), io_reason(&e))),
        }
    }
    // 還沒讀的資料夾、正在讀的有幾個（都讀完、也沒人在讀時結束）
    let queue = std::sync::Mutex::new((starts, 0usize));
    let dirs = AtomicUsize::new(0);
    let all = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        for _ in 0..SCAN_THREADS {
            sc.spawn(|| {
                let (mut mine, mut subdirs) = (vec![], vec![]);
                loop {
                    let next = {
                        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                        match q.0.pop() {
                            Some(d) => {
                                q.1 += 1;
                                Some(d)
                            }
                            None if q.1 == 0 => break,
                            None => None,
                        }
                    };
                    let Some(dir) = next else {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };
                    let full = stop.load(Ordering::Relaxed) || dirs.fetch_add(1, Ordering::Relaxed) >= MAX_DIRS || found.load(Ordering::Relaxed) >= MAX_PHOTOS;
                    let before = mine.len();
                    if !full {
                        let _ = read_one(&dir, &old, &mut mine, &mut subdirs);
                        found.fetch_add(mine.len() - before, Ordering::Relaxed);
                    }
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    q.0.append(&mut subdirs);
                    q.1 -= 1;
                }
                all.lock().unwrap_or_else(|e| e.into_inner()).append(&mut mine);
            });
        }
    });
    out.photos = all.into_inner().unwrap_or_else(|e| e.into_inner());
    out.photos.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// 只讀一個資料夾（不含子資料夾）：這一層的相片、子資料夾的完整路徑。點進資料夾時用，很快
pub fn list_dir(dir: &str, known: &[Photo]) -> Result<(Vec<Photo>, Vec<String>), String> {
    let old: HashMap<String, &Photo> = known.iter().map(|p| (p.path.to_lowercase(), p)).collect();
    let (mut photos, mut subdirs) = (vec![], vec![]);
    read_one(Path::new(dir), &old, &mut photos, &mut subdirs).map_err(|e| io_reason(&e))?;
    photos.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((photos, subdirs.into_iter().map(|d| d.display().to_string()).collect()))
}

/// 讀不到資料夾的原因（看得懂的說法）
fn io_reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => crate::tr!("找不到這個資料夾（NAS 沒開、或還沒連上）", "Folder not found (the NAS may be off or not connected)").into(),
        std::io::ErrorKind::PermissionDenied => crate::tr!("沒有權限讀取：先在檔案總管開啟一次、登入 NAS 的帳號", "Access denied. Open it in File Explorer once and sign in to the NAS").into(),
        _ => e.to_string(),
    }
}

/// 存在 photo-index.json 的索引：哪些相片資料夾、找到的相片
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PhotoIndex {
    pub roots: Vec<String>,
    pub photos: Vec<Photo>,
    /// 每個相片資料夾上次整個掃描完的時間（毫秒）：一天內開啟時不再整個掃（點進資料夾時只讀那一層）
    #[serde(default)]
    pub scanned: Vec<(String, f64)>,
}

pub fn index_file() -> PathBuf {
    data_dir().join("photo-index.json")
}

pub fn load_index(file: &Path) -> PhotoIndex {
    std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_index(file: &Path, idx: &PhotoIndex) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = crate::paths::write_atomic(file, &serde_json::to_vec(idx).unwrap_or_default()) {
        crate::warn!("[相片] 存不了索引：{e}");
    }
}

// ───────────── 縮圖與顯示 ─────────────

fn thumbs_dir() -> PathBuf {
    data_dir().join("photo-thumbs")
}

/// 同時產生幾張縮圖（程式裡解碼、FFmpeg）：NAS 讀檔慢，多幾張一起讀
static DECODE_SEM: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);
static FFMPEG_SEM: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);

/// 縮圖檔的名字（路徑 + 大小 + 修改時間）
fn thumb_key(path: &str, size: u64, mtime: f64) -> String {
    let mut h = Sha256::new();
    h.update(format!("{}|{size}|{}", path.to_lowercase(), mtime as i64).as_bytes());
    h.finalize().iter().take(20).map(|b| format!("{b:02x}")).collect()
}

/// 同時讀幾張相片裡附的小縮圖（只讀開頭一小段）
static QUICK_SEM: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// 相片裡附的小縮圖（背景）：已經有做好的縮圖、排到了卻看不到了（wanted 回答 false）時不讀
pub async fn quick_thumb_async(path: String, size: u64, mtime: f64, wanted: impl Fn() -> bool) -> Option<(u32, u32, Vec<u8>)> {
    if !matches!(ext_of(&path).as_str(), "jpg" | "jpeg") || cached_thumb(&path, size, mtime).is_some() {
        return None;
    }
    let _permit = QUICK_SEM.acquire().await.ok()?;
    if !wanted() {
        return None;
    }
    tokio::task::spawn_blocking(move || quick_thumb(&path)).await.ok().flatten()
}

/// 已經做好的縮圖（快取裡有）
pub fn cached_thumb(path: &str, size: u64, mtime: f64) -> Option<PathBuf> {
    Some(thumbs_dir().join(format!("{}.jpg", thumb_key(path, size, mtime)))).filter(|p| p.is_file())
}

/// 這張相片的縮圖（JPEG，最長的一邊 THUMB_MAX）；產生不了時 None。
/// wanted：排到了再問一次還要不要（捲過去、看不到了就不做，讓看得到的先做）；不要時也是 None
pub async fn thumb(ffmpeg: Option<PathBuf>, path: String, size: u64, mtime: f64, wanted: impl Fn() -> bool) -> Option<PathBuf> {
    thumb_in(thumbs_dir(), ffmpeg, path, size, mtime, wanted).await
}

async fn thumb_in(dir: PathBuf, ffmpeg: Option<PathBuf>, path: String, size: u64, mtime: f64, wanted: impl Fn() -> bool) -> Option<PathBuf> {
    let out = dir.join(format!("{}.jpg", thumb_key(&path, size, mtime)));
    if out.is_file() {
        return Some(out);
    }
    let _ = std::fs::create_dir_all(&dir);
    let tmp = out.with_extension("tmp.jpg");
    if !needs_ffmpeg(&path) {
        let _permit = DECODE_SEM.acquire().await.ok()?;
        if !wanted() {
            return None;
        }
        let (src, t) = (path.clone(), tmp.clone());
        let ok = tokio::task::spawn_blocking(move || -> Option<()> {
            let img = decode_oriented(&std::fs::read(&src).ok()?, THUMB_MAX)?;
            std::fs::write(&t, encode_jpeg(&img.to_rgb8(), 82)?).ok()
        })
        .await
        .ok()
        .flatten()
        .is_some();
        if ok && std::fs::rename(&tmp, &out).is_ok() {
            return Some(out);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    // HEIC 等（或程式裡解不了的 JPG）：FFmpeg 取一張畫面縮小（HEIC 的方向 FFmpeg 會自己轉正）
    let ffmpeg = ffmpeg?;
    let _permit = FFMPEG_SEM.acquire().await.ok()?;
    if !wanted() {
        return None;
    }
    let scale = format!("scale={THUMB_MAX}:{THUMB_MAX}:force_original_aspect_ratio=decrease:flags=bilinear");
    let t = tmp.display().to_string();
    let args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-threads", "2", "-i", &path, "-frames:v", "1", "-vf", &scale, "-q:v", "5", "-y", &t].iter().map(|s| s.to_string()).collect();
    let r = crate::process::run(&ffmpeg, &args, std::time::Duration::from_secs(30)).await;
    if r.code == 0 && std::fs::metadata(&tmp).is_ok_and(|m| m.len() > 0) && std::fs::rename(&tmp, &out).is_ok() {
        return Some(out);
    }
    crate::info!("[相片] 縮圖產生失敗：{path}：{}", r.stderr.trim());
    let _ = std::fs::remove_file(&tmp);
    None
}

/// 讀縮圖檔（JPEG）成 RGBA：（寬, 高, 像素）
pub fn read_thumb(file: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(&std::fs::read(file).ok()?).ok()?.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// 縮圖快取太多時刪掉最舊的（開啟相片分頁時在背景做一次）
pub fn prune_thumbs() {
    let Ok(rd) = std::fs::read_dir(thumbs_dir()) else { return };
    let mut files: Vec<(PathBuf, f64)> = rd.flatten().filter_map(|e| e.metadata().ok().map(|m| (e.path(), mtime_ms(&m)))).collect();
    if files.len() <= THUMB_FILES {
        return;
    }
    files.sort_by(|a, b| a.1.total_cmp(&b.1));
    let extra = files.len() - THUMB_FILES * 9 / 10;
    for (p, _) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

fn encode_jpeg(rgb: &image::RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(rgb).ok()?;
    Some(out)
}

/// 程式裡解碼（JPG、PNG、WebP、BMP）、縮到最長的一邊不超過 max、照 EXIF 的方向轉正
fn decode_oriented(bytes: &[u8], max: u32) -> Option<image::DynamicImage> {
    use image::ImageDecoder;
    let mut dec = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).ok()?;
    if img.width() > max || img.height() > max {
        img = img.thumbnail(max, max);
    }
    img.apply_orientation(orientation);
    Some(img)
}

/// 用 FFmpeg 解出第一張畫面（BMP 從管線讀回來）
fn decode_ffmpeg(ffmpeg: &Path, path: &str, max: u32) -> Option<image::DynamicImage> {
    let out = crate::process::std_command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i", path, "-frames:v", "1", "-f", "image2pipe", "-c:v", "bmp", "-"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Bmp).ok()?;
    Some(if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img })
}

/// 檢視器要的整張相片：轉正、最長的一邊不超過 max（顯示卡的上限）；回傳圖與原本的大小（轉正後的寬高）
pub fn load_full(ffmpeg: Option<&Path>, path: &str, max: u32) -> Option<(image::RgbaImage, (u32, u32))> {
    let orig = |img: &image::DynamicImage, bytes: Option<&[u8]>| -> (u32, u32) {
        // 縮小前的大小：JPG 等從檔頭讀（轉 90 度時寬高互換）
        let dims = bytes.and_then(|b| image::ImageReader::new(std::io::Cursor::new(b)).with_guessed_format().ok()?.into_dimensions().ok());
        match dims {
            Some((w, h)) if (img.width() >= img.height()) == (w >= h) => (w, h),
            Some((w, h)) => (h, w),
            None => (img.width(), img.height()),
        }
    };
    if !needs_ffmpeg(path) {
        if let Ok(bytes) = std::fs::read(path) {
            if let Some(img) = decode_oriented(&bytes, max) {
                let size = orig(&img, Some(&bytes));
                return Some((img.to_rgba8(), size));
            }
        }
    }
    let img = decode_ffmpeg(ffmpeg?, path, u32::MAX)?;
    let size = (img.width(), img.height());
    let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
    Some((img.to_rgba8(), size))
}

/// 整張相片複製到剪貼簿（圖片，可以直接貼到 LINE、Word、信件）；HEIC 也可以
pub fn copy_to_clipboard(ffmpeg: Option<&Path>, path: &str) -> bool {
    let Some((img, _)) = load_full(ffmpeg, path, 8192) else { return false };
    let (w, h) = img.dimensions();
    let data = arboard::ImageData { width: w as usize, height: h as usize, bytes: std::borrow::Cow::Owned(img.into_raw()) };
    match arboard::Clipboard::new().and_then(|mut c| c.set_image(data)) {
        Ok(()) => true,
        Err(e) => {
            crate::info!("[相片] 無法複製到剪貼簿：{e}");
            false
        }
    }
}

// ───────────── 整理 ─────────────

/// 重新命名（副檔名不變；同一個資料夾裡已經有這個名稱時不改）；回傳新的路徑
pub fn rename(path: &str, new_base: &str) -> Result<String, String> {
    let base = new_base.trim();
    if let Some(why) = crate::format::check_recording_name(base) {
        return Err(why.to_string());
    }
    let p = Path::new(path);
    let ext = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let name = if ext.is_empty() { base.to_string() } else { format!("{base}.{ext}") };
    let to = p.with_file_name(&name);
    if to == p {
        return Ok(path.to_string());
    }
    // 只差大小寫（Windows 上是同一個檔案）：照樣改
    let same = to.display().to_string().eq_ignore_ascii_case(path);
    if to.exists() && !same {
        return Err(crate::trf!("這個資料夾裡已經有「{name}」", "“{name}” already exists in this folder"));
    }
    std::fs::rename(p, &to).map_err(|e| crate::trf!("無法重新命名：{e}", "Couldn't rename: {e}"))?;
    Ok(to.display().to_string())
}

/// 在網路資料夾（NAS）上：刪除時不會進這台電腦的資源回收筒
pub fn is_network(path: &str) -> bool {
    if path.starts_with("\\\\") || path.starts_with("//") {
        return true;
    }
    #[cfg(windows)]
    {
        let b = path.as_bytes();
        if b.len() >= 2 && b[1] == b':' {
            let root: Vec<u16> = format!("{}:\\", path[..1].to_ascii_uppercase()).encode_utf16().chain([0]).collect();
            // DRIVE_REMOTE = 4（對應的網路磁碟機）
            return unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(root.as_ptr())) } == 4;
        }
    }
    false
}

/// 刪除這些相片（這台電腦上的移到資源回收筒；NAS 上的直接刪除）
pub fn delete(paths: &[String]) -> Result<(), String> {
    crate::recycle::delete_for_sure(paths).map_err(|e| e.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 做一段 TIFF（小端或大端）：IFD0 有 DateTime 與指到 Exif IFD 的指標，Exif IFD 有 DateTimeOriginal
    fn tiff(le: bool, ifd0_date: &str, original: Option<&str>) -> Vec<u8> {
        let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut t = vec![];
        t.extend(if le { b"II" } else { b"MM" });
        t.extend(w16(42));
        t.extend(w32(8));
        // IFD0：2 筆（DateTime、ExifIFD），8 + 2 + 24 + 4 = 38
        let ifd0_end = 8 + 2 + 2 * 12 + 4;
        let date0_at = ifd0_end;
        let exif_at = date0_at + 20;
        t.extend(w16(2));
        t.extend(w16(0x0132));
        t.extend(w16(2));
        t.extend(w32(20));
        t.extend(w32(date0_at as u32));
        t.extend(w16(0x8769));
        t.extend(w16(4));
        t.extend(w32(1));
        t.extend(w32(exif_at as u32));
        t.extend(w32(0));
        let mut d0 = ifd0_date.as_bytes().to_vec();
        d0.resize(20, 0);
        t.extend(d0);
        let n = original.map_or(0, |_| 1u16);
        let date1_at = exif_at + 2 + n as usize * 12 + 4;
        t.extend(w16(n));
        if original.is_some() {
            t.extend(w16(0x9003));
            t.extend(w16(2));
            t.extend(w32(20));
            t.extend(w32(date1_at as u32));
        }
        t.extend(w32(0));
        if let Some(o) = original {
            let mut d1 = o.as_bytes().to_vec();
            d1.resize(20, 0);
            t.extend(d1);
        }
        t
    }

    fn jpeg_with(tiff: &[u8]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x4A, 0x46];
        let len = (2 + 6 + tiff.len()) as u16;
        j.extend([0xFF, 0xE1]);
        j.extend(len.to_be_bytes());
        j.extend(b"Exif\0\0");
        j.extend(tiff);
        j.extend([0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        j
    }

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn exif_date_from_jpeg_and_others() {
        // DateTimeOriginal 優先於 IFD0 的 DateTime
        for le in [true, false] {
            let j = jpeg_with(&tiff(le, "2024:06:01 08:00:00", Some("2024:05:12 14:30:22")));
            assert_eq!(exif_date(&j), Some(dt("2024-05-12 14:30:22")), "le={le}");
        }
        // 只有 IFD0 的 DateTime
        assert_eq!(exif_date(&jpeg_with(&tiff(true, "2023:01:02 03:04:05", None))), Some(dt("2023-01-02 03:04:05")));
        // HEIC 等：在檔案中間找「Exif\0\0」
        let mut heic = b"\0\0\0\x18ftypheic....meta....".to_vec();
        heic.extend([0, 0, 0, 6]);
        heic.extend(b"Exif\0\0");
        heic.extend(tiff(false, "2022:12:31 23:59:59", Some("2022:12:31 23:59:58")));
        assert_eq!(exif_date(&heic), Some(dt("2022-12-31 23:59:58")));
        // 沒設時間的相機（0000:00:00）、沒有 EXIF、壞掉的資料
        assert_eq!(exif_date(&jpeg_with(&tiff(true, "0000:00:00 00:00:00", None))), None);
        assert_eq!(exif_date(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]), None);
        assert_eq!(exif_date(b"Exif\0\0II*\0\xff\xff\xff\xff"), None);
        let mut cut = jpeg_with(&tiff(true, "2024:06:01 08:00:00", Some("2024:05:12 14:30:22")));
        cut.truncate(30);
        assert_eq!(exif_date(&cut), None);
    }

    #[test]
    fn dates_in_file_names() {
        let d = |s| date_from_name(s);
        assert_eq!(d("IMG_20240512_143022.jpg"), Some(dt("2024-05-12 14:30:22")));
        assert_eq!(d("PXL_20240512_063022123.jpg"), Some(dt("2024-05-12 06:30:22")));
        assert_eq!(d("2024-05-12 14.30.22.jpg"), Some(dt("2024-05-12 14:30:22")));
        assert_eq!(d("Screenshot_2024-05-12-14-30-22.png"), Some(dt("2024-05-12 14:30:22")));
        assert_eq!(d("20240512.jpg"), Some(dt("2024-05-12 12:00:00")));
        assert_eq!(d("旅行 2023.08.01.heic"), Some(dt("2023-08-01 12:00:00")));
        // 不是日期的數字
        assert_eq!(d("IMG_1234.JPG"), None);
        assert_eq!(d("DSC_20241399.jpg"), None);
        assert_eq!(d("photo_123202405121.jpg"), None);
    }

    #[test]
    fn photo_names_and_system_dirs() {
        assert!(is_photo_name("IMG_0001.HEIC") && is_photo_name("a.jpeg") && is_photo_name("b.WebP"));
        assert!(!is_photo_name("._IMG_0001.JPG") && !is_photo_name("Rec.mp4") && !is_photo_name("readme"));
        assert!(needs_ffmpeg("a.heic") && needs_ffmpeg("b.GIF") && !needs_ffmpeg("c.jpg"));
        assert!(is_system_dir("#recycle") && is_system_dir("@eaDir") && is_system_dir(".thumbnails") && !is_system_dir("travel"));
    }

    #[test]
    fn scan_finds_photos_and_keeps_known_dates() {
        let root = std::env::temp_dir().join(format!("sr-photos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["travel/2024", "@eaDir/travel", "#recycle"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let j = jpeg_with(&tiff(true, "2024:06:01 08:00:00", Some("2024:05:12 14:30:22")));
        std::fs::write(root.join("travel/2024/海邊.jpg"), &j).unwrap();
        std::fs::write(root.join("travel/IMG_20230101_101010.png"), b"x").unwrap();
        std::fs::write(root.join("@eaDir/travel/SYNOPHOTO_THUMB.jpg"), b"x").unwrap();
        std::fs::write(root.join("#recycle/old.jpg"), b"x").unwrap();
        std::fs::write(root.join("travel/notes.txt"), b"x").unwrap();
        let roots = vec![root.display().to_string(), root.join("missing").display().to_string()];
        let (found, stop) = (AtomicUsize::new(0), AtomicBool::new(false));
        let mut s = scan(&roots, &[], &found, &stop);
        assert_eq!(found.load(Ordering::Relaxed), 2);
        assert_eq!(s.errors.len(), 1, "讀不到的資料夾要回報");
        let mut names: Vec<&str> = s.photos.iter().map(|p| p.name()).collect();
        names.sort();
        assert_eq!(names, ["IMG_20230101_101010.png", "海邊.jpg"]);
        let at = |list: &[Photo], name: &str| list.iter().position(|p| p.name() == name).unwrap();
        let (img, sea) = (at(&s.photos, "IMG_20230101_101010.png"), at(&s.photos, "海邊.jpg"));
        assert_eq!(s.photos[img].from, DateFrom::Name);
        assert_eq!(s.photos[sea].from, DateFrom::File);
        // 補上 EXIF 的拍攝時間
        fill_dates(&mut s.photos, &stop);
        assert_eq!(s.photos[sea].from, DateFrom::Exif);
        assert_eq!(s.photos[sea].taken(), dt("2024-05-12 14:30:22"));
        assert!(s.photos.iter().all(|p| p.exif_read));
        // 再掃一次：沒改過的沿用讀過的日期
        let again = scan(&roots, &s.photos, &found, &stop);
        assert_eq!(again.photos[at(&again.photos, "海邊.jpg")].from, DateFrom::Exif);
        // 存檔、讀回來
        let file = root.join("idx.json");
        save_index(&file, &PhotoIndex { roots: roots.clone(), photos: again.photos.clone(), scanned: vec![(roots[0].clone(), 1.0)] });
        let back = load_index(&file);
        assert_eq!(back.scanned, [(roots[0].clone(), 1.0)]);
        // 只讀一層：這一層的相片、子資料夾（@eaDir 不算）
        let (here, subs) = list_dir(&root.join("travel").display().to_string(), &back.photos).unwrap();
        assert_eq!(here.iter().map(|p| p.name()).collect::<Vec<_>>(), ["IMG_20230101_101010.png"]);
        assert_eq!(subs.len(), 1);
        assert!(subs[0].ends_with("2024"));
        assert!(list_dir(&root.join("missing").display().to_string(), &[]).is_err());
        assert_eq!(back.photos.len(), again.photos.len());
        for (a, b) in back.photos.iter().zip(&again.photos) {
            assert!(a.path == b.path && a.when == b.when && a.from == b.from && same_mtime(a.mtime, b.mtime));
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// 同時讀好幾個資料夾：很多層、很多個資料夾也全部找到，不會重複
    #[test]
    fn scan_many_folders_in_parallel() {
        let root = std::env::temp_dir().join(format!("sr-photos-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut n = 0;
        for y in 0..6 {
            for m in 0..8 {
                let d = root.join(format!("{}/{:02}/a/b", 2020 + y, m + 1));
                std::fs::create_dir_all(&d).unwrap();
                for i in 0..3 {
                    std::fs::write(d.join(format!("IMG_{i}.jpg")), b"x").unwrap();
                    n += 1;
                }
                std::fs::write(root.join(format!("{}/{:02}/top.png", 2020 + y, m + 1)), b"x").unwrap();
                n += 1;
            }
        }
        let found = AtomicUsize::new(0);
        let s = scan(&[root.display().to_string()], &[], &found, &AtomicBool::new(false));
        assert_eq!((s.photos.len(), found.load(Ordering::Relaxed)), (n, n));
        let mut paths: Vec<&str> = s.photos.iter().map(|p| p.path.as_str()).collect();
        paths.dedup();
        assert_eq!(paths.len(), n);
        // 停下來：不再讀
        let stopped = scan(&[root.display().to_string()], &[], &AtomicUsize::new(0), &AtomicBool::new(true));
        assert!(stopped.photos.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rename_keeps_extension_and_refuses_clashes() {
        let dir = std::env::temp_dir().join(format!("sr-photo-ren-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("IMG_0001.JPG");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(dir.join("生日.JPG"), b"b").unwrap();
        let a = a.display().to_string();
        assert!(rename(&a, "生日").is_err(), "已經有同名的");
        assert!(rename(&a, "a/b").is_err(), "不能有斜線");
        let new = rename(&a, " 海邊 ").unwrap();
        assert!(new.ends_with("海邊.JPG") && Path::new(&new).is_file() && !Path::new(&a).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 相片裡附的小縮圖：IFD1 的 JPEG，照 IFD0 的方向轉正
    #[test]
    fn embedded_thumbnail() {
        // 4×2 的小縮圖、方向 6（向右轉 90 度）→ 2×4
        let small = encode_jpeg(&image::RgbImage::new(4, 2), 90).unwrap();
        // TIFF：IFD0（方向）→ IFD1（縮圖的位置、長度）→ 縮圖
        let mut t = b"II*\0\x08\0\0\0".to_vec();
        let ifd1 = 8 + 2 + 12 + 4;
        let data = ifd1 + 2 + 2 * 12 + 4;
        t.extend(1u16.to_le_bytes());
        t.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        t.extend((ifd1 as u32).to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend([0x01, 0x02, 4, 0, 1, 0, 0, 0]);
        t.extend((data as u32).to_le_bytes());
        t.extend([0x02, 0x02, 4, 0, 1, 0, 0, 0]);
        t.extend((small.len() as u32).to_le_bytes());
        t.extend(0u32.to_le_bytes());
        t.extend(&small);
        let big = encode_jpeg(&image::RgbImage::new(40, 20), 90).unwrap();
        let mut j = big[..2].to_vec();
        j.extend([0xFF, 0xE1]);
        j.extend(((2 + 6 + t.len()) as u16).to_be_bytes());
        j.extend(b"Exif\0\0");
        j.extend(&t);
        j.extend(&big[2..]);
        let dir = std::env::temp_dir().join(format!("sr-photo-quick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("IMG_0001.JPG");
        std::fs::write(&f, &j).unwrap();
        let (w, h, px) = quick_thumb(&f.display().to_string()).unwrap();
        assert_eq!((w, h, px.len()), (2, 4, 32));
        // 沒有附縮圖、不是 JPG
        std::fs::write(&f, &big).unwrap();
        assert!(quick_thumb(&f.display().to_string()).is_none());
        assert!(quick_thumb(&dir.join("a.png").display().to_string()).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 打不開的檔案（NAS 斷線）不算讀過，之後再讀
    #[test]
    fn unreadable_files_are_read_again_later() {
        let mut list = vec![Photo::new("/no/such/dir/IMG_0001.JPG".into(), 1, 0.0)];
        fill_dates(&mut list, &AtomicBool::new(false));
        assert!(!list[0].exif_read);
    }

    /// HEIC、GIF、TIFF 等用 FFmpeg：縮圖、整張（沒有 FFmpeg 時略過）
    #[test]
    fn ffmpeg_formats() {
        let ff = std::env::var_os("SCREENRECORDER_TEST_FFMPEG").map(PathBuf::from).unwrap_or_else(|| "ffmpeg".into());
        if !std::process::Command::new(&ff).arg("-version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("略過：找不到 FFmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("sr-photo-ff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for (name, src) in [("掃描.tif", "rgbtestsrc=s=640x480"), ("動畫.gif", "testsrc=s=600x300")] {
            let f = dir.join(name);
            let ok = std::process::Command::new(&ff).args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", src, "-frames:v", "1", "-y"]).arg(&f).status().is_ok_and(|s| s.success());
            if !ok {
                eprintln!("略過 {name}：這個 FFmpeg 做不出來");
                continue;
            }
            let path = f.display().to_string();
            assert!(needs_ffmpeg(&path));
            let (img, size) = load_full(Some(&ff), &path, 4096).expect(name);
            assert_eq!((img.width(), img.height()), size);
            assert!(size.0 >= 600, "{name}");
            // 沒有 FFmpeg 時看不到
            assert!(load_full(None, &path, 4096).is_none());
            let meta = std::fs::metadata(&f).unwrap();
            let t = rt.block_on(thumb_in(dir.join("thumbs"), Some(ff.clone()), path.clone(), meta.len(), mtime_ms(&meta), || true)).expect(name);
            // 不要了（捲過去）：不做
            assert!(rt.block_on(thumb_in(dir.join("thumbs2"), Some(ff.clone()), path.clone(), meta.len(), mtime_ms(&meta), || false)).is_none());
            let (w, h, _) = read_thumb(&t).unwrap();
            assert!(w.max(h) == THUMB_MAX && w > h, "{name}: {w}×{h}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decodes_and_orients_jpeg() {
        // 2×1 的圖、EXIF 方向 6（要向右轉 90 度）→ 轉正後 1×2
        let mut rgb = image::RgbImage::new(2, 1);
        rgb.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        let plain = encode_jpeg(&rgb, 95).unwrap();
        let img = decode_oriented(&plain, 100).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        // 在 SOI 後面插一段只有方向的 EXIF
        let mut t = b"II*\0\x08\0\0\0\x01\0".to_vec();
        t.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        let mut j = plain[..2].to_vec();
        j.extend([0xFF, 0xE1]);
        j.extend(((2 + 6 + t.len()) as u16).to_be_bytes());
        j.extend(b"Exif\0\0");
        j.extend(&t);
        j.extend(&plain[2..]);
        let img = decode_oriented(&j, 100).unwrap();
        assert_eq!((img.width(), img.height()), (1, 2));
        let dir = std::env::temp_dir().join(format!("sr-photo-full-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("p.jpg");
        std::fs::write(&f, &j).unwrap();
        let (full, size) = load_full(None, &f.display().to_string(), 4096).unwrap();
        assert_eq!((full.width(), full.height(), size), (1, 2, (1, 2)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
