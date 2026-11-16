//! 相片分頁的「人物」：在這台電腦上找出相片裡的臉、把同一個人分在一起（core::faces），什麼都不上傳。
//!
//! - 使用者在「人物」選單按「開始」才開啟（記在設定的 photoFaces）；模型沒下載時先下載（設定 → 外部元件的「人臉模型」）
//! - 背景一批批找（core::faces::analyze_many，同時 3 張），找完一批就分組、更新畫面，每 30 秒存一次 photo-faces.json；可以暫停
//! - 「人物」選單：選一個人只看有他的相片；「整理人物…」開啟視窗：取名、合併、隱藏、把分錯的臉移出或移到別人
//! - 搜尋也找人物的名稱；檢視器的標題列寫相片裡有誰
//! - 有人閉眼的相片：「人物」選單可以只看這些（判斷的寬嚴記在設定的 photoBlink），縮圖右下角標出來；隱藏的人（卡通、雕像、路人）不算。
//!   舊版找過的相片會在背景再看一次眼睛（只讀有臉的那些），原本分好的人、取的名字不變

use super::theme::{self, Btn, Icon};
use super::thumbs::{Lru, Thumb};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Id, Layout, Rect, RichText, Sense, Stroke};
use screenrecorder_core::components::Kind;
use screenrecorder_core::faces::{self, FaceIndex, PersonSummary};
use screenrecorder_core::{tr, trf};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 刪除全部人臉資料時加一：在那之前開始的背景工作不再存檔（不然刪掉的檔案又被寫回來）
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 臉的小圖多大（像素）
const FACE_PX: u32 = 96;
/// 「有人閉眼的」判斷的寬嚴（設定的 photoBlink：寬鬆、一般、嚴格）→ 兩眼閉上的程度要到多少
const BLINK: [f32; 3] = [0.6, 0.7, 0.8];
/// 整理人物的視窗最多列幾個人、一個人最多列幾張臉（每張臉第一次都要讀相片做小圖）
const MAX_TILES: usize = 300;
const MAX_FACES: usize = 600;

/// 相片分頁裡人物的狀態（放在 PhotoBrowse 裡）
pub struct People {
    /// 臉的資料（讀好之前 None）；背景找臉時也會改，所以共用一份加鎖
    pub index: Option<Arc<Mutex<FaceIndex>>>,
    /// 每個人的整理結果（選單、整理人物視窗用；資料改了就重算）
    pub summary: Vec<PersonSummary>,
    /// 每張相片裡有取名的人（小寫，空白隔開；搜尋用）
    pub names: HashMap<String, String>,
    /// 只看有這個人的相片（或 eyes：只看有人閉眼的）、那些相片
    pub filter: Option<u32>,
    pub eyes: bool,
    pub filter_set: HashSet<String>,
    /// 有人閉眼的相片（縮圖上標出來）、判斷的寬嚴（BLINK 的第幾個）、還沒看過眼睛的舊資料有幾張
    pub closed: HashSet<String>,
    pub blink_level: u8,
    pub old: usize,
    /// 只看有人閉眼的時，一起列出前後幾秒拍的（挑睜眼的那張留下來）；看過眼睛、沒有人閉眼的相片
    pub near: bool,
    pub open: HashSet<String>,
    /// 只看臉模糊的、那些相片
    pub blur: bool,
    pub blurry: HashSet<String>,
    /// 還要同時有這幾個人（filter 以外的；「小明＋小美」）
    pub also: Vec<u32>,
    /// 相片裡要有幾張臉：0 不限、1、2、3 = 3 張以上；每張相片有幾張臉
    pub count: u8,
    counts: HashMap<String, usize>,
    /// 只看有人在笑的、那些相片
    pub smile: bool,
    pub smiling: HashSet<String>,
    /// 每張相片拍得好不好（同時間拍的裡面挑最好的一張）
    pub quality: HashMap<String, f32>,
    /// 只看一個人時每年挑幾張代表、他在每張相片裡拍得好不好
    pub yearly: bool,
    scores: HashMap<String, f32>,
    /// 分組的寬嚴（faces::LEVELS 的第幾個）、正在重新分組
    pub level: u8,
    regrouping: bool,
    /// 同一張相片裡的臉（一定是不同的人）兩兩比較：幾組、相似度到三個門檻的各幾組（看模型分不分得開）
    diag: (usize, [usize; 3]),
    /// 接上背景找臉了（暫停的狀態照背景的）、上次看到的是第幾批
    synced: bool,
    rev: u64,
    /// 背景正在找：（這次找了幾張, 這次要找幾張）；暫停、停下來（關掉分頁）
    pub running: Option<(usize, usize)>,
    pub done: usize,
    pub paused: bool,
    pub stop: Arc<AtomicBool>,
    /// 全部都找過了（相片換了時重設）、上次檢查的時間
    all_done: bool,
    checked_at: Option<Instant>,
    saved_at: Instant,
    pub error: Option<String>,
    /// 整理人物的視窗
    pub dlg: Option<Dlg>,
    /// 臉的小圖（「相片#第幾張臉」）
    tex: Lru<String>,
    /// 功能開了沒、模型下載的進度、下載失敗的原因、模型不見了（tick 更新，查詢列畫選單用）
    pub enabled: bool,
    pub dl: Option<f32>,
    pub dl_error: Option<String>,
    pub missing: bool,
}

impl Default for People {
    fn default() -> Self {
        People {
            index: None,
            summary: vec![],
            names: HashMap::new(),
            filter: None,
            eyes: false,
            filter_set: HashSet::new(),
            closed: HashSet::new(),
            blink_level: 1,
            old: 0,
            near: true,
            open: HashSet::new(),
            blur: false,
            blurry: HashSet::new(),
            also: vec![],
            count: 0,
            counts: HashMap::new(),
            smile: false,
            smiling: HashSet::new(),
            quality: HashMap::new(),
            yearly: false,
            scores: HashMap::new(),
            level: 1,
            regrouping: false,
            diag: (0, [0; 3]),
            synced: false,
            rev: 0,
            running: None,
            done: 0,
            paused: false,
            stop: Arc::new(AtomicBool::new(false)),
            all_done: false,
            checked_at: None,
            saved_at: Instant::now(),
            error: None,
            dlg: None,
            tex: Lru::new(1200),
            enabled: false,
            dl: None,
            dl_error: None,
            missing: false,
        }
    }
}

/// 整理人物的視窗
pub struct Dlg {
    selected: Option<u32>,
    /// 選的那個人的臉（相片, 第幾張）、勾選的臉
    faces: Vec<(String, usize)>,
    picked: HashSet<(String, usize)>,
    name: String,
    show_hidden: bool,
    show_single: bool,
    search: String,
    /// 勾選的幾組（一次隱藏）
    multi: HashSet<u32>,
}

impl People {
    /// 相片換了（掃描完、刪除…）：再看看有沒有要找的
    pub fn photos_changed(&mut self) {
        self.all_done = false;
    }

    /// 資料改了：重算整理結果、每張相片的名稱、篩選的相片
    fn refresh(&mut self) {
        let Some(idx) = self.index.clone() else { return };
        let idx = idx.lock().unwrap_or_else(|e| e.into_inner());
        self.summary = idx.summary();
        let named: HashMap<u32, String> = idx.people.iter().filter(|p| !p.name.is_empty()).map(|p| (p.id, p.name.to_lowercase())).collect();
        self.names = idx
            .photos
            .iter()
            .filter_map(|(path, pf)| {
                let mut v: Vec<&str> = pf.faces.iter().filter_map(|f| named.get(&f.person).map(String::as_str)).collect();
                v.dedup();
                (!v.is_empty()).then(|| (path.clone(), v.join(" ")))
            })
            .collect();
        self.closed = idx.closed_eyes(self.threshold());
        self.open = idx.open_eyes(self.threshold());
        self.blurry = idx.blurry();
        self.smiling = idx.smiling();
        self.counts = idx.face_counts();
        self.quality = idx.quality(self.threshold());
        self.old = idx.old_photos();
        self.level = idx.level.unwrap_or(1).min(2);
        self.diag = idx.same_photo_similarity();
        self.recompute(&idx);
        if let Some(d) = self.dlg.as_mut() {
            if d.selected.is_some_and(|id| idx.person(id).is_none()) {
                d.selected = None;
            }
            d.faces = d.selected.map(|id| idx.faces_of(id)).unwrap_or_default();
            d.picked.retain(|f| d.faces.contains(f));
        }
    }

    /// 這張相片裡有取名的人
    pub fn names_of(&self, path: &str) -> Vec<String> {
        self.index.as_ref().map(|i| i.lock().unwrap_or_else(|e| e.into_inner()).names_in(path)).unwrap_or_default()
    }

    fn threshold(&self) -> f32 {
        BLINK[(self.blink_level as usize).min(BLINK.len() - 1)]
    }

    /// 照現在的條件重算符合的相片：某個人（加上同時要有的人）、有人閉眼的、臉模糊的、有人在笑的，再加上幾張臉
    fn recompute(&mut self, idx: &FaceIndex) {
        if self.filter.is_some_and(|id| idx.person(id).is_none()) {
            self.filter = None;
            self.also.clear();
        }
        self.also.retain(|id| idx.person(*id).is_some());
        let mut set = if self.eyes {
            Some(self.closed.clone())
        } else if self.blur {
            Some(self.blurry.clone())
        } else if self.smile {
            Some(self.smiling.clone())
        } else if let Some(id) = self.filter {
            let mut s = idx.photos_of(id);
            for a in &self.also {
                let o = idx.photos_of(*a);
                s.retain(|p| o.contains(p));
            }
            Some(s)
        } else {
            None
        };
        if self.count != 0 {
            let want = self.count;
            let ok = |n: usize| if want >= 3 { n >= 3 } else { n == want as usize };
            let base: HashSet<String> = self.counts.iter().filter(|(_, n)| ok(**n)).map(|(k, _)| k.clone()).collect();
            set = Some(match set {
                Some(s) => s.into_iter().filter(|p| base.contains(p)).collect(),
                None => base,
            });
        }
        self.filter_set = set.unwrap_or_default();
        self.scores = match self.filter {
            Some(id) if self.yearly && self.also.is_empty() => idx.person_scores(id, self.threshold()),
            _ => HashMap::new(),
        };
    }

    fn recompute_now(&mut self) {
        match self.index.clone() {
            Some(idx) => self.recompute(&idx.lock().unwrap_or_else(|e| e.into_inner())),
            // 臉的資料還沒讀好：照手上有的（有人閉眼的…）算
            None => self.recompute(&FaceIndex::default()),
        }
    }

    fn mode(&mut self, filter: Option<u32>, eyes: bool, blur: bool, smile: bool) {
        (self.filter, self.eyes, self.blur, self.smile) = (filter, eyes, blur, smile);
        self.also.clear();
        self.recompute_now();
    }

    /// 只看有人閉眼的相片
    pub fn set_eyes(&mut self, on: bool) {
        self.mode(None, on, false, false);
    }

    /// 只看臉模糊的相片
    pub fn set_blur(&mut self) {
        self.mode(None, false, true, false);
    }

    /// 只看有人在笑的相片
    pub fn set_smile(&mut self) {
        self.mode(None, false, false, true);
    }

    /// 相片裡要有幾張臉（0 不限）
    pub fn set_count(&mut self, n: u8) {
        self.count = n;
        self.recompute_now();
    }

    /// 還要同時有這個人（還沒選人時就是只看他）
    pub fn add_person(&mut self, id: u32) {
        match self.filter {
            None => self.mode(Some(id), false, false, false),
            Some(f) if f == id || self.also.contains(&id) => {}
            Some(_) => {
                self.also.push(id);
                self.recompute_now();
            }
        }
    }

    pub fn set_yearly(&mut self, on: bool) {
        self.yearly = on;
        self.recompute_now();
    }

    /// 只看一個人、每年挑幾張代表時：他在每張相片裡拍得好不好
    pub fn yearly_scores(&self) -> Option<&HashMap<String, f32>> {
        (self.yearly && self.filter.is_some() && self.also.is_empty()).then_some(&self.scores)
    }

    /// 全部的條件都拿掉
    pub fn clear(&mut self) {
        self.count = 0;
        self.mode(None, false, false, false);
    }

    /// 有篩選（某個人、有人閉眼的、臉模糊的、有人在笑的、幾張臉）
    pub fn filtering(&self) -> bool {
        self.filter.is_some() || self.eyes || self.blur || self.smile || self.count != 0
    }

    /// 只看有人閉眼的、一起列出同時間拍的
    pub fn bursts(&self) -> bool {
        self.eyes && self.near
    }

    /// 這張相片裡閉眼的臉（第幾張, 位置）
    pub fn closed_faces(&self, path: &str) -> Vec<(usize, [f32; 4])> {
        if !self.closed.contains(path) {
            return vec![];
        }
        self.index.as_ref().map(|i| i.lock().unwrap_or_else(|e| e.into_inner()).closed_faces(path, self.threshold())).unwrap_or_default()
    }

    /// 選了某個人：只看有他的相片
    pub fn set_filter(&mut self, id: Option<u32>) {
        self.mode(id, false, false, false);
    }

    /// 「挑相片」選單上寫的：有人閉眼、臉模糊、有人在笑；沒有挑時 None
    pub fn describe_pick(&self) -> Option<String> {
        if self.eyes {
            Some(tr!("有人閉眼", "Eyes closed").into())
        } else if self.blur {
            Some(tr!("臉模糊", "Blurry faces").into())
        } else if self.smile {
            Some(tr!("有人在笑", "Smiling").into())
        } else {
            None
        }
    }

    /// 「人物」選單上寫的：選的人、幾個人；都沒有時 None
    pub fn describe_people(&self) -> Option<String> {
        if self.describe_pick().is_some() && self.count == 0 {
            return None;
        }
        let all = self.describe()?;
        match self.describe_pick() {
            Some(pick) => all.strip_prefix(&pick).map(|r| r.trim_start_matches(['、', ',', ' ']).to_string()).filter(|r| !r.is_empty()),
            None => Some(all),
        }
    }

    /// 現在篩選的說法（選單、位置列）：「小明＋小美」「有人閉眼」…再加上幾張臉；沒有篩選時 None
    pub fn describe(&self) -> Option<String> {
        let mut parts: Vec<String> = vec![];
        if let Some(id) = self.filter {
            let mut names = vec![self.label(id)];
            names.extend(self.also.iter().map(|a| self.label(*a)));
            parts.push(names.join("＋"));
        } else if self.eyes {
            parts.push(tr!("有人閉眼", "Eyes closed").into());
        } else if self.blur {
            parts.push(tr!("臉模糊", "Blurry faces").into());
        } else if self.smile {
            parts.push(tr!("有人在笑", "Smiling").into());
        }
        match self.count {
            0 => {}
            1 => parts.push(tr!("1 個人", "1 person").into()),
            2 => parts.push(tr!("2 個人", "2 people").into()),
            _ => parts.push(tr!("3 人以上", "3+ people").into()),
        }
        (!parts.is_empty()).then(|| parts.join(tr!("、", ", ")))
    }

    fn label(&self, id: u32) -> String {
        self.summary.iter().find(|s| s.person.id == id).map(|s| person_name(&s.person.name)).unwrap_or_default()
    }
}

fn person_name(name: &str) -> String {
    if name.is_empty() {
        tr!("未取名", "Unnamed").into()
    } else {
        name.to_string()
    }
}

// ───────────── 讀取、背景找臉 ─────────────

/// 開啟相片分頁時：有開啟人物功能（或有存過的資料）就讀臉的資料。整個程式只有一份（app.face_index），背景找臉也改同一份
pub fn open(app: &mut UiApp) {
    if let (Some(ix), Some(d)) = (app.face_index.clone(), app.photos.as_mut()) {
        d.people.index = Some(ix);
        d.people.blink_level = app.s.photo_blink;
        d.people.refresh();
        d.dirty = true;
        return;
    }
    if !app.s.photo_faces && !faces::index_file().exists() {
        return;
    }
    load_index(app);
}

/// 在背景讀臉的資料（讀好放進 app.face_index；相片分頁開著就接上去）
fn load_index(app: &mut UiApp) {
    if app.face_loading {
        return;
    }
    app.face_loading = true;
    let file = faces::index_file();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                // 舊版的資料：門檻提高了，分數不夠的臉拿掉；舊方法分的組重新分
                let mut idx = faces::load_index(&file);
                let changed = idx.upgrade();
                if changed {
                    idx.cluster();
                    faces::save_index(&file, &idx);
                }
                idx
            })
            .await
            .unwrap_or_default()
        },
        move |app, idx| {
            app.face_loading = false;
            if app.face_index.is_none() {
                app.face_index = Some(Arc::new(Mutex::new(idx)));
            }
            let level = app.s.photo_blink;
            if let (Some(ix), Some(d)) = (app.face_index.clone(), app.photos.as_mut()) {
                d.people.index = Some(ix);
                d.people.blink_level = level;
                d.people.refresh();
                d.dirty = true;
            }
        },
    );
}

/// 每一格畫面（相片分頁開著時）：把背景找臉的進度接到畫面上、該找就開始找
pub fn tick(app: &mut UiApp) {
    let enabled = app.s.photo_faces;
    let dl = downloading(app);
    let dl_error = app.core.component_status(Kind::Faces).error;
    let level = app.s.photo_blink;
    let (index, work) = (app.face_index.clone(), app.face_work.clone());
    let mut recheck = false;
    if let Some(d) = app.photos.as_mut() {
        let p = &mut d.people;
        (p.enabled, p.dl, p.dl_error) = (enabled, dl, dl_error.clone());
        if p.index.is_none() {
            if let Some(ix) = index {
                p.index = Some(ix);
                p.refresh();
                d.dirty = true;
            }
        }
        if p.blink_level != level {
            p.blink_level = level;
            p.refresh();
            d.dirty |= p.eyes;
        }
        // 相片換了、按了「再試一次」：再看看有沒有要找的
        if !p.all_done {
            p.all_done = true;
            recheck = true;
        }
        if let Some(w) = &work {
            // 第一次接上：暫停的狀態照背景的；之後照畫面上按的
            if !p.synced {
                p.synced = true;
                p.paused = w.paused.load(Ordering::Relaxed);
            }
            if let Some(e) = w.error.lock().unwrap_or_else(|x| x.into_inner()).take() {
                p.error = Some(e);
                p.paused = true;
            }
            w.paused.store(p.paused, Ordering::Relaxed);
            let (done, total) = (w.done.load(Ordering::Relaxed), w.total.load(Ordering::Relaxed));
            p.done = done;
            p.running = w.running.load(Ordering::Relaxed).then_some((done, total));
            let rev = w.rev.load(Ordering::Relaxed);
            if rev != p.rev {
                p.rev = rev;
                p.refresh();
                // 篩選、用名稱搜尋時跟著更新
                if p.filtering() || !d.searched.is_empty() {
                    d.dirty = true;
                }
            }
        }
    }
    if recheck {
        app.face_idle = false;
    }
    start_work(app);
}

/// 每一格畫面（不管檔案庫有沒有開）：錄影時讓路；相片分頁沒開時也在背景接著找（重開程式後自動繼續）
pub fn background(app: &mut UiApp) {
    use screenrecorder_core::types::RecorderState;
    faces::HOLD.store(app.status.recorder.state != RecorderState::Idle, Ordering::Relaxed);
    if app.photos.is_none() {
        start_work(app);
    }
}

/// 該找就開始找：開啟了人物功能、模型在、沒在找、沒暫停、還有沒找過的相片
fn start_work(app: &mut UiApp) {
    if !app.s.photo_faces || app.face_idle {
        return;
    }
    if app.face_work.as_ref().is_some_and(|w| w.running.load(Ordering::Relaxed) || w.paused.load(Ordering::Relaxed)) {
        return;
    }
    // 剛開程式先等一下（讓畫面、錄影先準備好）；之後每幾秒看一次
    let wait = if app.photos.is_some() { 2 } else { 15 };
    if app.face_checked.elapsed() < Duration::from_secs(wait) {
        return;
    }
    app.face_checked = Instant::now();
    if downloading(app).is_some() {
        return;
    }
    if !faces::models_ready() {
        // 已經開啟人物功能、模型不齊（新版多了模型）：自動下載缺的（這次開啟只試一次，失敗時在選單上再試）
        let failed = app.core.component_status(Kind::Faces).error.is_some();
        if let Some(d) = app.photos.as_mut() {
            d.people.missing = true;
        }
        if !app.face_auto_dl && !failed {
            app.face_auto_dl = true;
            app.core.component_download(Kind::Faces);
        }
        return;
    }
    if let Some(d) = app.photos.as_mut() {
        d.people.missing = false;
        // 相片清單還沒讀好：等一下
        if !d.loaded {
            return;
        }
    }
    let Some(index) = app.face_index.clone() else {
        load_index(app);
        return;
    };
    // 要找的相片：最近拍的先找。相片分頁開著用它的清單，沒開就讀上次存的清單
    let list: Option<Vec<(String, u64, f64, i64)>> = app.photos.as_ref().map(|d| d.photos.iter().map(|ph| (ph.path.clone(), ph.size, ph.mtime, ph.when)).collect());
    let work = Arc::new(faces::Work::default());
    work.running.store(true, Ordering::Relaxed);
    app.face_work = Some(work.clone());
    let (ffmpeg, ctx) = (app.core.ffmpeg_path(), app.ctx.clone());
    let generation = GENERATION.load(Ordering::Relaxed);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let mut list = list.unwrap_or_else(|| {
                    let idx = screenrecorder_core::photos::load_index(&screenrecorder_core::photos::index_file());
                    idx.photos.iter().map(|ph| (ph.path.clone(), ph.size, ph.mtime, ph.when)).collect()
                });
                list.sort_by_key(|x| std::cmp::Reverse(x.3));
                let photos = list.into_iter().map(|(p, s, m, _)| (p, s, m)).collect();
                let n = faces::run_work(&index, &work, photos, ffmpeg, || GENERATION.load(Ordering::Relaxed) == generation, || ctx.request_repaint());
                work.running.store(false, Ordering::Relaxed);
                work.rev.fetch_add(1, Ordering::Relaxed);
                (n, work.stop.load(Ordering::Relaxed) || work.error.lock().unwrap_or_else(|x| x.into_inner()).is_some())
            })
            .await
            .unwrap_or((0, true))
        },
        |app, (n, stopped)| {
            // 沒有要找的了：不用一直看（相片換了再看）
            if n == 0 && !stopped {
                app.face_idle = true;
            }
        },
    );
}

/// 在背景存臉的資料
fn save(index: Arc<Mutex<FaceIndex>>, saved_at: &mut Instant, app: &UiApp) {
    *saved_at = Instant::now();
    let generation = GENERATION.load(Ordering::Relaxed);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let idx = index.lock().unwrap_or_else(|e| e.into_inner());
                if GENERATION.load(Ordering::Relaxed) == generation {
                    faces::save_index(&faces::index_file(), &idx);
                }
            })
            .await
            .ok()
        },
        |_, _| {},
    );
}

/// 改了人物（取名、合併…）之後：重算畫面上的、存起來
fn changed(app: &mut UiApp) {
    let Some(d) = app.photos.as_mut() else { return };
    d.people.refresh();
    d.dirty = true;
    if let Some(index) = d.people.index.clone() {
        let mut at = d.people.saved_at;
        save(index, &mut at, app);
        if let Some(d) = app.photos.as_mut() {
            d.people.saved_at = at;
        }
    }
}

/// 開啟人物功能：模型還沒下載就先下載，好了就開始找
fn enable(app: &mut UiApp) {
    app.s.photo_faces = true;
    app.save_settings();
    if let Some(d) = app.photos.as_mut() {
        (d.people.dl_error, d.people.missing, d.people.checked_at) = (None, false, None);
    }
    if !faces::models_ready() {
        app.core.component_download(Kind::Faces);
        app.toast(trf!("正在下載人臉模型（約 {} MB），好了就開始找相片裡的人", "Downloading the face models (about {} MB). Finding people starts when it's done", faces::MB), false);
    } else {
        app.toast(tr!("開始找相片裡的人（在這台電腦上，不上傳）", "Finding people in your photos (on this computer; nothing is uploaded)"), false);
    }
    if app.photos.as_ref().is_some_and(|d| d.people.index.is_none()) {
        if let Some(d) = app.photos.as_mut() {
            let ix = Arc::new(Mutex::new(FaceIndex { grouping: faces::GROUPING, ..Default::default() }));
            app.face_index = Some(ix.clone());
            d.people.index = Some(ix);
            d.people.paused = false;
        }
        open(app);
    }
}

// ───────────── 查詢列的「人物」 ─────────────

/// 查詢列的「人物」選單（開啟前是說明與「開始」）；回傳要做的事
pub enum MenuAct {
    None,
    Enable,
    Organize,
    Filter(Option<u32>),
    Eyes,
    BlinkLevel(u8),
    Near(bool),
    Blur,
    Smile,
    Add(u32),
    Count(u8),
    Yearly(bool),
}

pub fn menu(ui: &mut egui::Ui, p: &People, enabled: bool, downloading: Option<f32>) -> MenuAct {
    let mut act = MenuAct::None;
    let text = p.describe_people().unwrap_or_else(|| tr!("全部的人", "Everyone").to_string());
    egui::ComboBox::from_id_salt("photo-person")
        .selected_text(text)
        .width(110.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(260.0);
            if !enabled {
                ui.label(RichText::new(tr!("找出相片裡的人", "Find people in your photos")).strong());
                ui.add(
                    egui::Label::new(theme::muted(
                        ui,
                        trf!(
                            "在這台電腦上找出相片裡的臉，把同一個人分在一起，取了名字就能只看有他的相片、用名字搜尋，也能找出有人閉眼的相片。相片和臉都不會上傳；第一次要下載約 {} MB 的模型，相片多時要在背景找一陣子。",
                            "Finds faces in your photos on this computer and groups each person together. Name them to see only their photos or search by name, and find photos where someone's eyes are closed. No photos or faces are uploaded. The first time downloads about {} MB of models, and with many photos it takes a while in the background.",
                            faces::MB
                        ),
                    ))
                    .wrap(),
                );
                if Btn::new(tr!("開始", "Start")).primary().small().show(ui).clicked() {
                    act = MenuAct::Enable;
                    ui.close();
                }
                return;
            }
            if let Some(f) = downloading {
                ui.label(theme::muted(ui, trf!("正在下載人臉模型 {}%…", "Downloading face models {}%…", (f * 100.0) as u32)));
            } else if p.dl_error.is_some() || p.missing {
                let why = p.dl_error.clone().unwrap_or_else(|| tr!("人臉模型不在了", "The face models are missing").into());
                ui.add(egui::Label::new(RichText::new(trf!("{why}。可以再試一次，或到「設定 → 外部元件」用瀏覽器下載", "{why}. Try again, or download them in a browser from Settings → Components")).color(theme::pal(ui).warn)).wrap());
                if Btn::new(tr!("再試一次", "Try again")).small().show(ui).clicked() {
                    act = MenuAct::Enable;
                    ui.close();
                }
                ui.separator();
            }
            if ui.selectable_label(p.filter.is_none() && p.count == 0, tr!("全部的人", "Everyone")).clicked() {
                act = MenuAct::Filter(None);
            }
            let named: Vec<&PersonSummary> = p.summary.iter().filter(|s| !s.person.name.is_empty() && !s.person.hidden).collect();
            for s in &named {
                let on = p.filter == Some(s.person.id) || p.also.contains(&s.person.id);
                ui.horizontal(|ui| {
                    if ui.selectable_label(on, format!("{}　{}", s.person.name, s.photos)).clicked() {
                        act = MenuAct::Filter(Some(s.person.id));
                    }
                    // 已經選了人：「＋」再加一個，只看他們都在的相片
                    if p.filter.is_some() && !on && ui.small_button("＋").on_hover_text(tr!("加上這個人：只看他們都在的相片", "Add this person: show only photos with all of them")).clicked() {
                        act = MenuAct::Add(s.person.id);
                    }
                });
            }
            if p.filter.is_some() && p.also.is_empty() {
                let mut yearly = p.yearly;
                if ui
                    .checkbox(&mut yearly, tr!("每年挑幾張代表", "A few highlights per year"))
                    .on_hover_text(tr!("一年一排，挑這個人拍得最好的幾張（臉大、清楚、沒閉眼、在笑），看得出每年的變化", "One row per year with this person's best shots (big, sharp, eyes open, smiling), to see how they change over the years"))
                    .changed()
                {
                    act = MenuAct::Yearly(yearly);
                }
            }
            if named.is_empty() {
                ui.label(theme::muted(ui, tr!("還沒有取名的人：按下面的「整理人物…」幫他們取名", "Nobody is named yet. Use “Organize people…” below to name them")));
            }
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("幾個人", "People")));
                for (n, zh, en) in [(0u8, "不限", "Any"), (1, "1 人", "1"), (2, "2 人", "2"), (3, "3 人以上", "3+")] {
                    if ui.selectable_label(p.count == n, tr!(zh, en)).on_hover_text(tr!("相片裡有幾張臉（隱藏的人不算），可以和上面的條件一起用", "How many faces are in the photo (hidden people don't count); combines with the choice above")).clicked() {
                        act = MenuAct::Count(n);
                    }
                }
            });
            ui.separator();
            let unnamed = p.summary.iter().filter(|s| s.person.name.is_empty() && !s.person.hidden && s.photos > 1).count();
            let label = if unnamed > 0 { trf!("整理人物…（{unnamed} 組還沒取名）", "Organize people… ({unnamed} unnamed)") } else { tr!("整理人物…", "Organize people…").to_string() };
            if ui.button(label).clicked() {
                act = MenuAct::Organize;
                ui.close();
            }
        })
        .response
        .on_hover_text(tr!("只看有某個人的相片；在這台電腦上找臉、分組，不上傳", "Show only photos of one person. Faces are found and grouped on this computer; nothing is uploaded"));
    // 「挑相片」：閉眼、模糊、在笑（人物功能開了才有）
    if enabled {
        let text = p.describe_pick().unwrap_or_else(|| tr!("挑相片", "Pick photos").to_string());
        egui::ComboBox::from_id_salt("photo-pick")
            .selected_text(text)
            .width(96.0)
            .show_ui(ui, |ui| {
                ui.set_min_width(260.0);
                if ui.selectable_label(!(p.eyes || p.blur || p.smile), tr!("不挑", "Don't pick")).clicked() && (p.eyes || p.blur || p.smile) {
                    act = MenuAct::Filter(None);
                }
                // 有人閉眼的相片、判斷的寬嚴
                let tip = tr!(
                    "兩隻眼睛都閉上的臉（太小的臉、隱藏的人不算）。在這台電腦上判斷，不上傳；偶爾會把瞇眼大笑當成閉眼",
                    "Faces with both eyes closed (tiny faces and hidden people don't count). Checked on this computer; nothing is uploaded. A big squinting smile is sometimes counted as closed"
                );
                if ui.selectable_label(p.eyes, trf!("有人閉眼的　{}", "Eyes closed  {}", p.closed.len())).on_hover_text(tip).clicked() {
                    act = MenuAct::Eyes;
                }
                ui.horizontal(|ui| {
                    ui.label(theme::muted(ui, tr!("閉眼的判斷", "Closed eyes")));
                    for (i, (zh, en, tip_zh, tip_en)) in [
                        ("寬鬆", "Loose", "半閉、瞇眼也算（找到的多，看錯的也多）", "Half-closed and squinting count too (finds more, with more mistakes)"),
                        ("一般", "Normal", "眼睛閉上的", "Eyes closed"),
                        ("嚴格", "Strict", "只算很確定是閉上的（找到的少）", "Only eyes that are clearly closed (finds fewer)"),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        if ui.selectable_label(p.blink_level as usize == i, tr!(zh, en)).on_hover_text(tr!(tip_zh, tip_en)).clicked() {
                            act = MenuAct::BlinkLevel(i as u8);
                        }
                    }
                });
                let mut near = p.near;
                if ui
                    .checkbox(&mut near, tr!("一起列出同時間拍的", "Also list photos taken at the same time"))
                    .on_hover_text(tr!(
                        "看「有人閉眼的」時，把前後 10 秒內拍的相片排在一起：閉眼的有標示，綠色眼睛是沒有人閉眼的，勾選不要的一起刪",
                        "With “Eyes closed”, photos taken within 10 seconds are listed together: closed eyes are marked, a green eye means nobody's eyes are closed, and you can select the ones to delete"
                    ))
                    .changed()
                {
                    act = MenuAct::Near(near);
                }
                if ui
                    .selectable_label(p.blur, trf!("臉模糊的　{}", "Blurry faces  {}", p.blurry.len()))
                    .on_hover_text(tr!(
                        "相片裡最清楚的那張臉也模糊（失焦、手震）；背景的人模糊不算，太小的臉、隱藏的人不看。在這台電腦上判斷，不上傳",
                        "Even the sharpest face in the photo is blurry (out of focus or camera shake). Blurry people in the background don't count; tiny faces and hidden people are skipped. Checked on this computer; nothing is uploaded"
                    ))
                    .clicked()
                {
                    act = MenuAct::Blur;
                }
                if ui
                    .selectable_label(p.smile, trf!("有人在笑的　{}", "Smiling  {}", p.smiling.len()))
                    .on_hover_text(tr!("有人嘴角上揚的相片（太小的臉、隱藏的人不看）。在這台電腦上判斷，不上傳", "Photos where someone is smiling (tiny faces and hidden people are skipped). Checked on this computer; nothing is uploaded"))
                    .clicked()
                {
                    act = MenuAct::Smile;
                }
                if p.old > 0 {
                    ui.label(theme::muted(ui, trf!("還有 {} 張舊的相片要再看一次（在背景）", "{} older photos still need another look (in the background)", p.old)));
                }
            })
            .response
            .on_hover_text(tr!("挑出拍壞的或拍得好的：有人閉眼的、臉模糊的、有人在笑的；在這台電腦上判斷，不上傳", "Pick out bad or good shots: eyes closed, blurry faces, smiling. Checked on this computer; nothing is uploaded"));
    }
    act
}

/// 處理選單按了的
pub fn menu_act(app: &mut UiApp, act: MenuAct) {
    match act {
        MenuAct::None => {}
        MenuAct::Enable => enable(app),
        MenuAct::Organize => {
            if let Some(d) = app.photos.as_mut() {
                let sel = d.people.filter;
                d.people.dlg =
                    Some(Dlg { selected: None, faces: vec![], picked: HashSet::new(), name: String::new(), show_hidden: false, show_single: false, search: String::new(), multi: HashSet::new() });
                select(&mut d.people, sel);
            }
        }
        // 選了某個人、有人閉眼的：看全部資料夾（直接列出符合的相片，不用一層層點進去）
        MenuAct::Filter(id) => {
            if let Some(d) = app.photos.as_mut() {
                match id {
                    Some(_) => d.people.set_filter(id),
                    None => d.people.clear(),
                }
                if id.is_some() {
                    d.go_top();
                }
                d.want_offset = Some(0.0);
                d.dirty = true;
            }
        }
        MenuAct::Eyes => {
            if let Some(d) = app.photos.as_mut() {
                d.people.set_eyes(true);
                d.go_top();
                d.want_offset = Some(0.0);
                d.dirty = true;
            }
        }
        MenuAct::Near(on) => {
            if let Some(d) = app.photos.as_mut() {
                d.people.near = on;
                d.dirty = true;
            }
        }
        MenuAct::Blur => {
            if let Some(d) = app.photos.as_mut() {
                d.people.set_blur();
                d.go_top();
                d.want_offset = Some(0.0);
                d.dirty = true;
            }
        }
        MenuAct::Smile | MenuAct::Add(_) | MenuAct::Count(_) | MenuAct::Yearly(_) => {
            if let Some(d) = app.photos.as_mut() {
                match act {
                    MenuAct::Smile => d.people.set_smile(),
                    MenuAct::Add(id) => d.people.add_person(id),
                    MenuAct::Count(n) => d.people.set_count(n),
                    MenuAct::Yearly(on) => d.people.set_yearly(on),
                    _ => {}
                }
                d.go_top();
                d.want_offset = Some(0.0);
                d.dirty = true;
            }
        }
        MenuAct::BlinkLevel(level) => {
            app.s.photo_blink = level;
            app.save_settings();
            // 下一格 tick 會重算有人閉眼的相片
        }
    }
}

/// 最下面一行：找臉的進度、暫停 / 繼續；回傳寬度
pub fn footer(ui: &mut egui::Ui, p: &mut People) {
    if p.enabled && p.dl.is_none() && (p.dl_error.is_some() || p.missing) {
        let why = p.dl_error.clone().unwrap_or_else(|| tr!("人臉模型不在了", "The face models are missing").into());
        ui.label(RichText::new(tr!("人臉模型沒有下載好", "Face models aren't downloaded")).color(theme::pal(ui).warn)).on_hover_text(trf!(
            "{why}。可以在「人物」選單再試一次，或到「設定 → 外部元件」用瀏覽器下載",
            "{why}. Try again from the People menu, or download them in a browser from Settings → Components"
        ));
        return;
    }
    if let Some(f) = p.dl {
        ui.label(theme::muted(ui, trf!("正在下載人臉模型 {}%", "Downloading face models {}%", (f * 100.0) as u32)));
        ui.spinner();
        return;
    }
    if let Some(e) = &p.error {
        let r = ui.label(RichText::new(tr!("找臉時出了問題", "Couldn't find faces")).color(theme::pal(ui).warn)).on_hover_text(e);
        if r.clicked() || Btn::new(tr!("再試一次", "Try again")).small().ghost().show(ui).clicked() {
            p.error = None;
            p.paused = false;
            p.all_done = false;
        }
        return;
    }
    let Some((done, total)) = p.running.or_else(|| p.paused.then_some((p.done, 0))) else { return };
    if p.paused {
        if Btn::new(tr!("繼續找人臉", "Resume finding faces")).small().ghost().show(ui).clicked() {
            p.paused = false;
            p.checked_at = None;
        }
        return;
    }
    if Btn::new(tr!("暫停", "Pause")).small().ghost().tooltip(tr!("暫停找人臉（NAS 很忙時）", "Pause finding faces (when the NAS is busy)")).show(ui).clicked() {
        p.paused = true;
    }
    if faces::HOLD.load(Ordering::Relaxed) {
        ui.label(theme::muted(ui, trf!("錄影中，找人臉先停一下 {done}/{total}", "Recording — finding people is on hold {done}/{total}")));
        return;
    }
    ui.label(theme::muted(ui, trf!("正在找相片裡的人 {done}/{total}", "Finding people {done}/{total}"))).on_hover_text(tr!(
        "在這台電腦上找臉、分組，不上傳；找好的會馬上出現在「人物」。關掉檔案庫也會在背景繼續找，重開程式後接著找",
        "Faces are found and grouped on this computer and nothing is uploaded. Results show up in People right away. It keeps going in the background after you close the library, and picks up again after a restart"
    ));
    ui.spinner();
}

// ───────────── 整理人物的視窗 ─────────────

fn select(p: &mut People, id: Option<u32>) {
    let faces = match (id, p.index.as_ref()) {
        (Some(id), Some(i)) => i.lock().unwrap_or_else(|e| e.into_inner()).faces_of(id),
        _ => vec![],
    };
    let name = id.and_then(|id| p.summary.iter().find(|s| s.person.id == id)).map(|s| s.person.name.clone()).unwrap_or_default();
    if let Some(d) = p.dlg.as_mut() {
        d.selected = id;
        d.faces = faces;
        d.picked.clear();
        d.name = name;
    }
}

/// 視窗裡按了什麼（畫完才處理）
enum Act {
    None,
    Close,
    Select(u32),
    Rename(u32, String),
    Hide(u32, bool),
    HideMany(Vec<u32>),
    Merge(u32, u32),
    NotThis(Vec<(String, usize)>, u32),
    MoveTo(Vec<(String, usize)>, Option<u32>),
    ShowPhotos(u32),
    DeleteAll,
    ToJellyfin,
    /// 分組的寬嚴改了（重新分組）、照現在的寬嚴重新分組
    Regroup(Option<u8>),
}

/// 臉的小圖材質（還沒有時在背景做）
fn face_tex(app: &mut UiApp, path: &str, i: usize) -> Option<egui::TextureHandle> {
    let key = format!("{path}#{i}");
    let d = app.photos.as_mut()?;
    match d.people.tex.get(&key) {
        Some(Thumb::Ready(t)) => return Some(t.clone()),
        Some(Thumb::Loading) => return None,
        Some(Thumb::Failed(at)) if at.elapsed() < Duration::from_secs(120) => return None,
        _ => {}
    }
    let rect = d.people.index.as_ref()?.lock().unwrap_or_else(|e| e.into_inner()).photos.get(path)?.faces.get(i)?.rect;
    d.people.tex.insert(key.clone(), Thumb::Loading);
    let (ffmpeg, ctx, p2) = (app.core.ffmpeg_path(), app.ctx.clone(), path.to_string());
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let file = faces::face_thumb(ffmpeg.as_deref(), &p2, rect, FACE_PX)?;
                let (w, h, px) = screenrecorder_core::photos::read_thumb(&file)?;
                Some(ctx.load_texture("face", egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px), egui::TextureOptions::LINEAR))
            })
            .await
            .ok()
            .flatten()
        },
        move |app, tex| {
            if let Some(d) = app.photos.as_mut() {
                d.people.tex.insert(key, tex.map_or(Thumb::Failed(Instant::now()), Thumb::Ready));
            }
        },
    );
    None
}

/// 圓形的臉（還沒有小圖時是人的圖示）
fn paint_face(ui: &egui::Ui, rect: Rect, tex: Option<&egui::TextureHandle>) {
    let p = theme::pal(ui);
    ui.painter().rect_filled(rect, rect.width() / 2.0, p.surface2);
    match tex {
        Some(t) => {
            egui::Image::new(t).corner_radius(rect.width() / 2.0).paint_at(ui, rect);
        }
        None => theme::paint_icon(ui.painter(), rect.shrink(rect.width() * 0.3), Icon::Image, p.muted),
    }
}

/// 整理人物的視窗（在檔案庫上面）
pub fn dialog(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut dlg) = app.photos.as_mut().and_then(|d| d.people.dlg.take()) else { return };
    let summary: Vec<PersonSummary> = app.photos.as_ref().map(|d| d.people.summary.clone()).unwrap_or_default();
    let running = app.photos.as_ref().and_then(|d| d.people.running);
    let has_jf = app.jellyfin().is_some();
    let (level, regrouping, diag) = app.photos.as_ref().map(|d| (d.people.level, d.people.regrouping, d.people.diag)).unwrap_or((1, false, (0, [0; 3])));
    // 左邊列出的人（只出現一次的、隱藏的平常不列；找名字）
    let q = dlg.search.trim().to_lowercase();
    let listed: Vec<PersonSummary> = summary
        .iter()
        .filter(|s| (dlg.show_hidden || !s.person.hidden) && (dlg.show_single || s.photos > 1 || !s.person.name.is_empty()) && (q.is_empty() || s.person.name.to_lowercase().contains(&q)))
        .cloned()
        .collect();
    // 要畫的臉的小圖（先要好，畫的時候不用 app）：只要列出來的那些（每張都要讀相片）
    let covers: HashMap<u32, Option<egui::TextureHandle>> =
        listed.iter().take(MAX_TILES).filter_map(|s| s.cover.clone().map(|c| (s.person.id, c))).map(|(id, (path, i))| (id, face_tex(app, &path, i))).collect();
    let face_texs: Vec<Option<egui::TextureHandle>> = dlg.faces.iter().take(MAX_FACES).map(|(path, i)| face_tex(app, path, *i)).collect();
    let mut act = Act::None;
    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 80.0).min(1100.0), (screen.height() - 80.0).min(760.0));
    let modal = egui::Modal::new(Id::new("photo-people")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(size.x);
        ui.set_height(size.y);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("人物", "People")).font(theme::font_bold(17.0)));
            ui.add_space(8.0);
            ui.label(theme::muted(ui, tr!("臉的資料只存在這台電腦，不上傳", "Face data stays on this computer and is never uploaded")));
            if let Some((done, total)) = running {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(theme::muted(ui, trf!("還在找 {done}/{total}", "Still finding {done}/{total}")));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    act = Act::Close;
                }
            });
        });
        ui.add_space(6.0);
        let body = ui.available_rect_before_wrap();
        let left = Rect::from_min_max(body.min, pos2(body.min.x + body.width() * 0.55, body.max.y));
        let right = Rect::from_min_max(pos2(left.max.x + 16.0, body.min.y), body.max);
        // 左邊：每個人一格（有取名的在前）
        {
            let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(left).layout(Layout::top_down(Align::Min)));
            ui.horizontal(|ui| {
                ui.add(super::toolbar::search_box(&mut dlg.search, tr!("找名字", "Find a name"), 160.0));
                ui.checkbox(&mut dlg.show_single, tr!("只出現一次的", "Seen once"))
                    .on_hover_text(tr!("只在一張相片出現的臉（多半是路人）平常不列出來", "Faces seen in only one photo (often passers-by) are usually not listed"));
                ui.checkbox(&mut dlg.show_hidden, tr!("隱藏的", "Hidden"));
                if !dlg.multi.is_empty() {
                    let n = dlg.multi.len();
                    if Btn::new(&trf!("隱藏勾選的 {n} 組", "Hide {n} selected"))
                        .small()
                        .danger()
                        .tooltip(tr!("不是真人（卡通、雕像、海報、寵物）、路人：一次藏起來", "Not real people (cartoons, statues, posters, pets) or passers-by: hide them all at once"))
                        .show(ui)
                        .clicked()
                    {
                        act = Act::HideMany(dlg.multi.iter().copied().collect());
                    }
                    if Btn::new(tr!("取消勾選", "Clear")).small().ghost().show(ui).clicked() {
                        dlg.multi.clear();
                    }
                }
            });
            ui.add_space(6.0);
            let list: Vec<&PersonSummary> = listed.iter().take(MAX_TILES).collect();
            if list.is_empty() {
                ui.label(theme::muted(
                    ui,
                    if running.is_some() {
                        tr!("還在找相片裡的臉，找到的會出現在這裡", "Still looking for faces. People show up here as they're found")
                    } else {
                        tr!("沒有符合的人", "Nobody matches")
                    },
                ));
            }
            let cell = vec2(96.0, 124.0);
            let cols = ((ui.available_width() + 8.0) / (cell.x + 8.0)).floor().max(1.0) as usize;
            egui::ScrollArea::vertical().id_salt("people-grid").auto_shrink([false, false]).show_rows(ui, cell.y + 8.0, list.len().div_ceil(cols), |ui, rows| {
                for row in rows {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        for s in list.iter().skip(row * cols).take(cols) {
                            let (r, resp) = ui.allocate_exact_size(cell, Sense::click());
                            let on = dlg.selected == Some(s.person.id);
                            if on || resp.hovered() {
                                ui.painter().rect_filled(r, 8.0, if on { p.accent.gamma_multiply(0.18) } else { p.surface2 });
                            }
                            let face = Rect::from_center_size(pos2(r.center().x, r.min.y + 44.0), vec2(72.0, 72.0));
                            paint_face(ui, face, covers.get(&s.person.id).and_then(|t| t.as_ref()));
                            let name = person_name(&s.person.name);
                            let color = if s.person.name.is_empty() { p.muted } else { p.text };
                            let g = ui.painter().layout(name, theme::font_bold(12.5), color, cell.x - 4.0);
                            ui.painter().with_clip_rect(r).galley(pos2(r.center().x - g.size().x / 2.0, face.max.y + 4.0), g, color);
                            let n = trf!("{} 張", "{} photos", s.photos);
                            ui.painter().text(pos2(r.center().x, r.max.y - 4.0), egui::Align2::CENTER_BOTTOM, n, theme::font(11.5), p.muted);
                            if s.person.hidden {
                                ui.painter().text(face.right_top(), egui::Align2::RIGHT_TOP, tr!("隱藏", "Hidden"), theme::font(10.5), p.muted);
                            }
                            // 滑鼠移上去：右上角「隱藏」（卡通、雕像、路人一個個按掉）
                            let mut hid = false;
                            if !s.person.hidden && ui.rect_contains_pointer(r) {
                                let x = Rect::from_center_size(pos2(r.max.x - 13.0, r.min.y + 13.0), vec2(22.0, 22.0));
                                let xr = ui
                                    .interact(x, ui.id().with(("hide-person", s.person.id)), Sense::click())
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text(tr!("隱藏（不是真人、路人）", "Hide (not a real person, or a passer-by)"));
                                ui.painter().circle_filled(x.center(), 11.0, if xr.hovered() { p.rec } else { egui::Color32::from_black_alpha(140) });
                                theme::paint_icon(ui.painter(), x.shrink(6.0), Icon::Close, egui::Color32::WHITE);
                                if xr.clicked() {
                                    act = Act::Hide(s.person.id, true);
                                    hid = true;
                                }
                            }
                            // 左上角勾選（一次隱藏好幾組）：滑鼠移上去、或已經有勾選時看得到；按著 Ctrl 點也是勾選
                            let ticked = dlg.multi.contains(&s.person.id);
                            if !s.person.hidden && (ticked || !dlg.multi.is_empty() || ui.rect_contains_pointer(r)) {
                                let c = Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(20.0, 20.0));
                                let cr = ui.interact(c, ui.id().with(("tick-person", s.person.id)), Sense::click()).on_hover_text(tr!("勾選（一次隱藏好幾組）", "Select (to hide several at once)"));
                                ui.painter().rect_filled(c, 5.0, if ticked { p.accent } else { egui::Color32::from_black_alpha(140) });
                                ui.painter().rect_stroke(c, 5.0, Stroke::new(1.5, egui::Color32::WHITE), egui::StrokeKind::Inside);
                                if ticked {
                                    theme::paint_icon(ui.painter(), c.shrink(4.0), Icon::Check, egui::Color32::WHITE);
                                }
                                if cr.clicked() {
                                    if !dlg.multi.remove(&s.person.id) {
                                        dlg.multi.insert(s.person.id);
                                    }
                                    hid = true;
                                }
                            }
                            if resp.clicked() && !hid {
                                if ui.input(|i| i.modifiers.command) && !s.person.hidden {
                                    if !dlg.multi.remove(&s.person.id) {
                                        dlg.multi.insert(s.person.id);
                                    }
                                } else {
                                    act = Act::Select(s.person.id);
                                }
                            }
                        }
                    });
                }
            });
        }
        // 右邊：選的那個人
        let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(right).layout(Layout::top_down(Align::Min)));
        ui.painter().vline(right.min.x - 8.0, right.y_range(), Stroke::new(1.0, p.border));
        let Some(id) = dlg.selected else {
            ui.add_space(40.0);
            ui.label(theme::muted(
                ui,
                tr!("按左邊的一個人：取名、看他的相片、合併、把分錯的臉移出去", "Pick someone on the left to name them, see their photos, merge, or move out faces that don't belong"),
            ));
            // 分組的寬嚴、重新分組、診斷
            ui.add_space(16.0);
            ui.label(RichText::new(tr!("分組", "Grouping")).strong());
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("寬嚴", "Strictness")));
                for (i, (zh, en, tip_zh, tip_en)) in [
                    ("寬鬆", "Loose", "比較容易把臉歸成同一個人：組數少，但長得像的人可能混在一起", "Groups faces more readily: fewer groups, but look-alikes may be mixed"),
                    ("一般", "Normal", "建議", "Recommended"),
                    ("嚴格", "Strict", "要很像才算同一個人：不容易混，但同一個人可能分成好幾組（再合併）", "Faces must be very alike: fewer mix-ups, but one person may be split into several groups (merge them)"),
                ]
                .into_iter()
                .enumerate()
                {
                    if ui.add_enabled(!regrouping, egui::Button::selectable(level as usize == i, tr!(zh, en))).on_hover_text(tr!(tip_zh, tip_en)).clicked() && level as usize != i {
                        act = Act::Regroup(Some(i as u8));
                    }
                }
                if regrouping {
                    ui.spinner();
                }
            });
            ui.add(
                egui::Label::new(theme::muted(
                    ui,
                    tr!(
                        "不同的人被分在同一組時改成「嚴格」。改了會重新分組：取了名、隱藏的人留著，你手動移過的臉不動；同一張相片裡的臉不會分在同一組。",
                        "If different people end up in one group, choose Strict. Changing this regroups: named and hidden people are kept and faces you moved by hand stay put; faces in the same photo are never put in one group."
                    ),
                ))
                .wrap(),
            );
            if Btn::new(tr!("重新分組", "Regroup")).small().enabled(!regrouping).tooltip(tr!("照現在的寬嚴重新分一次", "Group again with the current strictness")).show(ui).clicked() {
                act = Act::Regroup(None);
            }
            if diag.0 > 0 {
                let pct = |n: usize| n as f64 * 100.0 / diag.0 as f64;
                let text = trf!(
                    "診斷：同一張相片裡的臉（一定是不同的人）{} 組，相似度到寬鬆 / 一般 / 嚴格門檻的各 {:.1}% / {:.1}% / {:.1}%",
                    "Check: {} pairs of faces within the same photo (always different people); {:.1}% / {:.1}% / {:.1}% reach the Loose / Normal / Strict thresholds",
                    diag.0,
                    pct(diag.1[0]),
                    pct(diag.1[1]),
                    pct(diag.1[2])
                );
                ui.add_space(4.0);
                ui.add(egui::Label::new(theme::muted(ui, text.clone())).wrap()).on_hover_text(tr!(
                    "比例越高，表示相片裡的人對模型來說越像（例如家人），越需要用「嚴格」。這些數字不含名字和相片，可以複製給開發者參考",
                    "The higher these are, the more alike the people look to the model (e.g. family), and the more Strict helps. The numbers contain no names or photos and can be copied for the developer"
                ));
                if Btn::new(tr!("複製診斷", "Copy check")).small().ghost().show(ui).clicked() {
                    ui.ctx().copy_text(text);
                }
            }
            if has_jf {
                ui.add_space(16.0);
                if Btn::new(tr!("把照片帶到 Jellyfin 的人物…", "Send photos to Jellyfin people…"))
                    .small()
                    .tooltip(tr!(
                        "Jellyfin 上同名、還沒有照片的人物，補上這裡拍得最好的一張臉（已經有照片的不動）",
                        "For Jellyfin people with the same name and no photo yet, add their best face from here (people who already have a photo aren't changed)"
                    ))
                    .show(ui)
                    .clicked()
                {
                    act = Act::ToJellyfin;
                }
            }
            ui.add_space(24.0);
            if Btn::new(tr!("刪除全部人臉資料…", "Delete all face data…"))
                .small()
                .danger()
                .tooltip(tr!("刪掉找到的臉、取的名字，並關閉人物功能（相片不會動）", "Deletes the faces found and the names, and turns People off (photos aren't touched)"))
                .show(ui)
                .clicked()
            {
                act = Act::DeleteAll;
            }
            return;
        };
        let me = summary.iter().find(|s| s.person.id == id);
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut dlg.name).hint_text(tr!("名字（例如：小明）", "Name (e.g. Alex)")).desired_width(180.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if Btn::new(tr!("取名", "Save name")).small().primary().tooltip(tr!("和已經有的名字一樣時兩組會合在一起", "Using an existing name merges the two")).show(ui).clicked() || enter
            {
                act = Act::Rename(id, dlg.name.clone());
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if Btn::new(&trf!("看相片（{} 張）", "Show photos ({})", me.map_or(0, |s| s.photos))).icon(Icon::Image).small().show(ui).clicked() {
                act = Act::ShowPhotos(id);
            }
            let others: Vec<&PersonSummary> = summary.iter().filter(|s| s.person.id != id && !s.person.name.is_empty()).collect();
            let b = Btn::new(tr!("合併到…", "Merge into…"))
                .small()
                .menu()
                .tooltip(tr!("和另一個人是同一個人（例如小時候和長大後）", "This is the same person as someone else (e.g. as a child and grown up)"))
                .show(ui);
            egui::Popup::menu(&b).show(|ui| {
                ui.set_min_width(180.0);
                if others.is_empty() {
                    ui.label(theme::muted(ui, tr!("先幫另一個人取名", "Name someone else first")));
                }
                for o in &others {
                    if ui.button(format!("{}　{}", o.person.name, o.photos)).clicked() {
                        act = Act::Merge(id, o.person.id);
                        ui.close();
                    }
                }
            });
            let hidden = me.is_some_and(|s| s.person.hidden);
            let tip = tr!(
                "不是真人（卡通、雕像、海報、寵物）、路人、不想看到的：不列在人物裡，之後找到同一個的臉也會自動歸進來一起藏著",
                "Not a real person (cartoons, statues, posters, pets), passers-by, or people you don't want to see: they aren't listed in People, and matching faces found later are hidden with them"
            );
            if Btn::new(if hidden { tr!("取消隱藏", "Unhide") } else { tr!("隱藏", "Hide") }).small().ghost().tooltip(tip).show(ui).clicked() {
                act = Act::Hide(id, !hidden);
            }
        });
        ui.add_space(8.0);
        ui.separator();
        // 這個人的每一張臉：勾選分錯的，移出或移到別人
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("{} 張臉", "{} faces", dlg.faces.len())).strong());
            if dlg.picked.is_empty() {
                ui.label(theme::muted(ui, tr!("點臉勾選分錯的", "Click faces that don't belong")));
            } else {
                let picked: Vec<(String, usize)> = dlg.picked.iter().cloned().collect();
                if Btn::new(&trf!("不是這個人（{}）", "Not this person ({})", picked.len()))
                    .small()
                    .danger()
                    .tooltip(tr!("拿出去重新分組，不會再分回這個人", "Regroups them and never puts them back here"))
                    .show(ui)
                    .clicked()
                {
                    act = Act::NotThis(picked.clone(), id);
                }
                let b = Btn::new(tr!("移到…", "Move to…")).small().menu().show(ui);
                egui::Popup::menu(&b).show(|ui| {
                    ui.set_min_width(180.0);
                    for o in summary.iter().filter(|s| s.person.id != id && !s.person.name.is_empty()) {
                        if ui.button(&o.person.name).clicked() {
                            act = Act::MoveTo(picked.clone(), Some(o.person.id));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button(RichText::new(tr!("＋ 新的人…", "＋ New person…")).color(p.accent)).clicked() {
                        act = Act::MoveTo(picked.clone(), None);
                        ui.close();
                    }
                });
                if Btn::new(tr!("取消勾選", "Clear")).small().ghost().show(ui).clicked() {
                    dlg.picked.clear();
                }
            }
        });
        ui.add_space(4.0);
        let cell = 72.0;
        let cols = ((ui.available_width() + 6.0) / (cell + 6.0)).floor().max(1.0) as usize;
        let n = dlg.faces.len().min(face_texs.len());
        egui::ScrollArea::vertical().id_salt(("person-faces", id)).auto_shrink([false, false]).show_rows(ui, cell + 6.0, n.div_ceil(cols), |ui, rows| {
            for row in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (from, to) = (row * cols, ((row + 1) * cols).min(n));
                    for (f, tex) in dlg.faces[from..to].iter().cloned().zip(&face_texs[from..to]) {
                        let (r, resp) = ui.allocate_exact_size(vec2(cell, cell), Sense::click());
                        match tex {
                            Some(t) => {
                                egui::Image::new(t).corner_radius(6.0).paint_at(ui, r);
                            }
                            None => {
                                ui.painter().rect_filled(r, 6.0, p.surface2);
                            }
                        }
                        let on = dlg.picked.contains(&f);
                        if on {
                            ui.painter().rect_stroke(r, 6.0, Stroke::new(3.0, p.rec), egui::StrokeKind::Inside);
                            theme::paint_icon(ui.painter(), Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(16.0, 16.0)), Icon::Close, p.rec);
                        } else if resp.hovered() {
                            ui.painter().rect_stroke(r, 6.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
                        }
                        let resp = resp.on_hover_text(super::dialogs::file_name(&f.0));
                        if resp.clicked() && !dlg.picked.remove(&f) {
                            dlg.picked.insert(f);
                        }
                    }
                });
            }
            if dlg.faces.len() > n {
                ui.label(theme::muted(ui, trf!("只列出前 {n} 張", "Showing the first {n}")));
            }
        });
    });
    if modal.should_close() && app.ask.is_none() {
        act = Act::Close;
    }
    let Some(d) = app.photos.as_mut() else { return };
    d.people.dlg = Some(dlg);
    let Some(index) = d.people.index.clone() else { return };
    let with = |f: &mut dyn FnMut(&mut FaceIndex)| f(&mut index.lock().unwrap_or_else(|e| e.into_inner()));
    match act {
        Act::None => {}
        Act::Close => d.people.dlg = None,
        Act::Select(id) => select(&mut d.people, Some(id)),
        Act::Rename(id, name) => {
            let mut to = id;
            with(&mut |i| to = i.rename(id, &name));
            changed(app);
            if let Some(d) = app.photos.as_mut() {
                select(&mut d.people, Some(to));
            }
        }
        Act::Hide(id, on) => {
            with(&mut |i| i.set_hidden(id, on));
            changed(app);
        }
        Act::HideMany(ids) => {
            with(&mut |i| {
                for id in &ids {
                    i.set_hidden(*id, true);
                }
            });
            if let Some(dlg) = d.people.dlg.as_mut() {
                dlg.multi.clear();
                if dlg.selected.is_some_and(|id| ids.contains(&id)) {
                    dlg.selected = None;
                }
            }
            changed(app);
        }
        Act::Merge(from, into) => {
            with(&mut |i| i.merge(from, into));
            changed(app);
            if let Some(d) = app.photos.as_mut() {
                select(&mut d.people, Some(into));
            }
        }
        Act::NotThis(list, id) => {
            with(&mut |i| i.not_this(&list, id));
            changed(app);
        }
        Act::MoveTo(list, Some(to)) => {
            with(&mut |i| i.assign(&list, to));
            changed(app);
        }
        Act::MoveTo(list, None) => {
            let ask = super::dialogs::Ask::input(tr!("新的人", "New person"), tr!("名字", "Name"), String::new(), tr!("移過去", "Move"), move |app, name| {
                if name.trim().is_empty() {
                    return;
                }
                let Some(index) = app.photos.as_ref().and_then(|d| d.people.index.clone()) else { return };
                {
                    let mut i = index.lock().unwrap_or_else(|e| e.into_inner());
                    let to = i.add_person(&name);
                    i.assign(&list, to);
                }
                changed(app);
            });
            app.ask = Some(ask);
        }
        Act::ShowPhotos(id) => {
            d.people.dlg = None;
            d.people.set_filter(Some(id));
            d.go_top();
            d.want_offset = Some(0.0);
            d.dirty = true;
        }
        Act::Regroup(level) => {
            // 臉多時要算一陣子：在背景做
            d.people.regrouping = true;
            let generation = GENERATION.load(Ordering::Relaxed);
            app.spawn(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let mut i = index.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(l) = level {
                            i.level = Some(l);
                        }
                        i.regroup();
                        if GENERATION.load(Ordering::Relaxed) == generation {
                            faces::save_index(&faces::index_file(), &i);
                        }
                    })
                    .await
                    .ok()
                },
                |app, _| {
                    if let Some(d) = app.photos.as_mut() {
                        d.people.regrouping = false;
                        if let Some(dlg) = d.people.dlg.as_mut() {
                            dlg.selected = None;
                            dlg.multi.clear();
                        }
                    }
                    changed(app);
                    app.toast(tr!("已重新分組", "Regrouped"), false);
                },
            );
        }
        Act::ToJellyfin => {
            let ask = super::dialogs::Ask::confirm(
                tr!("把照片帶到 Jellyfin 的人物？", "Send photos to Jellyfin people?"),
                tr!(
                    "這裡取了名的人，Jellyfin 上有同名的人物而且還沒有照片時，補上他拍得最好的一張臉（直式）。已經有照片的不會動；只有這幾張臉的照片會傳到你的 Jellyfin 伺服器。",
                    "For people named here, if Jellyfin has a person with the same name and no photo yet, their best face (portrait) is added. People who already have a photo aren't changed; only these face photos are sent to your Jellyfin server."
                ),
                tr!("帶過去", "Send"),
                |app, _| to_jellyfin(app),
            );
            app.ask = Some(ask);
        }
        Act::DeleteAll => {
            let mut ask = super::dialogs::Ask::confirm(
                tr!("刪除全部人臉資料？", "Delete all face data?"),
                tr!(
                    "找到的臉、取的名字都會刪掉，人物功能也會關閉。相片不會動；之後要用時可以再開啟、重新找。",
                    "The faces found and the names you gave are deleted, and People is turned off. Your photos aren't touched; you can turn it on again later and start over."
                ),
                tr!("刪除", "Delete"),
                |app, _| {
                    GENERATION.fetch_add(1, Ordering::Relaxed);
                    if let Some(w) = app.face_work.take() {
                        w.stop.store(true, Ordering::Relaxed);
                    }
                    (app.face_index, app.face_idle) = (None, false);
                    if let Some(d) = app.photos.as_mut() {
                        d.people = People::default();
                        d.dirty = true;
                    }
                    app.s.photo_faces = false;
                    app.save_settings();
                    app.spawn(async { tokio::task::spawn_blocking(faces::delete_all_data).await.unwrap_or_else(|e| Err(e.to_string())) }, |app, r| match r {
                        Ok(()) => app.toast(tr!("已刪除全部人臉資料", "All face data deleted"), false),
                        Err(e) => app.toast(e, true),
                    });
                },
            );
            ask.danger = true;
            app.ask = Some(ask);
        }
    }
}

/// 取了名的人 → Jellyfin 上同名、還沒有照片的人物：補上拍得最好的一張臉（背景做，好了通知）
fn to_jellyfin(app: &mut UiApp) {
    let (Some(jf), Some(index)) = (app.jellyfin(), app.photos.as_ref().and_then(|d| d.people.index.clone())) else { return };
    let (user, ffmpeg) = (app.s.jellyfin_user.clone(), app.core.ffmpeg_path());
    app.toast(tr!("正在把照片帶到 Jellyfin…", "Sending photos to Jellyfin…"), false);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<(usize, usize, usize), String> {
                let missing = jf.persons_without_photo(&user)?;
                // （Jellyfin 的人物編號, 相片, 臉的位置）：先在鎖裡挑好，讀相片、上傳時不佔著臉的資料
                let (jobs, named) = {
                    let idx = index.lock().unwrap_or_else(|e| e.into_inner());
                    let named: Vec<(u32, String)> = idx.people.iter().filter(|p| !p.name.is_empty() && !p.hidden).map(|p| (p.id, p.name.trim().to_lowercase())).collect();
                    let jobs: Vec<(String, String, [f32; 4])> = named
                        .iter()
                        .filter_map(|(id, name)| {
                            let pid = missing.iter().find(|(_, n)| n.trim().to_lowercase() == *name).map(|(pid, _)| pid.clone())?;
                            let (path, rect) = idx.best_face(*id)?;
                            Some((pid, path, rect))
                        })
                        .collect();
                    (jobs, named.len())
                };
                let (mut ok, mut failed) = (0, 0);
                for (pid, path, rect) in jobs {
                    match faces::portrait_jpeg(ffmpeg.as_deref(), &path, rect).ok_or_else(|| "讀不到相片".to_string()).and_then(|jpg| jf.set_person_photo(&pid, &jpg)) {
                        Ok(()) => ok += 1,
                        Err(e) => {
                            screenrecorder_core::info!("[人臉] 人物照片傳到 Jellyfin 失敗：{e}");
                            failed += 1;
                        }
                    }
                }
                Ok((ok, failed, named))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| match r {
            Ok((ok, failed, named)) => {
                let rest = named.saturating_sub(ok + failed);
                let mut msg = trf!("已補上 {ok} 個人物的照片", "Added photos for {ok} people");
                if rest > 0 {
                    msg += &trf!("；另外 {rest} 個在 Jellyfin 上已經有照片，或沒有同名的人物", "; {rest} more already have a photo in Jellyfin or have no person with the same name");
                }
                if failed > 0 {
                    msg += &trf!("；{failed} 個失敗（原因在記錄檔）", "; {failed} failed (see the log)");
                }
                app.toast(msg, failed > 0);
            }
            Err(e) => app.toast(trf!("連不到 Jellyfin：{e}", "Couldn't reach Jellyfin: {e}"), true),
        },
    );
}

/// 檢視器的標題列：相片裡有誰（有人閉眼時也寫出來）
pub fn names_of(app: &UiApp, path: &str) -> Vec<String> {
    let Some(d) = app.photos.as_ref() else { return vec![] };
    let mut v = d.people.names_of(path);
    if d.people.closed.contains(path) {
        v.push(tr!("有人閉眼", "Eyes closed").into());
    }
    if d.people.blurry.contains(path) {
        v.push(tr!("臉模糊", "Blurry faces").into());
    }
    v
}

/// 同時間拍的裡面挑最好的一張：這張相片的分數（沒看過眼睛的沒有）
pub fn quality(p: &People, path: &str) -> Option<f32> {
    p.quality.get(path).copied()
}

/// 檢視器：這張相片裡閉眼的臉（第幾張, 位置佔相片的比例）
pub fn closed_faces(app: &UiApp, path: &str) -> Vec<(usize, [f32; 4])> {
    app.photos.as_ref().map(|d| d.people.closed_faces(path)).unwrap_or_default()
}

/// 檢視器按了「不是閉眼」：這張臉之後不再算閉眼
pub fn not_closed(app: &mut UiApp, path: &str, i: usize) {
    let Some(index) = app.photos.as_ref().and_then(|d| d.people.index.clone()) else { return };
    index.lock().unwrap_or_else(|e| e.into_inner()).set_eyes_ok(path, i);
    changed(app);
}

/// 縮圖右下角：看過眼睛、沒有人閉眼（和閉眼的排在一起時標出來）
pub fn open_eyes(p: &People, path: &str) -> bool {
    p.bursts() && p.open.contains(path)
}

/// 縮圖右下角：有人閉眼
pub fn closed_eyes(p: &People, path: &str) -> bool {
    p.closed.contains(path)
}

/// 下載模型的進度（選單上顯示）
pub fn downloading(app: &UiApp) -> Option<f32> {
    let st = app.core.component_status(Kind::Faces);
    st.running.then_some(st.progress)
}

/// 相片分頁關掉：在背景找的停下來（找到的照樣存）
pub fn stop(p: &People) {
    p.stop.store(true, Ordering::Relaxed);
}

/// 只看某個人時的那一行說明（位置列右邊）
pub fn filter_chip(ui: &mut egui::Ui, p: &People) -> bool {
    let Some(label) = p.describe() else { return false };
    Btn::new(&trf!("只看「{}」 ×", "Only “{}” ×", label)).small().ghost().selected(true).tooltip(tr!("按一下看全部的人", "Click to show everyone")).show(ui).clicked()
}
