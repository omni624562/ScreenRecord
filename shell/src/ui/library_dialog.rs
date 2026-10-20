//! 全部錄影 / 全部截圖：搜尋、篩選、排序、分頁（每頁筆數依視窗大小決定，不出現捲軸）、勾選多筆移到資源回收筒。
//! 錄影用表格：加速版（含 GIF）以子列列在原檔底下，各自有長度、大小與播放 / 資料夾按鈕；
//! 勾選原檔時連同底下的加速版一起勾選，可再個別取消。
//! 截圖用縮圖格：點縮圖開啟。

use super::dialogs::{base_name, date_labels, day_label, day_parts, entry_time, file_name, is_cut_name, is_default_name, short_date, time_labels, Ask};
use super::entry_menu::Place;
use super::theme::{self, chip, Btn, Icon, Tone};
use super::{EntryAction, ToastAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use screenrecorder_core::actions;
use screenrecorder_core::args::Quality;
use screenrecorder_core::format::{check_recording_name, format_bytes, human_duration, short_clock, speed_label};
use screenrecorder_core::i18n::is_en;
use screenrecorder_core::library::{grid_layout, GridPlace};
use screenrecorder_core::pins::PinStore;
use screenrecorder_core::types::{
    DayGroup, ExportFormat, ExportInfo, ExportKind, ExportState, FacetQuery, GridFit, LibraryEntry, LibraryFilter, LibraryPage, LibraryQuery, LibrarySort, MediaInfo, LIBRARY_HITS_SHOWN,
    LIBRARY_ROW_MAIN_PX, LIBRARY_ROW_SUB_PX,
};
use screenrecorder_core::{tr, trf};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

mod batch;
mod browse;

/// 看錄影還是截圖
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Shot,
}

pub struct LibraryDialog {
    /// 需要重新讀取（開啟、換頁、轉檔完成）
    pub dirty: bool,
    /// 前後一頁的縮圖：這一頁的縮圖都排上去之後才開始產生（換頁時馬上看得到）
    prefetch: Vec<(String, f64)>,
    /// 工具列搜尋框右邊所有東西的寬度（上一格量到的；[錄影, 截圖][完整, 只有圖示]）：視窗窄時搜尋框縮短、按鈕只顯示圖示
    bar_w: [[f32; 2]; 2],
    kind: Kind,
    /// 看哪個資料夾：None = 目前的儲存位置；也可以看之前用過的資料夾
    dir: Option<String>,
    /// 上次讀取的資料夾（儲存位置改了就重新讀取）
    loaded_dir: String,
    query: String,
    filter: LibraryFilter,
    sort: LibrarySort,
    page: usize,
    data: Option<LibraryPage>,
    loading: bool,
    error: Option<String>,
    selected: HashSet<String>,
    search_at: Option<Instant>,
    /// 視窗大小變了（每頁筆數跟著變）：停下來 0.25 秒再重新讀，拖曳調整大小時不會一直掃資料夾
    fit_at: Option<Instant>,
    fit_px: f64,
    /// 截圖格一頁放幾張（依對話框大小）
    grid_fit: Option<GridFit>,
    /// 換縮圖大小或視窗大小後：讀取含有這一筆（從 0 算）的那一頁
    page_anchor: Option<usize>,
    seq: u64,
    focus_search: bool,
    /// 點了說過的話：開啟影片並跳到那一句
    jump: Option<(LibraryEntry, f64)>,
    /// 合併成一支的對話框、進行中的合併工作（編號）
    merge: Option<MergeDlg>,
    merge_job: Option<u64>,
    /// 一次處理多支的對話框
    batch: Option<batch::BatchDlg>,
    /// 剛做好的檔案（合併的成品）：排在最上面並標示「新的」
    fresh: Option<String>,
    /// 拼成一張的對話框
    combine: Option<CombineDlg>,
    /// 鍵盤（Delete、F2、Enter）操作的那一列：滑鼠所在或上次點的
    cursor_row: Option<String>,
    /// Shift＋點一下時，從這一筆選到點的那一筆（上次點或勾選的）
    anchor: Option<String>,
    /// 滾輪換頁：累積的捲動量、上次換頁的時間
    wheel: f32,
    wheel_at: Option<Instant>,
    /// 只看已選取（勾選的檔案，可能在不同頁）
    only_selected: bool,
    /// 全部截圖用滑鼠框選：開始拖曳時的勾選（框到的加在上面）
    band_base: Option<HashSet<String>>,
    /// 按下了、還沒移動到算拖曳的距離（只點一下的話交給縮圖的點一下）
    band_pending: bool,
    /// 只看有星號的
    only_pinned: bool,
    /// 清理舊截圖的對話框
    cleanup: Option<CleanupDlg>,
    /// 找相似截圖的對話框
    similar: Option<SimilarDlg>,
    /// 這個畫面要開始拖到其他程式的檔案（畫完再交給系統的拖曳）
    drag_out: Option<Vec<String>>,
    /// 全部截圖左邊的瀏覽欄開著、選的條件
    browse: bool,
    facet: FacetQuery,
    /// 月曆（開著時取代縮圖格）
    calendar: Option<browse::CalView>,
    /// 搜尋框的建議
    suggest: browse::Suggest,
    /// 已經開始建立截圖的文字索引（打開全部截圖時一次）
    ocr_started: bool,
    /// 上一個畫面文字索引還在做（做完時重新搜尋）
    ocr_running: bool,
}

/// 清理舊截圖：超過幾天的截圖（有星號的不算）移到資源回收筒
struct CleanupDlg {
    days: u32,
    /// 查到的截圖（None = 查詢中）
    list: Option<Vec<MediaInfo>>,
    seq: u64,
    busy: bool,
}

/// 找出相似的截圖：一組一列，每組預設留最新的一張（有星號的也留），其他的移到資源回收筒
struct SimilarDlg {
    /// 0 完全一樣、1 幾乎一樣、2 相似
    level: u8,
    /// 找到的組（None = 比對中）
    groups: Option<Vec<Vec<MediaInfo>>>,
    /// 要移到資源回收筒的
    remove: HashSet<String>,
    seq: u64,
    busy: bool,
}

/// 合併成一支：依清單的順序（可上下調整）接成一支；加速版預設不放進去
struct MergeDlg {
    items: Vec<MergeItem>,
    name: String,
    /// None = 保留原畫質（格式一樣時直接接起來，很快）；Some = 重新壓縮成這個畫質
    quality: Option<Quality>,
    error: Option<String>,
    busy: bool,
}

struct MergeItem {
    path: String,
    /// 「10/11 19:32」
    when: String,
    name: String,
    dur: Option<f64>,
    /// 加速版（預設不放進去）
    export: bool,
    on: bool,
}

/// 拼成一張：預覽、調整順序、左右排／上下排、間距
struct CombineDlg {
    items: Vec<LibraryEntry>,
    vertical: bool,
    /// 0 無、1 小、2 大
    spacing: u8,
    busy: bool,
}

impl LibraryDialog {
    /// 全部截圖（不是全部錄影）
    pub fn is_shot(&self) -> bool {
        self.kind == Kind::Shot
    }

    /// 上面蓋著其他對話框（教學的提示先收起來）
    pub fn covered(&self) -> bool {
        self.overlay()
    }

    /// 上面蓋著合併、拼成一張、清理舊截圖的對話框
    fn overlay(&self) -> bool {
        self.merge.is_some() || self.batch.is_some() || self.combine.is_some() || self.cleanup.is_some() || self.similar.is_some()
    }

    /// 檔案搬動或資料夾改了：重新讀取
    pub fn reload(&mut self) {
        self.selected.clear();
        self.data = None;
        self.dirty = true;
    }

    pub fn new(kind: Kind) -> Self {
        Self {
            dirty: true,
            prefetch: vec![],
            bar_w: [[440.0, 300.0], [560.0, 330.0]],
            kind,
            dir: None,
            loaded_dir: String::new(),
            query: String::new(),
            filter: LibraryFilter::All,
            sort: LibrarySort::New,
            page: 1,
            data: None,
            loading: false,
            error: None,
            selected: HashSet::new(),
            search_at: None,
            fit_at: None,
            fit_px: 0.0,
            grid_fit: None,
            page_anchor: None,
            seq: 0,
            focus_search: true,
            jump: None,
            merge: None,
            batch: None,
            merge_job: None,
            fresh: None,
            combine: None,
            cursor_row: None,
            anchor: None,
            wheel: 0.0,
            wheel_at: None,
            only_selected: false,
            band_base: None,
            band_pending: false,
            only_pinned: false,
            cleanup: None,
            similar: None,
            drag_out: None,
            browse: true,
            facet: FacetQuery::default(),
            calendar: None,
            suggest: browse::Suggest::default(),
            ocr_started: false,
            ocr_running: false,
        }
    }

    /// 勾選 / 取消一個檔案；原檔連同底下的加速版一起（之後可個別取消加速版）
    fn toggle(&mut self, path: &str, on: bool) {
        let subs: Vec<String> = self.data.as_ref().and_then(|d| d.items.iter().find(|e| e.media.path == path)).map(|e| e.exports.iter().map(|x| x.media.path.clone()).collect()).unwrap_or_default();
        for p in std::iter::once(path.to_string()).chain(subs) {
            if on {
                self.selected.insert(p);
            } else {
                self.selected.remove(&p);
            }
        }
    }

    /// 選取符合條件的全部（每一頁，含加速版）；有星號的不選
    fn select_everything(&mut self, pins: &PinStore) {
        let all = self.data.as_ref().map(|x| x.all_paths.clone()).unwrap_or_default();
        self.selected.extend(all.into_iter().filter(|p| !pins.contains(p)));
    }

    /// Ctrl＋點一下：選取 / 取消這一筆；Shift＋點一下：選取從上次點的那一筆到這一筆（order：這一頁的順序）
    fn pick(&mut self, path: &str, order: &[String], shift: bool) {
        let from = self.anchor.as_ref().and_then(|a| order.iter().position(|p| p == a));
        if let (true, Some(a), Some(b)) = (shift, from, order.iter().position(|p| p == path)) {
            for p in &order[a.min(b)..=a.max(b)] {
                self.toggle(p, true);
            }
            return;
        }
        let on = !self.selected.contains(path);
        self.toggle(path, on);
        self.anchor = Some(path.to_string());
    }
}

/// 這一次點擊有沒有按著 Ctrl、Shift（多選）
fn multi_mods(ui: &egui::Ui) -> (bool, bool) {
    ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift))
}

/// Ctrl+A：選取這一頁全部
fn select_all_key(app: &UiApp, ui: &egui::Ui, d: &mut LibraryDialog, order: &[String]) {
    // 搜尋框是空的（開啟時游標在搜尋框）也可以用；有打字時 Ctrl+A 是選取搜尋框的文字
    let typing = ui.ctx().egui_wants_keyboard_input() && !d.query.is_empty();
    if typing || app.ask.is_some() || d.overlay() || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
        // 這一頁已經全選：再按一次選取全部（每一頁）
        let pins = &app.core.pins;
        let page: Vec<&String> = order.iter().filter(|p| !pins.contains(p)).collect();
        if !page.is_empty() && page.iter().all(|p| d.selected.contains(*p)) {
            d.select_everything(pins);
        } else {
            for p in page {
                d.toggle(p, true);
            }
        }
    }
}

/// 動圖的名稱（GIF、WebP）；不是動圖時 None
fn anim_name(x: &ExportInfo) -> Option<&'static str> {
    match x.format {
        Some(ExportFormat::Gif) => Some("GIF"),
        Some(ExportFormat::Webp) => Some("WebP"),
        _ => None,
    }
}

/// 匯出檔的標籤：4×、GIF 4×、GIF
pub fn export_tag(x: &ExportInfo) -> String {
    if let Some(name) = anim_name(x) {
        if x.speed > 1.0 {
            format!("{name} {}×", speed_label(x.speed))
        } else {
            name.into()
        }
    } else if x.format == Some(ExportFormat::Webm) {
        if x.speed <= 1.0 {
            tr!("WebM 壓縮版", "WebM compressed").into()
        } else {
            format!("WebM {}×", speed_label(x.speed))
        }
    } else if x.speed <= 1.0 {
        tr!("壓縮版", "Compressed").into()
    } else {
        format!("{}×", speed_label(x.speed))
    }
}

/// 子列的名稱：4× 加速版、GIF 4×、GIF（原速）。
/// 「指定長度」做出來的倍率是算出來的小數（例如 13.69×），不好讀，改用長度：30 秒版、GIF 30 秒版
fn export_label(x: &ExportInfo) -> String {
    let gif = anim_name(x).is_some();
    let anim = anim_name(x).unwrap_or("GIF");
    if (x.speed * 2.0).fract() != 0.0 {
        if let Some(d) = x.media.duration_sec {
            let len = trf!("{}版", "{} version", human_duration(d.round()));
            return if gif { format!("{anim} {len}") } else { len };
        }
    }
    if gif {
        if x.speed > 1.0 {
            format!("{anim} {}×", speed_label(x.speed))
        } else {
            trf!("{anim}（原速）", "{anim} (original speed)")
        }
    } else {
        let base = if x.speed <= 1.0 { tr!("壓縮版", "Compressed").to_string() } else { trf!("{}× 加速版", "{}× speed", speed_label(x.speed)) };
        if x.format == Some(ExportFormat::Webm) {
            format!("{base}（WebM）")
        } else {
            base
        }
    }
}

fn load(app: &mut UiApp) {
    let Some((kind, browsing)) = app.library.as_ref().map(|d| (d.kind, d.dir.is_some())) else {
        return;
    };
    let home = home_dir(app, kind);
    // 截圖資料夾＋留在原處的舊截圖（看其他資料夾時只看那個資料夾）
    let extra_dirs = if kind == Kind::Shot && !browsing { app.s.shot_extra(&app.env) } else { vec![] };
    let pinned = app.core.pins.list();
    // 截圖：在背景讀出圖裡的字（搜尋用）；打開時開始一次，已經做過的不再做
    if kind == Kind::Shot && app.library.as_ref().is_some_and(|d| !d.ocr_started) {
        let dirs: Vec<String> = match app.library.as_ref().and_then(|d| d.dir.clone()) {
            Some(dir) => vec![dir],
            None => std::iter::once(home.clone()).chain(extra_dirs.iter().cloned()).collect(),
        };
        let core = app.core.clone();
        app.spawn(async move { actions::ocr_index(&core, dirs).await }, |_, _| {});
        if let Some(d) = app.library.as_mut() {
            d.ocr_started = true;
        }
    }
    let Some(d) = app.library.as_mut() else {
        return;
    };
    d.dirty = false;
    d.loading = true;
    d.seq += 1;
    let seq = d.seq;
    let q = Some(d.query.clone()).filter(|q| !q.trim().is_empty());
    let only = if d.only_selected {
        Some(d.selected.iter().cloned().collect())
    } else if d.only_pinned {
        Some(pinned)
    } else {
        None
    };
    let query = match d.kind {
        Kind::Video => {
            LibraryQuery { q, filter: d.filter, sort: d.sort, page: Some(d.page as f64), page_size: None, fit_px: Some(d.fit_px), audio: true, extra_dirs: vec![], only, ..Default::default() }
        }
        Kind::Shot => LibraryQuery {
            q,
            filter: LibraryFilter::Shot,
            sort: d.sort,
            page: Some(d.page as f64),
            page_size: None,
            fit_px: None,
            audio: false,
            extra_dirs,
            only,
            grid: d.grid_fit,
            anchor: d.page_anchor.take(),
            facet: (d.browse || !d.facet.is_empty()).then(|| d.facet.clone()),
            // core 依設定決定（actions::library）
            subdirs: false,
        },
    };
    let dir = d.dir.clone().unwrap_or(home);
    d.loaded_dir = dir.clone();
    let core = app.core.clone();
    app.spawn(async move { actions::library(&core, &dir, &query).await }, move |app, page| {
        let Some(d) = app.library.as_mut() else {
            return;
        };
        if seq != d.seq {
            return; // 已有較新的查詢（例如還在輸入搜尋字）
        }
        d.loading = false;
        d.error = page.error.clone();
        d.page = page.page.max(1);
        for e in &page.items {
            app.known.insert(e.media.path.clone(), e.clone());
        }
        d.prefetch = page.ahead.clone();
        d.data = Some(page);
    });
}

/// 清單上按了什麼（關掉對話框後才處理，處理時要用到 app）
enum Req {
    Entry(EntryAction, LibraryEntry),
    Rename(LibraryEntry),
    /// 勾選的檔案移到資源回收筒
    DeleteSelected,
    /// 換儲存位置（Windows 的選資料夾視窗）
    ChooseDir,
    /// 匯入影片（選檔案；MOV、MKV 等轉成 MP4）
    Import,
    /// 在檔案總管開啟資料夾
    OpenDir(String),
    /// 看哪個資料夾（None = 目前的儲存位置）
    Browse(Option<String>),
    /// 關掉清單、開始錄影
    Record,
    /// 開啟「合併成一支」：勾選的 MP4
    Merge(Vec<String>),
    /// 開啟「一次處理」：勾選的 MP4
    Batch(Vec<String>),
    /// 開啟「拼成一張」
    Combine,
    /// 比對勾選的兩張截圖
    Compare,
    /// 這些檔案移到資源回收筒（先確認）
    Trash(Vec<String>),
    /// 截圖複製到剪貼簿（圖片）
    CopyImage(String),
    /// 加上 / 移除星號
    Pin(String, bool),
}

/// 清單平常看的資料夾：錄影是儲存位置，截圖是截圖資料夾
fn home_dir(app: &UiApp, kind: Kind) -> String {
    match kind {
        Kind::Video => app.s.out_dir(&app.env),
        Kind::Shot => app.s.shots_dir(&app.env),
    }
}

/// 記住用過的儲存位置（最近用的在前；資料夾選單的「之前用過的資料夾」）
pub fn remember_dir(app: &mut UiApp, dir: &str) {
    let dir = dir.trim();
    if dir.is_empty() || app.s.recent_dirs.first().is_some_and(|d| d.eq_ignore_ascii_case(dir)) {
        return;
    }
    app.s.recent_dirs.retain(|d| !d.eq_ignore_ascii_case(dir));
    app.s.recent_dirs.insert(0, dir.to_string());
    app.s.recent_dirs.truncate(super::settings::RECENT_DIRS);
    app.save_settings();
}

/// 太長的路徑從中間省略（保留開頭的磁碟與最後的資料夾名稱）：C:\Users\…\Videos\Timelapse
fn middle_truncate(ui: &egui::Ui, text: &str, font: &egui::FontId, max_w: f32) -> String {
    let width = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        // 後面保留多一點：最後的資料夾名稱最重要
        let head = keep / 3;
        let tail = keep - head;
        chars[..head].iter().chain(std::iter::once(&'…')).chain(chars[chars.len() - tail..].iter()).collect()
    };
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// 標題下的資料夾：資料夾圖示與路徑（太長時從中間省略），點一下在檔案總管開啟，滑鼠提示完整路徑
fn folder_link(ui: &mut egui::Ui, dir: &str, max_w: f32) -> egui::Response {
    let p = theme::pal(ui);
    let font = theme::font(12.5);
    let text = middle_truncate(ui, dir, &font, (max_w - 22.0).max(40.0));
    let g = ui.painter().layout_no_wrap(text, font, p.muted);
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0 + g.size().x, 22.0), Sense::click());
    let color = if resp.hovered() { p.accent } else { p.muted };
    theme::paint_icon(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 8.0, rect.center().y), vec2(15.0, 15.0)), Icon::Folder, color);
    let at = pos2(rect.min.x + 22.0, rect.center().y - g.size().y / 2.0);
    if resp.hovered() {
        ui.painter().hline(at.x..=at.x + g.size().x, at.y + g.size().y, Stroke::new(1.0, color));
    }
    ui.painter().galley_with_override_text_color(at, g, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(trf!("{dir}\n點一下開啟資料夾", "{dir}\nClick to open the folder"))
}

/// 清單是空的或讀不到資料夾：說明與按鈕（置中）；回傳按了第幾個按鈕
fn empty_state(ui: &mut egui::Ui, area: Rect, msg: &str, buttons: &[(&str, bool)]) -> Option<usize> {
    let p = theme::pal(ui);
    let r = Rect::from_center_size(pos2(area.center().x, area.min.y + 130.0), vec2(area.width().min(640.0), 110.0));
    let ui = &mut ui.new_child(UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Center)));
    ui.add(egui::Label::new(RichText::new(msg).font(theme::font(14.5)).color(p.muted)).wrap());
    let mut clicked = None;
    if !buttons.is_empty() {
        ui.add_space(10.0);
        // 一排按鈕置中：先量好總寬度
        let gap = 8.0;
        let total: f32 = buttons.iter().map(|(t, _)| Btn::new(*t).width(ui)).sum::<f32>() + gap * (buttons.len() - 1) as f32;
        let row = Rect::from_center_size(pos2(r.center().x, ui.cursor().min.y + 17.0), vec2(total, 34.0));
        let bui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        bui.spacing_mut().item_spacing.x = gap;
        for (i, (t, primary)) in buttons.iter().enumerate() {
            let b = Btn::new(*t);
            if (if *primary { b.primary() } else { b }).show(bui).clicked() {
                clicked = Some(i);
            }
        }
    }
    clicked
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 80.0).min(1040.0), screen.height() - 80.0);
    let mut close = false;
    let mut req: Option<Req> = None;
    // 目前的儲存位置記在「用過的資料夾」最前面（換了儲存位置之後，舊的還能從選單切回去看）
    let video_dir = app.s.out_dir(&app.env);
    remember_dir(app, &video_dir);
    let Some(kind) = app.library.as_ref().map(|d| d.kind) else { return };
    // 截圖看截圖資料夾（之前用過的資料夾只有錄影有）
    let save_dir = home_dir(app, kind);
    // 留在原處的舊截圖資料夾（清單也會列出）
    let extra: Vec<String> = if kind == Kind::Shot { app.s.shot_extra(&app.env) } else { vec![] };
    let recent: Vec<String> = if kind == Kind::Video { app.s.recent_dirs.iter().filter(|d| !d.eq_ignore_ascii_case(&save_dir)).cloned().collect() } else { vec![] };
    // 縮圖要用 app，先取出對話框
    let Some(mut d) = app.library.take() else {
        return;
    };
    watch_merge(app, &mut d);
    // 只看已選取，但全部取消了（或刪掉了）：回到全部
    if d.only_selected && d.selected.is_empty() {
        d.only_selected = false;
        d.page = 1;
        d.dirty = true;
    }
    let dir = d.dir.clone().unwrap_or_else(|| save_dir.clone());
    // 儲存位置改了（例如按了「變更儲存位置…」）：重新讀取
    if !d.loading && !d.dirty && !d.loaded_dir.is_empty() && d.loaded_dir != dir {
        d.page = 1;
        d.selected.clear();
        d.data = None;
        d.dirty = true;
    }
    // 搜尋建議開著時，Esc 只關掉建議（要在對話框檢查 Esc 之前先拿走）
    if browse::showing(&d) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        d.suggest.open = false;
    }
    let mut grid_size = app.s.shot_grid_size;
    let modal = egui::Modal::new(Id::new("library")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(size.x - 40.0);
        ui.set_height(size.y - 40.0);
        // 標題列：左邊標題與資料夾，右邊「錄影｜截圖」與關閉。
        // 先排右邊，路徑只用剩下的寬度（太長時從中間省略），不會被「錄影｜截圖」蓋住
        let head = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 48.0));
        let right_w = {
            let rui = &mut ui.new_child(UiBuilder::new().max_rect(head).layout(Layout::right_to_left(Align::Min)));
            if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(rui).clicked() {
                close = true;
            }
            rui.add_space(8.0);
            // 由右往左排：畫面上是「錄影｜截圖」
            if theme::segmented(rui, &mut d.kind, &[(Kind::Shot, tr!("截圖", "Screenshots")), (Kind::Video, tr!("錄影", "Recordings"))], true) {
                d.filter = LibraryFilter::All;
                if d.sort == LibrarySort::Duration {
                    d.sort = LibrarySort::New;
                }
                d.page = 1;
                d.selected.clear();
                d.data = None;
                // 錄影與截圖存在不同的資料夾：換了就回到各自的資料夾
                d.dir = None;
                d.dirty = true;
            }
            rui.min_rect().width()
        };
        {
            let left = Rect::from_min_max(head.min, pos2(head.max.x - right_w - 16.0, head.max.y));
            let lui = &mut ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::top_down(Align::Min)));
            lui.spacing_mut().item_spacing.y = 2.0;
            let shot = d.kind == Kind::Shot;
            // 和主畫面的「檔案庫：錄影｜截圖」同一個名稱；右上角的「錄影｜截圖」就是切換
            lui.add(egui::Label::new(RichText::new(tr!("檔案庫", "Library")).font(theme::font_bold(17.0))).truncate());
            lui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let other = d.dir.is_some();
                // 留給下拉按鈕與「之前用過的資料夾」標籤
                let with_extra = !other && !extra.is_empty();
                let max_w = left.width() - 34.0 - if other || with_extra { 120.0 } else { 0.0 };
                if folder_link(ui, &dir, max_w).clicked() {
                    req = Some(Req::OpenDir(dir.clone()));
                }
                let b = Btn::icon_only(Icon::ChevD).ghost().small().tooltip(tr!("看其他資料夾、變更儲存位置", "View another folder or change the save location")).show(ui);
                egui::Popup::menu(&b).show(|ui| {
                    ui.set_min_width(280.0);
                    let font = theme::font(13.0);
                    ui.label(theme::muted(ui, if shot { tr!("截圖資料夾", "Screenshot folder") } else { tr!("目前儲存位置", "Current save location") }));
                    if ui.selectable_label(!other, middle_truncate(ui, &save_dir, &font, 360.0)).on_hover_text(&save_dir).clicked() {
                        req = Some(Req::Browse(None));
                    }
                    if !recent.is_empty() {
                        ui.add_space(4.0);
                        ui.label(theme::muted(ui, tr!("之前用過的資料夾", "Previously used folders")));
                        for r in &recent {
                            if ui.selectable_label(d.dir.as_deref() == Some(r.as_str()), middle_truncate(ui, r, &font, 360.0)).on_hover_text(r).clicked() {
                                req = Some(Req::Browse(Some(r.clone())));
                            }
                        }
                    }
                    ui.separator();
                    if ui.button(if shot { tr!("變更截圖資料夾…", "Change screenshot folder…") } else { tr!("變更儲存位置…", "Change save location…") }).clicked() {
                        req = Some(Req::ChooseDir);
                    }
                });
                if other {
                    ui.add_space(6.0);
                    chip(ui, tr!("之前用過的資料夾", "Previous folder"), Tone::Warn, false).on_hover_text(tr!("新的錄影與截圖不是存在這裡", "New recordings and screenshots aren't saved here"));
                } else if with_extra {
                    ui.add_space(6.0);
                    let tip = format!("{}\n{}", tr!("也列出留在原處的舊截圖：", "Also listing older screenshots left in:"), extra.join("\n"));
                    chip(ui, &trf!("＋舊資料夾 {n} 個", "+{n} old folder(s)", n = extra.len()), Tone::Plain, false).on_hover_text(tip);
                }
            });
        }
        ui.advance_cursor_after_rect(head);
        let shot = d.kind == Kind::Shot;
        ui.add_space(8.0);
        // 搜尋、篩選、排序：放不下（視窗窄、介面放大）時搜尋框縮短，再放不下按鈕只顯示圖示（名稱在滑鼠提示）
        ui.horizontal(|ui| {
            const SEARCH_MIN: f32 = 180.0;
            let row_w = ui.available_width();
            let gap = ui.spacing().item_spacing.x;
            let widths = d.bar_w[shot as usize];
            let compact = row_w - widths[0] - gap < SEARCH_MIN;
            let search_w = (row_w - widths[compact as usize] - gap).clamp(SEARCH_MIN, 340.0);
            // 按鈕：放得下時圖示加文字，放不下時只有圖示
            let btn = |text: &str, icon: Icon, tip: &str| {
                if compact {
                    Btn::icon_only(icon).tooltip(format!("{text}：{tip}"))
                } else {
                    Btn::new(text).icon(icon).tooltip(tip)
                }
            };
            let hint = if shot {
                tr!("搜尋檔名、日期（例如 10/11）或圖裡的字", "Search names, dates (e.g. 10/11) or text in the image")
            } else {
                tr!("搜尋檔名、日期（例如 10/11）或字幕裡說過的話", "Search names, dates (e.g. 10/11) or words spoken")
            };
            // 右邊留位置給「✕」清除鈕
            let r = ui.add(
                egui::TextEdit::singleline(&mut d.query)
                    .hint_text(hint)
                    .desired_width(search_w)
                    .min_size(vec2(0.0, 30.0))
                    .margin(egui::Margin { left: 6, right: 28, top: 2, bottom: 2 })
                    .vertical_align(Align::Center),
            );
            super::tour::mark(ui, super::tour::Spot::LbSearch, r.rect);
            let search_right = r.rect.right();
            if d.focus_search {
                r.request_focus();
                d.focus_search = false;
            }
            if r.changed() {
                d.search_at = Some(Instant::now());
                if shot {
                    browse::query_changed(&mut d);
                }
            }
            if shot {
                browse::popup(ui, &mut d, &r);
            }
            if !d.query.is_empty() {
                let x = Rect::from_center_size(pos2(r.rect.max.x - 15.0, r.rect.center().y), vec2(22.0, 22.0));
                let xr = ui.interact(x, Id::new("libSearchClear"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("清除搜尋", "Clear search"));
                if xr.hovered() {
                    ui.painter().circle_filled(x.center(), 10.0, p.surface2);
                }
                theme::paint_icon(ui.painter(), Rect::from_center_size(x.center(), vec2(12.0, 12.0)), Icon::Close, if xr.hovered() { p.text } else { p.muted });
                if xr.clicked() {
                    d.query.clear();
                    d.search_at = None;
                    d.page = 1;
                    d.dirty = true;
                    d.focus_search = true;
                }
            }
            if !shot {
                let filters = [
                    (LibraryFilter::All, tr!("全部", "All")),
                    (LibraryFilter::Original, tr!("原始錄影", "Originals")),
                    (LibraryFilter::Cut, tr!("剪輯版", "Edited")),
                    (LibraryFilter::Speed, tr!("有加速版", "Has sped-up version")),
                    (LibraryFilter::Audio, tr!("有聲音", "Has audio")),
                ];
                egui::ComboBox::from_id_salt("libFilter").selected_text(filters.iter().find(|f| f.0 == d.filter).map(|f| f.1).unwrap_or("")).show_ui(ui, |ui| {
                    for (v, t) in filters {
                        if ui.selectable_label(d.filter == v, t).clicked() {
                            d.filter = v;
                            d.page = 1;
                            d.dirty = true;
                        }
                    }
                });
            }
            let sorts = [
                (LibrarySort::New, tr!("最新的在前", "Newest first")),
                (LibrarySort::Old, tr!("最舊的在前", "Oldest first")),
                (LibrarySort::Duration, tr!("長度（長到短）", "Longest first")),
                (LibrarySort::Size, tr!("檔案大小（大到小）", "Largest first")),
            ];
            let sorts: Vec<_> = sorts.into_iter().filter(|s| !(shot && s.0 == LibrarySort::Duration)).collect();
            egui::ComboBox::from_id_salt("libSort").selected_text(sorts.iter().find(|s| s.0 == d.sort).map(|s| s.1).unwrap_or("")).show_ui(ui, |ui| {
                for (v, t) in sorts {
                    if ui.selectable_label(d.sort == v, t).clicked() {
                        d.sort = v;
                        d.page = 1;
                        d.dirty = true;
                    }
                }
            });
            // 截圖：縮圖大小（小的一頁放比較多張；也可以在縮圖上按住 Ctrl 轉滾輪）
            if shot {
                ui.add_space(4.0);
                let mut size = grid_size;
                let tip = tr!(
                    "縮圖大小：小的一頁可以放比較多張。也可以在縮圖上按住 Ctrl 轉滾輪調整",
                    "Thumbnail size: smaller fits more per page. You can also hold Ctrl and turn the mouse wheel over the thumbnails"
                );
                // 標籤放在前面：「縮圖 小｜中｜大」
                if !compact {
                    ui.label(theme::muted(ui, tr!("縮圖", "Thumbnails"))).on_hover_text(tip);
                }
                if theme::segmented(ui, &mut size, &[(0u8, tr!("小", "S")), (1, tr!("中", "M")), (2, tr!("大", "L"))], true) {
                    grid_size = size;
                }
            }
            // 截圖：用點的找（瀏覽欄）、月曆
            if shot {
                ui.add_space(4.0);
                if btn(
                    tr!("用點的找", "Browse"),
                    Icon::List,
                    tr!(
                        "左邊列出程式、內容、常出現的詞、時間、顏色，點幾下就能縮小範圍，不用記得關鍵字（再按一次收起來）",
                        "Lists apps, content types, common words, dates and colors on the left. Click a few to narrow down without remembering keywords (click again to hide)"
                    ),
                )
                .small()
                .ghost()
                .selected(d.browse && d.calendar.is_none())
                .show(ui)
                .clicked()
                {
                    if d.calendar.is_some() {
                        d.calendar = None;
                        d.browse = true;
                    } else {
                        d.browse = !d.browse;
                    }
                    d.dirty = true;
                }
                let cal = btn(
                    tr!("月曆", "Calendar"),
                    Icon::Clock,
                    tr!(
                        "一個月一頁，看每天截了幾張、那天圖裡常出現什麼字；點一天就列出那天的截圖",
                        "One month per page: how many screenshots each day and what they were about. Click a day to list them"
                    ),
                )
                .small()
                .ghost()
                .selected(d.calendar.is_some())
                .show(ui);
                super::tour::mark(ui, super::tour::Spot::LbCalendar, cal.rect);
                if cal.clicked() {
                    d.calendar = if d.calendar.is_some() { None } else { Some(browse::CalView::this_month()) };
                }
            }
            // 只看有星號的
            ui.add_space(4.0);
            if btn(tr!("有星號的", "Starred"), Icon::Star, tr!("只列出加了星號的檔案（再按一次顯示全部）", "Show only starred files (click again to show all)"))
                .small()
                .ghost()
                .selected(d.only_pinned)
                .show(ui)
                .clicked()
            {
                d.only_pinned = !d.only_pinned;
                d.only_selected = false;
                d.page = 1;
                d.dirty = true;
            }
            // 匯入其他地方的影片（也可以直接把檔案拖進視窗）
            if !shot
                && btn(
                    tr!("匯入影片…", "Import videos…"),
                    Icon::Folder,
                    tr!(
                        "把手機、其他軟體錄的影片放進儲存位置（MOV、MKV、AVI、WMV、WebM 等會轉成 MP4，原本的檔案不會動）。也可以直接把檔案拖進這個視窗",
                        "Bring videos from your phone or other apps into your save folder (MOV, MKV, AVI, WMV, WebM and others are converted to MP4; the originals stay as they are). You can also drop files onto this window"
                    ),
                )
                .small()
                .ghost()
                .show(ui)
                .clicked()
            {
                req = Some(Req::Import);
            }
            if shot {
                let b = Btn::new(tr!("整理", "Tidy up")).icon(Icon::ChevD).small().ghost().tooltip(tr!("清理舊截圖、找出相似的截圖", "Clean up old screenshots, find similar ones")).show(ui);
                egui::Popup::menu(&b).show(|ui| {
                    ui.set_min_width(220.0);
                    if ui.button(tr!("清理舊截圖…", "Clean up old screenshots…")).clicked() {
                        d.cleanup = Some(CleanupDlg { days: 90, list: None, seq: 0, busy: false });
                    }
                    if ui.button(tr!("找出相似的截圖…", "Find similar screenshots…")).clicked() {
                        d.similar = Some(SimilarDlg { level: 1, groups: None, remove: HashSet::new(), seq: 0, busy: false });
                    }
                });
            }
            d.bar_w[shot as usize][compact as usize] = ui.min_rect().right() - search_right;
        });
        if shot {
            browse::tick(app, &mut d);
        }
        if d.search_at.is_some_and(|t| t.elapsed().as_millis() > 250) {
            d.search_at = None;
            d.page = 1;
            d.dirty = true;
        }
        ui.add_space(8.0);
        let foot_h = 40.0;
        let table = Rect::from_min_max(ui.cursor().min, pos2(ui.max_rect().max.x, ui.max_rect().max.y - foot_h - 8.0));
        if shot && d.calendar.is_some() {
            browse::calendar(app, ui, &mut d, table);
        } else if shot {
            let grid = if d.browse {
                let side = Rect::from_min_max(table.min, pos2(table.min.x + browse::PANEL_W, table.max.y));
                browse::panel(ui, &mut d, side);
                super::tour::mark(ui, super::tour::Spot::LbBrowse, side);
                Rect::from_min_max(pos2(side.max.x + 12.0, table.min.y), table.max)
            } else {
                table
            };
            shot_grid(app, ui, &mut d, grid, &dir, &mut req, &mut grid_size);
        } else {
            video_table(app, ui, &mut d, table, &dir, &mut req);
        }
        // 換頁中：馬上把舊的那頁變淡、顯示「讀取第 N 頁」，不會像沒反應
        if d.loading && d.data.as_ref().is_some_and(|x| x.page != d.page) {
            ui.painter().rect_filled(table, CornerRadius::same(6), p.surface.gamma_multiply(0.6));
            let chip = Rect::from_center_size(table.center(), vec2(150.0, 34.0));
            ui.painter().rect_filled(chip, CornerRadius::same(17), p.surface2);
            ui.painter().text(chip.center(), egui::Align2::CENTER_CENTER, trf!("讀取第 {} 頁…", "Loading page {}…", d.page), theme::font_bold(13.0), p.text);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // 這一頁的縮圖都排好了，再排前後一頁的
        if !d.loading {
            for (path, mtime) in std::mem::take(&mut d.prefetch) {
                app.thumb(&path, mtime);
            }
        }
        // 下方：選取與分頁
        let painter = ui.painter().clone();
        let foot = Rect::from_min_max(pos2(table.min.x, ui.max_rect().max.y - foot_h), ui.max_rect().max);
        painter.hline(foot.x_range(), foot.min.y - 4.0, Stroke::new(1.0, p.border));
        let n = d.selected.len();
        let total = d.data.as_ref().map(|x| x.total).unwrap_or(0);
        // 右邊先放頁碼，量好寬度；左邊（勾選的數量、操作、選取）只用剩下的地方，不會疊在頁碼上
        let pager_w = if total > 0 {
            let right = &mut ui.new_child(UiBuilder::new().max_rect(foot).layout(Layout::right_to_left(Align::Center)));
            pager(right, &mut d);
            right.min_rect().width() + 16.0
        } else {
            0.0
        };
        let fui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(foot.min, pos2(foot.max.x - pager_w, foot.max.y))).layout(Layout::left_to_right(Align::Center)));
        // 勾選的有幾個不在這一頁（換頁時勾選會保留）
        let others = d
            .data
            .as_ref()
            .map(|x| {
                let here: HashSet<&str> = x.items.iter().flat_map(|e| std::iter::once(e.media.path.as_str()).chain(e.exports.iter().map(|x| x.media.path.as_str()))).collect();
                d.selected.iter().filter(|p| !here.contains(p.as_str())).count()
            })
            .unwrap_or(0);
        // 沒有任何檔案時不顯示操作說明
        let hint = match (n, shot) {
            (0, _) if total == 0 => String::new(),
            (0, true) => tr!(
                "勾選多張可一次刪除或拼成一張（從空白處拖曳框選、Ctrl / Shift＋點一下、Ctrl+A）；縮圖可以拖到其他程式",
                "Select several to delete or combine them (drag a box from empty space, Ctrl/Shift+click, Ctrl+A); drag thumbnails into other apps"
            )
            .into(),
            (0, false) => {
                tr!("勾選多筆可一次刪除（Ctrl / Shift＋點一下、Ctrl+A 全選本頁，再按一次選取全部）", "Select several to delete them at once (Ctrl/Shift+click, Ctrl+A for this page, again for all)")
                    .into()
            }
            _ if is_en() => {
                let more = if others > 0 { format!(" ({others} on other pages)") } else { String::new() };
                format!("{n} {}{} selected{more}", if shot { "screenshot" } else { "file" }, if n == 1 { "" } else { "s" })
            }
            _ => {
                let unit = if shot { "張" } else { "個檔案" };
                let more = if others > 0 { format!("（其他頁 {others} {}）", if shot { "張" } else { "個" }) } else { String::new() };
                format!("已選取 {n} {unit}{more}")
            }
        };
        // 截圖的文字索引進度（做完時重新搜尋，才找得到剛讀出來的字）
        let ocr = if shot { app.core.ocr.progress() } else { None };
        if let Some((done, all)) = ocr.filter(|(_, all)| *all > 0) {
            ctx.request_repaint_after(std::time::Duration::from_millis(800));
            if n == 0 {
                fui.label(theme::muted(fui, trf!("正在讀取截圖裡的字 {done} / {all}（搜尋時會一起找）", "Reading text in screenshots {done} / {all} (search includes it)")));
            }
        } else if d.ocr_running && !d.query.trim().is_empty() {
            d.dirty = true;
        }
        d.ocr_running = ocr.is_some();
        if n > 0 {
            fui.label(theme::muted(fui, hint));
        } else if !hint.is_empty() && ocr.filter(|(_, all)| *all > 0).is_none() {
            // 沒勾選時操作說明收起來，滑鼠移上去才看
            fui.label(theme::muted(fui, tr!("多選小技巧", "Multi-select tips"))).on_hover_text(hint);
        }
        if n > 0 && Btn::new(tr!("移到資源回收筒", "Move to Recycle Bin")).icon(Icon::Trash).danger().small().show(fui).clicked() {
            req = Some(Req::DeleteSelected);
        }
        // MP4 和 MOV、MKV 等其他影片格式都能合併（成品是 MP4）
        let mp4s: Vec<String> = d.selected.iter().filter(|p| screenrecorder_core::import::is_importable(p)).cloned().collect();
        let merging = app.status.export.as_ref().filter(|x| x.state == ExportState::Running && x.kind == ExportKind::Merge).map(|x| x.progress);
        if !shot
            && mp4s.len() >= 2
            && merging.is_none()
            && Btn::new(tr!("合併成一支…", "Merge into one…"))
                .small()
                .tooltip(tr!(
                    "把勾選的錄影依時間順序接成一支（可調整順序；畫面尺寸以最早的那支為準）；GIF 不算",
                    "Joins the selected recordings in time order into one video (you can change the order; the frame size follows the earliest one). GIFs are skipped"
                ))
                .show(fui)
                .clicked()
        {
            req = Some(Req::Merge(mp4s.clone()));
        }
        let batching = batch::running(app);
        if !shot
            && mp4s.len() >= 2
            && batching.is_none()
            && Btn::new(tr!("一次處理…", "Process all…"))
                .small()
                .tooltip(tr!(
                    "勾選的錄影一次剪掉沒動靜的片段、產生字幕、做加速版或壓縮（一支一支依序做）",
                    "Cut idle parts, generate subtitles, make sped-up versions or compress all the selected recordings (one at a time)"
                ))
                .show(fui)
                .clicked()
        {
            req = Some(Req::Batch(mp4s));
        }
        if let Some((i, n)) = batching.filter(|_| !shot) {
            fui.add_space(8.0);
            fui.label(RichText::new(trf!("一次處理中（第 {i}/{n} 支）…", "Processing {i} of {n}…")).font(theme::font_bold(12.5)).color(p.accent));
        }
        if let Some(frac) = merging {
            // 進度條＋百分比＋取消（原本只有一行小字，很容易錯過）
            fui.add_space(8.0);
            fui.label(RichText::new(tr!("合併中", "Merging")).font(theme::font_bold(12.5)).color(p.accent));
            let (bar, _) = fui.allocate_exact_size(vec2(120.0, 6.0), Sense::hover());
            fui.painter().rect_filled(bar, CornerRadius::same(3), p.surface2);
            fui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * (frac as f32).clamp(0.0, 1.0), bar.height())), CornerRadius::same(3), p.accent);
            fui.label(RichText::new(format!("{:.0}%", frac * 100.0)).font(theme::font_bold(12.5)).color(p.accent));
            if Btn::new(tr!("取消", "Cancel")).small().ghost().tooltip(tr!("停止合併（做到一半的檔案會刪掉）", "Stop merging (the unfinished file is deleted)")).show(fui).clicked() {
                let _ = app.core.exporter.cancel();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        // 拼成一張一次最多 20 張
        if shot
            && (2..=20).contains(&n)
            && Btn::new(tr!("拼成一張…", "Combine into one…"))
                .small()
                .tooltip(tr!("把勾選的截圖拼成一張新的截圖（可調整順序、排法與間距）", "Combines the selected screenshots into a new screenshot (you can change the order, layout and spacing)"))
                .show(fui)
                .clicked()
        {
            req = Some(Req::Combine);
        }
        if shot
            && n == 2
            && Btn::new(tr!("比較差異", "Compare"))
                .small()
                .tooltip(tr!(
                    "找出兩張截圖不一樣的地方，在較新的那張上用紅框標出來，存成新的截圖",
                    "Finds what's different between the two screenshots and marks it with red boxes on the newer one, saved as a new screenshot"
                ))
                .show(fui)
                .clicked()
        {
            req = Some(Req::Compare);
        }
        // 取消選取、只看已選取、選取全部：放在下面這一行（高度固定），勾選時上面的清單才不會跳動
        if n > 0 {
            fui.add_space(6.0);
            selection_links(fui, &mut d, shot, &app.core.pins);
        }
    });
    if grid_size != app.s.shot_grid_size {
        app.s.shot_grid_size = grid_size;
        app.save_settings();
    }
    let covered = d.overlay();
    merge_dialog(app, ctx, &mut d);
    batch::show(app, ctx, &mut d);
    combine_dialog(app, ctx, &mut d);
    cleanup_dialog(app, ctx, &mut d);
    similar_dialog(app, ctx, &mut d);
    if let Some(list) = d.drag_out.take() {
        drag_out(app, &list);
    }
    if modal.should_close() && app.ask.is_none() && !covered {
        close = true;
    }
    if close || matches!(req, Some(Req::Record)) {
        if matches!(req, Some(Req::Record)) {
            start_recording(app);
        }
        return;
    }
    let dirty = d.dirty;
    let jump = d.jump.take();
    app.library = Some(d);
    if dirty {
        load(app);
    }
    if let Some((e, t)) = jump {
        super::viewer::open_at(app, e, t);
    }
    match req {
        None | Some(Req::Record) => {}
        Some(Req::Entry(act, e)) => {
            // 剪輯影片、製作加速版時關掉清單；編輯截圖蓋在清單上，關掉編輯就回到清單
            if matches!(act, EntryAction::Edit | EntryAction::Export | EntryAction::Compress | EntryAction::Vertical) && !super::dialogs::is_image(&e.media.path) && app.blocked(act).is_none() {
                app.library = None;
            }
            app.act(act, e);
        }
        Some(Req::Rename(e)) => rename_entry(app, e),
        Some(Req::DeleteSelected) => remove_selected(app),
        Some(Req::Import) => super::import::pick(app),
        Some(Req::ChooseDir) => match app.library.as_ref().map(|d| d.kind) {
            Some(Kind::Shot) => super::settings_dialog::choose_shot_dir(app),
            _ => super::settings_dialog::choose_output_dir(app),
        },
        Some(Req::OpenDir(dir)) => {
            if let Err(e) = actions::open(actions::OpenAction::Folder, &dir) {
                app.toast(e.message().to_string(), true);
            }
        }
        Some(Req::Browse(dir)) => {
            if let Some(d) = &mut app.library {
                if d.dir != dir {
                    d.dir = dir;
                    d.page = 1;
                    d.selected.clear();
                    d.data = None;
                    d.dirty = true;
                }
            }
        }
        Some(Req::Merge(paths)) => open_merge(app, paths),
        Some(Req::Batch(paths)) => {
            let dlg = batch::BatchDlg::new(app, paths);
            if let Some(d) = &mut app.library {
                d.batch = Some(dlg);
            }
        }
        Some(Req::Combine) => open_combine(app),
        Some(Req::Compare) => compare_selected(app),
        Some(Req::Trash(paths)) => trash_paths(app, paths),
        Some(Req::CopyImage(path)) => super::viewer::copy_png(app, path),
        Some(Req::Pin(path, on)) => set_star(app, &path, on),
    }
}

/// 頁碼要列出哪幾頁（None = 省略號）：1 … 4 5 6 … 12
fn page_numbers(page: usize, pages: usize) -> Vec<Option<usize>> {
    if pages <= 7 {
        return (1..=pages).map(Some).collect();
    }
    let mut out = vec![Some(1)];
    let (lo, hi) = (page.saturating_sub(1).max(2), (page + 1).min(pages - 1));
    let (lo, hi) = if page <= 3 {
        (2, 4)
    } else if page >= pages - 2 {
        (pages - 3, pages - 1)
    } else {
        (lo, hi)
    };
    if lo > 2 {
        out.push(None);
    }
    out.extend((lo..=hi).map(Some));
    if hi < pages - 1 {
        out.push(None);
    }
    out.push(Some(pages));
    out
}

/// 分頁（由右往左排）：‹ 1 2 3 … 7 › 共 60 筆；只有一、兩頁時「第 1 / 2 頁・共 30 筆」
fn pager(ui: &mut egui::Ui, d: &mut LibraryDialog) {
    let Some(x) = d.data.as_ref() else { return };
    let (pages, total) = (x.pages.max(1), x.total);
    let mut go = None;
    ui.spacing_mut().item_spacing.x = 2.0;
    if Btn::icon_only(Icon::ChevR).ghost().small().enabled(d.page < pages).tooltip(tr!("下一頁（滾輪往下）", "Next page (scroll down)")).show(ui).clicked() {
        go = Some(d.page + 1);
    }
    if pages > 2 {
        for n in page_numbers(d.page, pages).into_iter().rev() {
            match n {
                Some(n) => {
                    if Btn::new(n.to_string()).small().ghost().selected(n == d.page).min_width(28.0).show(ui).clicked() && n != d.page {
                        go = Some(n);
                    }
                }
                None => {
                    ui.label(theme::muted(ui, "…"));
                }
            }
        }
    } else {
        let text = if is_en() { format!("Page {} of {pages}", d.page) } else { format!("第 {} / {pages} 頁", d.page) };
        ui.label(RichText::new(text).font(theme::font(12.5)));
    }
    if Btn::icon_only(Icon::ChevL).ghost().small().enabled(d.page > 1).tooltip(tr!("上一頁（滾輪往上）", "Previous page (scroll up)")).show(ui).clicked() {
        go = Some(d.page - 1);
    }
    ui.add_space(8.0);
    let count = if is_en() { format!("{total} item{}", if total == 1 { "" } else { "s" }) } else { format!("共 {total} 筆") };
    ui.label(theme::muted(ui, count));
    if let Some(n) = go {
        d.page = n.clamp(1, pages);
        d.dirty = true;
    }
}

/// 可以開始錄影（有 FFmpeg、沒在錄影或轉檔）
fn can_record(app: &UiApp) -> bool {
    app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && !app.exporting() && !app.locked()
}

/// 關掉清單、用目前的設定開始錄影（與主畫面的「開始錄影」相同）
fn start_recording(app: &mut UiApp) {
    app.library = None;
    let config = app.s.record_config(&app.env);
    let core = app.core.clone();
    app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
}

/// 只有聲音的檔案（存成 M4A 做出來的）
fn is_audio_file(name: &str) -> bool {
    name.to_lowercase().ends_with(".m4a")
}

/// 移到資源回收筒的項目（紅字，會先確認）
fn trash_item(ui: &mut egui::Ui) -> bool {
    ui.button(RichText::new(tr!("移到資源回收筒…", "Move to Recycle Bin…")).color(theme::pal(ui).rec)).clicked()
}

/// 移到資源回收筒的檔案：錄影連同底下的加速版、GIF
fn trash_list(e: &LibraryEntry) -> Vec<String> {
    std::iter::once(e.media.path.clone()).chain(e.exports.iter().map(|x| x.media.path.clone())).collect()
}

/// 滾輪在清單上：往下捲換下一頁、往上捲換上一頁（捲一段距離才換，連續捲動時至少隔 0.35 秒）
fn wheel_pages(ui: &egui::Ui, d: &mut LibraryDialog, area: Rect) {
    // 上面蓋著合併、拼成一張的對話框或選單時不換頁
    if !ui.rect_contains_pointer(area) || egui::Popup::is_any_open(ui.ctx()) || d.overlay() {
        d.wheel = 0.0;
        return;
    }
    let dy = ui.input(|i| super::editor::wheel_delta(i).y);
    if dy == 0.0 {
        return;
    }
    d.wheel += dy;
    let pages = d.data.as_ref().map(|x| x.pages).unwrap_or(1);
    if d.wheel.abs() < 40.0 || d.wheel_at.is_some_and(|t| t.elapsed().as_millis() < 350) || d.loading {
        return;
    }
    let next = if d.wheel < 0.0 { d.page + 1 } else { d.page.saturating_sub(1) };
    d.wheel = 0.0;
    if (1..=pages).contains(&next) && next != d.page {
        d.page = next;
        d.dirty = true;
        d.wheel_at = Some(Instant::now());
    }
}

/// 鍵盤：Delete 移到資源回收筒（有勾選時是勾選的全部，和下方按鈕一樣；沒勾選時是滑鼠所在或上次點的那一列）、F2 改名、Enter 播放
fn row_keys(app: &UiApp, ui: &egui::Ui, d: &LibraryDialog, rows: &[LibraryEntry], req: &mut Option<Req>) {
    if req.is_some() || app.ask.is_some() || d.overlay() || ui.ctx().egui_wants_keyboard_input() || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let (del, f2, enter) = ui.input(|i| (i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::F2), i.key_pressed(egui::Key::Enter)));
    if del && !d.selected.is_empty() {
        *req = Some(Req::DeleteSelected);
        return;
    }
    let Some(e) = d.cursor_row.as_ref().and_then(|p| rows.iter().find(|e| &e.media.path == p)) else { return };
    if del {
        *req = Some(Req::Trash(trash_list(e)));
    } else if f2 && !is_audio_file(&e.media.name) {
        *req = Some(Req::Rename(e.clone()));
    } else if enter {
        *req = Some(Req::Entry(EntryAction::Play, e.clone()));
    }
}

/// 錄影：表格（高度固定，伺服器依每筆的子列數分頁，一頁剛好放滿）。
/// 依日期分組：每天第一筆在「日期」欄寫「今天」「昨天」「10/08（三）」，其餘只寫時間
fn video_table(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, table: Rect, dir: &str, req: &mut Option<Req>) {
    let p = theme::pal(ui);
    let head_h = 30.0;
    let fit = ((table.height() - head_h - 4.0) as f64).max(LIBRARY_ROW_MAIN_PX * 3.0).floor();
    if (fit - d.fit_px).abs() > 0.5 {
        d.fit_px = fit;
        if d.data.is_none() {
            d.dirty = true;
        } else {
            d.fit_at = Some(Instant::now());
        }
    }
    if d.fit_at.is_some_and(|t| t.elapsed().as_millis() > 250) {
        d.fit_at = None;
        d.dirty = true;
    } else if d.fit_at.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(260));
    }
    wheel_pages(ui, d, table);
    let rows = d.data.as_ref().map(|x| x.items.clone()).unwrap_or_default();
    let order: Vec<String> = rows.iter().map(|e| e.media.path.clone()).collect();
    let (ctrl, shift) = multi_mods(ui);
    select_all_key(app, ui, d, &order);
    // 勾選、日期、檔案、長度、解析度、大小、操作
    let cols = [36.0, 104.0, 0.0, 70.0, 86.0, 90.0, 166.0];
    let name_w = table.width() - cols.iter().sum::<f32>();
    let col_x = |i: usize| -> f32 {
        let widths = [cols[0], cols[1], name_w, cols[3], cols[4], cols[5], cols[6]];
        table.min.x + widths[..i].iter().sum::<f32>()
    };
    let painter = ui.painter().clone();
    let num = |t: &str, x: f32, y: f32, color: Color32, size: f32| {
        painter.text(pos2(x, y), egui::Align2::RIGHT_CENTER, t, theme::font(size), color);
    };
    painter.rect_filled(Rect::from_min_size(table.min, vec2(table.width(), head_h)), CornerRadius::same(6), p.surface2);
    // 標題列（核取方塊與每一列的對齊）
    {
        let head = Rect::from_min_size(table.min, vec2(table.width(), head_h));
        let all = !rows.is_empty() && rows.iter().all(|e| d.selected.contains(&e.media.path) && e.exports.iter().all(|x| d.selected.contains(&x.media.path)));
        let mut all2 = all;
        let ui2 = &mut ui
            .new_child(UiBuilder::new().max_rect(Rect::from_min_max(head.min + vec2(10.0, 0.0), pos2(head.min.x + col_x(1) - table.min.x, head.max.y))).layout(Layout::left_to_right(Align::Center)));
        if theme::check(ui2, &mut all2, "").on_hover_text(tr!("全選本頁", "Select all on this page")).changed() {
            for e in &rows {
                d.toggle(&e.media.path, all2);
            }
        }
        let heads = [(tr!("日期", "Date"), false), (tr!("檔案", "File"), false), (tr!("長度", "Duration"), true), (tr!("解析度", "Resolution"), true), (tr!("大小", "Size"), true)];
        for (i, (t, right)) in heads.iter().enumerate() {
            let x0 = col_x(i + 1);
            let x1 = col_x(i + 2);
            let (pos, align) = if *right { (pos2(x1 - 10.0, head.center().y), egui::Align2::RIGHT_CENTER) } else { (pos2(x0 + 6.0, head.center().y), egui::Align2::LEFT_CENTER) };
            painter.text(pos, align, *t, theme::font_bold(12.5), p.muted);
        }
    }
    // 內容
    let mut y = table.min.y + head_h + 2.0;
    let times = time_labels(rows.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
    // 時間一律寫「時:分」；同一分鐘有好幾支時，後面用淡色小字補秒數。欄寬固定，檔名才會切齊
    let hm_w = ui.fonts_mut(|f| f.layout_no_wrap("00:00".into(), theme::font_bold(13.0), p.text).size().x);
    let sec_w = ui.fonts_mut(|f| f.layout_no_wrap(":00".into(), theme::font(11.0), p.muted).size().x);
    let time_w = hm_w + if times.values().any(|(_, t)| t.len() > 5) { sec_w + 1.0 } else { 0.0 };
    // 每一天（這一頁）的支數、總長度、總大小：日期欄第二行與提示
    let mut days: HashMap<chrono::NaiveDate, (usize, f64, u64)> = HashMap::new();
    for e in &rows {
        if let Some((dd, _)) = times.get(&e.media.name) {
            let x = days.entry(*dd).or_default();
            x.0 += 1;
            x.1 += e.media.duration_sec.unwrap_or(0.0);
            x.2 += e.media.bytes;
        }
    }
    // 檔名開始的位置（主列：縮圖、時間之後；加速版子列也對齊這裡）
    let name_x = col_x(2) + 4.0 + 53.0 + 10.0 + time_w + 12.0;
    let today = chrono::Local::now().date_naive();
    let mut last_day = None;
    if rows.is_empty() {
        let can = can_record(app);
        empty_list(ui, d, table, dir, req, tr!("這個資料夾還沒有錄影。", "No recordings in this folder yet."), tr!("沒有符合條件的錄影。", "No matching recordings."), can);
    }
    for e in &rows {
        let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_MAIN_PX as f32));
        // 整列可按右鍵（所有操作的選單）；點一下記住這一列（鍵盤 Delete、F2、Enter 操作它）
        let resp = ui.interact(row.shrink2(vec2(0.0, 1.0)), Id::new(("librow", &e.media.path)), drag_sense());
        // 子列（加速版）也算在這一列的範圍內：滑鼠在子列上時，原片的按鈕也亮起來
        let sub_rows = e.exports.len() + e.hits.len().min(LIBRARY_HITS_SHOWN);
        let group = Rect::from_min_size(row.min, vec2(row.width(), LIBRARY_ROW_MAIN_PX as f32 + sub_rows as f32 * LIBRARY_ROW_SUB_PX as f32));
        let hot = ui.rect_contains_pointer(group);
        let audio = is_audio_file(&e.media.name);
        let fresh = d.fresh.as_deref() == Some(e.media.path.as_str());
        let on = d.selected.contains(&e.media.path);
        if resp.hovered() || resp.clicked() || resp.secondary_clicked() {
            d.cursor_row = Some(e.media.path.clone());
        }
        // 勾選的列、剛做好的列：淡淡的強調色底
        if on || fresh {
            painter.rect_filled(row, CornerRadius::same(4), p.accent_soft);
        } else if resp.hovered() {
            painter.rect_filled(row, CornerRadius::same(4), p.surface2.gamma_multiply(0.6));
        }
        if fresh {
            painter.rect_filled(Rect::from_min_size(row.min + vec2(0.0, 4.0), vec2(3.0, row.height() - 8.0)), CornerRadius::same(2), p.accent);
        }
        let groups = super::entry_menu::groups(app, e, Place::Library);
        resp.context_menu(|ui| {
            if let Some(a) = super::entry_menu::menu(ui, &groups) {
                *req = Some(Req::Entry(a, e.clone()));
            }
        });
        if resp.clicked() && (ctrl || shift) {
            d.pick(&e.media.path, &order, shift);
        }
        let (day, time) = times.get(&e.media.name).cloned().unzip();
        // 新的一天：上方用深一點的線分隔，日期欄寫日期
        let new_day = day.is_some() && day != last_day;
        if new_day && last_day.is_some() {
            painter.hline(row.x_range(), row.min.y - 1.0, Stroke::new(1.0, p.border_strong));
        }
        last_day = day;
        painter.hline(row.x_range(), row.max.y, Stroke::new(1.0, p.border));
        let rui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        rui.add_space(10.0);
        let mut on2 = on;
        if theme::check(rui, &mut on2, "").changed() {
            if shift {
                d.pick(&e.media.path, &order, true);
            } else {
                d.toggle(&e.media.path, on2);
                d.anchor = Some(e.media.path.clone());
            }
        }
        if new_day {
            if let Some(dd) = day {
                // 第一行「12/30 週二」；第二行淡色小字：年份（不是今年時）、這天有幾支
                let (main, year) = day_parts(dd, today);
                let (cnt, dur, bytes) = days.get(&dd).copied().unwrap_or_default();
                let mut sub: Vec<String> = year.into_iter().collect();
                if cnt > 1 {
                    sub.push(trf!("{cnt} 支", "{cnt} videos"));
                }
                let x = col_x(1) + 6.0;
                if sub.is_empty() {
                    painter.text(pos2(x, row.center().y), egui::Align2::LEFT_CENTER, &main, theme::font_bold(13.0), p.text);
                } else {
                    painter.text(pos2(x, row.center().y - 1.0), egui::Align2::LEFT_BOTTOM, &main, theme::font_bold(13.0), p.text);
                    painter.text(pos2(x, row.center().y + 1.0), egui::Align2::LEFT_TOP, sub.join(" · "), theme::font(11.0), p.muted);
                }
                // 點日期：勾選（或取消）這一天全部
                let cell = Rect::from_min_max(pos2(col_x(1), row.min.y + 4.0), pos2(col_x(2) - 4.0, row.max.y - 4.0));
                let dresp = ui.interact(cell, Id::new(("libday", dd)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                let same: Vec<&String> = rows.iter().filter(|x| times.get(&x.media.name).map(|t| t.0) == Some(dd)).map(|x| &x.media.path).collect();
                let all_on = same.iter().all(|p| d.selected.contains(*p));
                let tip = trf!(
                    "{}（這一頁）：{cnt} 支，共 {}、{}\n點一下{}這一天全部",
                    "{} (this page): {cnt} videos, {} total, {}\nClick to {} all of this day",
                    day_label(dd, today),
                    short_clock(dur),
                    format_bytes(bytes),
                    if all_on { tr!("取消勾選", "unselect") } else { tr!("勾選", "select") }
                );
                if dresp.on_hover_text(tip).clicked() {
                    for p in same {
                        d.toggle(p, !all_on);
                    }
                }
            }
        }
        // 檔案欄：縮圖（點一下播放）、時間、檔名（資料夾裡看到的名稱），右邊是剪輯版、有聲音
        let name_rect = Rect::from_min_max(pos2(col_x(2), row.min.y), pos2(col_x(3), row.max.y));
        let thumb = Rect::from_min_size(pos2(name_rect.min.x + 4.0, row.center().y - 15.0), vec2(53.0, 30.0));
        let tresp = ui.interact(thumb, Id::new(("libthumb", &e.media.path)), drag_sense()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.drag_started_by(egui::PointerButton::Primary) || tresp.drag_started_by(egui::PointerButton::Primary) {
            d.drag_out = Some(drag_list(d, &e.media.path));
        }
        painter.rect_filled(thumb, CornerRadius::same(4), p.surface2);
        if audio {
            theme::paint_icon(&painter, Rect::from_center_size(thumb.center(), vec2(18.0, 18.0)), Icon::Speaker, p.muted);
        } else if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
            theme::paint_thumb(&painter, thumb, &t);
        }
        if tresp.hovered() {
            // 滑鼠移上去：縮圖上出現播放鍵；影片在旁邊浮出預覽，左右移動看不同時間的畫面
            painter.rect_filled(thumb, CornerRadius::same(4), Color32::from_black_alpha(90));
            theme::paint_icon(&painter, Rect::from_center_size(thumb.center() + vec2(1.0, 0.0), vec2(16.0, 16.0)), Icon::Play, Color32::WHITE);
            if !audio && !tresp.dragged() {
                super::thumbs::scrub_preview(app, ui, thumb, thumb, &e.media.path, e.media.mtime, e.media.duration_sec.unwrap_or(0.0));
            }
        }
        // 影片滑過時有預覽，不另外顯示「播放」提示（會擋住預覽）
        let tresp = if audio { tresp.on_hover_text(tr!("播放", "Play")) } else { tresp };
        if tresp.clicked() {
            if ctrl || shift {
                d.pick(&e.media.path, &order, shift);
            } else {
                *req = Some(Req::Entry(EntryAction::Play, e.clone()));
            }
        }
        {
            let cell = Rect::from_min_max(pos2(thumb.max.x + 10.0, row.min.y), pos2(name_rect.max.x - 8.0, name_rect.max.y));
            // 先排右邊的標記，檔名用剩下的寬度（太長時截斷，不會擠掉標記）
            let tui = &mut ui.new_child(UiBuilder::new().max_rect(cell).layout(Layout::right_to_left(Align::Center)));
            // 星號（最右邊，位置固定）
            let (sr, _) = tui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            star_button(tui, sr, Id::new(("vidstar", &e.media.path)), app.core.pins.contains(&e.media.path), hot, false, req, &e.media.path);
            // 大部分錄影都有聲音，只標出「沒有聲音」的；位置固定保留，星號才會切齊
            if e.media.has_audio == Some(false) && e.media.duration_sec.is_some() && !audio {
                no_audio_mark(tui);
            } else {
                tui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            }
            if fresh {
                if Btn::new(tr!("播放", "Play")).icon(Icon::Play).primary().small().show(tui).clicked() {
                    *req = Some(Req::Entry(EntryAction::Play, e.clone()));
                }
                chip(tui, tr!("新的", "New"), Tone::Accent, false);
            }
            let tags_w = tui.min_rect().width();
            let left = Rect::from_min_max(cell.min, pos2(cell.max.x - tags_w - 6.0, cell.max.y));
            let nui = &mut ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)));
            nui.spacing_mut().item_spacing.x = 0.0;
            let (tr_rect, _) = nui.allocate_exact_size(vec2(time_w, row.height()), Sense::hover());
            let time = time.unwrap_or_default();
            let (hm, sec) = time.split_at(time.len().min(5));
            painter.text(pos2(tr_rect.min.x, row.center().y), egui::Align2::LEFT_CENTER, hm, theme::font_bold(13.0), p.text);
            if !sec.is_empty() {
                painter.text(pos2(tr_rect.min.x + hm_w + 1.0, row.center().y + 1.0), egui::Align2::LEFT_CENTER, sec, theme::font(11.0), p.muted);
            }
            nui.add_space((name_x - nui.cursor().min.x).max(0.0));
            // 格式、剪輯版等標記緊接在檔名後面（不要隔一大段空白）；先量好寬度，檔名太長時截斷
            let mut tags: Vec<(String, Tone, Option<&str>)> = Vec::new();
            if screenrecorder_core::import::is_other_video(&e.media.name) {
                tags.push((
                    screenrecorder_core::import::format_label(&e.media.name),
                    Tone::Plain,
                    Some(tr!(
                        "可以播放、合併；要剪輯、製作加速版等，請在「⋯」選「轉成 MP4」（原本的檔案會保留）",
                        "You can play and merge it. To edit it or make a sped-up version, choose “Convert to MP4” under “⋯” (the original is kept)"
                    )),
                ));
            }
            if is_cut_name(&e.media.name) {
                tags.push((tr!("剪輯版", "Edited").into(), Tone::Warn, None));
            }
            if audio {
                tags.push((
                    tr!("只有聲音", "Audio only").into(),
                    Tone::Plain,
                    Some(tr!("存成 M4A 的聲音檔（可以在這裡播放，或在「⋯」用 Windows 開啟）", "Audio saved as M4A (play it here, or open it in Windows from “⋯”)")),
                ));
            }
            let chips_w: f32 = tags.iter().map(|(t, _, _)| ui.fonts_mut(|f| f.layout_no_wrap(t.clone(), theme::font_bold(11.5), p.text).size().x) + 14.0 + 6.0).sum();
            // 自己取的名稱用一般字色，預設檔名（Rec_日期_時間）用灰色
            let custom = !is_default_name(&e.media.name);
            let name = RichText::new(&e.media.name).font(theme::font(if custom { 13.0 } else { 12.5 })).color(if custom { p.text } else { p.muted });
            let tip = if audio { e.media.name.clone() } else { trf!("{}\n按兩下改名（F2）", "{}\nDouble-click to rename (F2)", e.media.name) };
            if !e.media.folder.is_empty() {
                nui.spacing_mut().item_spacing.x = 6.0;
                folder_chip(nui, &e.media.folder);
            }
            let room = (nui.available_width() - chips_w).max(40.0);
            let want = nui.fonts_mut(|f| f.layout_no_wrap(e.media.name.clone(), theme::font(if custom { 13.0 } else { 12.5 }), p.text).size().x) + 1.0;
            let r = nui
                .allocate_ui_with_layout(vec2(want.min(room), row.height()), Layout::left_to_right(Align::Center), |ui| ui.add(egui::Label::new(name).truncate().sense(Sense::click())))
                .inner
                .on_hover_text(tip);
            nui.spacing_mut().item_spacing.x = 6.0;
            for (t, tone, hint) in &tags {
                nui.add_space(6.0);
                let c = chip(nui, t, *tone, false);
                if let Some(h) = hint {
                    c.on_hover_text(*h);
                }
            }
            if r.double_clicked() && !audio {
                *req = Some(Req::Rename(e.clone()));
            } else if r.clicked() && (ctrl || shift) {
                d.pick(&e.media.path, &order, shift);
            }
        }
        num(&e.media.duration_sec.map(short_clock).unwrap_or_else(|| "—".into()), col_x(4) - 10.0, row.center().y, p.text, 13.0);
        // 畫面大小用白話（4K 直、1080p 橫），滑鼠移上去看實際像素
        match (e.media.width, e.media.height) {
            (Some(w), Some(h)) if !audio => {
                let rr = Rect::from_min_max(pos2(col_x(4), row.min.y + 8.0), pos2(col_x(5) - 6.0, row.max.y - 8.0));
                let label = super::dialogs::res_label(w, h);
                let g = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), theme::font(12.5), p.muted));
                let tx = rr.max.x - 4.0 - g.size().x;
                painter.galley(pos2(tx, row.center().y - g.size().y / 2.0), g, p.muted);
                // 直／橫的小框
                let (iw, ih) = if w < h {
                    (7.0, 11.0)
                } else if w > h {
                    (11.0, 7.0)
                } else {
                    (9.0, 9.0)
                };
                let ir = Rect::from_center_size(pos2(tx - 5.0 - iw / 2.0, row.center().y), vec2(iw, ih));
                painter.rect_stroke(ir, CornerRadius::same(1), Stroke::new(1.2, p.muted), egui::StrokeKind::Inside);
                ui.interact(rr, Id::new(("libres", &e.media.path)), Sense::hover()).on_hover_text(format!("{w}×{h}"));
            }
            _ => num("—", col_x(5) - 10.0, row.center().y, p.muted, 12.5),
        }
        num(&format_bytes(e.media.bytes), col_x(6) - 10.0, row.center().y, p.text, 13.0);
        {
            // 播放、剪輯、製作、傳到媒體庫直接按得到，其他在「⋯」（和主畫面卡片、檢視器同一份清單）
            let aui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(col_x(6), row.min.y), row.max)).layout(Layout::left_to_right(Align::Center)));
            aui.spacing_mut().item_spacing.x = 2.0;
            if let Some(a) = super::entry_menu::quick_icons(aui, &groups, hot, tr!("更多（也可以在這一列按右鍵）", "More (or right-click the row)")) {
                *req = Some(Req::Entry(a, e.clone()));
            }
        }
        y += LIBRARY_ROW_MAIN_PX as f32;
        // 加速版 / GIF 子列
        for x in &e.exports {
            let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_SUB_PX as f32));
            let sresp = ui.interact(row.shrink2(vec2(0.0, 1.0)), Id::new(("libsub", &x.media.path)), Sense::click());
            let on = d.selected.contains(&x.media.path);
            if on {
                painter.rect_filled(row, CornerRadius::same(4), p.accent_soft);
            } else if sresp.hovered() {
                painter.rect_filled(row, CornerRadius::same(4), p.surface2.gamma_multiply(0.6));
            }
            let entry = LibraryEntry { media: x.media.clone(), exports: vec![], hits: vec![] };
            let sub_groups = super::entry_menu::groups(app, &entry, Place::Library);
            sresp.context_menu(|ui| {
                if let Some(a) = super::entry_menu::menu(ui, &sub_groups) {
                    *req = Some(Req::Entry(a, entry.clone()));
                }
            });
            painter.hline(row.x_range(), row.max.y, Stroke::new(1.0, p.border.gamma_multiply(0.6)));
            let sui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
            sui.add_space(10.0);
            let mut on2 = on;
            if theme::check(sui, &mut on2, "").changed() {
                if on2 {
                    d.selected.insert(x.media.path.clone());
                } else {
                    d.selected.remove(&x.media.path);
                }
            }
            // 樹狀線
            let bx = col_x(2) + 30.0;
            painter.line_segment([pos2(bx, row.min.y - 6.0), pos2(bx, row.center().y)], Stroke::new(1.0, p.border_strong));
            painter.line_segment([pos2(bx, row.center().y), pos2(bx + 12.0, row.center().y)], Stroke::new(1.0, p.border_strong));
            {
                let nui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(bx + 18.0, row.min.y), pos2(col_x(3) - 8.0, row.max.y))).layout(Layout::left_to_right(Align::Center)));
                chip(nui, &export_label(x), Tone::Accent, false).on_hover_text(trf!("{}（{}×）", "{} ({}×)", x.media.name, speed_label(x.speed)));
                // 檔名和主列的檔名切齊（標籤太寬時就接在後面）
                let gap = name_x - nui.cursor().min.x;
                if gap > 0.0 {
                    nui.add_space(gap);
                }
                nui.add(egui::Label::new(RichText::new(&x.media.name).font(theme::font(12.0)).color(p.muted)).truncate());
            }
            num(&x.media.duration_sec.map(short_clock).unwrap_or_else(|| "—".into()), col_x(4) - 10.0, row.center().y, p.text, 12.5);
            let res = match (x.media.width, x.media.height) {
                (Some(w), Some(h)) => format!("{w}×{h}"),
                _ => "—".into(),
            };
            num(&res, col_x(5) - 10.0, row.center().y, p.muted, 12.0);
            num(&format_bytes(x.media.bytes), col_x(6) - 10.0, row.center().y, p.text, 12.5);
            // 與原片的按鈕對齊：播放 ·（不能剪輯、製作）· 傳到媒體庫 · ⋯
            let aui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(col_x(6), row.min.y), row.max)).layout(Layout::left_to_right(Align::Center)));
            aui.spacing_mut().item_spacing.x = 2.0;
            if let Some(a) = super::entry_menu::quick_icons(aui, &sub_groups, hot, tr!("更多（也可以在這一列按右鍵）", "More (or right-click the row)")) {
                *req = Some(Req::Entry(a, entry.clone()));
            }
            y += LIBRARY_ROW_SUB_PX as f32;
        }
        // 搜尋到的說過的話：時間與那一句（點一下從那裡播放）
        let shown = e.hits.len().min(LIBRARY_HITS_SHOWN);
        for (i, h) in e.hits.iter().take(shown).enumerate() {
            let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_SUB_PX as f32));
            let resp = ui.interact(row, Id::new(("libhit", &e.media.path, i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.hovered() {
                painter.rect_filled(row, CornerRadius::same(4), p.accent.gamma_multiply(0.08));
            }
            painter.hline(row.x_range(), row.max.y, Stroke::new(1.0, p.border.gamma_multiply(0.6)));
            let bx = col_x(2) + 30.0;
            painter.line_segment([pos2(bx, row.min.y - 6.0), pos2(bx, row.center().y)], Stroke::new(1.0, p.border_strong));
            painter.line_segment([pos2(bx, row.center().y), pos2(bx + 12.0, row.center().y)], Stroke::new(1.0, p.border_strong));
            let more = if i + 1 == shown && e.hits.len() > shown { trf!("　（還有 {} 句）", " ({} more)", e.hits.len() - shown) } else { String::new() };
            let mut job = egui::text::LayoutJob::default();
            job.append(&format!("{}  ", short_clock(h.start)), 0.0, egui::TextFormat::simple(theme::mono(12.5), p.accent));
            highlight(&mut job, &h.text, d.query.trim(), p.text, p.accent);
            job.append(&more, 0.0, egui::TextFormat::simple(theme::font(12.5), p.muted));
            job.wrap = egui::text::TextWrapping::truncate_at_width(col_x(6) - bx - 30.0);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(pos2(bx + 18.0, row.center().y - galley.size().y / 2.0), galley, p.text);
            if resp.clicked() {
                d.jump = Some((e.clone(), h.start));
            }
            y += LIBRARY_ROW_SUB_PX as f32;
        }
    }
    row_keys(app, ui, d, &rows, req);
}

/// 清單是空的：讀取中、資料夾讀不到（選別的資料夾）、還沒有檔案（開始錄影）、搜尋沒有結果（清除搜尋）
#[allow(clippy::too_many_arguments)]
fn empty_list(ui: &mut egui::Ui, d: &mut LibraryDialog, area: Rect, dir: &str, req: &mut Option<Req>, none: &str, no_match: &str, record: bool) {
    let open_dir = tr!("開啟資料夾", "Open folder");
    if d.loading {
        empty_state(ui, area, tr!("讀取中…", "Loading…"), &[]);
    } else if let Some(e) = d.error.clone() {
        // 看之前用過的資料夾時，可以回到目前的儲存位置
        let mut buttons = vec![(tr!("選擇資料夾…", "Choose folder…"), true), (open_dir, false)];
        if d.dir.is_some() {
            buttons.push((tr!("看目前的儲存位置", "View current save location"), false));
        }
        match empty_state(ui, area, &e, &buttons) {
            Some(0) => *req = Some(Req::ChooseDir),
            Some(1) => *req = Some(Req::OpenDir(dir.to_string())),
            Some(_) => *req = Some(Req::Browse(None)),
            None => {}
        }
    } else if d.data.as_ref().is_some_and(|x| x.total == 0) && d.query.trim().is_empty() && d.filter == LibraryFilter::All {
        let mut buttons = vec![(open_dir, false), (tr!("變更儲存位置…", "Change save location…"), false)];
        if record {
            buttons.insert(0, (tr!("開始錄影", "Start recording"), true));
        }
        match empty_state(ui, area, none, &buttons).map(|i| if record { i } else { i + 1 }) {
            Some(0) => *req = Some(Req::Record),
            Some(1) => *req = Some(Req::OpenDir(dir.to_string())),
            Some(_) => *req = Some(Req::ChooseDir),
            None => {}
        }
    } else if d.data.is_some() {
        let searching = !d.query.trim().is_empty();
        let buttons = if searching { vec![(tr!("清除搜尋", "Clear search"), false)] } else { vec![] };
        if empty_state(ui, area, no_match, &buttons).is_some() {
            d.query.clear();
            d.page = 1;
            d.dirty = true;
        }
    }
}

/// 一句話，搜尋的字用強調色、粗體
fn highlight(job: &mut egui::text::LayoutJob, text: &str, query: &str, color: Color32, accent: Color32) {
    let normal = egui::TextFormat::simple(theme::font(12.5), color);
    let strong = egui::TextFormat::simple(theme::font_bold(12.5), accent);
    let lower = text.to_lowercase();
    let q = query.to_lowercase();
    // 小寫後長度不變時才標示（中文、英文都是）；否則整句一般顯示
    if q.is_empty() || lower.len() != text.len() {
        job.append(text, 0.0, normal);
        return;
    }
    let mut at = 0;
    while let Some(i) = lower[at..].find(&q).map(|i| i + at) {
        job.append(&text[at..i], 0.0, normal.clone());
        job.append(&text[i..i + q.len()], 0.0, strong.clone());
        at = i + q.len();
    }
    job.append(&text[at..], 0.0, normal);
}

/// 「有聲音」：灰色小喇叭（滑鼠提示說明），不用彩色標籤
/// 在哪個子資料夾：灰色小標籤（只寫最後一層，滑鼠提示有完整的）；不在子資料夾時不顯示
pub fn folder_chip(ui: &mut egui::Ui, folder: &str) {
    if folder.is_empty() {
        return;
    }
    let last = folder.rsplit(['\\', '/']).next().unwrap_or(folder);
    theme::chip(ui, last, theme::Tone::Plain, false).on_hover_text(trf!("在子資料夾「{}」", "In subfolder “{}”", screenrecorder_core::facets::folder_label(folder)));
}

/// 沒有聲音：灰色喇叭加叉
pub fn no_audio_mark(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    theme::paint_icon(ui.painter(), r, Icon::Muted, theme::pal(ui).muted);
    resp.on_hover_text(tr!("沒有聲音", "No audio"))
}

pub fn audio_mark(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    theme::paint_icon(ui.painter(), r, Icon::Speaker, theme::pal(ui).muted);
    resp.on_hover_text(tr!("有聲音", "Has audio"))
}

/// 截圖：縮圖格（一頁放幾張依對話框大小），點縮圖開啟
/// 縮圖大小（0 小、1 中、2 大）：每格最小寬度
const GRID_MIN_W: [f32; 3] = [120.0, 210.0, 300.0];

fn shot_grid(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, area: Rect, dir: &str, req: &mut Option<Req>, size: &mut u8) {
    let p = theme::pal(ui);
    grid_zoom(ui, d, area, size);
    // 小縮圖只放名稱：點一下檢視、按右鍵看全部操作
    let small = *size == 0;
    let gap = if small { 10.0 } else { 14.0 };
    let cols = ((area.width() + gap) / (GRID_MIN_W[(*size as usize).min(2)] + gap)).floor().max(1.0);
    let cell_w = (area.width() - gap * (cols - 1.0)) / cols;
    let img_h = (cell_w * 9.0 / 16.0).round();
    let cell_h = img_h + if small { 30.0 } else { 64.0 };
    // 依日期分組：每天從新的一列開始，上面一列日期標題（含「選這一天」）
    let head_h = 30.0;
    let fit = GridFit { cols: cols as usize, height: area.height() as f64 + gap as f64, row_h: (cell_h + gap) as f64, head_h };
    if d.grid_fit != Some(fit) {
        // 換縮圖大小或視窗大小：停在原本這一頁第一張所在的那一頁
        if d.grid_fit.is_some() {
            d.page_anchor = d.data.as_ref().map(|x| x.start);
        }
        d.grid_fit = Some(fit);
        d.dirty = true;
    }
    let items = d.data.as_ref().map(|x| x.items.clone()).unwrap_or_default();
    let painter = ui.painter().clone();
    if items.is_empty() {
        let none = match app.keys.label(2) {
            k if k.is_empty() => tr!("還沒有截圖。按主畫面的「截圖」試試看。", "No screenshots yet. Try “Screenshot” in the main window.").to_string(),
            k => trf!("還沒有截圖。按主畫面的「截圖」或 {k} 試試看。", "No screenshots yet. Try “Screenshot” in the main window or press {k}."),
        };
        empty_list(ui, d, area, dir, req, &none, tr!("沒有符合條件的截圖。", "No matching screenshots."), false);
    }
    let dates = date_labels(items.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
    wheel_pages(ui, d, area);
    let order: Vec<String> = items.iter().map(|e| e.media.path.clone()).collect();
    let (ctrl, shift) = multi_mods(ui);
    select_all_key(app, ui, d, &order);
    // 每一筆的日期（由這一頁的分組得出）與位置；與 core 分頁用同一套排法
    let groups = d.data.as_ref().map(|x| x.groups.clone()).unwrap_or_default();
    let days: Vec<String> = (0..items.len()).map(|i| groups.iter().rev().find(|g| g.first <= i).map(|g| g.day.clone()).unwrap_or_default()).collect();
    let places = grid_layout(&days, fit);
    let cell_at = |i: usize| {
        let pl = places.get(i).copied().unwrap_or(GridPlace { page: 0, y: 0.0, col: i, head_y: None });
        Rect::from_min_size(pos2(area.min.x + pl.col as f32 * (cell_w + gap), area.min.y + pl.y as f32), vec2(cell_w, cell_h))
    };
    // 用滑鼠框選（先登記，縮圖、按鈕的點一下照樣優先；拖曳超過幾個像素才算框選）
    let band = band_select(ui, d, area, &order, &cell_at, img_h, ctrl);
    // 日期標題列（在框選之後登記，勾選框才點得到）
    day_headers(app, ui, d, area, &groups, &places);
    for (i, e) in items.iter().enumerate() {
        let cell = cell_at(i);
        let pinned_e = app.core.pins.contains(&e.media.path);
        let hot = ui.rect_contains_pointer(cell);
        if hot {
            d.cursor_row = Some(e.media.path.clone());
        }
        let on = d.selected.contains(&e.media.path);
        // 勾選的：格子底下淡淡的強調色
        if on {
            painter.rect_filled(cell.expand(4.0), CornerRadius::same(theme::RADIUS_SM + 2), p.accent_soft);
        }
        let img = Rect::from_min_size(cell.min, vec2(cell_w, img_h));
        let resp = ui.interact(img, Id::new(("shotcell", &e.media.path)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(trf!(
            "{}\n點一下檢視（Enter）、Ctrl＋點一下多選、按右鍵看更多\n拖到其他程式（LINE、Word、檔案總管…）可以直接傳過去",
            "{}\nClick to view (Enter), Ctrl+click to select; right-click for more\nDrag into other apps (chat, Word, File Explorer…) to share it",
            e.media.name
        ));
        resp.context_menu(|ui| shot_menu(ui, e, true, pinned_e, req));
        let border = if on {
            Stroke::new(2.0, p.accent)
        } else if resp.hovered() {
            Stroke::new(1.0, p.border_strong)
        } else {
            Stroke::new(1.0, p.border)
        };
        painter.rect_filled(img, CornerRadius::same(theme::RADIUS_SM), p.surface2);
        if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
            theme::paint_thumb(&painter.with_clip_rect(img.shrink(1.0)), img.shrink(4.0), &t);
        }
        painter.rect_stroke(img, CornerRadius::same(theme::RADIUS_SM), border, egui::StrokeKind::Inside);
        if resp.clicked() {
            if ctrl || shift {
                d.pick(&e.media.path, &order, shift);
            } else {
                *req = Some(Req::Entry(EntryAction::Play, e.clone()));
            }
        }
        // 左上角勾選（白底，在深色縮圖上也看得到）
        let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(24.0, 22.0));
        if on || resp.hovered() || ui.rect_contains_pointer(cb) {
            painter.rect_filled(cb, CornerRadius::same(5), p.surface.gamma_multiply(0.92));
        }
        {
            let cui = &mut ui.new_child(UiBuilder::new().max_rect(cb.shrink2(vec2(4.0, 1.0))));
            let mut v = on;
            if theme::check(cui, &mut v, "").changed() {
                if shift {
                    d.pick(&e.media.path, &order, true);
                } else {
                    d.toggle(&e.media.path, v);
                    d.anchor = Some(e.media.path.clone());
                }
            }
        }
        // 右上角星號
        let sr = Rect::from_min_size(pos2(img.max.x - 30.0, img.min.y + 6.0), vec2(24.0, 22.0));
        star_button(ui, sr, Id::new(("shotstar", &e.media.path)), pinned_e, hot, true, req, &e.media.path);
        // 搜尋時是因為圖裡的字找到的：左下角標出來
        if let Some(h) = e.hits.first() {
            let br = Rect::from_min_size(pos2(img.min.x + 6.0, img.max.y - 26.0), vec2(26.0, 20.0));
            painter.rect_filled(br, CornerRadius::same(5), p.accent);
            painter.text(br.center(), egui::Align2::CENTER_CENTER, tr!("字", "T"), theme::font_bold(12.0), p.accent_ink);
            ui.interact(br, Id::new(("ocrhit", &e.media.path)), Sense::hover()).on_hover_text(trf!("圖裡有「{}」", "The image contains “{}”", h.text));
        }
        // 名稱（預設檔名顯示日期時間）；中、大縮圖時右邊寫解析度・大小（下面一行留給按鈕，不會被擠掉）
        let title = if is_default_name(&e.media.name) { dates.get(&e.media.name).cloned().unwrap_or_default() } else { base_name(&e.media.name) };
        let mut tr = Rect::from_min_max(pos2(cell.min.x + 2.0, img.max.y + 6.0), pos2(cell.max.x, img.max.y + 28.0));
        if !small {
            let size = e.media.width.zip(e.media.height).map(|(w, h)| format!("{w}×{h}・")).unwrap_or_default();
            let info = format!("{size}{}", format_bytes(e.media.bytes));
            let g = painter.layout_no_wrap(info.clone(), theme::font(12.0), p.muted);
            // 標題至少留 70 點；放不下時只寫檔案大小
            let g = if tr.width() - g.size().x - 8.0 >= 70.0 { g } else { painter.layout_no_wrap(format_bytes(e.media.bytes), theme::font(12.0), p.muted) };
            let gw = g.size().x;
            painter.galley(pos2(tr.max.x - gw - 2.0, tr.center().y - g.size().y / 2.0), g, p.muted);
            ui.interact(Rect::from_min_max(pos2(tr.max.x - gw - 2.0, tr.min.y), tr.max), Id::new(("shotinfo", &e.media.path)), Sense::hover()).on_hover_text(&info);
            tr.max.x -= gw + 10.0;
        }
        {
            let tui = &mut ui.new_child(UiBuilder::new().max_rect(tr).layout(Layout::left_to_right(Align::Center)));
            tui.spacing_mut().item_spacing.x = 6.0;
            if !small {
                folder_chip(tui, &e.media.folder);
            }
            tui.add(egui::Label::new(RichText::new(title).font(theme::font_bold(if small { 12.0 } else { 13.0 }))).truncate().selectable(false));
        }
        if small {
            continue;
        }
        // 按鈕自己一行（靠右）
        let br = Rect::from_min_max(pos2(cell.min.x + 2.0, img.max.y + 30.0), cell.max);
        let bui = &mut ui.new_child(UiBuilder::new().max_rect(br).layout(Layout::right_to_left(Align::Center)));
        bui.scope(|ui| {
            // 由右往左排：畫面上是「檢視 · 編輯 · 資料夾 · 改名 · ⋯」（與全部錄影的順序相同）
            ui.spacing_mut().item_spacing.x = 2.0;
            let more = Btn::icon_only(Icon::More).ghost().small().quiet(!hot).tooltip(tr!("更多（也可以在圖上按右鍵）", "More (or right-click the image)")).show(ui);
            egui::Popup::menu(&more).show(|ui| shot_menu(ui, e, false, pinned_e, req));
            if Btn::icon_only(Icon::Rename).ghost().small().quiet(!hot).tooltip(tr!("重新命名（F2）", "Rename (F2)")).show(ui).clicked() {
                *req = Some(Req::Rename(e.clone()));
            }
            if Btn::icon_only(Icon::Folder).ghost().small().quiet(!hot).tooltip(tr!("在資料夾中顯示", "Show in folder")).show(ui).clicked() {
                *req = Some(Req::Entry(EntryAction::Reveal, e.clone()));
            }
            if Btn::icon_only(Icon::Edit)
                .ghost()
                .small()
                .quiet(!hot)
                .tooltip(tr!("編輯：標註、遮個資、裁切（另存一張）", "Edit: annotate, redact personal info, crop (saves a copy)"))
                .show(ui)
                .clicked()
            {
                *req = Some(Req::Entry(EntryAction::Edit, e.clone()));
            }
            if Btn::icon_only(Icon::Eye).ghost().small().quiet(!hot).tooltip(tr!("檢視（Enter）", "View (Enter)")).show(ui).clicked() {
                *req = Some(Req::Entry(EntryAction::Play, e.clone()));
            }
        });
    }
    if let Some(r) = band {
        painter.rect_filled(r, CornerRadius::same(2), p.accent.gamma_multiply(0.15));
        painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
    }
    row_keys(app, ui, d, &items, req);
}

/// 截圖格的日期標題列：「☐ 7/27（一）· 8 張」，勾選框選取或取消那一天全部（每一頁；有星號的不選）
fn day_headers(app: &UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, area: Rect, groups: &[DayGroup], places: &[GridPlace]) {
    let p = theme::pal(ui);
    let today = chrono::Local::now().date_naive();
    for g in groups {
        let Some(y) = places.get(g.first).and_then(|pl| pl.head_y) else { continue };
        let r = Rect::from_min_size(pos2(area.min.x, area.min.y + y as f32), vec2(area.width(), 26.0));
        let pick: Vec<&String> = g.paths.iter().filter(|x| !app.core.pins.contains(x)).collect();
        let all_on = !pick.is_empty() && pick.iter().all(|x| d.selected.contains(*x));
        let hui = &mut ui.new_child(UiBuilder::new().max_rect(r).layout(Layout::left_to_right(Align::Center)));
        hui.spacing_mut().item_spacing.x = 6.0;
        let mut v = all_on;
        if theme::check(hui, &mut v, "").on_hover_text(tr!("選取這一天全部（包括其他頁的；有星號的不選）", "Select this whole day, including other pages (starred files are skipped)")).changed()
        {
            for x in &pick {
                if v {
                    d.selected.insert((*x).clone());
                } else {
                    d.selected.remove(*x);
                }
            }
        }
        let label = chrono::NaiveDate::parse_from_str(&g.day, "%Y-%m-%d").map(|dd| day_label(dd, today)).unwrap_or_else(|_| tr!("日期不明", "Unknown date").into());
        hui.label(RichText::new(label).font(theme::font_bold(13.5)).color(p.text));
        let more = if g.continued { tr!("（接上一頁）", " (continued)") } else { "" };
        hui.label(theme::muted(hui, trf!("· {} 張{more}", "· {}{more}", g.total)));
        let line_x = hui.min_rect().max.x + 8.0;
        ui.painter().hline(line_x..=r.max.x, r.center().y, Stroke::new(1.0, p.border));
    }
}

/// 全部截圖用滑鼠框選：從任何地方（含縮圖上）按住左鍵拖曳，框到的縮圖都勾起來。
/// 直接拖曳：這一頁改成只勾框到的（其他頁的勾選保留）；按住 Ctrl 拖曳：加選。回傳正在框的範圍（畫出來用）
#[allow(clippy::too_many_arguments)]
fn band_select(ui: &egui::Ui, d: &mut LibraryDialog, area: Rect, order: &[String], cell_at: &dyn Fn(usize) -> Rect, img_h: f32, ctrl: bool) -> Option<Rect> {
    if d.overlay() || egui::Popup::is_any_open(ui.ctx()) {
        d.band_base = None;
        return None;
    }
    let resp = ui.interact(area, Id::new("shotBand"), Sense::drag());
    // 只感應拖曳的範圍一按下就算拖曳：要移動超過幾個像素才開始（否則點縮圖時會變成拖出檔案、點不到）
    if resp.drag_started_by(egui::PointerButton::Primary) {
        d.band_pending = true;
        d.band_base = None;
    }
    if !resp.dragged() {
        d.band_pending = false;
        d.band_base = None;
        return None;
    }
    let origin = ui.input(|i| i.pointer.press_origin());
    if d.band_pending {
        let moved = ui.input(|i| i.pointer.interact_pos()).zip(origin).is_some_and(|(p, o)| p.distance(o) > DRAG_DIST);
        if !moved {
            return None;
        }
        d.band_pending = false;
        // 從縮圖上開始拖：把檔案拖到其他程式（和檔案總管一樣；從空白處、檔名開始拖才是框選）
        let on_img = (0..order.len()).find(|&i| origin.is_some_and(|o| Rect::from_min_size(cell_at(i).min, vec2(cell_at(i).width(), img_h)).contains(o)));
        if let (Some(i), true, false) = (on_img, can_drag_out(), ctrl) {
            d.drag_out = Some(drag_list(d, &order[i]));
            return None;
        }
        let mut base = d.selected.clone();
        if !ctrl {
            for p in order {
                base.remove(p);
            }
        }
        d.band_base = Some(base);
    }
    let base = d.band_base.as_ref()?;
    let (Some(from), Some(to)) = (ui.input(|i| i.pointer.press_origin()), ui.input(|i| i.pointer.interact_pos())) else { return None };
    let r = Rect::from_two_pos(from, to).intersect(area.expand(2.0));
    let hits: Vec<&String> = order.iter().enumerate().filter(|(i, _)| cell_at(*i).intersects(r)).map(|(_, p)| p).collect();
    let mut sel = base.clone();
    sel.extend(hits.iter().map(|p| (*p).clone()));
    d.selected = sel;
    if let Some(last) = hits.last() {
        d.anchor = Some((*last).clone());
    }
    Some(r)
}

/// 從按下的地方移動超過這個距離才算拖曳（框選、拖出檔案）
const DRAG_DIST: f32 = 6.0;

/// 可以把檔案拖到其他程式（Windows；開發測試時 SCREENRECORDER_FAKE_DRAG=1 只記錄）
fn can_drag_out() -> bool {
    cfg!(windows) || (cfg!(debug_assertions) && std::env::var_os("SCREENRECORDER_FAKE_DRAG").is_some())
}

fn drag_sense() -> Sense {
    if can_drag_out() {
        Sense::click_and_drag()
    } else {
        Sense::click()
    }
}

/// 從這個檔案開始拖：它有勾選就拖全部勾選的，否則只拖它
fn drag_list(d: &LibraryDialog, path: &str) -> Vec<String> {
    if d.selected.contains(path) {
        let mut v: Vec<String> = d.selected.iter().cloned().collect();
        v.sort();
        v
    } else {
        vec![path.to_string()]
    }
}

/// 交給系統拖曳（放開滑鼠才回來）
fn drag_out(app: &mut UiApp, list: &[String]) {
    screenrecorder_core::info!("[拖曳] {} 個檔案", list.len());
    if !cfg!(windows) {
        return;
    }
    let r = screenrecorder_core::dragout::drag_files(list);
    app.release_pointer = true;
    // 放回自己的視窗時，不要當成拖進來匯入
    app.dragged_out = Some((list.to_vec(), std::time::Instant::now()));
    match r {
        Ok(true) => screenrecorder_core::info!("[拖曳] 已放下"),
        Ok(false) => {}
        Err(e) => app.toast(trf!("沒辦法拖曳：{e}", "Couldn't drag: {e}"), true),
    }
}

/// 有勾選時，下方操作按鈕的後面：取消選取、只看已選取、選取全部（每一頁）。
/// 放不下時收成「取消選取」＋「⋯」選單，再放不下只剩「⋯」。不放在搜尋列或另外一行：
/// 那裡出現、消失會讓清單整個上下跳動（清單變矮還會重新分頁），也可能疊在別的按鈕上
fn selection_links(ui: &mut egui::Ui, d: &mut LibraryDialog, shot: bool, pins: &PinStore) {
    let total = d.data.as_ref().map(|x| x.total).unwrap_or(0);
    let all_n = d.data.as_ref().map(|x| x.all_paths.len()).unwrap_or(0);
    let everything = all_n > 0 && d.data.as_ref().is_some_and(|x| x.all_paths.iter().all(|p| d.selected.contains(p)));
    // 選取全部（每一頁）：搭配搜尋，例如打「7/27」再選取全部
    let all_label =
        (!everything && !d.only_selected).then(|| if shot { trf!("選取全部 {total} 張", "Select all {total}") } else { trf!("選取全部 {all_n} 個檔案", "Select all {all_n} files") });
    let all_tip = tr!(
        "選取符合目前搜尋的全部（每一頁）；也可以在全選這一頁後再按一次 Ctrl+A",
        "Selects everything that matches the current search, on every page. Pressing Ctrl+A again after selecting this page does the same"
    );
    let clear_label = tr!("取消選取", "Clear selection");
    let only_label = if d.only_selected { tr!("顯示全部", "Show all") } else { tr!("只看已選取", "Show selected only") };
    let only_tip = tr!("只列出勾選的檔案（不管在哪一頁），刪除前可以再確認一次", "Lists only the selected files, whatever page they're on, so you can check them before deleting");
    let clear = Btn::new(clear_label).ghost().small();
    let only = Btn::new(only_label).ghost().small().selected(d.only_selected).tooltip(only_tip);
    let all = all_label.clone().map(|l| Btn::new(l).ghost().small().tooltip(all_tip));
    let gap = 4.0;
    let more_w = 28.0;
    let clear_w = clear.width(ui);
    let full_w = clear_w + gap + only.width(ui) + all.as_ref().map_or(0.0, |b| gap + b.width(ui));
    let avail = ui.available_width();
    let toggle_only = |d: &mut LibraryDialog| {
        d.only_selected = !d.only_selected;
        d.page = 1;
        d.dirty = true;
    };
    ui.spacing_mut().item_spacing.x = gap;
    // 收起來時：「⋯」選單裡的項目（menu_clear = 取消選取也放進選單）
    let menu = |ui: &mut egui::Ui, d: &mut LibraryDialog, menu_clear: bool| {
        let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("取消選取、只看已選取、選取全部", "Clear, show selected only, select all")).show(ui);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(200.0);
            if menu_clear && ui.button(clear_label).clicked() {
                d.selected.clear();
                d.anchor = None;
            }
            if ui.button(only_label).on_hover_text(only_tip).clicked() {
                toggle_only(d);
            }
            if let Some(l) = &all_label {
                if ui.button(l).on_hover_text(all_tip).clicked() {
                    d.select_everything(pins);
                }
            }
        });
    };
    if avail < clear_w + gap + more_w {
        menu(ui, d, true);
        return;
    }
    if clear.show(ui).clicked() {
        d.selected.clear();
        d.anchor = None;
    }
    if avail < full_w {
        menu(ui, d, false);
        return;
    }
    if only.show(ui).clicked() {
        toggle_only(d);
    }
    if let Some(b) = all {
        if b.show(ui).clicked() {
            d.select_everything(pins);
        }
    }
}

/// 星號（釘選）按鈕：有星號時一直顯示（黃色實心），沒有時 show 才顯示外框；點一下切換
#[allow(clippy::too_many_arguments)]
fn star_button(ui: &mut egui::Ui, rect: Rect, id: Id, pinned: bool, show: bool, bg: bool, req: &mut Option<Req>, path: &str) {
    if !pinned && !show {
        return;
    }
    let p = theme::pal(ui);
    let r = ui.interact(rect, id, Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if pinned {
        tr!("移除星號", "Remove star")
    } else {
        tr!("加上星號：重要的檔案，「選取全部」「全選本頁」與清理舊截圖都會略過", "Add a star: “Select all”, Ctrl+A and cleaning up old screenshots skip starred files")
    });
    if bg {
        ui.painter().rect_filled(rect, CornerRadius::same(5), p.surface.gamma_multiply(0.92));
    }
    let icon_rect = Rect::from_center_size(rect.center(), vec2(15.0, 15.0));
    let color = if pinned {
        Color32::from_rgb(0xF5, 0xB7, 0x00)
    } else if r.hovered() {
        p.text
    } else {
        p.muted
    };
    theme::paint_icon(ui.painter(), icon_rect, if pinned { Icon::StarOn } else { Icon::Star }, color);
    if r.clicked() {
        *req = Some(Req::Pin(path.to_string(), !pinned));
    }
}

/// 在縮圖上按住 Ctrl 轉滾輪：調整縮圖大小（往上放大、往下縮小）
fn grid_zoom(ui: &egui::Ui, d: &mut LibraryDialog, area: Rect, size: &mut u8) {
    if !ui.rect_contains_pointer(area) || egui::Popup::is_any_open(ui.ctx()) || d.overlay() {
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
    let next = if dy > 0.0 { size.saturating_add(1).min(2) } else { size.saturating_sub(1) };
    if next != *size {
        *size = next;
        d.wheel_at = Some(Instant::now());
    }
}

/// 一張截圖的操作（「⋯」與按右鍵的選單共用）；full：按右鍵的選單，最上面加上檢視、編輯、在資料夾中顯示、重新命名
fn shot_menu(ui: &mut egui::Ui, e: &LibraryEntry, full: bool, pinned: bool, req: &mut Option<Req>) {
    ui.set_min_width(210.0);
    let pick = |a: EntryAction, req: &mut Option<Req>| *req = Some(Req::Entry(a, e.clone()));
    if full {
        if ui.button(tr!("檢視", "View")).clicked() {
            pick(EntryAction::Play, req);
        }
        if ui.button(tr!("編輯…", "Edit…")).clicked() {
            pick(EntryAction::Edit, req);
        }
        if ui.button(tr!("在資料夾中顯示", "Show in folder")).clicked() {
            pick(EntryAction::Reveal, req);
        }
        if ui.button(tr!("重新命名…", "Rename…")).clicked() {
            *req = Some(Req::Rename(e.clone()));
        }
        ui.separator();
    }
    if ui.button(if pinned { tr!("移除星號", "Remove star") } else { tr!("加上星號", "Add star") }).clicked() {
        *req = Some(Req::Pin(e.media.path.clone(), !pinned));
    }
    if ui.button(tr!("複製圖片（貼到 LINE、Word）", "Copy image (paste into LINE or Word)")).clicked() {
        *req = Some(Req::CopyImage(e.media.path.clone()));
    }
    if ui.button(tr!("複製檔案（貼到資料夾）", "Copy file (paste into a folder)")).clicked() {
        pick(EntryAction::CopyFile, req);
    }
    ui.separator();
    if trash_item(ui) {
        *req = Some(Req::Trash(vec![e.media.path.clone()]));
    }
}

/// 清單裡找這個檔案的資訊（目前這一頁、或之前讀過的）；第二個值：是不是加速版
fn find_media(app: &UiApp, path: &str) -> Option<(MediaInfo, bool)> {
    let page = app.library.as_ref().and_then(|d| d.data.as_ref()).map(|x| x.items.iter()).into_iter().flatten();
    for e in page.chain(app.known.values()) {
        if e.media.path == path {
            return Some((e.media.clone(), false));
        }
        if let Some(x) = e.exports.iter().find(|x| x.media.path == path) {
            return Some((x.media.clone(), true));
        }
    }
    None
}

/// 開啟「合併成一支」：勾選的錄影依時間排好；加速版預設不放進去
pub(super) fn open_merge(app: &mut UiApp, paths: Vec<String>) {
    let mut items: Vec<(f64, MergeItem)> = paths
        .iter()
        .map(|p| {
            let (m, export) = find_media(app, p).unwrap_or_else(|| (MediaInfo { path: p.clone(), name: file_name(p), ..Default::default() }, false));
            let export = export || screenrecorder_core::format::parse_export_name(&m.name).is_some();
            let t = entry_time(&m.name, m.time_ms()).map(|t| t.and_utc().timestamp_millis() as f64).unwrap_or(m.time_ms());
            (t, MergeItem { path: p.clone(), when: short_date(&m.name, m.time_ms(), false), name: m.name.clone(), dur: m.duration_sec, export, on: !export })
        })
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let items: Vec<MergeItem> = items.into_iter().map(|(_, i)| i).collect();
    // 預設名稱：合併_10-11（第一支錄影的日期）
    let day = items.iter().find(|i| i.on).and_then(|i| find_media(app, &i.path)).and_then(|(m, _)| entry_time(&m.name, m.time_ms())).unwrap_or_else(|| chrono::Local::now().naive_local());
    let name = trf!("合併_{}", "Merged_{}", day.format("%m-%d"));
    if let Some(d) = &mut app.library {
        d.merge = Some(MergeDlg { items, name, quality: None, error: None, busy: false });
    }
}

/// 上下移動的小按鈕（第一個 / 最後一個停用）
fn move_buttons(ui: &mut egui::Ui, i: usize, n: usize) -> Option<(usize, usize)> {
    let mut swap = None;
    if Btn::new("↓").ghost().small().enabled(i + 1 < n).tooltip(tr!("往後移", "Move down")).show(ui).clicked() {
        swap = Some((i, i + 1));
    }
    if Btn::new("↑").ghost().small().enabled(i > 0).tooltip(tr!("往前移", "Move up")).show(ui).clicked() {
        swap = Some((i, i - 1));
    }
    swap
}

/// 「合併成一支」對話框（蓋在全部錄影上）
fn merge_dialog(app: &mut UiApp, ctx: &egui::Context, d: &mut LibraryDialog) {
    let Some(m) = &mut d.merge else { return };
    let mut close = false;
    let mut start = false;
    let modal = egui::Modal::new(Id::new("merge")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(520.0);
        ui.label(RichText::new(tr!("合併成一支", "Merge into one")).font(theme::font_bold(16.0)));
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(theme::muted(
                ui,
                tr!(
                    "依下面的順序接成一支（可用 ↑↓ 調整）；畫面尺寸以第一支為準，其他的縮放並補黑邊。加速版預設不放進去。",
                    "Joins them in this order (use ↑↓ to change it). The frame size follows the first one; others are scaled with black bars. Sped-up versions are left out by default."
                ),
            ))
            .wrap(),
        );
        ui.add_space(6.0);
        let n = m.items.len();
        let mut swap = None;
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 360.0).max(120.0)).show(ui, |ui| {
                let mut k = 0;
                for (i, it) in m.items.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.set_height(30.0);
                        theme::check(ui, &mut it.on, "");
                        let num = if it.on {
                            k += 1;
                            format!("{k}.")
                        } else {
                            "—".into()
                        };
                        ui.add_sized(vec2(22.0, 20.0), egui::Label::new(RichText::new(num).font(theme::font_bold(13.0)).color(if it.on { p.text } else { p.muted })));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if let Some(sw) = move_buttons(ui, i, n) {
                                swap = Some(sw);
                            }
                            ui.add_space(6.0);
                            ui.label(RichText::new(it.dur.map(short_clock).unwrap_or_else(|| "—".into())).font(theme::font(12.5)).color(p.muted));
                            if it.export {
                                chip(ui, tr!("加速版", "Sped-up"), Tone::Accent, false);
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.label(RichText::new(&it.when).font(theme::font_bold(13.0)).color(if it.on { p.text } else { p.muted }));
                                ui.add(egui::Label::new(RichText::new(&it.name).font(theme::font(12.5)).color(p.muted)).truncate()).on_hover_text(&it.name);
                            });
                        });
                    });
                }
            });
        });
        if let Some((a, b)) = swap {
            m.items.swap(a, b);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("名稱", "Name")).color(p.muted));
            let r = ui.add_enabled(!m.busy, egui::TextEdit::singleline(&mut m.name).desired_width(300.0));
            if r.changed() {
                m.error = None;
            }
            ui.label(RichText::new(".mp4").color(p.muted));
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("畫質", "Quality")).color(p.muted));
            let items: Vec<(Option<Quality>, &str)> = std::iter::once((None, tr!("原畫質", "Original"))).chain(Quality::ALL.iter().map(|q| (Some(*q), q.label()))).collect();
            theme::segmented(ui, &mut m.quality, &items, !m.busy);
        });
        let hint = match m.quality {
            None => tr!(
                "格式一樣的影片（同一支手機、同樣設定錄的）直接接起來：幾秒就好，畫質跟原檔一樣；格式不同時以高畫質重新壓縮",
                "Videos in the same format (same phone or same settings) are joined directly: takes seconds and keeps the original quality. Otherwise they're re-encoded in high quality"
            ),
            Some(q) => q.hint(),
        };
        ui.add(egui::Label::new(theme::muted(ui, hint)).wrap());
        if let Some(e) = &m.error {
            ui.label(RichText::new(e).color(p.rec));
        }
        ui.add_space(10.0);
        let count = m.items.iter().filter(|i| i.on).count();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if m.busy { tr!("正在開始…", "Starting…") } else { tr!("開始合併", "Start merging") };
            let tip = if count < 2 { tr!("至少要勾兩支", "Select at least two") } else { "" };
            if Btn::new(label).primary().enabled(count >= 2 && !m.busy).tooltip(tip).show(ui).clicked() {
                start = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().enabled(!m.busy).show(ui).clicked() {
                close = true;
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let total: f64 = m.items.iter().filter(|i| i.on).filter_map(|i| i.dur).sum();
                ui.label(theme::muted(ui, trf!("{count} 支・共 {}", "{count} clips · {} total", short_clock(total))));
            });
        });
    });
    if (modal.should_close() || close) && !m.busy {
        d.merge = None;
        return;
    }
    if !start {
        return;
    }
    let name = m.name.trim().trim_end_matches(".mp4").trim_end_matches(".MP4").to_string();
    if let Some(bad) = check_recording_name(&name) {
        m.error = Some(bad.to_string());
        return;
    }
    m.busy = true;
    let paths: Vec<String> = m.items.iter().filter(|i| i.on).map(|i| i.path.clone()).collect();
    let n = paths.len();
    let quality = m.quality;
    let core = app.core.clone();
    app.spawn(async move { actions::merge_start(&core, &paths, Some(&name), quality).await }, move |app, r| {
        let Some(d) = &mut app.library else { return };
        match r {
            Ok(id) => {
                d.merge = None;
                d.merge_job = Some(id);
                d.selected.clear();
                app.dismissed_job = None;
                app.toast(trf!("開始合併 {n} 支錄影，完成後會排在清單最上面", "Merging {n} recordings. The result will appear at the top of the list"), false);
            }
            Err(e) => {
                if let Some(m) = &mut d.merge {
                    m.busy = false;
                    m.error = Some(e.message().to_string());
                }
            }
        }
    });
}

/// 合併完成：成品排在清單最上面（最新的在前）並標示「新的」
fn watch_merge(app: &UiApp, d: &mut LibraryDialog) {
    let Some(id) = d.merge_job else { return };
    let Some(st) = app.status.export.as_ref().filter(|s| s.id == id) else { return };
    match st.state {
        ExportState::Running => {}
        ExportState::Done => {
            d.merge_job = None;
            d.fresh = Some(st.output.clone());
            d.kind = Kind::Video;
            d.sort = LibrarySort::New;
            d.filter = LibraryFilter::All;
            d.query.clear();
            d.page = 1;
            d.dirty = true;
        }
        _ => d.merge_job = None,
    }
}

/// 開啟「拼成一張」：勾選的截圖依時間排好（由舊到新）
fn open_combine(app: &mut UiApp) {
    let Some(d) = &app.library else { return };
    let mut items: Vec<LibraryEntry> =
        d.selected.iter().filter(|p| super::dialogs::is_image(p)).filter_map(|p| find_media(app, p)).map(|(m, _)| LibraryEntry { media: m, exports: vec![], hits: vec![] }).collect();
    // 依截圖的時間（檔名裡的時間，沒有時用檔案時間）由舊到新
    items.sort_by_key(|e| entry_time(&e.media.name, e.media.time_ms()));
    if items.len() > 20 {
        return app.toast(tr!("一次最多拼 20 張", "You can combine up to 20 screenshots at a time"), true);
    }
    if let Some(d) = &mut app.library {
        d.combine = Some(CombineDlg { items, vertical: false, spacing: 1, busy: false });
    }
}

/// 比對勾選的兩張截圖（較舊的當作「之前」）：做好後開啟結果
fn compare_selected(app: &mut UiApp) {
    let Some(d) = &app.library else { return };
    let mut items: Vec<_> = d.selected.iter().filter(|p| super::dialogs::is_image(p)).filter_map(|p| find_media(app, p)).map(|(m, _)| m).collect();
    if items.len() != 2 {
        return app.toast(tr!("請勾選兩張截圖", "Select two screenshots"), true);
    }
    items.sort_by_key(|m| entry_time(&m.name, m.time_ms()));
    let (before, after) = (items[0].path.clone(), items[1].path.clone());
    let core = app.core.clone();
    let dir = app.s.shots_dir(&app.env);
    app.toast(tr!("正在比較…", "Comparing…"), false);
    app.spawn(async move { core.compare_shots(before, after, &dir).await }, |app, r| match r {
        Ok((shot, n)) => {
            if let Some(d) = &mut app.library {
                d.selected.clear();
            }
            app.last_shot_seq = app.last_shot_seq.max(shot.seq);
            let msg = if n == 0 {
                tr!("兩張看起來一樣，沒有找到不一樣的地方", "The two look the same; no differences found").to_string()
            } else if screenrecorder_core::i18n::is_en() {
                format!("Found {n} difference{}, marked with red boxes on the newer screenshot", if n == 1 { "" } else { "s" })
            } else {
                format!("找到 {n} 處不一樣，已在較新的那張上用紅框標出")
            };
            let (p1, p2) = (shot.path.clone(), shot.path.clone());
            app.toast_with(
                msg,
                false,
                vec![
                    ToastAction::new(tr!("看結果", "View"), move |app: &mut UiApp| super::editor::shot::open(app, p1)),
                    ToastAction::new(tr!("在資料夾中顯示", "Show in folder"), move |app: &mut UiApp| {
                        if let Err(e) = actions::open(actions::OpenAction::Reveal, &p2) {
                            app.toast(e.message().to_string(), true);
                        }
                    }),
                ],
            );
            app.shots_changed();
        }
        Err(e) => app.toast(e.message().to_string(), true),
    });
}

/// 清理舊截圖：選天數，看會刪幾張、多大，確認後移到資源回收筒（有星號的不刪）
fn cleanup_dialog(app: &mut UiApp, ctx: &egui::Context, d: &mut LibraryDialog) {
    let Some(c) = &mut d.cleanup else { return };
    // 天數改了（或剛打開）：重新查
    let want = c.days as u64 + 1;
    if c.seq != want {
        c.seq = want;
        c.list = None;
        let dirs = shot_dirs(app, &d.dir);
        let (core, days) = (app.core.clone(), c.days);
        app.spawn(async move { actions::old_shots(&core, dirs, days).await }, move |app, list| {
            let pins = app.core.pins.clone();
            if let Some(c) = app.library.as_mut().and_then(|d| d.cleanup.as_mut()).filter(|c| c.seq == want) {
                c.list = Some(list.into_iter().filter(|m| !pins.contains(&m.path)).collect());
            }
        });
    }
    let Some(c) = &mut d.cleanup else { return };
    let mut close = false;
    let mut go = false;
    let modal = egui::Modal::new(Id::new("cleanup")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(460.0);
        ui.label(RichText::new(tr!("清理舊截圖", "Clean up old screenshots")).font(theme::font_bold(16.0)));
        ui.add_space(6.0);
        ui.label(tr!("把超過這段時間的截圖移到資源回收筒（可以還原；有星號的不會動）：", "Move screenshots older than this to the Recycle Bin (you can restore them; starred ones are kept):"));
        ui.add_space(6.0);
        let mut days = c.days;
        theme::segmented(
            ui,
            &mut days,
            &[(7, tr!("一週", "1 week")), (30, tr!("一個月", "1 month")), (90, tr!("三個月", "3 months")), (180, tr!("半年", "6 months")), (365, tr!("一年", "1 year"))],
            !c.busy,
        );
        c.days = days;
        ui.add_space(10.0);
        let (n, bytes) = c.list.as_ref().map(|l| (l.len(), l.iter().map(|m| m.bytes).sum::<u64>())).unwrap_or((0, 0));
        match &c.list {
            None => {
                ui.label(theme::muted(ui, tr!("正在找…", "Looking…")));
            }
            Some(l) if l.is_empty() => {
                ui.label(theme::muted(ui, tr!("沒有超過這段時間的截圖。", "No screenshots are that old.")));
            }
            Some(l) => {
                let span = |m: &MediaInfo| entry_time(&m.name, m.time_ms()).map(|t| t.format("%Y/%m/%d").to_string()).unwrap_or_default();
                ui.label(RichText::new(trf!("{n} 張，共 {}", "{n} screenshots, {} in total", format_bytes(bytes))).font(theme::font_bold(14.0)).color(p.text));
                ui.label(theme::muted(ui, trf!("最舊 {}，最新 {}", "Oldest {}, newest {}", span(&l[0]), span(&l[l.len() - 1]))));
            }
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label =
                if c.busy { tr!("正在移到資源回收筒…", "Moving to the Recycle Bin…").to_string() } else { trf!("移到資源回收筒（{n} 張）", "Move {n} to the Recycle Bin") };
            if Btn::new(label).danger().enabled(!c.busy && n > 0).show(ui).clicked() {
                go = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().enabled(!c.busy).show(ui).clicked() {
                close = true;
            }
        });
    });
    if (modal.should_close() || close) && !c.busy {
        d.cleanup = None;
        return;
    }
    if !go {
        return;
    }
    c.busy = true;
    let paths: Vec<String> = c.list.as_ref().map(|l| l.iter().map(|m| m.path.clone()).collect()).unwrap_or_default();
    let core = app.core.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || actions::delete(&core, &paths)).await.unwrap_or_else(|e| Err(screenrecorder_core::error::Error::other(e.to_string()))) }, |app, r| {
        match r {
            Ok(n) => app.toast(trf!("已將 {n} 張舊截圖移到資源回收筒", "Moved {n} old screenshots to the Recycle Bin"), false),
            Err(e) => app.toast(e.message().to_string(), true),
        }
        if let Some(d) = &mut app.library {
            d.cleanup = None;
            d.reload();
        }
        app.load_recent();
    });
}

/// 這次要看的截圖資料夾：選了某個資料夾就只看那個，否則截圖資料夾和其他會放截圖的資料夾
fn shot_dirs(app: &UiApp, dir: &Option<String>) -> Vec<String> {
    match dir {
        Some(dir) => vec![dir.clone()],
        None => std::iter::once(app.s.shots_dir(&app.env)).chain(app.s.shot_extra(&app.env)).collect(),
    }
}

/// 找出相似的截圖：選靈敏度，一組一列；點縮圖切換「保留／刪除」，確認後移到資源回收筒
fn similar_dialog(app: &mut UiApp, ctx: &egui::Context, d: &mut LibraryDialog) {
    let Some(c) = &mut d.similar else { return };
    let want = c.level as u64 + 1;
    if c.seq != want {
        c.seq = want;
        c.groups = None;
        let (core, level, dirs) = (app.core.clone(), c.level, shot_dirs(app, &d.dir));
        app.spawn(async move { actions::similar_shots(&core, dirs, level).await }, move |app, groups| {
            let pins = app.core.pins.clone();
            if let Some(c) = app.library.as_mut().and_then(|d| d.similar.as_mut()).filter(|c| c.seq == want) {
                // 每組留最新的一張和有星號的
                c.remove = groups.iter().flat_map(|g| g.iter().skip(1)).filter(|m| !pins.contains(&m.path)).map(|m| m.path.clone()).collect();
                c.groups = Some(groups);
            }
        });
    }
    let pins = app.core.pins.clone();
    let thumbs: Vec<Vec<Option<egui::TextureHandle>>> = match d.similar.as_ref().and_then(|c| c.groups.as_ref()) {
        Some(g) => g.iter().map(|g| g.iter().map(|m| app.thumb(&m.path, m.mtime)).collect()).collect(),
        None => vec![],
    };
    let Some(c) = &mut d.similar else { return };
    let mut close = false;
    let mut go = false;
    let modal = egui::Modal::new(Id::new("similar")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(640.0);
        ui.label(RichText::new(tr!("找出相似的截圖", "Find similar screenshots")).font(theme::font_bold(16.0)));
        ui.add_space(6.0);
        ui.label(tr!(
            "長得一樣或很像的截圖排在同一組。每組預設留最新的一張（有星號的也會留），點縮圖可以切換保留或刪除。",
            "Screenshots that look the same are grouped. The newest in each group is kept by default (starred ones too); click a thumbnail to switch between keep and delete."
        ));
        ui.add_space(6.0);
        let mut level = c.level;
        theme::segmented(ui, &mut level, &[(0u8, tr!("完全一樣", "Identical")), (1, tr!("幾乎一樣", "Nearly identical")), (2, tr!("相似", "Similar"))], !c.busy);
        c.level = level;
        ui.add_space(8.0);
        match &c.groups {
            None => {
                let (done, total) = (app.core.hashes.done.load(std::sync::atomic::Ordering::Relaxed), app.core.hashes.total.load(std::sync::atomic::Ordering::Relaxed));
                let text = if total > 0 && done < total {
                    trf!("正在比對… {done} / {total}（第一次比較久，之後會快很多）", "Comparing… {done} / {total} (the first time takes longer)")
                } else {
                    tr!("正在比對…", "Comparing…").to_string()
                };
                ui.label(theme::muted(ui, text));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
            Some(g) if g.is_empty() => {
                ui.label(theme::muted(ui, tr!("沒有找到相似的截圖。", "No similar screenshots found.")));
            }
            Some(groups) => {
                let (tw, th) = (128.0, 80.0);
                egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (gi, g) in groups.iter().enumerate() {
                        ui.label(theme::muted(ui, trf!("第 {} 組 · {} 張", "Group {} · {}", gi + 1, g.len())));
                        egui::ScrollArea::horizontal().id_salt(("simrow", gi)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (mi, m) in g.iter().enumerate() {
                                    let pinned = pins.contains(&m.path);
                                    let del = c.remove.contains(&m.path);
                                    let (r, resp) = ui.allocate_exact_size(vec2(tw, th + 20.0), if pinned || c.busy { Sense::hover() } else { Sense::click() });
                                    let img = Rect::from_min_size(r.min, vec2(tw, th));
                                    ui.painter().rect_filled(img, CornerRadius::same(4), p.border);
                                    if let Some(Some(t)) = thumbs.get(gi).and_then(|t| t.get(mi)) {
                                        theme::paint_thumb(ui.painter(), img.shrink(1.0), t);
                                    }
                                    if del {
                                        ui.painter().rect_filled(img, CornerRadius::same(4), Color32::from_black_alpha(140));
                                    }
                                    let col = if del { p.rec } else { p.ok };
                                    ui.painter().rect_stroke(img, CornerRadius::same(4), Stroke::new(if resp.hovered() { 3.0 } else { 2.0 }, col), egui::StrokeKind::Inside);
                                    let tag = if pinned {
                                        tr!("★ 保留", "★ Keep")
                                    } else if del {
                                        tr!("刪除", "Delete")
                                    } else {
                                        tr!("保留", "Keep")
                                    };
                                    let tr_ = Rect::from_min_size(img.min + vec2(4.0, 4.0), vec2(if pinned { 52.0 } else { 40.0 }, 18.0));
                                    ui.painter().rect_filled(tr_, CornerRadius::same(3), col);
                                    ui.painter().text(tr_.center(), egui::Align2::CENTER_CENTER, tag, theme::font_bold(11.0), Color32::WHITE);
                                    let when = entry_time(&m.name, m.time_ms()).map(|t| t.format("%m/%d %H:%M").to_string()).unwrap_or_default();
                                    ui.painter().text(pos2(img.min.x + 2.0, img.max.y + 10.0), egui::Align2::LEFT_CENTER, format!("{when} · {}", format_bytes(m.bytes)), theme::font(11.0), p.muted);
                                    let resp = resp.on_hover_text(&m.name);
                                    if resp.clicked() && !c.remove.remove(&m.path) {
                                        c.remove.insert(m.path.clone());
                                    }
                                }
                            });
                        });
                        ui.add_space(6.0);
                    }
                });
            }
        }
        ui.add_space(10.0);
        let all: Vec<&MediaInfo> = c.groups.iter().flatten().flatten().filter(|m| c.remove.contains(&m.path)).collect();
        let (n, bytes) = (all.len(), all.iter().map(|m| m.bytes).sum::<u64>());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if c.busy {
                tr!("正在移到資源回收筒…", "Moving to the Recycle Bin…").to_string()
            } else {
                trf!("移到資源回收筒（{n} 張，{}）", "Move {n} to the Recycle Bin ({})", format_bytes(bytes))
            };
            if Btn::new(label).danger().enabled(!c.busy && n > 0).show(ui).clicked() {
                go = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().enabled(!c.busy).show(ui).clicked() {
                close = true;
            }
        });
    });
    if (modal.should_close() || close) && !c.busy {
        d.similar = None;
        return;
    }
    if !go {
        return;
    }
    c.busy = true;
    let paths: Vec<String> = c.remove.iter().cloned().collect();
    let core = app.core.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || actions::delete(&core, &paths)).await.unwrap_or_else(|e| Err(screenrecorder_core::error::Error::other(e.to_string()))) }, |app, r| {
        match r {
            Ok(n) => app.toast(trf!("已將 {n} 張相似的截圖移到資源回收筒", "Moved {n} similar screenshots to the Recycle Bin"), false),
            Err(e) => app.toast(e.message().to_string(), true),
        }
        if let Some(d) = &mut app.library {
            d.similar = None;
            d.reload();
        }
        app.load_recent();
    });
}

/// 「拼成一張」對話框：上面是預覽，下面調整排法、間距與順序
fn combine_dialog(app: &mut UiApp, ctx: &egui::Context, d: &mut LibraryDialog) {
    let Some(c) = &mut d.combine else { return };
    let mut close = false;
    let mut start = false;
    let thumbs: Vec<Option<egui::TextureHandle>> = c.items.iter().map(|e| app.thumb(&e.media.path, e.media.mtime)).collect();
    let modal = egui::Modal::new(Id::new("combine")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(560.0);
        ui.label(RichText::new(tr!("拼成一張", "Combine into one")).font(theme::font_bold(16.0)));
        ui.add_space(6.0);
        // 預覽：依原圖大小等比例排好（間距、白底與成品相同比例）
        let box_h = (ctx.content_rect().height() - 470.0).clamp(140.0, 240.0);
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), box_h), Sense::hover());
        ui.painter().rect_filled(area, CornerRadius::same(theme::RADIUS_SM), p.surface2);
        let sizes: Vec<Vec2> = c.items.iter().map(|e| vec2(e.media.width.unwrap_or(1600) as f32, e.media.height.unwrap_or(900) as f32)).collect();
        let min_side = sizes.iter().map(|s| s.x.min(s.y)).fold(f32::INFINITY, f32::min);
        let auto = (min_side / 40.0).floor().clamp(8.0, 24.0);
        let gap = match c.spacing {
            0 => 0.0,
            1 => auto,
            _ => auto * 3.0,
        };
        let n = sizes.len() as f32;
        let (along, across) = if c.vertical {
            (sizes.iter().map(|s| s.y).sum::<f32>() + gap * (n + 1.0), sizes.iter().map(|s| s.x).fold(0.0, f32::max) + gap * 2.0)
        } else {
            (sizes.iter().map(|s| s.x).sum::<f32>() + gap * (n + 1.0), sizes.iter().map(|s| s.y).fold(0.0, f32::max) + gap * 2.0)
        };
        let full = if c.vertical { vec2(across, along) } else { vec2(along, across) };
        let k = ((area.width() - 16.0) / full.x).min((area.height() - 16.0) / full.y);
        let canvas = Rect::from_center_size(area.center(), full * k);
        ui.painter().rect_filled(canvas, CornerRadius::ZERO, Color32::WHITE);
        ui.painter().rect_stroke(canvas, CornerRadius::ZERO, Stroke::new(1.0, p.border), egui::StrokeKind::Outside);
        let mut pos = gap;
        for (s, t) in sizes.iter().zip(&thumbs) {
            let r = if c.vertical {
                Rect::from_min_size(canvas.min + vec2((full.x - s.x) / 2.0 * k, pos * k), *s * k)
            } else {
                Rect::from_min_size(canvas.min + vec2(pos * k, (full.y - s.y) / 2.0 * k), *s * k)
            };
            ui.painter().rect_filled(r, CornerRadius::ZERO, p.border);
            if let Some(t) = t {
                theme::paint_thumb(ui.painter(), r, t);
            }
            pos += if c.vertical { s.y } else { s.x } + gap;
        }
        ui.add_space(4.0);
        ui.label(theme::muted(ui, trf!("約 {:.0}×{:.0}", "About {:.0}×{:.0}", full.x, full.y)));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("排法", "Layout")).color(p.muted));
            theme::segmented(ui, &mut c.vertical, &[(false, tr!("左右排", "Side by side")), (true, tr!("上下排", "Top to bottom"))], !c.busy);
            ui.add_space(12.0);
            ui.label(RichText::new(tr!("間距", "Spacing")).color(p.muted));
            theme::segmented(ui, &mut c.spacing, &[(0u8, tr!("無", "None")), (1, tr!("小", "Small")), (2, tr!("大", "Large"))], !c.busy);
        });
        ui.add_space(6.0);
        // 順序
        let n = c.items.len();
        let mut swap = None;
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(egui::Margin::symmetric(10, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                let dates = date_labels(c.items.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
                for (i, e) in c.items.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.set_height(28.0);
                        ui.add_sized(vec2(22.0, 20.0), egui::Label::new(RichText::new(format!("{}.", i + 1)).font(theme::font_bold(13.0))));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if !c.busy {
                                if let Some(sw) = move_buttons(ui, i, n) {
                                    swap = Some(sw);
                                }
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let title = if is_default_name(&e.media.name) { dates.get(&e.media.name).cloned().unwrap_or_default() } else { base_name(&e.media.name) };
                                ui.add(egui::Label::new(RichText::new(title).font(theme::font(13.0))).truncate()).on_hover_text(&e.media.name);
                            });
                        });
                    });
                }
            });
        });
        if let Some((a, b)) = swap {
            c.items.swap(a, b);
        }
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if c.busy { tr!("正在拼接…", "Combining…") } else { tr!("拼成一張", "Combine") };
            if Btn::new(label).primary().enabled(!c.busy && n >= 2).show(ui).clicked() {
                start = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().enabled(!c.busy).show(ui).clicked() {
                close = true;
            }
        });
    });
    if (modal.should_close() || close) && !c.busy {
        d.combine = None;
        return;
    }
    if !start {
        return;
    }
    c.busy = true;
    let paths: Vec<String> = c.items.iter().map(|e| e.media.path.clone()).collect();
    let (vertical, spacing) = (c.vertical, c.spacing);
    let core = app.core.clone();
    let shot_dir = app.s.shots_dir(&app.env);
    app.spawn(async move { core.combine_shots(paths, vertical, spacing, &shot_dir).await }, |app, r| match r {
        Ok(shot) => {
            if let Some(d) = &mut app.library {
                d.combine = None;
                d.selected.clear();
            }
            // 狀態更新的「已截圖」不用再顯示一次
            app.last_shot_seq = app.last_shot_seq.max(shot.seq);
            let copied = if shot.copied { tr!("，並複製到剪貼簿", " and copied to the clipboard") } else { "" };
            let text = trf!("已拼成 {}（{}×{}）{copied}", "Combined into {} ({}×{}){copied}", file_name(&shot.path), shot.width, shot.height);
            let (p1, p2) = (shot.path.clone(), shot.path.clone());
            app.toast_with(
                text,
                false,
                vec![
                    ToastAction::new(tr!("編輯", "Edit"), move |app: &mut UiApp| super::editor::shot::open(app, p1)),
                    ToastAction::new(tr!("在資料夾中顯示", "Show in folder"), move |app: &mut UiApp| {
                        if let Err(e) = actions::open(actions::OpenAction::Reveal, &p2) {
                            app.toast(e.message().to_string(), true);
                        }
                    }),
                ],
            );
            app.shots_changed();
        }
        Err(e) => {
            if let Some(c) = app.library.as_mut().and_then(|d| d.combine.as_mut()) {
                c.busy = false;
            }
            app.toast(e.message().to_string(), true);
        }
    });
}

/// 改名：原片與底下的加速版一起改；送出時檢查衝突、使用中，錯誤留在對話框裡。
/// 同時在背景依內容想一個名稱（字幕、畫面上的字、在用的程式），想好顯示在輸入欄下面
pub fn rename_entry(app: &mut UiApp, entry: LibraryEntry) {
    let path = entry.media.path.clone();
    rename_dialog(app, entry);
    if let Some(a) = &mut app.ask {
        a.suggest_pending = true;
        a.suggest_key = path.clone();
    }
    let core = app.core.clone();
    let key = path.clone();
    app.spawn(async move { actions::suggest_name(&core, &path).await }, move |app, r: Option<String>| {
        if let Some(a) = app.ask.as_mut().filter(|a| a.suggest_key == key) {
            a.suggest_pending = false;
            a.suggest = r;
        }
    });
}

fn rename_dialog(app: &mut UiApp, entry: LibraryEntry) {
    let old = base_name(&entry.media.name);
    let n = entry.exports.len();
    let mut ask = Ask::input(tr!("重新命名", "Rename"), tr!("新名稱", "New name"), old.clone(), tr!("改名", "Rename"), move |app, value| {
        let name = value.trim().trim_end_matches(".mp4").trim_end_matches(".MP4").trim_end_matches(".png").trim_end_matches(".PNG").trim_end_matches(".jpg").trim_end_matches(".JPG").to_string();
        if name == old {
            app.ask = None;
            return;
        }
        if let Some(bad) = check_recording_name(&name) {
            if let Some(a) = &mut app.ask {
                a.error = Some(bad.to_string());
                a.on_ok = None;
            }
            // 重新允許送出
            rename_retry(app, entry.clone());
            return;
        }
        if let Some(a) = &mut app.ask {
            a.busy = true;
        }
        let core = app.core.clone();
        let path = entry.media.path.clone();
        let e2 = entry.clone();
        app.spawn(async move { actions::rename(&core, &path, &value).await }, move |app, r| match r {
            Ok(new_path) => {
                // 檢視器開著這個檔案：路徑跟著改
                super::viewer::renamed(app, &e2.media.path, &new_path);
                app.sync_renamed(renamed_pairs(&e2, &new_path));
                app.ask = None;
                app.toast(trf!("已改名為 {name}", "Renamed to {name}"), false);
                if let Some(d) = &mut app.library {
                    d.selected.remove(&e2.media.path);
                    for x in &e2.exports {
                        d.selected.remove(&x.media.path);
                    }
                    d.dirty = true;
                }
                app.load_recent();
            }
            Err(err) => {
                if let Some(a) = &mut app.ask {
                    a.busy = false;
                    a.error = Some(err.message().to_string());
                }
                rename_retry(app, e2);
            }
        });
    });
    if n > 0 {
        ask.message = Some(if is_en() {
            if n == 1 {
                "Its sped-up version will be renamed too.".to_string()
            } else {
                format!("Its {n} sped-up versions will be renamed too.")
            }
        } else {
            format!("底下的 {n} 個加速版會一起改名。")
        });
    }
    app.ask = Some(ask);
}

/// 改名前後的路徑（錄影和底下的加速版、GIF：名稱開頭跟著換），媒體庫那一份照這個改
fn renamed_pairs(e: &LibraryEntry, new_path: &str) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
    let new = std::path::PathBuf::from(new_path);
    let dir = new.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let (old_base, new_base) = (base_name(&e.media.name), base_name(&file_name(new_path)));
    let mut pairs = vec![(std::path::PathBuf::from(&e.media.path), new)];
    for x in &e.exports {
        if let Some(rest) = x.media.name.strip_prefix(&old_base) {
            pairs.push((std::path::PathBuf::from(&x.media.path), dir.join(format!("{new_base}{rest}"))));
        }
    }
    pairs
}

/// 改名失敗後：保留對話框（含輸入的文字與錯誤訊息），讓使用者再送出一次
fn rename_retry(app: &mut UiApp, entry: LibraryEntry) {
    let Some(a) = app.ask.take() else { return };
    let value = a.input.clone().unwrap_or_default();
    let error = a.error.clone();
    rename_dialog(app, entry);
    if let Some(n) = &mut app.ask {
        n.input = Some(value);
        n.error = error;
        (n.suggest, n.suggest_pending, n.suggest_key) = (a.suggest, a.suggest_pending, a.suggest_key);
    }
}

fn remove_selected(app: &mut UiApp) {
    let Some(d) = &app.library else { return };
    let list: Vec<String> = d.selected.iter().cloned().collect();
    // 原檔要刪、但取消勾選了部分加速版：提醒這些會保留
    let kept: usize =
        d.data.as_ref().map(|x| x.items.iter().filter(|e| d.selected.contains(&e.media.path)).map(|e| e.exports.iter().filter(|x| !d.selected.contains(&x.media.path)).count()).sum()).unwrap_or(0);
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let (title, message) = if is_en() {
        let kept_text = match kept {
            0 => String::new(),
            1 => "\nThe unselected sped-up version will be kept.".to_string(),
            k => format!("\nThe {k} unselected sped-up versions will be kept."),
        };
        let them = if list.len() == 1 { "it" } else { "them" };
        (format!("Move {} file{} to the Recycle Bin?", list.len(), plural(list.len())), format!("You can restore {them} from the Recycle Bin.{kept_text}"))
    } else {
        (format!("把 {} 個檔案移到資源回收筒？", list.len()), format!("可從資源回收筒還原。{}", if kept > 0 { format!("\n未勾選的 {kept} 個加速版會保留。") } else { String::new() }))
    };
    let mut list = list;
    list.sort_by_key(|p| file_name(p));
    app.confirm_trash(title, &message, list, trashed);
}

/// 加上 / 移除星號
pub fn set_star(app: &mut UiApp, path: &str, on: bool) {
    app.core.pins.set(path, on);
    // 加上星號就不要留在勾選裡（避免接著刪掉）；只看有星號時重新讀取
    if let Some(d) = &mut app.library {
        if on {
            d.selected.remove(path);
        }
        if d.only_pinned {
            d.dirty = true;
        }
    }
}

/// 一支錄影（連同底下的加速版、GIF）移到資源回收筒：先確認
pub fn trash_entry(app: &mut UiApp, e: &LibraryEntry) {
    trash_paths(app, trash_list(e));
}

/// 這些檔案（錄影連同底下的加速版、GIF）移到資源回收筒：先確認（媒體庫也有時問要不要一起刪）
fn trash_paths(app: &mut UiApp, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let n = paths.len() - 1;
    let title = match n {
        0 => tr!("把這個檔案移到資源回收筒？", "Move this file to the Recycle Bin?").to_string(),
        _ if is_en() => format!("Move this recording and its {n} sped-up version{} to the Recycle Bin?", if n == 1 { "" } else { "s" }),
        _ => format!("把這部錄影和底下的 {n} 個加速版移到資源回收筒？"),
    };
    app.confirm_trash(title, tr!("可從資源回收筒還原。", "You can restore files from the Recycle Bin."), paths, trashed);
}

/// 刪好之後：清單的勾選拿掉、重新讀取
fn trashed(app: &mut UiApp, paths: &[String], ok: bool) {
    if !ok {
        return;
    }
    if let Some(d) = &mut app.library {
        for p in paths {
            d.selected.remove(p);
        }
        d.dirty = true;
    }
    app.load_recent();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_number_buttons() {
        let show = |p, n| page_numbers(p, n).into_iter().map(|x| x.map(|v| v.to_string()).unwrap_or("…".into())).collect::<Vec<_>>().join(" ");
        assert_eq!(show(1, 5), "1 2 3 4 5");
        assert_eq!(show(1, 12), "1 2 3 4 … 12");
        assert_eq!(show(6, 12), "1 … 5 6 7 … 12");
        assert_eq!(show(12, 12), "1 … 9 10 11 12");
        assert_eq!(show(4, 12), "1 … 3 4 5 … 12");
    }
}
