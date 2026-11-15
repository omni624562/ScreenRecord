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

/// 一批找幾張相片
const BATCH: usize = 12;
/// 刪除全部人臉資料時加一：在那之前開始的背景工作不再存檔（不然刪掉的檔案又被寫回來）
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 多久存一次臉的資料
const SAVE_EVERY: Duration = Duration::from_secs(30);
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
    /// 模型不齊時自動下載過了（這次開啟只試一次；之前開啟過人物功能的，新版多了看眼睛的模型）
    auto_dl: bool,
    /// 背景正在找：（這次找了幾張, 這次要找幾張）；暫停、停下來（關掉分頁）
    pub running: Option<(usize, usize)>,
    pub done: usize,
    pub paused: bool,
    pub stop: Arc<AtomicBool>,
    /// 這次開啟找不到的（打不開的檔案）：不再一直試
    failed: HashSet<String>,
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
            auto_dl: false,
            running: None,
            done: 0,
            paused: false,
            stop: Arc::new(AtomicBool::new(false)),
            failed: HashSet::new(),
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
        self.closed = idx.closed_eyes(BLINK[(self.blink_level as usize).min(BLINK.len() - 1)]);
        self.old = idx.old_photos();
        if self.eyes {
            self.filter_set = self.closed.clone();
        } else if let Some(id) = self.filter {
            if idx.person(id).is_none() {
                self.filter = None;
                self.filter_set.clear();
            } else {
                self.filter_set = idx.photos_of(id);
            }
        }
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

    /// 只看有人閉眼的相片
    pub fn set_eyes(&mut self, on: bool) {
        (self.filter, self.eyes) = (None, on);
        self.filter_set = if on { self.closed.clone() } else { HashSet::new() };
    }

    /// 有篩選（某個人、有人閉眼的）
    pub fn filtering(&self) -> bool {
        self.filter.is_some() || self.eyes
    }

    /// 選了某個人：只看有他的相片
    pub fn set_filter(&mut self, id: Option<u32>) {
        (self.filter, self.eyes) = (id, false);
        self.filter_set = match (id, self.index.as_ref()) {
            (Some(id), Some(idx)) => idx.lock().unwrap_or_else(|e| e.into_inner()).photos_of(id),
            _ => HashSet::new(),
        };
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

/// 開啟相片分頁時：有開啟人物功能（或有存過的資料）就讀臉的資料
pub fn open(app: &mut UiApp) {
    let file = faces::index_file();
    if !app.s.photo_faces && !file.exists() {
        return;
    }
    let level = app.s.photo_blink;
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                // 舊版的資料：門檻提高了，分數不夠的臉拿掉
                let mut idx = faces::load_index(&file);
                let changed = idx.upgrade();
                if changed {
                    idx.cluster();
                }
                (idx, changed)
            })
            .await
            .unwrap_or_default()
        },
        move |app, (idx, upgraded)| {
            let Some(d) = app.photos.as_mut() else { return };
            d.people.index = Some(Arc::new(Mutex::new(idx)));
            d.people.blink_level = level;
            if upgraded {
                changed(app);
            } else {
                d.people.refresh();
                d.dirty = true;
            }
        },
    );
}

/// 每一格畫面（apply）：開啟了人物功能、模型在、沒在找、沒暫停時，找下一批
pub fn tick(app: &mut UiApp) {
    let enabled = app.s.photo_faces;
    let dl = downloading(app);
    let dl_error = app.core.component_status(Kind::Faces).error;
    let level = app.s.photo_blink;
    if let Some(d) = app.photos.as_mut() {
        (d.people.enabled, d.people.dl, d.people.dl_error) = (enabled, dl, dl_error);
        if d.people.blink_level != level {
            d.people.blink_level = level;
            d.people.refresh();
            d.dirty |= d.people.eyes;
        }
    }
    if !enabled {
        return;
    }
    let ffmpeg = app.core.ffmpeg_path();
    let downloading = dl.is_some();
    let Some(d) = app.photos.as_mut() else { return };
    let p = &mut d.people;
    if p.index.is_none() || p.running.is_some() || p.paused || p.all_done || downloading || !d.loaded {
        return;
    }
    if p.checked_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
        return;
    }
    p.checked_at = Some(Instant::now());
    p.missing = !faces::models_ready();
    if p.missing {
        // 已經開啟人物功能、模型不齊（新版多了看眼睛的模型）：自動下載缺的那幾個（這次開啟只試一次，失敗時在選單上再試）
        if !p.auto_dl && p.dl_error.is_none() {
            p.auto_dl = true;
            app.core.component_download(Kind::Faces);
        }
        return;
    }
    let Some(index) = p.index.clone() else { return };
    // 新的相片先找（最近拍的最常看）
    let (jobs, left) = {
        let idx = index.lock().unwrap_or_else(|e| e.into_inner());
        let mut todo: Vec<&screenrecorder_core::photos::Photo> = d.photos.iter().filter(|ph| !p.failed.contains(&ph.path) && idx.needs(&ph.path, ph.size, ph.mtime)).collect();
        todo.sort_by_key(|ph| std::cmp::Reverse(ph.when));
        let left = todo.len();
        (todo.into_iter().take(BATCH).map(|ph| faces::Job { path: ph.path.clone(), size: ph.size, mtime: ph.mtime }).collect::<Vec<_>>(), left)
    };
    if jobs.is_empty() {
        // 全部找完：存一次
        p.all_done = true;
        p.running = None;
        p.done = 0;
        let mut at = p.saved_at;
        save(index, &mut at, app);
        if let Some(d) = app.photos.as_mut() {
            d.people.saved_at = at;
        }
        return;
    }
    p.running = Some((p.done, p.done + left));
    let stop = p.stop.clone();
    let generation = GENERATION.load(Ordering::Relaxed);
    let save_now = p.saved_at.elapsed() > SAVE_EVERY;
    if save_now {
        p.saved_at = Instant::now();
    }
    let idx2 = index.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<(usize, Vec<String>), String> {
                let results = faces::analyze_many(jobs, ffmpeg, &stop)?;
                let mut failed = vec![];
                let n = results.len();
                let mut idx = idx2.lock().unwrap_or_else(|e| e.into_inner());
                for (job, r) in results {
                    match r {
                        Ok(f) => idx.put(job.path, job.size, job.mtime, f),
                        Err(e) => {
                            screenrecorder_core::info!("[人臉] 讀不到 {}：{e}", job.path);
                            failed.push(job.path);
                        }
                    }
                }
                idx.cluster();
                // 每 30 秒、關掉分頁時存一次
                if (save_now || stop.load(Ordering::Relaxed)) && GENERATION.load(Ordering::Relaxed) == generation {
                    faces::save_index(&faces::index_file(), &idx);
                }
                Ok((n, failed))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| {
            let Some(d) = app.photos.as_mut() else { return };
            let p = &mut d.people;
            p.running = None;
            match r {
                Ok((n, failed)) => {
                    p.done += n;
                    p.failed.extend(failed);
                    p.checked_at = None;
                    p.refresh();
                    if p.filtering() {
                        d.dirty = true;
                    }
                    // 有在用名稱搜尋時重新找
                    if !d.searched.is_empty() {
                        d.dirty = true;
                    }
                }
                Err(e) => {
                    screenrecorder_core::warn!("[人臉] 找臉失敗：{e}");
                    p.error = Some(e);
                    p.paused = true;
                }
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
            d.people.index = Some(Arc::new(Mutex::new(FaceIndex::default())));
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
}

pub fn menu(ui: &mut egui::Ui, p: &People, enabled: bool, downloading: Option<f32>) -> MenuAct {
    let mut act = MenuAct::None;
    let text = match p.filter {
        Some(id) => p.label(id),
        None if p.eyes => tr!("有人閉眼", "Eyes closed").to_string(),
        None => tr!("全部的人", "Everyone").to_string(),
    };
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
            if ui.selectable_label(p.filter.is_none(), tr!("全部的人", "Everyone")).clicked() {
                act = MenuAct::Filter(None);
            }
            let named: Vec<&PersonSummary> = p.summary.iter().filter(|s| !s.person.name.is_empty() && !s.person.hidden).collect();
            for s in &named {
                if ui.selectable_label(p.filter == Some(s.person.id), format!("{}　{}", s.person.name, s.photos)).clicked() {
                    act = MenuAct::Filter(Some(s.person.id));
                }
            }
            if named.is_empty() {
                ui.label(theme::muted(ui, tr!("還沒有取名的人：按下面的「整理人物…」幫他們取名", "Nobody is named yet. Use “Organize people…” below to name them")));
            }
            ui.separator();
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
            if p.old > 0 {
                ui.label(theme::muted(ui, trf!("還有 {} 張舊的相片要再看一次眼睛（在背景）", "{} older photos still need their eyes checked (in the background)", p.old)));
            }
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
                d.people.dlg = Some(Dlg { selected: None, faces: vec![], picked: HashSet::new(), name: String::new(), show_hidden: false, show_single: false, search: String::new() });
                select(&mut d.people, sel);
            }
        }
        // 選了某個人、有人閉眼的：看全部資料夾（直接列出符合的相片，不用一層層點進去）
        MenuAct::Filter(id) => {
            if let Some(d) = app.photos.as_mut() {
                d.people.set_filter(id);
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
    if Btn::new(tr!("暫停", "Pause")).small().ghost().tooltip(tr!("暫停找人臉（NAS 很忙、要錄影時）", "Pause finding faces (when the NAS is busy or you're recording)")).show(ui).clicked()
    {
        p.paused = true;
    }
    ui.label(theme::muted(ui, trf!("正在找相片裡的人 {done}/{total}", "Finding people {done}/{total}")))
        .on_hover_text(tr!("在這台電腦上找臉、分組，不上傳；找好的會馬上出現在「人物」", "Faces are found and grouped on this computer and nothing is uploaded. Results show up in People right away"));
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
    Merge(u32, u32),
    NotThis(Vec<(String, usize)>, u32),
    MoveTo(Vec<(String, usize)>, Option<u32>),
    ShowPhotos(u32),
    DeleteAll,
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
                            if resp.clicked() && !hid {
                                act = Act::Select(s.person.id);
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
                    if let Some(d) = app.photos.as_mut() {
                        d.people.stop.store(true, Ordering::Relaxed);
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

/// 檢視器的標題列：相片裡有誰（有人閉眼時也寫出來）
pub fn names_of(app: &UiApp, path: &str) -> Vec<String> {
    let Some(d) = app.photos.as_ref() else { return vec![] };
    let mut v = d.people.names_of(path);
    if d.people.closed.contains(path) {
        v.push(tr!("有人閉眼", "Eyes closed").into());
    }
    v
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
    let label = match p.filter {
        Some(id) => p.label(id),
        None if p.eyes => tr!("有人閉眼", "Eyes closed").to_string(),
        None => return false,
    };
    Btn::new(&trf!("只看「{}」 ×", "Only “{}” ×", label)).small().ghost().selected(true).tooltip(tr!("按一下看全部的人", "Click to show everyone")).show(ui).clicked()
}
