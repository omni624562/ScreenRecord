//! 檔案庫的「相片」分頁：瀏覽 NAS（或任何資料夾）上的相片。
//!
//! - 相片資料夾在標題下的下拉選單加入、移除（記在設定的 photoDirs，可以好幾個，例如 \\NAS\photo）
//! - 上面一排和其他分頁一樣：搜尋 → 篩選（年份、有星號的）→ 排序 →「資料夾｜日期」→ 縮圖大小 → 重新整理；
//!   下一行是目前位置（相片 › photo › travel › 2024），中間是縮圖格，最下面是勾選的操作與共幾張
//! - 「資料夾」一層層點進去（資料夾的縮圖是裡面最新的一張）；「日期」把目前資料夾底下（含子資料夾）的相片依月份排
//! - 清單存在 photo-index.json（core::photos）：開啟時馬上列出上次的，再在背景重新掃描；拍攝日期在背景一批批讀
//! - 點相片在檢視器看（上一張 / 下一張是目前畫面上的順序）；勾選的可以加星號、刪除；右鍵可以重新命名、複製、在資料夾中顯示

use super::theme::{self, Btn, Icon};
use super::thumbs::{Lru, Thumb};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke, UiBuilder};
use screenrecorder_core::photos::{self, DateFrom, Photo};
use screenrecorder_core::pins::PinStore;
use screenrecorder_core::types::{LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 縮圖格每一格大約多寬（小、中、大）
const CELL_W: [f32; 3] = [110.0, 160.0, 240.0];
const GAP: f32 = 8.0;
/// 月份、「資料夾」「相片」標題那一行的高度
const HEAD_H: f32 = 34.0;
/// 打字停一下才找
const SEARCH_WAIT: Duration = Duration::from_millis(300);
/// 一次讀幾張相片的拍攝日期（讀完一批更新畫面，再讀下一批）
const DATE_CHUNK: usize = 400;
/// 檢視器一次帶多少張（前後各這麼多；相片很多時不用整份複製）
const VIEW_SPAN: usize = 3000;

/// 排序
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoSort {
    New,
    Old,
    Name,
}

impl PhotoSort {
    pub const ALL: [PhotoSort; 3] = [PhotoSort::New, PhotoSort::Old, PhotoSort::Name];

    fn label(self) -> &'static str {
        match self {
            PhotoSort::New => tr!("拍攝日期：新到舊", "Date taken: newest"),
            PhotoSort::Old => tr!("拍攝日期：舊到新", "Date taken: oldest"),
            PhotoSort::Name => tr!("名稱", "Name"),
        }
    }
}

/// 每張相片在畫面上要用的：屬於哪個相片資料夾、在裡面的哪個子資料夾（用 / 分隔；空的 = 最上面）、搜尋用的小寫字
struct Meta {
    root: usize,
    dir: String,
    /// 子資料夾 + 檔名 + 日期（小寫），搜尋用
    text: String,
    year: i32,
}

/// 一張相片的 Meta（屬於最長的那個相片資料夾：一個在另一個裡面時）
fn meta_of(roots: &[String], p: &Photo) -> Meta {
    use chrono::Datelike;
    let (root, rest) = roots.iter().enumerate().filter_map(|(i, r)| under(&p.path, r).map(|rest| (i, rest))).min_by_key(|(_, rest)| rest.len()).unwrap_or((0, p.path.as_str()));
    let rest = rest.trim_start_matches(['\\', '/']).replace('\\', "/");
    let dir = rest.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let t = p.taken();
    let text = format!("{} {} {}", rest.to_lowercase(), t.format("%Y-%m-%d %Y/%m/%d"), t.format("%Y/%-m/%-d"));
    Meta { root, dir, text, year: t.year() }
}

/// 縮圖格的一格
enum Cell {
    /// 子資料夾（或好幾個相片資料夾時最上層的那幾個）：（第幾個相片資料夾, 子資料夾, 名稱, 幾張, 縮圖用哪一張）
    Folder {
        root: usize,
        rel: String,
        name: String,
        count: usize,
        cover: Option<usize>,
    },
    Photo(usize),
}

/// 排資料夾時算的：幾張、縮圖用最新的那張、最新和最舊的拍攝時間（排序用）
struct FolderAcc {
    root: usize,
    rel: String,
    name: String,
    count: usize,
    cover: Option<usize>,
    newest: i64,
    oldest: i64,
}

/// 一段：標題（月份、「資料夾」「相片」；空的 = 不寫）與裡面的格子
struct Section {
    title: String,
    cells: Vec<Cell>,
}

/// 正在背景掃描：已經找到幾張、要停下來
struct Scanning {
    found: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

pub struct PhotoBrowse {
    roots: Vec<String>,
    /// 全部的相片（依路徑排序，用二分搜尋找）與每一張的 Meta
    photos: Vec<Photo>,
    meta: Vec<Meta>,
    /// 讀過索引了（還沒讀到之前顯示「正在讀取」）
    loaded: bool,
    scanning: Option<Scanning>,
    /// 讀不到的相片資料夾（資料夾, 原因）
    errors: Vec<(String, String)>,
    /// 背景正在讀拍攝日期：（讀完幾張, 要讀幾張）；stop 是關掉分頁時停下來
    dating: Option<(usize, usize)>,
    date_stop: Arc<AtomicBool>,
    /// 目前位置：（第幾個相片資料夾, 子資料夾 a/b）；None = 最上層（好幾個相片資料夾時列出它們）
    at: Option<(usize, String)>,
    by_date: bool,
    search: String,
    searched: String,
    search_at: Option<Instant>,
    year: Option<i32>,
    only_starred: bool,
    sort: PhotoSort,
    grid: u8,
    /// 畫面上的段落、相片的順序（檢視器的上一張 / 下一張、Shift 選一段）；dirty：條件改了，下一格重排
    sections: Vec<Section>,
    order: Vec<usize>,
    dirty: bool,
    /// 目前範圍裡每一年有幾張（篩選的年份清單）
    years: Vec<(i32, usize)>,
    /// 勾選的相片（路徑）；Shift＋按一下從 anchor 選到點的那一張
    selected: HashSet<String>,
    anchor: Option<String>,
    thumbs: Lru<String>,
    /// 縮圖現在是相片裡附的小縮圖（糊），清楚的還要再做；正在做清楚的
    low: HashSet<String>,
    pending: HashSet<String>,
    /// 這一格畫面上看得到的相片（畫完換掉 visible；背景排到了再問還在不在，不在就不做，看得到的先做）
    seen_now: HashSet<String>,
    visible: Arc<std::sync::Mutex<HashSet<String>>>,
    /// 每一層捲到哪裡（回到上一層時還在原本的位置）；want_offset：換了位置，下一格捲到那裡
    offsets: HashMap<String, f32>,
    want_offset: Option<f32>,
    wheel_at: Option<Instant>,
    /// 上次存索引的時間（讀日期時每隔一陣子存一次）
    saved_at: Instant,
}

impl Drop for PhotoBrowse {
    /// 關掉檔案庫：背景的掃描、讀日期停下來
    fn drop(&mut self) {
        if let Some(s) = &self.scanning {
            s.stop.store(true, Ordering::Relaxed);
        }
        self.date_stop.store(true, Ordering::Relaxed);
    }
}

impl PhotoBrowse {
    fn new(app: &UiApp) -> Self {
        PhotoBrowse {
            roots: app.s.photo_dirs.clone(),
            photos: vec![],
            meta: vec![],
            loaded: false,
            scanning: None,
            errors: vec![],
            dating: None,
            date_stop: Arc::new(AtomicBool::new(false)),
            at: None,
            by_date: app.s.photo_by_date,
            search: String::new(),
            searched: String::new(),
            search_at: None,
            year: None,
            only_starred: false,
            sort: PhotoSort::ALL.get(app.s.photo_sort as usize).copied().unwrap_or(PhotoSort::New),
            grid: app.s.photo_grid_size.min(2),
            sections: vec![],
            order: vec![],
            dirty: true,
            years: vec![],
            selected: HashSet::new(),
            anchor: None,
            thumbs: Lru::new(500),
            low: HashSet::new(),
            pending: HashSet::new(),
            seen_now: HashSet::new(),
            visible: Arc::default(),
            offsets: HashMap::new(),
            want_offset: None,
            wheel_at: None,
            saved_at: Instant::now(),
        }
    }

    /// Esc：有勾選時先取消勾選（回傳 true），沒有才關掉檔案庫
    pub fn clear_selection(&mut self) -> bool {
        let had = !self.selected.is_empty();
        self.selected.clear();
        had
    }

    /// 這個路徑在 photos 的第幾個
    fn find(&self, path: &str) -> Option<usize> {
        self.photos.binary_search_by(|p| p.path.as_str().cmp(path)).ok()
    }

    /// 相片資料夾的名稱（路徑最後一段：\\NAS\photo → photo）
    fn root_name(&self, i: usize) -> String {
        let r = self.roots.get(i).map(|r| r.trim_end_matches(['\\', '/'])).unwrap_or("");
        r.rsplit(['\\', '/']).find(|s| !s.is_empty()).unwrap_or(r).to_string()
    }

    /// 目前位置的鍵（記捲到哪裡用）
    fn at_key(&self) -> String {
        match &self.at {
            None => String::new(),
            Some((r, rel)) => format!("{r}|{rel}|{}", self.by_date),
        }
    }

    /// 換到另一層：記下現在捲到哪裡，回到那一層時捲回原本的位置
    fn go(&mut self, at: Option<(usize, String)>) {
        self.at = at;
        self.selected.clear();
        self.want_offset = Some(self.offsets.get(&self.at_key()).copied().unwrap_or(0.0));
        self.dirty = true;
    }

    /// 只有一個相片資料夾時，最上層就是它
    fn fix_top(&mut self) {
        if self.at.is_none() && self.roots.len() == 1 {
            self.at = Some((0, String::new()));
        }
        if self.at.as_ref().is_some_and(|(r, _)| *r >= self.roots.len()) {
            self.at = if self.roots.len() == 1 { Some((0, String::new())) } else { None };
        }
    }

    /// 相片換了（掃描完、刪除、重新命名）：重算每一張的 Meta
    fn index_changed(&mut self) {
        self.meta = self.photos.iter().map(|p| meta_of(&self.roots, p)).collect();
        self.selected.retain(|p| self.photos.binary_search_by(|x| x.path.as_str().cmp(p)).is_ok());
        self.dirty = true;
    }

    /// 這一張在目前的範圍裡（目前資料夾底下，含子資料夾；最上層 = 全部）
    fn in_scope(&self, i: usize) -> bool {
        let m = &self.meta[i];
        match &self.at {
            None => true,
            Some((r, rel)) => m.root == *r && (rel.is_empty() || m.dir == *rel || m.dir.strip_prefix(rel.as_str()).is_some_and(|x| x.starts_with('/'))),
        }
    }

    /// 重排畫面上的段落（條件、位置、相片換了時）
    fn rebuild(&mut self, stars: &PinStore) {
        self.dirty = false;
        let words: Vec<String> = self.searched.to_lowercase().split_whitespace().map(str::to_string).collect();
        let scope: Vec<usize> = (0..self.photos.len()).filter(|&i| self.in_scope(i)).collect();
        let mut years: BTreeMap<i32, usize> = BTreeMap::new();
        for &i in &scope {
            *years.entry(self.meta[i].year).or_default() += 1;
        }
        self.years = years.into_iter().rev().collect();
        let keep = |i: usize| {
            let m = &self.meta[i];
            self.year.is_none_or(|y| m.year == y) && (!self.only_starred || stars.contains(&self.photos[i].path)) && words.iter().all(|w| m.text.contains(w.as_str()))
        };
        let photos = &self.photos;
        let sort = self.sort;
        let by = |list: &mut Vec<usize>| match sort {
            PhotoSort::New => list.sort_by(|a, b| photos[*b].when.cmp(&photos[*a].when).then_with(|| photos[*a].name().cmp(photos[*b].name()))),
            PhotoSort::Old => list.sort_by(|a, b| photos[*a].when.cmp(&photos[*b].when).then_with(|| photos[*a].name().cmp(photos[*b].name()))),
            PhotoSort::Name => list.sort_by_cached_key(|i| photos[*i].name().to_lowercase()),
        };
        let mut sections = vec![];
        // 搜尋、依日期：目前範圍裡符合的全部列出來，依月份分段（照名稱排時不分段）
        if !words.is_empty() || self.by_date {
            let mut list: Vec<usize> = scope.into_iter().filter(|&i| keep(i)).collect();
            by(&mut list);
            if sort == PhotoSort::Name {
                sections.push(Section { title: String::new(), cells: list.into_iter().map(Cell::Photo).collect() });
            } else {
                for i in list {
                    let t = photos[i].taken();
                    let title = month_title(&t);
                    match sections.last_mut() {
                        Some(s) if s.title == title => s.cells.push(Cell::Photo(i)),
                        _ => sections.push(Section { title, cells: vec![Cell::Photo(i)] }),
                    }
                }
            }
        } else {
            // 資料夾：子資料夾在前，接著是這一層的相片
            let mut folders: BTreeMap<String, FolderAcc> = BTreeMap::new();
            let mut here = vec![];
            for i in scope.into_iter().filter(|&i| keep(i)) {
                let m = &self.meta[i];
                let (root, child) = match &self.at {
                    None => (m.root, None),
                    Some((_, rel)) if m.dir == *rel => {
                        here.push(i);
                        continue;
                    }
                    Some((r, rel)) => {
                        let rest = if rel.is_empty() { m.dir.as_str() } else { &m.dir[rel.len() + 1..] };
                        let name = rest.split('/').next().unwrap_or(rest);
                        let full = if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
                        (*r, Some((name.to_string(), full)))
                    }
                };
                let (name, rel) = child.unwrap_or_else(|| (self.root_name(root), String::new()));
                let key = format!("{root}|{}", name.to_lowercase());
                let w = photos[i].when;
                let e = folders.entry(key).or_insert(FolderAcc { root, rel, name, count: 0, cover: None, newest: i64::MIN, oldest: i64::MAX });
                e.count += 1;
                if w > e.newest {
                    e.newest = w;
                    e.cover = Some(i);
                }
                e.oldest = e.oldest.min(w);
            }
            let mut folders: Vec<FolderAcc> = folders.into_values().collect();
            match sort {
                PhotoSort::New => folders.sort_by_key(|f| std::cmp::Reverse(f.newest)),
                PhotoSort::Old => folders.sort_by_key(|f| f.oldest),
                PhotoSort::Name => {}
            }
            let has_folders = !folders.is_empty();
            if has_folders {
                let cells = folders.into_iter().map(|f| Cell::Folder { root: f.root, rel: f.rel, name: f.name, count: f.count, cover: f.cover }).collect();
                sections.push(Section { title: tr!("資料夾", "Folders").into(), cells });
            }
            if !here.is_empty() {
                by(&mut here);
                let title = if has_folders { tr!("相片", "Photos").to_string() } else { String::new() };
                sections.push(Section { title, cells: here.into_iter().map(Cell::Photo).collect() });
            }
        }
        self.order = sections.iter().flat_map(|s| s.cells.iter()).filter_map(|c| if let Cell::Photo(i) = c { Some(*i) } else { None }).collect();
        self.sections = sections;
    }
}

/// path 在 root 底下時，回傳 root 後面那一段（不分英文大小寫；空的 = 就是 root）
fn under<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches(['\\', '/']);
    let rest = path.get(root.len()..)?;
    (path.get(..root.len())?.eq_ignore_ascii_case(root) && (rest.is_empty() || rest.starts_with(['\\', '/']))).then_some(rest)
}

/// root 底下的子資料夾 a/b 的完整路徑（照 root 用的分隔符號）
fn join_rel(root: &str, rel: &str) -> String {
    let root = root.trim_end_matches(['\\', '/']);
    if rel.is_empty() {
        return root.to_string();
    }
    let sep = if root.contains('\\') || root.ends_with(':') { "\\" } else { "/" };
    format!("{root}{sep}{}", rel.replace('/', sep))
}

/// 「2024 年 5 月」
fn month_title(t: &chrono::NaiveDateTime) -> String {
    use chrono::Datelike;
    if screenrecorder_core::i18n::is_en() {
        t.format("%B %Y").to_string()
    } else {
        format!("{} 年 {} 月", t.year(), t.month())
    }
}

/// 拍攝日期的說法（滑鼠提示、檢視器）：「2024/5/12 14:30（相片記的拍攝時間）」
pub fn date_text(p: &Photo) -> String {
    let t = p.taken();
    // 檔名裡只有日期時是中午 12 點（不寫時間）
    let no_time = p.from == DateFrom::Name && t.format("%H%M%S").to_string() == "120000";
    let when = t.format(if no_time { "%Y/%-m/%-d" } else { "%Y/%-m/%-d %H:%M" }).to_string();
    let from = match p.from {
        DateFrom::Exif => tr!("拍攝時間", "date taken"),
        DateFrom::Name => tr!("檔名裡的日期", "date in the file name"),
        DateFrom::File => tr!("檔案的修改時間", "file modified time"),
    };
    format!("{when}（{from}）")
}

// ───────────── 讀取 ─────────────

/// 開啟「相片」分頁：先列出上次的清單，再在背景重新掃描
pub fn open(app: &mut UiApp) {
    let mut d = PhotoBrowse::new(app);
    // 上次停的資料夾（在最長的那個相片資料夾底下）
    let last = app.s.photo_at.trim();
    if !last.is_empty() {
        let found = d.roots.iter().enumerate().filter_map(|(i, r)| under(last, r).map(|rest| (i, rest))).min_by_key(|(_, rest)| rest.len());
        if let Some((i, rest)) = found {
            d.at = Some((i, rest.trim_start_matches(['\\', '/']).replace('\\', "/")));
        }
    }
    d.fix_top();
    app.photos = Some(d);
    let file = photos::index_file();
    app.spawn(async move { tokio::task::spawn_blocking(move || photos::load_index(&file)).await.unwrap_or_default() }, |app, idx| {
        let Some(d) = app.photos.as_mut() else { return };
        // 相片資料夾換過了：上次的清單只留還在的資料夾裡的
        let roots = d.roots.clone();
        d.photos = idx.photos.into_iter().filter(|p| roots.iter().any(|r| under(&p.path, r).is_some())).collect();
        d.photos.sort_by(|a, b| a.path.cmp(&b.path));
        d.loaded = true;
        d.index_changed();
        scan(app);
    });
    // 縮圖快取太多時刪掉最舊的
    app.spawn(async { tokio::task::spawn_blocking(photos::prune_thumbs).await.ok() }, |_, _| {});
}

/// 在背景重新掃描相片資料夾（同一個檔案沒改過時沿用讀過的拍攝日期），掃完接著讀拍攝日期
fn scan(app: &mut UiApp) {
    let Some(d) = app.photos.as_mut() else { return };
    if let Some(s) = d.scanning.take() {
        s.stop.store(true, Ordering::Relaxed);
    }
    if d.roots.is_empty() {
        d.photos.clear();
        d.errors.clear();
        d.index_changed();
        return;
    }
    let s = Scanning { found: Arc::new(AtomicUsize::new(0)), stop: Arc::new(AtomicBool::new(false)) };
    let (found, stop) = (s.found.clone(), s.stop.clone());
    let stop_id = s.stop.clone();
    d.scanning = Some(s);
    let (roots, known) = (d.roots.clone(), d.photos.clone());
    app.spawn(async move { tokio::task::spawn_blocking(move || (roots.clone(), photos::scan(&roots, &known, &found, &stop))).await.ok() }, move |app, r| {
        let Some(d) = app.photos.as_mut() else { return };
        // 已經換成另一次掃描（加了、移除了資料夾）
        if !d.scanning.as_ref().is_some_and(|s| Arc::ptr_eq(&s.stop, &stop_id)) {
            return;
        }
        d.scanning = None;
        let Some((roots, result)) = r else { return };
        if roots != d.roots || stop_id.load(Ordering::Relaxed) {
            return;
        }
        d.errors = result.errors;
        // 讀不到的資料夾（NAS 沒開）：上次的清單留著，不要整個變空
        let missing: Vec<String> = d.errors.iter().map(|(r, _)| r.clone()).collect();
        let mut list = result.photos;
        if !missing.is_empty() {
            list.extend(d.photos.drain(..).filter(|p| missing.iter().any(|m| under(&p.path, m).is_some())));
            list.sort_by(|a, b| a.path.cmp(&b.path));
        }
        d.photos = list;
        d.index_changed();
        save(app);
        read_dates(app, false);
    });
}

/// 存索引（背景）
fn save(app: &mut UiApp) {
    let Some(d) = app.photos.as_mut() else { return };
    d.saved_at = Instant::now();
    let idx = photos::PhotoIndex { roots: d.roots.clone(), photos: d.photos.clone() };
    app.spawn(async move { tokio::task::spawn_blocking(move || photos::save_index(&photos::index_file(), &idx)).await.ok() }, |_, _| {});
}

/// 讀還沒讀過的拍攝日期（一次 DATE_CHUNK 張，讀完一批更新畫面再讀下一批；chain：上一批讀完接著讀，
/// 不是的話已經在讀時不再開始一串）
fn read_dates(app: &mut UiApp, chain: bool) {
    let Some(d) = app.photos.as_mut() else { return };
    if !chain && d.dating.is_some() {
        return;
    }
    let left: Vec<Photo> = d.photos.iter().filter(|p| !p.exif_read).take(DATE_CHUNK).cloned().collect();
    if left.is_empty() {
        if d.dating.take().is_some() {
            save(app);
        }
        return;
    }
    let total_left = d.photos.iter().filter(|p| !p.exif_read).count();
    let done = d.dating.map_or(0, |(n, _)| n);
    d.dating = Some((done, done + total_left));
    let stop = d.date_stop.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let mut list = left;
                photos::fill_dates(&mut list, &stop);
                list
            })
            .await
            .unwrap_or_default()
        },
        |app, list| {
            let Some(d) = app.photos.as_mut() else { return };
            let mut changed = false;
            let mut n = 0;
            for p in list.into_iter().filter(|p| p.exif_read) {
                n += 1;
                if let Some(i) = d.find(&p.path) {
                    let x = &mut d.photos[i];
                    if x.size == p.size && screenrecorder_core::paths::same_mtime(x.mtime, p.mtime) {
                        let moved = x.when != p.when;
                        (x.when, x.from, x.exif_read) = (p.when, p.from, true);
                        // 只重算日期改了的那幾張
                        if moved && i < d.meta.len() {
                            d.meta[i] = meta_of(&d.roots, &d.photos[i]);
                            changed = true;
                        }
                    }
                }
            }
            if n == 0 {
                // 停下來了（關掉分頁），或這一批都打不開（NAS 斷線）：按「重新整理」時再讀
                d.dating = None;
                return;
            }
            if let Some((done, _)) = d.dating.as_mut() {
                *done += n;
            }
            if changed {
                d.dirty = true;
            }
            if d.saved_at.elapsed() > Duration::from_secs(20) {
                save(app);
            }
            read_dates(app, true);
        },
    );
}

/// 加入相片資料夾（Windows 的選資料夾視窗；NAS 可以在網址列打 \\NAS\photo）
fn add_dir(app: &mut UiApp) {
    app.spawn(
        async move {
            tokio::task::spawn_blocking(|| {
                screenrecorder_core::filepick::pick_folder(
                    tr!("選擇放相片的資料夾（NAS 可以在網址列輸入 \\\\NAS\\photo）", "Choose a photo folder (for a NAS, type \\\\NAS\\photo in the address bar)"),
                    None,
                )
            })
            .await
            .unwrap_or(Ok(None))
        },
        |app, r| match r {
            Ok(Some(dir)) => {
                let dir = dir.display().to_string();
                let key = dir.trim_end_matches(['\\', '/']).to_lowercase();
                if app.s.photo_dirs.iter().any(|d| d.trim_end_matches(['\\', '/']).to_lowercase() == key) {
                    return app.toast(tr!("這個資料夾已經在清單裡了", "This folder is already in the list"), false);
                }
                app.s.photo_dirs.push(dir.clone());
                app.save_settings();
                app.toast(trf!("已加入 {dir}，正在找裡面的相片…", "Added {dir}. Looking for photos…"), false);
                roots_changed(app, Some(app.s.photo_dirs.len() - 1));
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

/// 相片資料夾換了：重新掃描（go：跳到第幾個相片資料夾）
fn roots_changed(app: &mut UiApp, go: Option<usize>) {
    let roots = app.s.photo_dirs.clone();
    let Some(d) = app.photos.as_mut() else { return };
    d.roots = roots;
    d.errors.clear();
    match go {
        Some(i) => d.go(Some((i, String::new()))),
        None => d.go(None),
    }
    d.fix_top();
    d.index_changed();
    scan(app);
}

// ───────────── 畫面 ─────────────

/// 分頁上按了什麼（畫完、放回 app 之後才處理）
#[derive(Default)]
pub struct Panel {
    /// 開啟這一張（在 order 裡的第幾個）
    open: Option<usize>,
    /// 按了資料夾：換到那一層
    go: Option<Option<(usize, String)>>,
    /// 要讀縮圖的相片
    want: Vec<usize>,
    add_dir: bool,
    remove_dir: Option<usize>,
    open_dir: Option<String>,
    rescan: bool,
    star: Option<(Vec<String>, bool)>,
    delete: Option<Vec<String>>,
    rename: Option<String>,
    reveal: Option<String>,
    external: Option<String>,
    copy: Option<String>,
    copy_file: Option<Vec<String>>,
}

/// 標題下面那一行：相片資料夾（按了是選單：在檔案總管開啟、移除、加入資料夾）
pub fn header(ui: &mut egui::Ui, d: &PhotoBrowse, out: &mut Panel) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // 一個：寫路徑；好幾個：寫幾個（按了列出全部）
        let text = match d.roots.len() {
            0 => tr!("還沒有相片資料夾", "No photo folders yet").to_string(),
            1 => d.roots[0].clone(),
            n => trf!("{n} 個相片資料夾", "{n} photo folders"),
        };
        let b = Btn::new(&text).icon(Icon::Folder).small().ghost().menu().tooltip(tr!("相片資料夾：加入、移除，或在檔案總管開啟", "Photo folders: add, remove or open in File Explorer")).show(ui);
        egui::Popup::menu(&b).show(|ui| {
            ui.set_min_width(320.0);
            ui.label(theme::muted(ui, tr!("相片資料夾", "Photo folders")));
            for (i, r) in d.roots.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.button(r).on_hover_text(tr!("在檔案總管開啟（NAS 要登入時會在這裡問帳號密碼）", "Open in File Explorer (it asks for the NAS sign-in here if needed)")).clicked()
                    {
                        out.open_dir = Some(r.clone());
                        ui.close();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button(tr!("移除", "Remove")).on_hover_text(tr!("只是不再列出，資料夾和相片都不會刪掉", "Only stops listing it. The folder and photos aren't deleted")).clicked()
                        {
                            out.remove_dir = Some(i);
                            ui.close();
                        }
                    });
                });
            }
            ui.separator();
            if ui.button(RichText::new(tr!("＋ 加入相片資料夾…", "＋ Add a photo folder…")).color(p.accent)).clicked() {
                out.add_dir = true;
                ui.close();
            }
        });
        // 加第二個、第三個 NAS 資料夾：直接一顆按鈕（不用先打開上面的選單）
        if !d.roots.is_empty()
            && Btn::new(tr!("＋ 加入資料夾", "＋ Add folder"))
                .small()
                .ghost()
                .tooltip(tr!(
                    "再加一個放相片的資料夾（另一台 NAS、另一個共用資料夾或這台電腦的資料夾），全部一起列出",
                    "Add another photo folder (another NAS, another share or a folder on this PC); they're all listed together"
                ))
                .show(ui)
                .clicked()
        {
            out.add_dir = true;
        }
        if let Some(s) = &d.scanning {
            ui.add_space(6.0);
            ui.spinner();
            ui.label(theme::muted(ui, trf!("正在找相片…已找到 {} 張", "Looking for photos… {} found", s.found.load(Ordering::Relaxed))));
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
    });
}

/// 在縮圖上按住 Ctrl 轉滾輪：換縮圖大小（轉一下換一級）
fn grid_zoom(ui: &egui::Ui, d: &mut PhotoBrowse, area: Rect) {
    if !ui.rect_contains_pointer(area) || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let dy: f32 = ui.input(|i| {
        i.raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, modifiers, .. } if modifiers.ctrl || modifiers.command => Some(delta.y),
                _ => None,
            })
            .sum()
    });
    if dy == 0.0 || d.wheel_at.is_some_and(|t| t.elapsed().as_millis() < 250) {
        return;
    }
    let next = if dy > 0.0 { d.grid.saturating_add(1).min(2) } else { d.grid.saturating_sub(1) };
    if next != d.grid {
        d.grid = next;
        d.wheel_at = Some(Instant::now());
    }
}

/// 鍵盤：Ctrl+A 勾選全部、Delete 刪除勾選的、F2 重新命名（只勾一張時）、Backspace 回到上一層
fn keyboard(ui: &egui::Ui, d: &mut PhotoBrowse, out: &mut Panel) {
    if ui.ctx().egui_wants_keyboard_input() || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let (all, del, f2, back) = ui.input(|i| (i.modifiers.command && i.key_pressed(egui::Key::A), i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::F2), i.key_pressed(egui::Key::Backspace)));
    if all {
        let paths: Vec<String> = d.order.iter().map(|i| d.photos[*i].path.clone()).collect();
        d.selected.extend(paths);
    }
    if del && !d.selected.is_empty() {
        out.delete = Some(d.selected.iter().cloned().collect());
    }
    if f2 && d.selected.len() == 1 {
        out.rename = d.selected.iter().next().cloned();
    }
    if back {
        go_up(d);
    }
}

/// 回到上一層
fn go_up(d: &mut PhotoBrowse) {
    match d.at.clone() {
        Some((r, rel)) if !rel.is_empty() => d.go(Some((r, rel.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default()))),
        Some(_) if d.roots.len() > 1 => d.go(None),
        _ => {}
    }
}

/// 檔案庫的「相片」分頁（keys：可以用鍵盤，上面沒有其他對話框）
pub fn panel(ui: &mut egui::Ui, d: &mut PhotoBrowse, out: &mut Panel, stars: &PinStore, keys: bool) {
    let p = theme::pal(ui);
    if keys {
        keyboard(ui, d, out);
    }
    if d.roots.is_empty() {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            theme::paint_icon(ui.painter(), r, Icon::Image, p.muted);
            ui.add_space(12.0);
            ui.label(RichText::new(tr!("在這裡看 NAS 上的相片", "View the photos on your NAS here")).font(theme::font_bold(16.0)));
            ui.add_space(4.0);
            ui.label(theme::muted(
                ui,
                tr!(
                    "加入放相片的資料夾（例如 NAS 上的 \\\\NAS\\photo，或這台電腦的任何資料夾），就能一層層瀏覽、依日期看、搜尋，加星號、重新命名、刪除。支援 JPG、PNG、HEIC（iPhone）、WebP、GIF 等。",
                    "Add a folder with photos (such as \\\\NAS\\photo on your NAS, or any folder on this PC) to browse it folder by folder or by date, search, star, rename and delete. JPG, PNG, HEIC (iPhone), WebP, GIF and more are supported."
                ),
            ));
            ui.add_space(12.0);
            if Btn::new(tr!("加入相片資料夾…", "Add a photo folder…")).icon(Icon::Folder).primary().show(ui).clicked() {
                out.add_dir = true;
            }
        });
        return;
    }
    let before = (d.year, d.only_starred, d.sort, d.by_date);
    super::toolbar::bar(ui, egui::Id::new("photobar"), |ui, part| match part {
        super::toolbar::Part::Search(w) => {
            let r = ui.add(super::toolbar::search_box(&mut d.search, tr!("搜尋檔名、資料夾、日期（例如 2024/5）", "Search names, folders, dates (e.g. 2024/5)"), w));
            if r.changed() {
                d.search_at = Some(Instant::now());
            }
            if super::toolbar::clear_x(ui, &r, &mut d.search) {
                d.search_at = Some(Instant::now() - SEARCH_WAIT);
            }
        }
        super::toolbar::Part::Left => {
            let only = if d.only_starred { tr!("有星號的", "Starred") } else { tr!("全部", "All") };
            egui::ComboBox::from_id_salt("photo-only").selected_text(only).width(100.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.only_starred, false, tr!("全部", "All"));
                ui.selectable_value(&mut d.only_starred, true, tr!("有星號的", "Starred"));
            });
            let year = d.year.map_or_else(|| tr!("全部年份", "All years").to_string(), |y| y.to_string());
            egui::ComboBox::from_id_salt("photo-year")
                .selected_text(year)
                .width(100.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.year, None, tr!("全部年份", "All years"));
                    for (y, n) in &d.years {
                        ui.selectable_value(&mut d.year, Some(*y), format!("{y}　{n}"));
                    }
                })
                .response
                .on_hover_text(tr!("依拍攝日期的年份（沒有拍攝時間的相片用檔名裡的日期或修改時間）", "By the year taken (photos without one use the date in the file name or the modified time)"));
            egui::ComboBox::from_id_salt("photo-sort").selected_text(d.sort.label()).width(140.0).show_ui(ui, |ui| {
                for s in PhotoSort::ALL {
                    ui.selectable_value(&mut d.sort, s, s.label());
                }
            });
        }
        super::toolbar::Part::Right(compact) => {
            let mut by = d.by_date;
            theme::segmented_h(ui, &mut by, &[(false, tr!("資料夾", "Folders")), (true, tr!("日期", "Dates"))], true, super::toolbar::H - 8.0);
            if by != d.by_date {
                d.by_date = by;
                d.want_offset = Some(0.0);
            }
            ui.add_space(6.0);
            let mut g = d.grid;
            let tip = tr!("縮圖大小。也可以在縮圖上按住 Ctrl 轉滾輪調整", "Thumbnail size. You can also hold Ctrl and turn the mouse wheel over the thumbnails");
            if super::toolbar::view_switch(ui, &mut g, &[(0u8, tr!("小", "S")), (1, tr!("中", "M")), (2, tr!("大", "L"))], compact, tip) {
                d.grid = g;
            }
            ui.add_space(6.0);
            let tip = tr!("重新讀取相片資料夾（NAS 上新增、改過的相片）", "Read the photo folders again (picks up new or changed photos on the NAS)");
            if super::toolbar::button(tr!("重新整理", "Refresh"), Icon::Refresh, compact, tip).enabled(d.scanning.is_none()).show(ui).clicked() {
                out.rescan = true;
            }
        }
    });
    if before != (d.year, d.only_starred, d.sort, d.by_date) {
        d.dirty = true;
    }
    if let Some(t) = d.search_at {
        if t.elapsed() >= SEARCH_WAIT {
            d.search_at = None;
            if d.search.trim() != d.searched {
                d.searched = d.search.trim().to_string();
                d.want_offset = Some(0.0);
                d.dirty = true;
            }
        } else {
            ui.ctx().request_repaint_after(SEARCH_WAIT);
        }
    }
    if d.dirty && d.loaded {
        d.rebuild(stars);
    }
    ui.add_space(4.0);
    // 目前位置：相片 › photo › travel › 2024（點前面的回到那一層）
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
        let multi = d.roots.len() > 1;
        let top = d.at.is_none() || (!multi && d.at.as_ref().is_some_and(|(_, r)| r.is_empty()));
        if Btn::new(tr!("相片", "Photos")).icon(Icon::Image).small().ghost().selected(top).show(ui).clicked() && !top {
            d.go(if multi { None } else { Some((0, String::new())) });
        }
        if let Some((r, rel)) = d.at.clone() {
            let mut parts: Vec<(String, String)> = vec![];
            if multi {
                parts.push((d.root_name(r), String::new()));
            }
            let mut acc = String::new();
            for seg in rel.split('/').filter(|s| !s.is_empty()) {
                acc = if acc.is_empty() { seg.to_string() } else { format!("{acc}/{seg}") };
                parts.push((seg.to_string(), acc.clone()));
            }
            let n = parts.len();
            for (i, (name, to)) in parts.into_iter().enumerate() {
                ui.label(theme::muted(ui, "›"));
                let last = i + 1 == n;
                if Btn::new(&name).small().ghost().selected(last).show(ui).clicked() && !last {
                    d.go(Some((r, to)));
                }
            }
        }
        if !d.searched.is_empty() {
            ui.label(theme::muted(ui, "›"));
            Btn::new(&trf!("搜尋「{}」", "Search “{}”", d.searched)).small().ghost().selected(true).show(ui);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if d.loaded {
                let n = d.order.len();
                let folders: usize = d.sections.iter().flat_map(|s| s.cells.iter()).filter(|c| matches!(c, Cell::Folder { .. })).count();
                let text = if folders > 0 && n > 0 {
                    trf!("{folders} 個資料夾、{n} 張相片", "{folders} folders, {n} photos")
                } else if folders > 0 {
                    trf!("{folders} 個資料夾", "{folders} folders")
                } else {
                    trf!("{n} 張相片", "{n} photos")
                };
                ui.label(RichText::new(text).font(theme::font(12.5)).color(p.muted));
            }
        });
    });
    // 讀不到的相片資料夾（NAS 沒開、要登入）
    for (root, why) in d.errors.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("讀不到 {root}：{why}", "Couldn't read {root}: {why}")).color(p.warn).font(theme::font(12.5)));
            if Btn::new(tr!("在檔案總管開啟", "Open in File Explorer")).small().ghost().show(ui).clicked() {
                out.open_dir = Some(root.clone());
            }
            if Btn::new(tr!("再試一次", "Try again")).small().ghost().enabled(d.scanning.is_none()).show(ui).clicked() {
                out.rescan = true;
            }
        });
    }
    ui.add_space(4.0);
    let foot_h = 40.0;
    let area = Rect::from_min_max(ui.cursor().min, pos2(ui.max_rect().max.x, ui.max_rect().max.y - foot_h - 8.0));
    grid_zoom(ui, d, area);
    {
        let bui = &mut ui.new_child(UiBuilder::new().max_rect(area).layout(Layout::top_down(Align::Min)));
        if !d.loaded || (d.photos.is_empty() && d.scanning.is_some()) {
            bui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr!("正在讀取相片資料夾…", "Reading the photo folders…"));
            });
        } else if d.sections.is_empty() {
            let msg = if !d.searched.is_empty() || d.year.is_some() || d.only_starred {
                tr!("找不到符合的相片（可以清掉搜尋，或把篩選改回「全部」）", "No matching photos (clear the search or set the filters back to All)")
            } else {
                tr!("這裡沒有相片", "No photos here")
            };
            bui.label(RichText::new(msg).color(p.muted));
        } else {
            grid(bui, d, out, stars);
        }
    }
    footer(ui, d, out, area, foot_h);
}

/// 縮圖格：只畫看得到的那幾排（相片上萬張也不會慢）
fn grid(ui: &mut egui::Ui, d: &mut PhotoBrowse, out: &mut Panel, stars: &PinStore) {
    let p = theme::pal(ui);
    let w = ui.available_width() - 12.0;
    let cols = ((w + GAP) / (CELL_W[d.grid as usize] + GAP)).floor().max(1.0) as usize;
    let cell = ((w - GAP * (cols as f32 - 1.0)) / cols as f32).floor();
    // 每一段的高度（標題 + 幾排）
    let heights: Vec<f32> = d.sections.iter().map(|s| if s.title.is_empty() { 0.0 } else { HEAD_H } + s.cells.len().div_ceil(cols) as f32 * (cell + GAP)).collect();
    let total: f32 = heights.iter().sum::<f32>() + 8.0;
    let mut sa = egui::ScrollArea::vertical().id_salt("photo-grid").auto_shrink([false, false]);
    if let Some(y) = d.want_offset.take() {
        sa = sa.vertical_scroll_offset(y);
    }
    let picking = !d.selected.is_empty();
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
    // 畫的時候要改 d（縮圖、勾選）：段落先拿出來，畫完放回去
    let sections = std::mem::take(&mut d.sections);
    let res = sa.show_viewport(ui, |ui, vp| {
        ui.set_height(total);
        ui.set_width(w);
        let origin = ui.min_rect().min;
        let mut y = 0.0;
        let mut seen_photo = 0usize;
        for (si, s) in sections.iter().enumerate() {
            let h = heights[si];
            let photos_in = s.cells.iter().filter(|c| matches!(c, Cell::Photo(_))).count();
            if y + h < vp.min.y || y > vp.max.y {
                y += h;
                seen_photo += photos_in;
                continue;
            }
            let mut top = y;
            if !s.title.is_empty() {
                let r = Rect::from_min_size(origin + vec2(0.0, y), vec2(w, HEAD_H));
                let n = s.cells.len();
                let unit = if matches!(s.cells.first(), Some(Cell::Folder { .. })) { tr!("個", "") } else { tr!("張", "") };
                ui.painter().text(r.left_center() + vec2(2.0, 2.0), Align2::LEFT_CENTER, &s.title, theme::font_bold(14.0), p.text);
                let g = ui.painter().layout_no_wrap(s.title.clone(), theme::font_bold(14.0), p.text);
                ui.painter().text(r.left_center() + vec2(g.size().x + 12.0, 2.0), Align2::LEFT_CENTER, format!("{n} {unit}").trim(), theme::font(12.5), p.muted);
                top += HEAD_H;
            }
            let mut idx_photo = seen_photo;
            for (row, chunk) in s.cells.chunks(cols).enumerate() {
                let ry = top + row as f32 * (cell + GAP);
                if ry + cell < vp.min.y || ry > vp.max.y {
                    idx_photo += chunk.iter().filter(|c| matches!(c, Cell::Photo(_))).count();
                    continue;
                }
                for (ci, c) in chunk.iter().enumerate() {
                    let rect = Rect::from_min_size(origin + vec2(ci as f32 * (cell + GAP), ry), vec2(cell, cell));
                    match c {
                        Cell::Folder { root, rel, name, count, cover } => {
                            if folder_cell(ui, d, rect, *root, rel, name, *count, *cover, out) {
                                out.go = Some(Some((*root, rel.clone())));
                            }
                        }
                        Cell::Photo(i) => {
                            photo_cell(ui, d, rect, *i, idx_photo, stars, picking, ctrl, shift, out);
                            idx_photo += 1;
                        }
                    }
                }
            }
            seen_photo += photos_in;
            y += h;
        }
    });
    d.sections = sections;
    let key = d.at_key();
    d.offsets.insert(key, res.state.offset.y);
    *d.visible.lock().unwrap_or_else(|e| e.into_inner()) = std::mem::take(&mut d.seen_now);
}

/// 這張的縮圖材質（還沒讀時記下要讀，先回傳 None）
fn thumb_of(d: &mut PhotoBrowse, i: usize, out: &mut Panel) -> Option<egui::TextureHandle> {
    let path = &d.photos[i].path;
    if !d.seen_now.contains(path) {
        d.seen_now.insert(path.clone());
    }
    match d.thumbs.get(path) {
        // 先放的是相片裡附的小縮圖：清楚的還沒在做（之前捲過去取消了）就再做
        Some(Thumb::Ready(t)) => {
            let t = t.clone();
            if d.low.contains(path) && !d.pending.contains(path) {
                out.want.push(i);
            }
            Some(t)
        }
        Some(Thumb::Loading) => None,
        Some(Thumb::Failed(at)) if at.elapsed() < Duration::from_secs(120) => None,
        _ => {
            out.want.push(i);
            None
        }
    }
}

/// 一個資料夾的格子：縮圖是裡面最新的那一張，下面一條寫名稱與幾張；按了回傳 true
#[allow(clippy::too_many_arguments)]
fn folder_cell(ui: &mut egui::Ui, d: &mut PhotoBrowse, rect: Rect, root: usize, rel: &str, name: &str, count: usize, cover: Option<usize>, out: &mut Panel) -> bool {
    let p = theme::pal(ui);
    let resp = ui.interact(rect, ui.id().with(("photo-folder", root, rel)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, p.surface2);
    let tex = cover.and_then(|i| thumb_of(d, i, out));
    let painter = ui.painter();
    match tex {
        Some(t) => theme::paint_thumb_cover(painter, rect.shrink(1.0), &t),
        None => theme::paint_icon(painter, Rect::from_center_size(rect.center() - vec2(0.0, 12.0), vec2(36.0, 36.0)), Icon::Folder, p.muted),
    }
    // 下面一條：資料夾圖示、名稱、幾張
    let band = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 40.0), rect.max);
    painter.rect_filled(band, egui::CornerRadius { nw: 0, ne: 0, sw: 8, se: 8 }, Color32::from_black_alpha(170));
    theme::paint_icon(painter, Rect::from_center_size(pos2(band.min.x + 14.0, band.center().y - 7.0), vec2(13.0, 13.0)), Icon::Folder, Color32::WHITE);
    let g = painter.layout(name.to_string(), theme::font_bold(12.5), Color32::WHITE, band.width() - 30.0);
    painter.with_clip_rect(band).galley(pos2(band.min.x + 24.0, band.min.y + 4.0), g, Color32::WHITE);
    painter.text(pos2(band.min.x + 24.0, band.max.y - 5.0), Align2::LEFT_BOTTOM, trf!("{count} 張", "{count} photos"), theme::font(11.5), Color32::from_gray(210));
    if resp.hovered() {
        painter.rect_stroke(rect, 8.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
    }
    resp.on_hover_text(name).clicked()
}

/// 一張相片的格子：滑鼠移上去時左上角勾選、右上角星號；按一下開啟（勾選中、按著 Ctrl 時是勾選，Shift 選一段）
#[allow(clippy::too_many_arguments)]
fn photo_cell(ui: &mut egui::Ui, d: &mut PhotoBrowse, rect: Rect, i: usize, pos: usize, stars: &PinStore, picking: bool, ctrl: bool, shift: bool, out: &mut Panel) {
    let p = theme::pal(ui);
    let path = d.photos[i].path.clone();
    let resp = ui.interact(rect, ui.id().with(("photo", &path)), Sense::click());
    let on = d.selected.contains(&path);
    let starred = stars.contains(&path);
    ui.painter().rect_filled(rect, 6.0, p.surface2);
    match thumb_of(d, i, out) {
        Some(t) => theme::paint_thumb_cover(ui.painter(), rect, &t),
        None => theme::paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(28.0, 28.0)), Icon::Image, p.muted),
    }
    let check = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(22.0, 22.0));
    let star = Rect::from_min_size(pos2(rect.max.x - 30.0, rect.min.y + 4.0), vec2(26.0, 26.0));
    let hovering = ui.rect_contains_pointer(rect);
    let mut used = false;
    if hovering || on || picking {
        let r = ui.interact(check, ui.id().with(("photo-check", &path)), Sense::click()).on_hover_text(tr!("勾選（一次加星號、刪除好幾張）", "Select (to star or delete several at once)"));
        let painter = ui.painter();
        painter.rect_filled(check, 5.0, if on { p.accent } else { Color32::from_black_alpha(150) });
        painter.rect_stroke(check, 5.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
        if on {
            theme::paint_icon(painter, check.shrink(4.0), Icon::Check, Color32::WHITE);
        }
        if r.clicked() {
            pick(d, &path, shift);
            used = true;
        }
    }
    if hovering || starred {
        let r = ui.interact(star, ui.id().with(("photo-star", &path)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if starred {
            tr!("移除星號", "Remove star")
        } else {
            tr!("加上星號（篩選「有星號的」找得到）", "Add a star (find it with the Starred filter)")
        });
        let painter = ui.painter();
        painter.circle_filled(star.center(), 12.0, Color32::from_black_alpha(if r.hovered() { 190 } else { 130 }));
        let color = if starred { Color32::from_rgb(0xF5, 0xB7, 0x00) } else { Color32::WHITE };
        theme::paint_icon(painter, Rect::from_center_size(star.center(), vec2(15.0, 15.0)), if starred { Icon::StarOn } else { Icon::Star }, color);
        if r.clicked() {
            out.star = Some((vec![path.clone()], !starred));
            used = true;
        }
    }
    let painter = ui.painter();
    if hovering || on {
        painter.rect_stroke(rect, 6.0, Stroke::new(if on { 3.0 } else { 2.0 }, p.accent), egui::StrokeKind::Inside);
    }
    // 需要 FFmpeg 的格式（HEIC 等）：左下角小字
    let ph = &d.photos[i];
    if photos::needs_ffmpeg(ph.name()) && rect.width() >= 120.0 {
        let ext = ph.name().rsplit('.').next().unwrap_or("").to_uppercase();
        let g = painter.layout_no_wrap(ext, theme::font_bold(10.0), Color32::WHITE);
        let r = Rect::from_min_size(pos2(rect.min.x + 5.0, rect.max.y - g.size().y - 9.0), g.size() + vec2(6.0, 3.0));
        painter.rect_filled(r, 3.0, Color32::from_black_alpha(150));
        painter.galley(r.min + vec2(3.0, 1.5), g, Color32::WHITE);
    }
    let tip = format!("{}\n{}\n{}", ph.name(), date_text(ph), screenrecorder_core::format::format_bytes(ph.size));
    let resp = resp.on_hover_text(tip);
    resp.context_menu(|ui| photo_menu(ui, d, &path, starred, out));
    if resp.clicked() && !used {
        if shift || picking || ctrl {
            pick(d, &path, shift);
        } else {
            out.open = Some(pos);
        }
    }
}

/// 勾選 / 取消這一張；shift：從上次勾的那一張選到這一張
fn pick(d: &mut PhotoBrowse, path: &str, shift: bool) {
    let from = d.anchor.as_ref().and_then(|a| d.order.iter().position(|i| d.photos[*i].path == *a));
    let to = d.order.iter().position(|i| d.photos[*i].path == path);
    if let (true, Some(a), Some(b)) = (shift, from, to) {
        let list: Vec<String> = d.order[a.min(b)..=a.max(b)].iter().map(|i| d.photos[*i].path.clone()).collect();
        d.selected.extend(list);
        return;
    }
    if !d.selected.remove(path) {
        d.selected.insert(path.to_string());
    }
    d.anchor = Some(path.to_string());
}

/// 相片的右鍵選單（這張有勾選時，刪除、星號是對全部勾選的）
fn photo_menu(ui: &mut egui::Ui, d: &PhotoBrowse, path: &str, starred: bool, out: &mut Panel) {
    ui.set_min_width(200.0);
    let many = d.selected.len() > 1 && d.selected.contains(path);
    let targets: Vec<String> = if many { d.selected.iter().cloned().collect() } else { vec![path.to_string()] };
    if !many {
        if ui.button(tr!("開啟", "Open")).clicked() {
            out.open = d.order.iter().position(|i| d.photos[*i].path == path);
            ui.close();
        }
        if ui.button(tr!("用 Windows 開啟", "Open in Windows")).clicked() {
            out.external = Some(path.to_string());
            ui.close();
        }
        if ui.button(tr!("在資料夾中顯示", "Show in folder")).clicked() {
            out.reveal = Some(path.to_string());
            ui.close();
        }
        ui.separator();
        if ui.button(tr!("複製圖片", "Copy image")).on_hover_text(tr!("可以直接貼到 LINE、Word、信件", "Paste it straight into LINE, Word or an email")).clicked() {
            out.copy = Some(path.to_string());
            ui.close();
        }
    }
    let copy_label = if many { trf!("複製 {} 個檔案", "Copy {} files", targets.len()) } else { tr!("複製檔案", "Copy file").to_string() };
    if ui.button(copy_label).on_hover_text(tr!("和在檔案總管按「複製」一樣，可以貼到資料夾、Teams、信件", "Same as Copy in File Explorer; paste into a folder, Teams or an email")).clicked()
    {
        out.copy_file = Some(targets.clone());
        ui.close();
    }
    ui.separator();
    let star = if starred && !many { tr!("移除星號", "Remove star") } else { tr!("加上星號", "Add star") };
    if ui.button(star).clicked() {
        out.star = Some((targets.clone(), !(starred && !many)));
        ui.close();
    }
    if !many && ui.button(tr!("重新命名…", "Rename…")).clicked() {
        out.rename = Some(path.to_string());
        ui.close();
    }
    let del = if many { trf!("刪除 {} 張…", "Delete {}…", targets.len()) } else { tr!("刪除…", "Delete…").to_string() };
    if ui.button(RichText::new(del).color(theme::pal(ui).rec)).clicked() {
        out.delete = Some(targets);
        ui.close();
    }
}

/// 最下面：勾選的操作、讀拍攝日期的進度、共幾張
fn footer(ui: &mut egui::Ui, d: &mut PhotoBrowse, out: &mut Panel, area: Rect, foot_h: f32) {
    let p = theme::pal(ui);
    let foot = Rect::from_min_max(pos2(area.min.x, ui.max_rect().max.y - foot_h), ui.max_rect().max);
    ui.painter().hline(foot.x_range(), foot.min.y - 4.0, Stroke::new(1.0, p.border));
    super::dialogs::toast_spot(ui.ctx(), pos2(foot.max.x, foot.min.y - 4.0));
    let right_w = {
        let right = &mut ui.new_child(UiBuilder::new().max_rect(foot).layout(Layout::right_to_left(Align::Center)));
        right.label(theme::muted(right, trf!("相片資料夾裡共 {} 張", "{} photos in all", d.photos.len())));
        if let Some((done, total)) = d.dating {
            right.add_space(12.0);
            right
                .label(theme::muted(right, trf!("正在讀拍攝日期 {done}/{total}", "Reading dates taken {done}/{total}")))
                .on_hover_text(tr!("還沒讀到的先用檔名裡的日期或修改時間排", "Photos not read yet are sorted by the date in the file name or the modified time"));
            right.spinner();
        }
        right.min_rect().width() + 16.0
    };
    let fui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(foot.min, pos2(foot.max.x - right_w, foot.max.y))).layout(Layout::left_to_right(Align::Center)));
    if d.selected.is_empty() {
        fui.label(theme::muted(fui, tr!("多選小技巧", "Multi-select tips"))).on_hover_text(tr!(
            "滑鼠移到相片上按左上角的方塊，或按住 Ctrl 點相片，勾選好幾張一次加星號或刪除；按住 Shift 按一下，從上次勾的那張到這一張全部勾起來；Ctrl+A 勾選全部",
            "Hover a photo and click the box in its top-left corner, or Ctrl+click photos, to star or delete several at once. Shift+click selects everything from the last one you checked. Ctrl+A selects all"
        ));
        return;
    }
    let list: Vec<String> = d.selected.iter().cloned().collect();
    fui.label(RichText::new(trf!("已勾選 {} 張", "{} selected", list.len())).font(theme::font_bold(13.0)));
    if Btn::new(tr!("加上星號", "Add star")).icon(Icon::Star).small().show(fui).clicked() {
        out.star = Some((list.clone(), true));
    }
    if Btn::new(tr!("移除星號", "Remove star")).small().ghost().show(fui).clicked() {
        out.star = Some((list.clone(), false));
    }
    if Btn::new(tr!("複製檔案", "Copy files"))
        .icon(Icon::Copy)
        .small()
        .tooltip(tr!("和在檔案總管按「複製」一樣，可以貼到資料夾、Teams、信件", "Same as Copy in File Explorer; paste into a folder, Teams or an email"))
        .show(fui)
        .clicked()
    {
        out.copy_file = Some(list.clone());
    }
    if Btn::new(tr!("刪除", "Delete")).icon(Icon::Trash).small().danger().show(fui).clicked() {
        out.delete = Some(list);
    }
    if Btn::new(tr!("取消勾選", "Clear selection")).small().ghost().show(fui).clicked() {
        d.selected.clear();
    }
}

/// 處理在分頁上按的（d 是從 app.photos 拿出來畫的那一份，處理完放回去）
pub fn apply(app: &mut UiApp, mut d: PhotoBrowse, out: Panel) {
    if let Some(to) = out.go.clone() {
        d.go(to);
    }
    // 記住縮圖大小、依日期、排序、停在哪個資料夾
    let at = match &d.at {
        Some((r, rel)) if *r < d.roots.len() => join_rel(&d.roots[*r], rel),
        _ => String::new(),
    };
    let sort = PhotoSort::ALL.iter().position(|s| *s == d.sort).unwrap_or(0) as u8;
    if (d.grid, d.by_date, sort, &at) != (app.s.photo_grid_size, app.s.photo_by_date, app.s.photo_sort, &app.s.photo_at) {
        (app.s.photo_grid_size, app.s.photo_by_date, app.s.photo_sort, app.s.photo_at) = (d.grid, d.by_date, sort, at);
        app.save_settings();
    }
    let mut want = out.want.clone();
    want.sort_unstable();
    want.dedup();
    for i in &want {
        if let Some(ph) = d.photos.get(*i) {
            if !d.low.contains(&ph.path) {
                d.thumbs.insert(ph.path.clone(), Thumb::Loading);
            }
            d.pending.insert(ph.path.clone());
        }
    }
    let open = out.open.map(|pos| viewer_list(&d, pos));
    let want: Vec<Photo> = want.iter().filter_map(|i| d.photos.get(*i).cloned()).collect();
    app.photos = Some(d);
    for ph in want {
        load_thumb(app, ph);
    }
    if let Some((list, idx)) = open {
        super::viewer::open_photos(app, list, idx);
    }
    if out.add_dir {
        add_dir(app);
    }
    if let Some(i) = out.remove_dir {
        if i < app.s.photo_dirs.len() {
            let gone = app.s.photo_dirs.remove(i);
            app.save_settings();
            app.toast(trf!("已移除 {gone}（資料夾和相片都沒有刪掉）", "Removed {gone} (the folder and photos weren't deleted)"), false);
            roots_changed(app, None);
        }
    }
    if let Some(dir) = out.open_dir {
        screenrecorder_core::desktop::open_with_explorer(&dir, false);
    }
    if out.rescan {
        if let Some(d) = app.photos.as_mut() {
            d.errors.clear();
        }
        scan(app);
    }
    if let Some((paths, on)) = out.star {
        set_star(app, &paths, on);
    }
    if let Some(paths) = out.delete {
        confirm_delete(app, paths);
    }
    if let Some(path) = out.rename {
        rename(app, path);
    }
    if let Some(path) = out.reveal {
        entry_act(app, super::EntryAction::Reveal, &path);
    }
    if let Some(path) = out.external {
        entry_act(app, super::EntryAction::External, &path);
    }
    if let Some(path) = out.copy {
        copy_image(app, path);
    }
    if let Some(paths) = out.copy_file {
        match screenrecorder_core::clipboard::copy_files(&paths) {
            Ok(()) => app.toast(tr!("已複製檔案，可以直接貼到資料夾、LINE、Teams 或信件", "Files copied. Paste them into a folder, LINE, Teams or an email"), false),
            Err(e) => app.toast(e, true),
        }
    }
}

/// 用檔案的動作（在資料夾中顯示、用 Windows 開啟）
fn entry_act(app: &mut UiApp, act: super::EntryAction, path: &str) {
    let entry = app.photos.as_ref().and_then(|d| d.find(path).map(|i| entry_of(d, i))).unwrap_or_else(|| LibraryEntry {
        media: MediaInfo { path: path.into(), name: super::dialogs::file_name(path), ..Default::default() },
        exports: vec![],
        hits: vec![],
    });
    app.act(act, entry);
}

/// 檢視器要的一張相片（created 放拍攝時間：當地時間當成 UTC 的毫秒，檢視器的標題列顯示）
fn entry_of(d: &PhotoBrowse, i: usize) -> LibraryEntry {
    let p = &d.photos[i];
    let folder = d.meta.get(i).map(|m| m.dir.clone()).unwrap_or_default();
    LibraryEntry {
        media: MediaInfo { path: p.path.clone(), name: p.name().to_string(), bytes: p.size, mtime: p.mtime, created: p.when as f64, folder, ..Default::default() },
        exports: vec![],
        hits: vec![],
    }
}

/// 檢視器的清單：畫面上的順序，點的那張前後各 VIEW_SPAN 張
fn viewer_list(d: &PhotoBrowse, pos: usize) -> (Vec<LibraryEntry>, usize) {
    let from = pos.saturating_sub(VIEW_SPAN);
    let to = (pos + VIEW_SPAN + 1).min(d.order.len());
    (d.order[from..to].iter().map(|i| entry_of(d, *i)).collect(), pos - from)
}

/// 背景產生縮圖、讀成材質。JPG 先放相片裡附的小縮圖（只讀開頭一小段，NAS 上很快），清楚的做好再換掉；
/// 排到了才發現捲過去、看不到了就不做（讓看得到的先做）
fn load_thumb(app: &mut UiApp, ph: Photo) {
    let Some(visible) = app.photos.as_ref().map(|d| d.visible.clone()) else { return };
    let ffmpeg = app.core.ffmpeg_path();
    let ctx = app.ctx.clone();
    let key = ph.path.clone();
    let wanted = {
        let (v, k) = (visible.clone(), key.clone());
        move || v.lock().unwrap_or_else(|e| e.into_inner()).contains(&k)
    };
    let to_tex = |ctx: &egui::Context, (w, h, rgba): (u32, u32, Vec<u8>)| {
        ctx.load_texture("photo-thumb", egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba), egui::TextureOptions::LINEAR)
    };
    // 小縮圖：還沒有清楚的才放上去
    if app.photos.as_ref().is_some_and(|d| !d.low.contains(&key)) {
        let (c2, k2, w2, p2) = (ctx.clone(), key.clone(), wanted.clone(), ph.path.clone());
        app.spawn(async move { photos::quick_thumb_async(p2, ph.size, ph.mtime, w2).await.map(|px| to_tex(&c2, px)) }, move |app, tex| {
            let Some(d) = app.photos.as_mut() else { return };
            if let Some(t) = tex.filter(|_| matches!(d.thumbs.peek(&k2), Some(Thumb::Loading))) {
                d.thumbs.insert(k2.clone(), Thumb::Ready(t));
                d.low.insert(k2);
            }
        });
    }
    app.spawn(
        async move {
            let w2 = wanted.clone();
            let file = photos::thumb(ffmpeg, ph.path.clone(), ph.size, ph.mtime, w2).await?;
            let px = tokio::task::spawn_blocking(move || photos::read_thumb(&file)).await.ok().flatten()?;
            Some(to_tex(&ctx, px))
        },
        move |app, tex| {
            let Some(d) = app.photos.as_mut() else { return };
            d.pending.remove(&key);
            match tex {
                Some(t) => {
                    d.thumbs.insert(key.clone(), Thumb::Ready(t));
                    d.low.remove(&key);
                }
                // 捲過去了（沒做）：看得到時再要
                None if !visible.lock().unwrap_or_else(|e| e.into_inner()).contains(&key) => {
                    if matches!(d.thumbs.peek(&key), Some(Thumb::Loading)) {
                        d.thumbs.retain(|k| *k != key);
                    }
                }
                // 做不出來：有小縮圖就一直用小的
                None => {
                    if !d.low.remove(&key) {
                        d.thumbs.insert(key, Thumb::Failed(Instant::now()));
                    }
                }
            }
        },
    );
}

/// 相片分頁已經有的縮圖（檢視器讀清楚的那張之前先放這張）
pub fn thumb_texture(app: &UiApp, path: &str) -> Option<egui::TextureHandle> {
    match app.photos.as_ref()?.thumbs.peek(&path.to_string()) {
        Some(Thumb::Ready(t)) => Some(t.clone()),
        _ => None,
    }
}

// ───────────── 整理（分頁與檢視器共用） ─────────────

/// 加上 / 移除星號
pub fn set_star(app: &mut UiApp, paths: &[String], on: bool) {
    for p in paths {
        app.core.photo_stars.set(p, on);
    }
    if let Some(d) = app.photos.as_mut() {
        if d.only_starred {
            d.dirty = true;
        }
    }
    let n = paths.len();
    let msg = match (on, n) {
        (true, 1) => tr!("已加上星號", "Starred").to_string(),
        (false, 1) => tr!("已移除星號", "Star removed").to_string(),
        (true, _) => trf!("已幫 {n} 張加上星號", "Starred {n} photos"),
        (false, _) => trf!("已移除 {n} 張的星號", "Removed the star from {n} photos"),
    };
    app.toast(msg, false);
}

/// 刪除前確認（NAS 上的相片不會進這台電腦的資源回收筒，說清楚）
pub fn confirm_delete(app: &mut UiApp, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let network = paths.iter().any(|p| photos::is_network(p));
    let n = paths.len();
    let title = if n == 1 { tr!("刪除這張相片？", "Delete this photo?").to_string() } else { trf!("刪除這 {n} 張相片？", "Delete these {n} photos?") };
    let msg = if network {
        tr!(
            "相片在 NAS 上，刪掉後不會進這台電腦的資源回收筒。NAS 有開啟資源回收筒時（例如 Synology 的 #recycle），可以從 NAS 那邊還原。",
            "These photos are on a NAS, so they won't go to this PC's Recycle Bin. If the NAS has its own recycle bin turned on (such as #recycle on Synology), you can restore them there."
        )
    } else {
        tr!("可從資源回收筒還原。", "You can restore them from the Recycle Bin.")
    };
    let names: Vec<String> = paths.iter().map(|p| super::dialogs::file_name(p)).collect();
    let mut ask = super::dialogs::Ask::confirm(title, msg, if network { tr!("刪除", "Delete") } else { tr!("移到資源回收筒", "Move to Recycle Bin") }, move |app, _| {
        app.ask = None;
        let list = paths.clone();
        app.spawn(async move { tokio::task::spawn_blocking(move || photos::delete(&list)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
            // 刪不掉的（還在的）留在清單上
            let gone: Vec<String> = paths.iter().filter(|p| !std::path::Path::new(p).exists()).cloned().collect();
            removed(app, &gone);
            match r {
                Ok(()) => app.toast(if gone.len() == 1 { tr!("已刪除", "Deleted").to_string() } else { trf!("已刪除 {} 張", "Deleted {}", gone.len()) }, false),
                Err(e) => app.toast(e, true),
            }
        });
    });
    ask.danger = true;
    ask.list = names;
    app.ask = Some(ask);
}

/// 這些相片刪掉了：從清單、星號、檢視器移除
fn removed(app: &mut UiApp, gone: &[String]) {
    if gone.is_empty() {
        return;
    }
    for p in gone {
        app.core.photo_stars.removed(p);
    }
    if let Some(d) = app.photos.as_mut() {
        let set: HashSet<&String> = gone.iter().collect();
        d.photos.retain(|p| !set.contains(&p.path));
        for p in gone {
            d.selected.remove(p);
        }
        d.index_changed();
    }
    super::viewer::photos_removed(app, gone);
    save(app);
}

/// 重新命名（副檔名不變）
pub fn rename(app: &mut UiApp, path: String) {
    let old = super::dialogs::file_name(&path);
    let base = old.rsplit_once('.').map_or(old.clone(), |(b, _)| b.to_string());
    let ext = old.rsplit_once('.').map(|(_, e)| format!(".{e}")).unwrap_or_default();
    let mut ask = super::dialogs::Ask::input(tr!("重新命名", "Rename"), tr!("新名稱", "New name"), base.clone(), tr!("重新命名", "Rename"), move |app, value| {
        let value = value.trim().trim_end_matches(ext.as_str()).to_string();
        if value == base {
            app.ask = None;
            return;
        }
        if let Some(a) = &mut app.ask {
            a.busy = true;
        }
        let p2 = path.clone();
        let v2 = value.clone();
        app.spawn(async move { tokio::task::spawn_blocking(move || photos::rename(&p2, &v2)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| match r {
            Ok(new) => {
                app.ask = None;
                renamed(app, &path, &new);
                app.toast(trf!("已重新命名為 {}", "Renamed to {}", super::dialogs::file_name(&new)), false);
            }
            Err(e) => {
                // 留在對話框，可以改了再按
                let p3 = path.clone();
                app.ask = None;
                rename(app, p3);
                if let Some(a) = &mut app.ask {
                    a.input = Some(value);
                    a.error = Some(e);
                }
            }
        });
    });
    ask.message = Some(tr!("副檔名不變。", "The file extension stays the same.").into());
    app.ask = Some(ask);
}

/// 改好名稱：清單、星號、檢視器跟著改
fn renamed(app: &mut UiApp, old: &str, new: &str) {
    app.core.photo_stars.renamed(old, new);
    if let Some(d) = app.photos.as_mut() {
        if let Some(i) = d.find(old) {
            let mut ph = d.photos.remove(i);
            ph.path = new.to_string();
            let at = d.photos.binary_search_by(|p| p.path.as_str().cmp(new)).unwrap_or_else(|x| x);
            d.photos.insert(at, ph);
        }
        if d.selected.remove(old) {
            d.selected.insert(new.to_string());
        }
        d.index_changed();
    }
    super::viewer::renamed(app, old, new);
    save(app);
}

/// 整張相片複製到剪貼簿（圖片）
pub fn copy_image(app: &mut UiApp, path: String) {
    app.toast(tr!("正在複製…", "Copying…"), false);
    let ffmpeg = app.core.ffmpeg_path();
    app.spawn(async move { tokio::task::spawn_blocking(move || photos::copy_to_clipboard(ffmpeg.as_deref(), &path)).await.unwrap_or(false) }, |app, ok| {
        if ok {
            app.toast(tr!("已複製到剪貼簿，可以直接貼到 LINE、Word、信件", "Copied. Paste it straight into LINE, Word or an email"), false);
        } else {
            app.toast(tr!("無法複製到剪貼簿", "Couldn't copy to clipboard"), true);
        }
    });
}

/// 相片的星號（檢視器的按鈕用）
pub fn starred(app: &UiApp, path: &str) -> bool {
    app.core.photo_stars.contains(path)
}

/// 拍攝日期的說法（檢視器的標題列用；不在清單裡時 None）
pub fn date_of(app: &UiApp, path: &str) -> Option<String> {
    let d = app.photos.as_ref()?;
    d.find(path).map(|i| date_text(&d.photos[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browse(roots: &[&str], paths: &[(&str, &str)]) -> PhotoBrowse {
        let mut d = PhotoBrowse {
            roots: roots.iter().map(|s| s.to_string()).collect(),
            photos: vec![],
            meta: vec![],
            loaded: true,
            scanning: None,
            errors: vec![],
            dating: None,
            date_stop: Arc::new(AtomicBool::new(false)),
            at: None,
            by_date: false,
            search: String::new(),
            searched: String::new(),
            search_at: None,
            year: None,
            only_starred: false,
            sort: PhotoSort::New,
            grid: 1,
            sections: vec![],
            order: vec![],
            dirty: true,
            years: vec![],
            selected: HashSet::new(),
            anchor: None,
            thumbs: Lru::new(10),
            low: HashSet::new(),
            pending: HashSet::new(),
            seen_now: HashSet::new(),
            visible: Arc::default(),
            offsets: HashMap::new(),
            want_offset: None,
            wheel_at: None,
            saved_at: Instant::now(),
        };
        d.photos = paths
            .iter()
            .map(|(p, date)| {
                let t = chrono::NaiveDateTime::parse_from_str(&format!("{date} 12:00:00"), "%Y-%m-%d %H:%M:%S").unwrap();
                Photo { path: p.to_string(), size: 1, mtime: 0.0, when: t.and_utc().timestamp_millis(), from: DateFrom::Exif, exif_read: true }
            })
            .collect();
        d.photos.sort_by(|a, b| a.path.cmp(&b.path));
        d.fix_top();
        d.index_changed();
        d
    }

    fn names(d: &PhotoBrowse) -> Vec<String> {
        d.sections
            .iter()
            .flat_map(|s| s.cells.iter())
            .map(|c| match c {
                Cell::Folder { name, count, .. } => format!("[{name} {count}]"),
                Cell::Photo(i) => d.photos[*i].name().to_string(),
            })
            .collect()
    }

    #[test]
    fn folders_then_photos_and_navigation() {
        let stars = PinStore::new(std::env::temp_dir().join(format!("sr-photo-stars-{}.json", std::process::id())));
        let mut d = browse(
            &["\\\\NAS\\photo"],
            &[
                ("\\\\NAS\\photo\\travel\\2024\\海邊.jpg", "2024-05-12"),
                ("\\\\NAS\\photo\\travel\\2023\\山上.jpg", "2023-08-01"),
                ("\\\\NAS\\photo\\travel\\封面.jpg", "2022-01-01"),
                ("\\\\NAS\\photo\\生日.heic", "2025-03-03"),
            ],
        );
        // 只有一個相片資料夾：最上層就是它
        assert_eq!(d.at, Some((0, String::new())));
        d.rebuild(&stars);
        assert_eq!(names(&d), ["[travel 3]", "生日.heic"]);
        d.go(Some((0, "travel".into())));
        d.rebuild(&stars);
        // 新到舊：2024 的資料夾在前
        assert_eq!(names(&d), ["[2024 1]", "[2023 1]", "封面.jpg"]);
        // 依日期：這個資料夾底下全部，依月份分段
        d.by_date = true;
        d.rebuild(&stars);
        assert_eq!(names(&d), ["海邊.jpg", "山上.jpg", "封面.jpg"]);
        assert_eq!(d.sections.len(), 3);
        // 年份
        d.year = Some(2023);
        d.rebuild(&stars);
        assert_eq!(names(&d), ["山上.jpg"]);
        assert_eq!(d.years, [(2024, 1), (2023, 1), (2022, 1)]);
        // 搜尋：資料夾名稱、日期
        d.year = None;
        d.by_date = false;
        d.go(Some((0, String::new())));
        d.searched = "2024".into();
        d.rebuild(&stars);
        assert_eq!(names(&d), ["海邊.jpg"]);
        d.searched = "2023/8/1".into();
        d.rebuild(&stars);
        assert_eq!(names(&d), ["山上.jpg"]);
        // 有星號的
        d.searched.clear();
        stars.set("\\\\NAS\\photo\\生日.heic", true);
        d.only_starred = true;
        d.rebuild(&stars);
        assert_eq!(names(&d), ["生日.heic"]);
        stars.set("\\\\NAS\\photo\\生日.heic", false);
        // 回到上一層
        d.only_starred = false;
        d.go(Some((0, "travel/2024".into())));
        go_up(&mut d);
        assert_eq!(d.at, Some((0, "travel".into())));
    }

    #[test]
    fn several_roots_and_shift_select() {
        let stars = PinStore::new(std::env::temp_dir().join(format!("sr-photo-stars2-{}.json", std::process::id())));
        let mut d = browse(&["/video/family", "/video/family/old", "D:\\相片"], &[("/video/family/a.jpg", "2024-01-01"), ("/video/family/old/b.jpg", "2020-01-01"), ("D:\\相片\\c.png", "2021-01-01")]);
        assert_eq!(d.at, None);
        d.rebuild(&stars);
        // 一個在另一個裡面時算在最長的那個
        assert_eq!(names(&d), ["[family 1]", "[相片 1]", "[old 1]"]);
        d.by_date = true;
        d.rebuild(&stars);
        assert_eq!(names(&d), ["a.jpg", "c.png", "b.jpg"]);
        pick(&mut d, "/video/family/a.jpg", false);
        pick(&mut d, "/video/family/old/b.jpg", true);
        assert_eq!(d.selected.len(), 3);
        pick(&mut d, "D:\\相片\\c.png", false);
        assert_eq!(d.selected.len(), 2);
        let (list, idx) = viewer_list(&d, 2);
        assert_eq!((list.len(), idx, list[idx].media.name.as_str()), (3, 2, "b.jpg"));
    }
}
