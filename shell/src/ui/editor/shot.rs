//! 截圖編輯（剪輯視窗的截圖模式）：沒有時間軸；標註與裁切和剪輯影片相同，另有輸出設定（外框、陰影、大小）。
//! 存成 *_編輯.png（原圖保留）並複製到剪貼簿；編輯設定記在專案裡，之後開啟編輯過的圖可以從原圖重新套用並修改。

use super::{BannerInfo, Editor, Pl, Tab};
use crate::ui::dialogs::file_name;
use crate::ui::theme::{self, segmented, switch, Btn, Icon};
use crate::ui::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, CursorIcon, Id, Key, Layout, PointerButton, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions};
use screenrecorder_core::actions::{self, ProjectMatch, ShotProjectInfo};
use screenrecorder_core::annotate::{self, Ann, AnnKind};
use screenrecorder_core::edit::{normalize_crop, CropInput, EditSpec};
use screenrecorder_core::ocr;
use screenrecorder_core::picture;
use screenrecorder_core::player::Frame;
use screenrecorder_core::shot_edit::{self, ShotSpec, BACKGROUNDS, BORDER_COLORS, PADDINGS, RADII, SCALES};
use screenrecorder_core::types::{LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

/// 開啟時先用的畫面最大邊長（輸出用原圖；放大到看得出差別時再換成更清楚的畫面）
const PREVIEW_MAX: u32 = 2560;
/// 編輯時畫面最多幾個像素：很大的圖、很長的長截圖用小一點的倍率，套用馬賽克等才不會卡
const DISPLAY_MAX_PIXELS: f64 = 24_000_000.0;
/// 畫面切成這麼大的小塊交給顯示卡（顯示卡對一張圖的大小有上限，長截圖會超過）
const TILE: u32 = 2048;
/// 畫面小塊的取樣方式：縮小很多時（長截圖整張看）用 mipmap 才不會閃爍、出現鋸齒
const TILE_OPTIONS: TextureOptions = TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..TextureOptions::LINEAR };
/// 縮放的範圍（螢幕實際像素 ÷ 圖的像素）
const ZOOM_MIN: f32 = 0.05;
const ZOOM_MAX: f32 = 8.0;
/// 按「－」「＋」時依序跳到這些倍率
const ZOOM_STEPS: [f32; 14] = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0];
/// 開啟的序號：圖讀好時視窗已經關掉（或換了一張）就不套用
static OPEN_SEQ: AtomicU64 = AtomicU64::new(0);

/// 圖怎麼放在畫面上
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    /// 還沒決定：第一次畫的時候依圖的長寬決定（長截圖用「符合寬度」）
    Auto,
    /// 整張放進畫面（符合視窗）
    Whole,
    /// 寬度剛好，上下捲動（符合寬度）
    Width,
    /// 指定的倍率（View::k）
    Scale,
}

/// 縮放與捲動
struct View {
    fit: Fit,
    /// 圖的一個像素在畫面上是幾個點（每一格依 fit 重算）
    k: f32,
    /// 畫面中間是圖上的哪一點（像素，旋轉後的座標）
    c: (f64, f64),
    /// 拖曳移動畫面中：(按下的位置, 那時的 c)
    pan: Option<(Pos2, (f64, f64))>,
    /// 拖曳捲軸中：(按下的位置, 那時的 c)
    bar: Option<(Pos2, (f64, f64))>,
    /// 這一格畫面（舞台）的範圍
    outer: Rect,
    /// 顯示的範圍（圖上的像素 x, y, 寬, 高）：整張，或裁切後（不在「裁切與旋轉」分頁時只顯示保留的部分）
    region: (f64, f64, f64, f64),
}

impl Default for View {
    fn default() -> Self {
        View { fit: Fit::Auto, k: 1.0, c: (0.0, 0.0), pan: None, bar: None, outer: Rect::NOTHING, region: (0.0, 0.0, 0.0, 0.0) }
    }
}

/// 畫面（套用馬賽克 / 模糊後）切成的小塊：(在畫面裡的範圍（畫面像素）, 圖)
#[derive(Default)]
struct Tiles {
    key: u64,
    list: Vec<(Rect, TextureHandle)>,
}

/// 輸出設定（復原 / 重做也記這些）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotOpts {
    pub border: Option<String>,
    pub border_width: f64,
    pub shadow: bool,
    pub scale: u32,
    /// 整張圖順時針轉的角度（0 / 90 / 180 / 270）
    #[serde(default)]
    pub rotate: u32,
    #[serde(default)]
    pub radius: u32,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default = "default_padding")]
    pub padding: u32,
    /// 自訂輸出的寬度（像素）；None = 用 scale
    #[serde(default)]
    pub width: Option<u32>,
    /// 「儲存」存成 JPG（檔案較小）；否則 PNG
    #[serde(default)]
    pub jpg: bool,
}

fn default_padding() -> u32 {
    ShotSpec::default().padding
}

impl Default for ShotOpts {
    fn default() -> Self {
        let d = ShotSpec::default();
        Self { border: d.border, border_width: d.border_width, shadow: d.shadow, scale: d.scale, rotate: 0, radius: d.radius, background: d.background, padding: d.padding, width: None, jpg: false }
    }
}

pub struct Shot {
    /// 原圖（編輯過的圖從原圖重新套用）
    path: String,
    opts: ShotOpts,
    /// 開啟的那張編輯過的圖（可以改回直接編輯它）
    opened_edit: Option<String>,
    /// 載入了上次的標註：之前編輯成的那張圖（儲存時可以取代它，或另存一張新的）
    target: Option<String>,
    /// 「輸出」分頁的預覽：(內容, 圖)
    preview: Option<(String, TextureHandle)>,
    /// 還沒旋轉的畫面（顯示用的解析度，原圖的 res 倍）與原圖大小
    base: Frame,
    orig: (u32, u32),
    /// 目前 ed.frame 轉了幾度（和 opts.rotate 不同時重新轉）
    applied: u32,
    /// 原圖（RGBA）：放大時從這裡做更清楚的畫面
    full: Arc<Vec<u8>>,
    /// base 是原圖的幾倍（1 = 原尺寸）；開啟時的倍率（縮小時不低於這個）
    res: f64,
    res0: f64,
    /// 背景正在做的畫面：(倍率, 做好時收到)
    sharpen: Option<(f64, Receiver<Frame>)>,
    /// 圖還在讀（開啟的序號）：先顯示「正在開啟圖片…」
    loading: Option<u64>,
    view: View,
    tiles: Tiles,
    /// ed.frame 換過幾次（換了就重做小塊）
    gen: u64,
    /// 複製的標註：(放進剪貼簿的文字, 標註)；貼上時剪貼簿還是這段文字就貼這個標註
    clip: Option<(String, Ann)>,
    /// 單獨按下了 V（Ctrl+V 按下時 egui 收不到 V，只收到放開）
    v_down: bool,
}

impl Shot {
    pub fn opts(&self) -> ShotOpts {
        self.opts.clone()
    }
    pub fn set_opts(&mut self, o: ShotOpts) {
        self.opts = o;
    }
}

/// 需要 app 的動作（按鈕與快速鍵）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Save,
    Copy,
    Pin,
    Ocr,
    /// 不從原圖重新套用，直接編輯開啟的那張
    Plain,
    /// 有還沒儲存的修改：先問要不要放棄修改再切換到直接編輯
    AskPlain,
    /// 另存新檔（選位置與格式：PNG / JPG）
    SaveAs,
    /// 影片：目前這一格存成截圖
    Grab,
    /// 找出個資並打上馬賽克
    FindPii,
    /// 影片：找出沒動靜的片段並刪掉
    FindIdle,
    /// 選一張圖片檔加上去（replace = 換掉選取的圖片）
    PickPicture {
        replace: bool,
    },
    /// 加上剪貼簿裡的圖片
    PastePicture,
    /// 再加一次上次用的圖片
    LastPicture,
    /// 影片：自動產生字幕（語音辨識）
    AutoSubs,
    /// 影片：匯入 SRT 字幕檔
    ImportSrt,
    /// 停止產生字幕
    CancelSubs,
}

/// 開啟截圖編輯：編輯過的圖會從原圖重新套用上次的編輯
pub fn open(app: &mut UiApp, path: String) {
    open_with(app, path, true);
}

fn open_with(app: &mut UiApp, path: String, use_project: bool) {
    if app.editor.is_some() {
        return app.toast(tr!("編輯視窗已經開著，請先關閉", "The editor is already open; close it first"), true);
    }
    // 先開好視窗顯示「正在開啟圖片…」；讀圖、縮小都在背景做（大圖、長截圖要一點時間，介面不會卡住）
    let seq = OPEN_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    app.editor = Some(loading_editor(app, &path, seq));
    let core = app.core.clone();
    let p = path.clone();
    app.spawn(
        async move {
            let info = if use_project { actions::shot_project(&core, &p).await } else { None };
            // 開啟的是編輯過的圖、原圖還在：改用原圖，套用上次的編輯
            let src = match &info {
                Some(i) if i.matched == ProjectMatch::Output && i.source_ok => i.project.source.clone(),
                _ => p.clone(),
            };
            let img = match actions::load_image(&src).await {
                Ok((rgba, w, h)) => tokio::task::spawn_blocking(move || Loaded::new(rgba, w, h)).await.map_err(|e| e.to_string()),
                Err(e) => Err(e.message().to_string()),
            };
            (info, src, img)
        },
        move |app, (info, src, img)| {
            // 讀好時視窗已經關掉（或換了一張）：不用了
            if !app.editor.as_ref().and_then(|ed| ed.shot.as_ref()).is_some_and(|s| s.loading == Some(seq)) {
                return;
            }
            match img {
                Err(e) => {
                    app.editor = None;
                    app.toast(e, true);
                }
                Ok(l) => {
                    if let Some(ed) = &mut app.editor {
                        fill(ed, path, src, l, info);
                    }
                }
            }
        },
    );
}

/// 讀好的圖：原圖與開啟時用的畫面（在背景縮小好）
struct Loaded {
    full: Vec<u8>,
    w: u32,
    h: u32,
    res: f64,
    base: Frame,
}

impl Loaded {
    fn new(full: Vec<u8>, w: u32, h: u32) -> Loaded {
        let res = first_res(w, h);
        let (rgba, pw, ph) = shot_edit::scale_rgba(&full, w, h, res);
        Loaded { full, w, h, res, base: Frame { time: 0.0, width: pw, height: ph, rgba } }
    }
}

/// 長截圖（比畫面長很多）：開啟時用「符合寬度」
fn is_tall(w: f64, h: f64) -> bool {
    h > w * 2.2
}

/// 開啟時畫面用原圖的幾倍：長邊不超過 PREVIEW_MAX（長截圖看寬度）、總像素不超過 DISPLAY_MAX_PIXELS
fn first_res(w: u32, h: u32) -> f64 {
    let side = if is_tall(w as f64, h as f64) { w } else { w.max(h) };
    (PREVIEW_MAX as f64 / side.max(1) as f64).min(res_cap(w, h))
}

/// 畫面最多用原圖的幾倍（總像素的上限）
fn res_cap(w: u32, h: u32) -> f64 {
    (DISPLAY_MAX_PIXELS / (w as f64 * h as f64).max(1.0)).sqrt().min(1.0)
}

/// 圖還在讀的編輯視窗（只有檔名）
fn loading_editor(app: &UiApp, path: &str, seq: u64) -> Editor {
    let entry = LibraryEntry { media: MediaInfo { path: path.to_string(), name: file_name(path), ..Default::default() }, exports: vec![], hits: vec![] };
    let mut ed = Editor::base(app, entry, PathBuf::new(), Pl(None), Tab::Ann);
    ed.duration = 1.0;
    ed.fps = 1.0;
    ed.spec = EditSpec { start: 0.0, end: 1.0, removed: vec![], crop: None, overlays: vec![], ..Default::default() };
    ed.view = (0.0, 1.0);
    let empty = Frame { time: 0.0, width: 0, height: 0, rgba: vec![] };
    ed.shot = Some(Shot {
        path: path.to_string(),
        opts: ShotOpts::default(),
        opened_edit: None,
        target: None,
        preview: None,
        base: empty,
        orig: (0, 0),
        applied: 0,
        full: Arc::default(),
        res: 1.0,
        res0: 1.0,
        sharpen: None,
        loading: Some(seq),
        view: View::default(),
        tiles: Tiles::default(),
        gen: 0,
        clip: None,
        v_down: false,
    });
    ed
}

/// 圖讀好了：放進編輯視窗。標題顯示實際開啟的檔案（從原圖重新套用時也是）
fn fill(ed: &mut Editor, opened: String, src: String, l: Loaded, info: Option<ShotProjectInfo>) {
    let (w, h) = (l.w, l.h);
    ed.entry.media = MediaInfo { path: src.clone(), name: file_name(&opened), width: Some(w), height: Some(h), ..Default::default() };
    ed.vw = w as f64;
    ed.vh = h as f64;
    ed.frame = Some(l.base.clone());
    if let Some(s) = &mut ed.shot {
        (s.path, s.base, s.orig, s.full, s.res, s.res0, s.loading) = (src, l.base, (w, h), Arc::new(l.full), l.res, l.res, None);
        s.gen += 1;
    }
    match info {
        Some(i) if i.matched == ProjectMatch::Output && i.source_ok => {
            apply_spec(ed, &i.spec);
            ed.replace_target = Some(opened.clone());
            if let Some(s) = &mut ed.shot {
                s.opened_edit = Some(opened.clone());
                s.target = Some(opened);
            }
        }
        Some(i) if i.matched == ProjectMatch::Output => ed.banner = BannerInfo::MissingSource(file_name(&i.project.source)),
        Some(i) => ed.banner = BannerInfo::HasProject { output: i.project.output, data: i.project.data },
        None => {}
    }
    ed.mark_saved();
}

/// 圖還在讀：顯示和編輯視窗一樣大的框，中間是「正在開啟圖片…」；按 ✕ 或 Esc 取消。
/// 回傳 true = 還在讀（不顯示編輯畫面）
pub fn show_opening(ed: &mut Editor, ctx: &egui::Context) -> bool {
    if !ed.shot.as_ref().is_some_and(|s| s.loading.is_some()) {
        return false;
    }
    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 32.0).max(600.0), (screen.height() - 32.0).max(400.0));
    egui::Modal::new(Id::new("editor")).frame(theme::modal_frame(ctx).inner_margin(0)).show(ctx, |ui| {
        ui.set_width(size.x);
        ui.set_height(size.y);
        let p = theme::pal(ui);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(tr!("編輯截圖", "Edit screenshot")).font(theme::font_bold(16.0)));
                    ui.label(RichText::new(&ed.entry.media.name).font(theme::mono(12.0)).color(p.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("取消（Esc）", "Cancel (Esc)")).show(ui).clicked() {
                        ed.close = true;
                    }
                });
            });
        });
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top() - 4.0, Stroke::new(1.0, p.border));
        let rest = Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + (ui.available_height() - 40.0).max(80.0)));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rest).layout(Layout::centered_and_justified(egui::Direction::TopDown)), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((rest.height() / 2.0 - 30.0).max(0.0));
                ui.add(egui::Spinner::new().size(28.0).color(p.accent));
                ui.add_space(8.0);
                ui.label(RichText::new(tr!("正在開啟圖片…", "Opening image…")).color(p.muted));
            });
        });
    });
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        ed.close = true;
    }
    true
}

/// 套用存起來的編輯
fn apply_spec(ed: &mut Editor, spec: &ShotSpec) {
    ed.spec.crop = spec.crop;
    ed.crop_on = spec.crop.is_some();
    ed.anns = spec.anns.iter().filter(|a| [a.x, a.y, a.w, a.h, a.size].iter().all(|v| v.is_finite())).cloned().collect();
    for a in &mut ed.anns {
        a.id = ed.next_id;
        ed.next_id += 1;
        (a.start, a.end) = (0.0, 1.0);
        annotate::measure(a);
    }
    if let Some(s) = &mut ed.shot {
        s.opts = ShotOpts {
            border: spec.border.clone(),
            border_width: spec.border_width,
            shadow: spec.shadow,
            scale: spec.scale,
            rotate: shot_edit::norm_rotate(spec.rotate),
            radius: spec.radius,
            background: spec.background.clone(),
            padding: spec.padding,
            width: spec.width,
            jpg: false,
        };
    }
    sync_rotation(ed);
}

/// 畫面轉成 opts.rotate 的角度（旋轉、復原 / 重做之後）；ed.vw × ed.vh 是旋轉後的大小
pub fn sync_rotation(ed: &mut Editor) {
    let Some(s) = &mut ed.shot else { return };
    let deg = shot_edit::norm_rotate(s.opts.rotate);
    if deg == s.applied {
        return;
    }
    // 轉了方向：重新放進畫面（換成更清楚的畫面時 applied 是 u32::MAX，不動縮放）
    if s.applied != u32::MAX {
        s.view.fit = Fit::Auto;
    }
    let (rgba, w, h) = shot_edit::rotate_rgba(&s.base.rgba, s.base.width, s.base.height, deg);
    let (vw, vh) = shot_edit::rotated_size(s.orig.0, s.orig.1, deg);
    s.applied = deg;
    s.gen += 1;
    ed.frame = Some(Frame { time: 0.0, width: w, height: h, rgba });
    (ed.vw, ed.vh) = (vw as f64, vh as f64);
    ed.video_key = 0;
}

// ───────────── 縮放與捲動 ─────────────

/// 這個縮放方式的倍率（圖的一個像素是畫面上幾個點）
fn fit_k(fit: Fit, k: f32, outer: Rect, vw: f64, vh: f64) -> f32 {
    let (w, h) = (vw.max(1.0) as f32, vh.max(1.0) as f32);
    match fit {
        Fit::Auto | Fit::Whole => (outer.width() / w).min(outer.height() / h),
        Fit::Width => outer.width() / w,
        Fit::Scale => k,
    }
}

/// 圖比畫面小的方向置中；比畫面大的方向不要捲出圖外
fn clamp_center(v: &mut View, outer: Rect) {
    let (x, y, w, h) = v.region;
    let k = v.k.max(1e-4) as f64;
    let (hw, hh) = (outer.width() as f64 / 2.0 / k, outer.height() as f64 / 2.0 / k);
    v.c.0 = if w <= hw * 2.0 { x + w / 2.0 } else { v.c.0.clamp(x + hw, x + w - hw) };
    v.c.1 = if h <= hh * 2.0 { y + h / 2.0 } else { v.c.1.clamp(y + hh, y + h - hh) };
}

/// 要顯示的範圍：有裁切而且不在「裁切與旋轉」分頁時只顯示保留的部分（裁好馬上看到結果），否則整張
fn region_of(ed: &Editor) -> (f64, f64, f64, f64) {
    let (vw, vh) = (ed.vw, ed.vh);
    if ed.crop_on && ed.tab != super::Tab::Crop {
        if let Some(c) = ed.spec.crop {
            let x = c.x.clamp(0.0, vw);
            let y = c.y.clamp(0.0, vh);
            let (w, h) = (c.width.min(vw - x), c.height.min(vh - y));
            if w >= 8.0 && h >= 8.0 {
                return (x, y, w, h);
            }
        }
    }
    (0.0, 0.0, vw, vh)
}

/// 顯示的範圍在畫面上的位置（rect 是整張圖在畫面上的位置）
pub fn shown_rect(ed: &Editor, rect: Rect) -> Rect {
    let Some(s) = &ed.shot else { return rect };
    let (x, y, w, h) = s.view.region;
    if w <= 0.0 || h <= 0.0 || ed.vw <= 0.0 {
        return rect;
    }
    let k = rect.width() as f64 / ed.vw;
    Rect::from_min_size(rect.min + vec2((x * k) as f32, (y * k) as f32), vec2((w * k) as f32, (h * k) as f32))
}

/// 縮放到 k（點 / 像素），at（畫面上的點）下的那一點縮放後還在原處
fn zoom_to(v: &mut View, outer: Rect, k: f32, at: Pos2) {
    let d = at - outer.center();
    let q = (v.c.0 + (d.x / v.k) as f64, v.c.1 + (d.y / v.k) as f64);
    v.k = k;
    v.c = (q.0 - (d.x / k) as f64, q.1 - (d.y / k) as f64);
    v.fit = Fit::Scale;
}

/// 截圖在畫面上的位置：處理 Ctrl+滾輪縮放（以游標為中心）、滾輪捲動（Shift 左右）；
/// 放大到看得出差別時在背景換成更清楚的畫面。回傳整張圖在畫面上的範圍（放大時比 outer 大）
pub fn place(ed: &mut Editor, ui: &egui::Ui, outer: Rect) -> Rect {
    poll_sharpen(ed);
    let (vw, vh) = (ed.vw, ed.vh);
    if ed.shot.is_none() || vw <= 0.0 || vh <= 0.0 || outer.width() < 1.0 || outer.height() < 1.0 {
        return outer;
    }
    let ppp = ui.ctx().pixels_per_point();
    let region = region_of(ed);
    let Some(s) = &mut ed.shot else { return outer };
    let v = &mut s.view;
    v.outer = outer;
    // 顯示的範圍變了（裁好了、回到裁切分頁、取消裁切）：重新放進畫面
    if v.region != region {
        if v.region.2 > 0.0 {
            v.fit = Fit::Auto;
        }
        v.region = region;
    }
    let (rx, ry, rw, rh) = region;
    if v.fit == Fit::Auto {
        // 長截圖：寬度剛好、從最上面開始，上下捲動
        let tall = is_tall(rw, rh) && outer.width() / rw as f32 > fit_k(Fit::Whole, 1.0, outer, rw, rh) * 1.5;
        (v.fit, v.c) = if tall { (Fit::Width, (rx + rw / 2.0, ry)) } else { (Fit::Whole, (rx + rw / 2.0, ry + rh / 2.0)) };
    }
    v.k = fit_k(v.fit, v.k, outer, rw, rh);
    clamp_center(v, outer);
    // 游標在畫面上（不是在蓋住它的選單上）才處理滾輪
    if ui.rect_contains_pointer(outer) {
        let (zoom, wheel, shift, at) = ui.input(|i| (i.zoom_delta(), super::wheel_delta(i), i.modifiers.shift, i.pointer.hover_pos()));
        if zoom != 1.0 {
            let whole = fit_k(Fit::Whole, 1.0, outer, rw, rh);
            let k = (v.k * zoom).clamp((ZOOM_MIN / ppp).min(whole), ZOOM_MAX / ppp);
            zoom_to(v, outer, k, at.unwrap_or(outer.center()));
            // 縮回到整張剛好放得下：回到「符合視窗」
            if (k / whole - 1.0).abs() < 0.03 {
                v.fit = Fit::Whole;
            }
        } else if wheel != egui::Vec2::ZERO {
            let (dx, dy) = if shift && wheel.x == 0.0 { (wheel.y, 0.0) } else { (wheel.x, wheel.y) };
            v.c.0 -= (dx / v.k) as f64;
            v.c.1 -= (dy / v.k) as f64;
        }
        clamp_center(v, outer);
    }
    let k = v.k;
    let rect = Rect::from_min_size(outer.center() - vec2(v.c.0 as f32 * k, v.c.1 as f32 * k), vec2(vw as f32 * k, vh as f32 * k));
    sharpen(s, ui.ctx(), k * ppp);
    rect
}

/// 背景做好了更清楚（或比較小）的畫面：換上去
fn poll_sharpen(ed: &mut Editor) {
    let Some(s) = &mut ed.shot else { return };
    let Some((res, rx)) = &s.sharpen else { return };
    match rx.try_recv() {
        Ok(f) => {
            (s.res, s.base, s.sharpen) = (*res, f, None);
            s.applied = u32::MAX;
            sync_rotation(ed);
        }
        Err(TryRecvError::Disconnected) => s.sharpen = None,
        Err(TryRecvError::Empty) => {}
    }
}

/// 依目前的縮放（螢幕實際像素 ÷ 圖的像素）決定畫面要用原圖的幾倍：放大後不夠清楚時、
/// 或縮小後畫面大了很多時，在背景從原圖重新縮放（取到 1/8 的倍數，避免一直重做）
fn sharpen(s: &mut Shot, ctx: &egui::Context, phys: f32) {
    if s.sharpen.is_some() || s.full.is_empty() {
        return;
    }
    let (w, h) = s.orig;
    let want = ((phys as f64 * 8.0).ceil() / 8.0).clamp(0.125, 1.0).min(res_cap(w, h));
    let target = if want > s.res + 1e-6 {
        want
    } else if s.res > want * 2.0 && s.res > s.res0 + 1e-6 {
        want.max(s.res0)
    } else {
        return;
    };
    let (full, ctx) = (s.full.clone(), ctx.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("shot-sharpen".into()).spawn(move || {
        let (rgba, pw, ph) = shot_edit::scale_rgba(&full, w, h, target);
        if tx.send(Frame { time: 0.0, width: pw, height: ph, rgba }).is_ok() {
            ctx.request_repaint();
        }
    });
    if spawned.is_ok() {
        s.sharpen = Some((target, rx));
    }
}

/// 按住空白鍵（或滑鼠中鍵）拖曳移動畫面（沒有選工具時點空白處也可以直接拖曳，見 start_drag_pan）；回傳 true = 正在移動畫面（這一格不放標註）
pub fn pan(ed: &mut Editor, ui: &egui::Ui, resp: &egui::Response) -> bool {
    let Some(s) = &mut ed.shot else { return false };
    let v = &mut s.view;
    let typing = ui.ctx().egui_wants_keyboard_input();
    let (space, pressed, middle, down, pos) = ui.input(|i| {
        (i.key_down(Key::Space) && !typing, i.pointer.primary_pressed(), i.pointer.button_pressed(PointerButton::Middle), i.pointer.primary_down() || i.pointer.middle_down(), i.pointer.latest_pos())
    });
    if let Some((p0, c0)) = v.pan {
        match pos.filter(|_| down) {
            Some(p) => {
                v.c = (c0.0 - ((p.x - p0.x) / v.k) as f64, c0.1 - ((p.y - p0.y) / v.k) as f64);
                let outer = v.outer;
                clamp_center(v, outer);
                theme::hand_cursor(ui.ctx(), true);
            }
            None => v.pan = None,
        }
        return true;
    }
    if resp.hovered() && ((space && pressed) || middle) {
        if let Some(p) = pos {
            v.pan = Some((p, v.c));
            theme::hand_cursor(ui.ctx(), true);
            return true;
        }
    }
    if space && resp.hovered() {
        theme::hand_cursor(ui.ctx(), false);
        return true;
    }
    false
}

/// 圖比畫面大（放大後有捲軸）時，點空白處按住左鍵可以拖曳移動畫面
pub fn can_drag_pan(ed: &Editor) -> bool {
    let Some(s) = &ed.shot else { return false };
    let v = &s.view;
    let k = v.k as f64;
    v.region.2 * k > v.outer.width() as f64 + 0.5 || v.region.3 * k > v.outer.height() as f64 + 0.5
}

/// 開始用左鍵拖曳移動畫面（點在空白處時）；圖沒有比畫面大時回傳 false
pub fn start_drag_pan(ed: &mut Editor, ui: &egui::Ui, p: Pos2) -> bool {
    if !can_drag_pan(ed) {
        return false;
    }
    let Some(s) = &mut ed.shot else { return false };
    s.view.pan = Some((p, s.view.c));
    theme::hand_cursor(ui.ctx(), true);
    true
}

/// 畫出截圖（切成小塊的畫面，只畫看得到的）
pub fn paint(ed: &Editor, painter: &egui::Painter, rect: Rect) {
    let (Some(s), Some(f)) = (&ed.shot, &ed.frame) else { return };
    let (sx, sy) = (rect.width() / f.width.max(1) as f32, rect.height() / f.height.max(1) as f32);
    let clip = painter.clip_rect();
    for (r, tex) in &s.tiles.list {
        let sr = Rect::from_min_max(rect.min + vec2(r.min.x * sx, r.min.y * sy), rect.min + vec2(r.max.x * sx, r.max.y * sy));
        if sr.intersects(clip) {
            painter.image(tex.id(), sr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}

/// 畫面（套用馬賽克 / 模糊）切成小塊交給顯示卡；畫面或馬賽克等變了才重做（取代影片的單張畫面）
pub fn update_tiles(ed: &mut Editor, ctx: &egui::Context) {
    let (vw, vh) = (ed.vw, ed.vh);
    let Editor { frame, shot, anns, .. } = ed;
    let (Some(f), Some(s)) = (frame.as_ref(), shot.as_mut()) else { return };
    if f.width == 0 || f.height == 0 {
        return;
    }
    let effects: Vec<&Ann> = anns.iter().filter(|a| a.kind.is_effect()).collect();
    let key = super::hash_of(&(s.gen, f.width, f.height, serde_json::to_string(&effects).unwrap_or_default()));
    if key == s.tiles.key && !s.tiles.list.is_empty() {
        return;
    }
    s.tiles.key = key;
    let (w, h) = (f.width as usize, f.height as usize);
    let owned;
    let rgba: &[u8] = if effects.is_empty() || vw <= 0.0 {
        &f.rgba
    } else {
        let mut px = f.rgba.clone();
        for a in &effects {
            screenrecorder_core::effects::apply(&mut px, w, h, a, vw, vh);
        }
        owned = px;
        &owned
    };
    let mut old = std::mem::take(&mut s.tiles.list).into_iter();
    let t = TILE as usize;
    for ty in (0..h).step_by(t) {
        for tx in (0..w).step_by(t) {
            let (tw, th) = (t.min(w - tx), t.min(h - ty));
            let mut buf = Vec::with_capacity(tw * th * 4);
            for y in ty..ty + th {
                let i = (y * w + tx) * 4;
                buf.extend_from_slice(&rgba[i..i + tw * 4]);
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([tw, th], &buf);
            let r = Rect::from_min_size(pos2(tx as f32, ty as f32), vec2(tw as f32, th as f32));
            let n = s.tiles.list.len();
            let tex = match old.next() {
                Some((_, mut tex)) => {
                    tex.set(img, TILE_OPTIONS);
                    tex
                }
                None => ctx.load_texture(format!("shot-tile-{n}"), img, TILE_OPTIONS),
            };
            s.tiles.list.push((r, tex));
        }
    }
}

/// 裁切後（不在裁切分頁）畫面上方的小列：「已裁切 470×312」「調整裁切」「還原整張」（也可以按 Ctrl+Z 復原）
fn cropped_bar(ed: &mut Editor, ui: &mut egui::Ui, outer: Rect) {
    let Some(s) = &ed.shot else { return };
    let (_, _, w, h) = s.view.region;
    if s.loading.is_some() || w <= 0.0 || (w >= ed.vw - 0.5 && h >= ed.vh - 0.5) {
        return;
    }
    let p = theme::pal(ui);
    let area = Rect::from_min_size(outer.left_top() + vec2(8.0, 8.0), vec2((outer.width() - 16.0).max(0.0), 34.0));
    let bg = ui.painter().add(egui::Shape::Noop);
    let (mut adjust, mut restore) = (false, false);
    let inner = ui
        .scope_builder(egui::UiBuilder::new().max_rect(area.shrink2(vec2(8.0, 3.0))).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(trf!("已裁切 {}×{}", "Cropped to {}×{}", w.round(), h.round())).font(theme::font(12.5)).color(p.muted));
            ui.add_space(4.0);
            adjust = Btn::new(tr!("調整裁切", "Adjust crop"))
                .icon(theme::Icon::Edit)
                .ghost()
                .small()
                .tooltip(tr!("回到「裁切與旋轉」分頁調整範圍", "Go to “Crop & rotate” to change the area"))
                .show(ui)
                .clicked();
            restore = Btn::new(tr!("還原整張", "Show full image"))
                .icon(theme::Icon::Undo)
                .ghost()
                .small()
                .tooltip(tr!("取消裁切，回到完整的圖（也可以按 Ctrl+Z 復原）", "Remove the crop and go back to the full image (or press Ctrl+Z)"))
                .show(ui)
                .clicked();
        })
        .response
        .rect;
    ui.painter().set(bg, egui::epaint::RectShape::new(inner.expand2(vec2(8.0, 3.0)), CornerRadius::same(8), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside));
    if adjust {
        ed.tab = super::Tab::Crop;
    }
    if restore {
        ed.crop_on = false;
    }
}

/// 捲軸（圖比畫面大時，可以拖曳）與右下角的縮放按鈕
pub fn overlays(ed: &mut Editor, ui: &mut egui::Ui, outer: Rect, rect: Rect) {
    cropped_bar(ed, ui, outer);
    let vw = ed.vw;
    let Some(s) = &mut ed.shot else { return };
    if vw <= 0.0 || s.loading.is_some() {
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let p = theme::pal(ui);
    let v = &mut s.view;
    // 捲軸：圖超出畫面的方向，貼著畫面的右邊 / 下面
    let thick = 8.0;
    let over_y = rect.height() > outer.height() + 0.5;
    let over_x = rect.width() > outer.width() + 0.5;
    for vertical in [true, false] {
        let (over, len, total, pos) = if vertical { (over_y, outer.height(), rect.height(), outer.top() - rect.top()) } else { (over_x, outer.width(), rect.width(), outer.left() - rect.left()) };
        if !over {
            continue;
        }
        let track = if vertical {
            Rect::from_min_max(pos2(outer.right() - thick - 3.0, outer.top() + 3.0), pos2(outer.right() - 3.0, outer.bottom() - if over_x { thick + 6.0 } else { 3.0 }))
        } else {
            Rect::from_min_max(pos2(outer.left() + 3.0, outer.bottom() - thick - 3.0), pos2(outer.right() - if over_y { thick + 6.0 } else { 3.0 }, outer.bottom() - 3.0))
        };
        let tl = if vertical { track.height() } else { track.width() };
        let size = (tl * len / total).max(24.0).min(tl);
        let at = (pos / (total - len).max(1.0)).clamp(0.0, 1.0) * (tl - size);
        let thumb = if vertical { Rect::from_min_size(pos2(track.left(), track.top() + at), vec2(thick, size)) } else { Rect::from_min_size(pos2(track.left() + at, track.top()), vec2(size, thick)) };
        let resp = ui.interact(thumb, Id::new(("shot-scrollbar", vertical)), Sense::drag());
        if resp.drag_started() {
            if let Some(p0) = resp.interact_pointer_pos() {
                v.bar = Some((p0, v.c));
            }
        }
        if let (Some((p0, c0)), Some(p1)) = (v.bar, resp.interact_pointer_pos().filter(|_| resp.dragged())) {
            // 捲軸移動一點 = 圖移動 total / tl 點
            let k = v.k.max(1e-4);
            if vertical {
                v.c.1 = c0.1 + ((p1.y - p0.y) * total / tl / k) as f64;
            } else {
                v.c.0 = c0.0 + ((p1.x - p0.x) * total / tl / k) as f64;
            }
            clamp_center(v, outer);
        }
        if resp.drag_stopped() {
            v.bar = None;
        }
        let hot = resp.hovered() || resp.dragged();
        let painter = ui.painter_at(outer);
        painter.rect(thumb, CornerRadius::same(4), Color32::from_black_alpha(if hot { 170 } else { 110 }), Stroke::new(1.0, Color32::from_white_alpha(140)), StrokeKind::Inside);
    }
    // 縮放：－ 100% ＋　符合視窗　（長截圖另有符合寬度）　原尺寸
    let pct = (v.k * ppp * 100.0).round();
    let (rx, ry, rw, rh) = v.region;
    let tall = rh / rw.max(1.0) > (outer.height() / outer.width()) as f64 * 1.05;
    let bar_h = 34.0;
    let right = outer.right() - if over_y { thick + 12.0 } else { 8.0 };
    let bottom = outer.bottom() - if over_x { thick + 12.0 } else { 8.0 };
    let area = Rect::from_min_max(pos2(outer.left() + 8.0, bottom - bar_h), pos2(right, bottom));
    let mut set: Option<(Fit, f32)> = None;
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = ui
        .scope_builder(egui::UiBuilder::new().max_rect(area.shrink2(vec2(4.0, 3.0))).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let tip = tr!(
                "Ctrl+滾輪也可以縮放；放大後用滾輪捲動，或在空白處按住左鍵（也可以按住空白鍵）拖曳移動畫面",
                "You can also zoom with Ctrl+wheel. When zoomed in, scroll with the wheel or drag an empty spot (or hold Space and drag) to move around"
            );
            let one = 1.0 / ppp;
            if Btn::new(tr!("原尺寸", "Actual size"))
                .ghost()
                .small()
                .selected(v.fit == Fit::Scale && (v.k - one).abs() < 1e-3)
                .tooltip(tr!("一個像素顯示成螢幕上的一個像素（100%）", "Show one image pixel as one screen pixel (100%)"))
                .show(ui)
                .clicked()
            {
                set = Some((Fit::Scale, one));
            }
            if tall
                && Btn::new(tr!("符合寬度", "Fit width"))
                    .ghost()
                    .small()
                    .selected(v.fit == Fit::Width)
                    .tooltip(tr!("寬度剛好，上下捲動（適合長截圖）", "Fit the width and scroll up and down (good for long screenshots)"))
                    .show(ui)
                    .clicked()
            {
                set = Some((Fit::Width, 0.0));
            }
            if Btn::new(tr!("符合視窗", "Fit")).ghost().small().selected(v.fit == Fit::Whole).tooltip(tr!("整張圖放進畫面", "Show the whole image")).show(ui).clicked() {
                set = Some((Fit::Whole, 0.0));
            }
            ui.add_space(4.0);
            let cur = v.k * ppp;
            let up = ZOOM_STEPS.iter().copied().find(|z| *z > cur * 1.01);
            if Btn::new("＋").ghost().small().enabled(up.is_some()).tooltip(trf!("放大（{}）", "Zoom in ({})", tip)).show(ui).clicked() {
                set = up.map(|z| (Fit::Scale, z / ppp));
            }
            let label = ui.add_sized(vec2(48.0, 28.0), egui::Label::new(RichText::new(format!("{pct}%")).font(theme::font_bold(12.5))).sense(Sense::click())).on_hover_text(tip);
            if label.clicked() {
                // 點百分比：在原尺寸與符合視窗之間切換
                set = Some(if v.fit == Fit::Scale && (v.k - one).abs() < 1e-3 { (Fit::Whole, 0.0) } else { (Fit::Scale, one) });
            }
            let down = ZOOM_STEPS.iter().rev().copied().find(|z| *z < cur * 0.99);
            if Btn::new("－").ghost().small().enabled(down.is_some()).tooltip(trf!("縮小（{}）", "Zoom out ({})", tip)).show(ui).clicked() {
                set = down.map(|z| (Fit::Scale, z / ppp));
            }
        })
        .response
        .rect;
    // 背景：只包住按鈕
    ui.painter().set(bg, egui::epaint::RectShape::new(inner.expand2(vec2(4.0, 3.0)), CornerRadius::same(8), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside));
    if let Some((fit, k)) = set {
        if fit == Fit::Scale {
            let whole = fit_k(Fit::Whole, 1.0, outer, rw, rh);
            zoom_to(v, outer, k.max((ZOOM_MIN / ppp).min(whole)), outer.center());
        } else {
            v.fit = fit;
            if fit == Fit::Width {
                v.c = (rx + rw / 2.0, ry);
            }
        }
    }
}

/// 整張圖向右（順時針）或向左轉 90 度：標註與裁切範圍跟著轉
pub fn rotate(ed: &mut Editor, clockwise: bool) {
    let (w, h) = (ed.vw, ed.vh);
    // 旋轉前 → 旋轉後的座標
    let t = |x: f64, y: f64| if clockwise { (h - y, x) } else { (y, w - x) };
    for a in &mut ed.anns {
        if a.kind == annotate::AnnKind::Arrow {
            let (p0, p1) = (t(a.x, a.y), t(a.x + a.w, a.y + a.h));
            (a.x, a.y, a.w, a.h) = (p0.0, p0.1, p1.0 - p0.0, p1.1 - p0.1);
            continue;
        }
        // 其他：中心跟著轉，標註本身多轉 90 度（放大鏡是圓的，不用轉）
        let (cx, cy) = t(a.x + a.w / 2.0, a.y + a.h / 2.0);
        a.x = cx - a.w / 2.0;
        a.y = cy - a.h / 2.0;
        if a.kind.rotatable() {
            a.rot = super::norm_deg(a.rot + if clockwise { 90.0 } else { -90.0 });
        }
    }
    if let Some(c) = ed.spec.crop {
        ed.spec.crop = Some(if clockwise {
            CropInput { x: h - (c.y + c.height), y: c.x, width: c.height, height: c.width }
        } else {
            CropInput { x: c.y, y: w - (c.x + c.width), width: c.height, height: c.width }
        });
    }
    if let Some(s) = &mut ed.shot {
        s.opts.rotate = (s.opts.rotate + if clockwise { 90 } else { 270 }) % 360;
    }
    sync_rotation(ed);
}

/// 目前的編輯（輸出用）
pub fn spec(ed: &Editor) -> ShotSpec {
    let crop =
        if ed.crop_on { normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32).map(|r| CropInput { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 }) } else { None };
    let o = ed.shot.as_ref().map(|s| s.opts()).unwrap_or_default();
    ShotSpec {
        crop,
        anns: ed.ordered().into_iter().cloned().collect(),
        border: o.border,
        border_width: o.border_width,
        shadow: o.shadow,
        scale: o.scale,
        rotate: o.rotate,
        radius: o.radius,
        background: o.background,
        padding: o.padding,
        width: o.width,
    }
}

/// 「儲存」存成 JPG（取代之前的編輯圖時沿用它的格式）
fn jpg(ed: &Editor) -> bool {
    ed.replace_target.is_none() && ed.shot.as_ref().is_some_and(|s| s.opts.jpg)
}

fn source(ed: &Editor) -> String {
    ed.shot.as_ref().map(|s| s.path.clone()).unwrap_or_default()
}

/// 什麼都還沒改（不用存）
fn unchanged(ed: &Editor) -> bool {
    let s = spec(ed);
    s.crop.is_none() && s.anns.is_empty() && s.border.is_none() && !s.shadow && s.scale == 100 && s.width.is_none() && s.rotate == 0 && s.radius == 0 && s.background.is_none()
}

// ───────────── 畫面 ─────────────

/// 說明列：載入了上次的標註（儲存時取代或另存）、找不到原圖、或原圖有上次的編輯可以載入
pub fn banner(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let (target, opened_edit) = ed.shot.as_ref().map(|s| (s.target.clone(), s.opened_edit.is_some())).unwrap_or_default();
    let (text, warn): (String, bool) = if target.is_some() {
        (tr!("已載入上次的標註，可以繼續修改。", "Your previous annotations are loaded and can be changed.").into(), false)
    } else {
        match &ed.banner {
            BannerInfo::None => return,
            BannerInfo::MissingSource(src) => (
                trf!(
                    "找不到原圖「{src}」，之前的標註已經存進這張圖、無法修改；只能在這張圖上繼續編輯。",
                    "Can't find the original image “{src}”. Earlier annotations are saved into this image and can't be changed; you can only keep editing this image."
                ),
                true,
            ),
            BannerInfo::HasProject { output, .. } => (trf!("這張截圖之前編輯成「{}」。", "This screenshot was previously edited into “{}”.", file_name(output)), false),
        }
    };
    egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 0)).show(ui, |ui| {
        egui::Frame::new().fill(if warn { p.warn_soft } else { p.accent_soft }).corner_radius(theme::RADIUS_SM).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).color(if warn { p.warn } else { p.text }));
                if let Some(target) = target {
                    // 儲存時：取代之前編輯成的那張，或另存一張新的
                    ui.add_space(10.0);
                    ui.label(theme::muted(ui, tr!("儲存時：", "When saving:")));
                    let mut replace = ed.replace_target.is_some();
                    let items = [(true, tr!("取代原本的編輯圖", "Replace the edited image")), (false, tr!("另存一張新的", "Save as a new image"))];
                    if segmented(ui, &mut replace, &items, true) {
                        ed.replace_target = replace.then(|| target.clone());
                    }
                    if opened_edit
                        && Btn::new(tr!("改成直接編輯這張", "Edit this image directly instead"))
                            .small()
                            .ghost()
                            .tooltip(trf!(
                                "直接在「{}」上加標註：標註會變成圖的一部分，之後不能再分開修改",
                                "Annotate “{}” itself: annotations become part of the image and can't be changed separately later",
                                ed.entry.media.name
                            ))
                            .show(ui)
                            .clicked()
                    {
                        // 有還沒儲存的修改：先問
                        ed.pending = Some(if ed.dirty() { Act::AskPlain } else { Act::Plain });
                    }
                } else if let BannerInfo::HasProject { output, data } = &ed.banner {
                    if Btn::new(tr!("載入上次的編輯來修改", "Load the previous edits to change them")).small().show(ui).clicked() {
                        let (output, data) = (output.clone(), data.clone());
                        if let Some(spec) = shot_edit::ShotProject::parse(&data) {
                            apply_spec(ed, &spec);
                            ed.replace_target = Some(output.clone());
                            if let Some(s) = &mut ed.shot {
                                s.target = Some(output);
                            }
                            ed.banner = BannerInfo::None;
                            ed.mark_saved();
                        }
                    }
                }
            });
        });
    });
}

/// 下方：輸出大小與按鈕
pub fn footer(ed: &mut Editor, ui: &mut egui::Ui) -> Option<Act> {
    let p = theme::pal(ui);
    let s = spec(ed);
    let (w, h) = output_size(ed, &s);
    let mut act = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(tr!("輸出 ", "Output "));
            ui.label(RichText::new(format!("{w}×{h}")).font(theme::font_bold(13.5)));
            let (ow, oh) = ed.shot.as_ref().map(|s| s.orig).unwrap_or_default();
            ui.label(trf!("（原圖 {ow}×{oh}）", " (original {ow}×{oh})"));
            if !ed.anns.is_empty() {
                let n = ed.anns.len();
                ui.label(if screenrecorder_core::i18n::is_en() { format!(" · {n} annotation{}", if n == 1 { "" } else { "s" }) } else { format!("・標註 {n} 個") });
            }
        });
        let line = match &ed.replace_target {
            Some(t) => trf!("儲存後取代 {}，並複製到剪貼簿", "Saving replaces {} and copies it to the clipboard", file_name(t)),
            None => {
                let name = file_name(&shot_edit::edited_path_as(std::path::Path::new(&source(ed)), if jpg(ed) { "jpg" } else { "png" }).display().to_string());
                trf!("另存成 {}（原圖保留），並複製到剪貼簿", "Saves as {} (the original is kept) and copies it to the clipboard", name)
            }
        };
        ui.label(RichText::new(line).font(theme::font(12.0)).color(p.muted));
    });
    // 右邊：儲存（主要，旁邊 ▾ 另存新檔）、複製、釘在桌面、文字辨識；全部重設收進「更多」，不會跟儲存並排被誤按
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let same = unchanged(ed);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 1.0;
            let arrow = Btn::icon_only(Icon::ChevD).primary().enabled(!ed.saving).tooltip(tr!("另存新檔（選位置與格式）", "Save as (choose a location and format)")).show(ui);
            let mut b = Btn::new(if ed.replace_target.is_some() { tr!("儲存修改", "Save changes") } else { tr!("儲存", "Save") }).primary().enabled(!same && !ed.saving).tooltip("Ctrl+S");
            if same {
                b = b.tooltip(tr!("還沒有任何標註、裁切或輸出設定", "No annotations, crop or output settings yet"));
            }
            if b.show(ui).clicked() {
                act = Some(Act::Save);
            }
            egui::Popup::menu(&arrow).align(egui::RectAlign::TOP_END).show(|ui| {
                ui.set_min_width(200.0);
                if ui
                    .button(tr!("另存新檔…", "Save as…"))
                    .on_hover_text(tr!("選擇儲存的位置與檔名，可以存成 PNG 或 JPG（檔案較小）", "Choose where to save and the file name; save as PNG or JPG (smaller file)"))
                    .clicked()
                {
                    act = Some(Act::SaveAs);
                }
            });
        });
        if Btn::new(tr!("複製", "Copy")).icon(Icon::Copy).tooltip(tr!("複製編輯後的圖到剪貼簿，不存檔（Ctrl+C）", "Copy the edited image to the clipboard without saving (Ctrl+C)")).show(ui).clicked()
        {
            act = Some(Act::Copy);
        }
        if Btn::new(tr!("釘在桌面", "Pin"))
            .icon(Icon::Pin)
            .ghost()
            .tooltip(tr!("把編輯後的圖變成浮在最上層的小視窗，方便對照著打字或填表", "Show the edited image in a small always-on-top window, handy for reference while you type"))
            .show(ui)
            .clicked()
        {
            act = Some(Act::Pin);
        }
        if Btn::new(tr!("文字辨識", "Copy text"))
            .icon(Icon::Ocr)
            .ghost()
            .tooltip(tr!("把圖裡的文字轉成可以複製的文字（Windows 內建的文字辨識）", "Turn the text in the image into text you can copy (uses the text recognition built into Windows)"))
            .show(ui)
            .clicked()
        {
            act = Some(Act::Ocr);
        }
        let more = Btn::icon_only(Icon::More).ghost().tooltip(tr!("更多：全部重設", "More: Reset all")).show(ui);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(220.0);
            if ui
                .add_enabled(!unchanged(ed), egui::Button::new(tr!("全部重設（回到原圖）", "Reset all (back to the original)")))
                .on_hover_text(tr!("清掉標註、裁切、旋轉與輸出設定；可以按 Ctrl+Z 復原", "Clear annotations, crop, rotation and output settings; press Ctrl+Z to undo"))
                .clicked()
            {
                ed.reset();
                if let Some(s) = &mut ed.shot {
                    s.opts = ShotOpts::default();
                }
            }
        });
    });
    act
}

/// 「輸出」分頁：外框、陰影、大小，以及輸出的樣子
pub fn output_panel(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let p = theme::pal(ui);
    let Some(mut o) = ed.shot.as_ref().map(|s| s.opts()) else { return };
    let label = |ui: &mut egui::Ui, t: &str| {
        ui.label(theme::muted(ui, t).font(theme::font(12.0)));
    };
    // 外框
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, tr!("外框", "Border"));
        let mut kind = match &o.border {
            None => 0,
            Some(_) if o.border_width <= 3.0 => 1,
            Some(_) => 2,
        };
        if segmented(ui, &mut kind, &[(0, tr!("無", "None")), (1, tr!("細", "Thin")), (2, tr!("粗", "Thick"))], true) {
            match kind {
                0 => o.border = None,
                k => {
                    o.border = Some(o.border.clone().unwrap_or_else(|| BORDER_COLORS[0].into()));
                    o.border_width = if k == 1 { 2.0 } else { 6.0 };
                }
            }
        }
        if o.border.is_some() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for (i, c) in BORDER_COLORS.iter().copied().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                    let resp = resp.on_hover_text(border_name(i));
                    let (cr, cg, cb) = annotate::parse_color(c);
                    if o.border.as_deref() == Some(c) {
                        ui.painter().circle_stroke(r.center(), 13.5, Stroke::new(2.0, p.accent));
                    }
                    ui.painter().circle(r.center(), 10.5, Color32::from_rgb(cr, cg, cb), Stroke::new(1.0, p.border_strong));
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        o.border = Some(c.to_string());
                    }
                }
            });
        }
    });
    // 圓角
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, tr!("圓角", "Corners"));
        let names = tr!(["無", "小", "中", "大"], ["None", "Small", "Medium", "Large"]);
        let items: Vec<(u32, &str)> = RADII.iter().copied().zip(names).collect();
        segmented(ui, &mut o.radius, &items, true);
    });
    // 背景
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, tr!("背景", "Background"));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
            let sw = r.shrink(2.0);
            ui.painter().rect(sw, CornerRadius::same(5), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
            ui.painter().line_segment([sw.left_bottom() + vec2(4.0, -4.0), sw.right_top() + vec2(-4.0, 4.0)], Stroke::new(1.5, p.muted));
            if o.background.is_none() {
                ui.painter().rect_stroke(r.expand(1.0), CornerRadius::same(7), Stroke::new(2.0, p.accent), StrokeKind::Inside);
            }
            if resp.on_hover_text(tr!("不要背景", "No background")).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                o.background = None;
            }
            for (i, bg) in BACKGROUNDS.iter().copied().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                let resp = resp.on_hover_text(background_name(i));
                gradient_swatch(ui.painter(), r.shrink(2.0), bg);
                if o.background.as_deref() == Some(bg) {
                    ui.painter().rect_stroke(r.expand(1.0), CornerRadius::same(7), Stroke::new(2.0, p.accent), StrokeKind::Inside);
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    o.background = Some(bg.to_string());
                }
            }
        });
        if o.background.is_some() {
            let items: Vec<(u32, &str)> = PADDINGS.iter().copied().zip(tr!(["留白少", "留白中", "留白多"], ["Padding S", "Padding M", "Padding L"])).collect();
            segmented(ui, &mut o.padding, &items, true);
        }
    });
    // 陰影
    switch(ui, &mut o.shadow, tr!("加上陰影", "Add shadow"), true).on_hover_text(tr!(
        "圖的四周加上柔和的陰影（沒有背景時四周留透明的邊），貼到文件或簡報比較立體",
        "Add a soft shadow around the image (with a transparent margin when there's no background) so it stands out in documents and slides"
    ));
    // 大小：比例，或自訂寬度（像素）
    let crop_w = {
        let sp = spec(ed);
        let (ow, oh) = ed.shot.as_ref().map(|s| s.orig).unwrap_or_default();
        let (vw, vh) = shot_edit::rotated_size(ow, oh, sp.rotate);
        shot_edit::crop_rect(&sp, vw, vh).2
    };
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, tr!("大小", "Size"));
        // 0 = 自訂寬度
        let mut pick = if o.width.is_some() { 0 } else { o.scale };
        let mut items: Vec<(u32, String)> = SCALES.iter().map(|s| (*s, format!("{s}%"))).collect();
        items.push((0, tr!("自訂", "Custom").into()));
        let refs: Vec<(u32, &str)> = items.iter().map(|(v, t)| (*v, t.as_str())).collect();
        if segmented(ui, &mut pick, &refs, true) {
            if pick == 0 {
                o.width = Some(((crop_w as f64 * o.scale as f64 / 100.0).round() as u32).clamp(16, crop_w.max(16)));
            } else {
                (o.width, o.scale) = (None, pick);
            }
        }
        if let Some(w) = o.width {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("自訂寬度（像素）", "Custom width (pixels)")).font(theme::font(12.0)));
                let mut v = w as f64;
                let max = crop_w.max(16) as f64;
                if theme::stepper(ui, Id::new("shot-width"), &mut v, 10.0, 16.0..=max, 0, "") {
                    o.width = Some(v.round().clamp(16.0, max) as u32);
                }
            });
            ui.label(theme::muted(ui, trf!("最大 {crop_w}（原本的寬度，不會放大）；高度依比例", "Up to {crop_w} (the original width; never enlarged); the height follows")).font(theme::font(12.0)));
        }
    });
    // 格式：「儲存」存成 PNG 或 JPG（取代之前的編輯圖時沿用它的格式）
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, tr!("格式", "Format"));
        let replacing = ed.replace_target.is_some();
        let mut f = o.jpg && !replacing;
        if segmented(ui, &mut f, &[(false, "PNG"), (true, tr!("JPG（檔案較小）", "JPG (smaller)"))], !replacing) {
            o.jpg = f;
        }
        let hint = if let Some(t) = &ed.replace_target {
            let ext = std::path::Path::new(t).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            trf!("取代原本的編輯圖時沿用它的格式（{ext}）。", "Replacing the edited image keeps its format ({ext}).")
        } else if f {
            tr!(
                "JPG 檔案較小，適合寄信或傳訊息；透明的地方會變成白色，也不會出現在「全部截圖」裡。",
                "JPG files are smaller, good for email or chat; transparent areas become white, and the file won't appear in “All screenshots”."
            )
            .into()
        } else {
            tr!("PNG 畫質最好，文字清楚。", "PNG keeps the best quality and the sharpest text.").into()
        };
        ui.label(theme::muted(ui, hint).font(theme::font(12.0)));
    });
    if let Some(s) = &mut ed.shot {
        s.opts = o;
    }
    let sp = spec(ed);
    // 輸出的樣子（與存檔用同一個 render）
    let key = serde_json::to_string(&sp).unwrap_or_default();
    let stale = ed.shot.as_ref().is_none_or(|s| s.preview.as_ref().is_none_or(|(k, _)| *k != key));
    if stale {
        // 用還沒旋轉的畫面與原圖大小（render 自己轉）
        if let Some((f, (ow, oh))) = ed.shot.as_ref().map(|s| (&s.base, s.orig)) {
            if let Some(pm) = shot_edit::render(&f.rgba, f.width, f.height, ow, oh, &sp, true) {
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                let tex = ctx.load_texture("shot-output", img, TextureOptions::LINEAR);
                if let Some(s) = &mut ed.shot {
                    s.preview = Some((key, tex));
                }
            }
        }
    }
    ui.label(theme::muted(ui, tr!("左邊顯示的就是輸出的樣子；輸出的大小顯示在下方。", "The left side shows how the output will look; the output size is shown below.")).font(theme::font(12.0)));
}

/// 輸出的大小（原圖大小 + 旋轉 + 裁切 + 縮放 + 陰影）
fn output_size(ed: &Editor, sp: &ShotSpec) -> (u32, u32) {
    let (ow, oh) = ed.shot.as_ref().map(|s| s.orig).unwrap_or((ed.vw as u32, ed.vh as u32));
    shot_edit::output_size(sp, ow, oh)
}

/// 旋轉按鈕（只在「裁切與旋轉」分頁）
pub fn rotate_buttons(ed: &mut Editor, ui: &mut egui::Ui) {
    if Btn::new(tr!("向左轉", "Rotate left"))
        .icon(Icon::RotL)
        .small()
        .tooltip(tr!("整張圖逆時針轉 90 度（標註跟著轉）", "Rotate the whole image 90° counterclockwise (annotations rotate too)"))
        .show(ui)
        .clicked()
    {
        rotate(ed, false);
    }
    if Btn::new(tr!("向右轉", "Rotate right"))
        .icon(Icon::RotR)
        .small()
        .tooltip(tr!("整張圖順時針轉 90 度（標註跟著轉）", "Rotate the whole image 90° clockwise (annotations rotate too)"))
        .show(ui)
        .clicked()
    {
        rotate(ed, true);
    }
}

/// 背景色塊的名稱（滑鼠移上去時顯示；順序同 BACKGROUNDS）
fn background_name(i: usize) -> &'static str {
    let names = tr!(
        ["藍色漸層", "粉紅漸層", "橘黃漸層", "綠藍漸層", "紫色漸層", "深藍漸層", "淺灰色", "深灰色"],
        ["Blue gradient", "Pink gradient", "Orange gradient", "Green-blue gradient", "Purple gradient", "Dark blue gradient", "Light gray", "Dark gray"]
    );
    names.get(i).copied().unwrap_or("")
}

/// 外框顏色的名稱（順序同 BORDER_COLORS）
fn border_name(i: usize) -> &'static str {
    let names = tr!(["淺灰色", "黑色", "紅色", "藍色"], ["Light gray", "Black", "Red", "Blue"]);
    names.get(i).copied().unwrap_or("")
}

/// 背景的色塊：單色或從左上到右下的漸層
pub(super) fn gradient_swatch(p: &egui::Painter, r: Rect, bg: &str) {
    let cols: Vec<Color32> = bg
        .split(',')
        .map(|c| {
            let (r, g, b) = annotate::parse_color(c.trim());
            Color32::from_rgb(r, g, b)
        })
        .collect();
    let (a, b) = (cols.first().copied().unwrap_or(Color32::GRAY), cols.last().copied().unwrap_or(Color32::GRAY));
    let mid = a.lerp_to_gamma(b, 0.5);
    let mut mesh = egui::Mesh::default();
    for (pt, c) in [(r.left_top(), a), (r.right_top(), mid), (r.right_bottom(), b), (r.left_bottom(), mid)] {
        mesh.colored_vertex(pt, c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
    p.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_black_alpha(40)), StrokeKind::Inside);
}

/// 「輸出」分頁的預覽圖（左邊大畫面顯示）
pub fn output_texture(ed: &Editor) -> Option<TextureHandle> {
    ed.shot.as_ref().and_then(|s| s.preview.as_ref()).map(|(_, t)| t.clone())
}

pub fn checker(p: &egui::Painter, r: Rect) {
    let n = 8.0;
    let (a, b) = (Color32::from_gray(236), Color32::from_gray(214));
    p.rect_filled(r, CornerRadius::ZERO, a);
    let mut y = r.top();
    let mut row = 0;
    while y < r.bottom() {
        let mut x = r.left() + if row % 2 == 0 { 0.0 } else { n };
        while x < r.right() {
            p.rect_filled(Rect::from_min_max(pos2(x, y), pos2((x + n).min(r.right()), (y + n).min(r.bottom()))), CornerRadius::ZERO, b);
            x += n * 2.0;
        }
        y += n;
        row += 1;
    }
}

// ───────────── 快速鍵 ─────────────

/// 標註工具的快速鍵（按鈕的提示也顯示）
pub fn tool_key(tool: super::Tool) -> Option<char> {
    use super::Tool;
    Some(match tool {
        Tool::Ann(AnnKind::Text) => 'T',
        Tool::Emoji => 'E',
        Tool::Ann(AnnKind::Arrow) => 'A',
        Tool::Ann(AnnKind::Rect) => 'R',
        Tool::Ann(AnnKind::Ellipse) => 'O',
        Tool::Ann(AnnKind::Highlight) => 'H',
        Tool::Ann(AnnKind::Pen) => 'P',
        Tool::Ann(AnnKind::Step) => 'N',
        Tool::Ann(AnnKind::Mosaic) => 'M',
        Tool::Ann(AnnKind::Blur) => 'B',
        Tool::Ann(AnnKind::Spotlight) => 'S',
        Tool::Ann(AnnKind::Magnify) => 'L',
        Tool::Ann(AnnKind::Image) => return None,
    })
}

/// 工具按鈕的提示：「箭頭（A）」
pub fn tool_tip(tool: super::Tool, label: &str) -> Option<String> {
    tool_key(tool).map(|k| trf!("{label}（{k}）", "{label} ({k})"))
}

/// 截圖的快速鍵（沒有在輸入文字時）：Ctrl+S 儲存、Ctrl+C 複製（選了標註時複製標註）、Ctrl+V 貼上（標註或圖片）、
/// Ctrl+D 複製一份選取的標註、方向鍵移動選取的標註（Shift 一次 10 像素）、字母鍵選工具；按住空白鍵拖曳移動畫面
pub fn keys(ed: &mut Editor, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    use egui::Modifiers;
    let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    // 空白鍵留給拖曳移動畫面（不要觸發剛按過、還有焦點的按鈕）
    key(Modifiers::NONE, Key::Space);
    if key(Modifiers::COMMAND, Key::S) {
        ed.pending = Some(Act::Save);
    }
    // Ctrl+C 會先變成「複製」事件
    if key(Modifiers::COMMAND, Key::C) || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        match ed.selected().cloned() {
            Some(a) => {
                // 剪貼簿放這個標註的文字（文字標註）或名稱：貼到別的程式也有東西；貼回來時認得出來
                let text = a.text.clone().filter(|t| a.kind == AnnKind::Text && !t.trim().is_empty()).unwrap_or_else(|| annotate::label(&a));
                screenrecorder_core::clipboard::copy_text(&text);
                if let Some(s) = &mut ed.shot {
                    s.clip = Some((text, a));
                }
                *toast = Some((tr!("已複製標註，按 Ctrl+V 貼上", "Annotation copied. Press Ctrl+V to paste").into(), false));
            }
            None => ed.pending = Some(Act::Copy),
        }
    }
    // Ctrl+V：剪貼簿還是複製標註時的文字 → 貼上標註；否則貼上剪貼簿裡的圖片
    let (paste_text, v_events) = ctx.input(|i| {
        let text = i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None });
        let v: Vec<(bool, bool)> = i.events.iter().filter_map(|e| if let egui::Event::Key { key: Key::V, pressed, modifiers, .. } = e { Some((*pressed, modifiers.command)) } else { None }).collect();
        (text, v)
    });
    let mut paste = paste_text.is_some();
    if paste_text.is_some() {
        if let Some(s) = &mut ed.shot {
            s.v_down = true;
        }
    }
    for (pressed, command) in v_events {
        let Some(s) = &mut ed.shot else { break };
        if pressed {
            // 開發測試直接送出 Ctrl+V 的按鍵
            paste |= command;
            s.v_down = true;
        } else {
            // 只收到放開：按下時被當成貼上（剪貼簿裡沒有文字，例如只有圖片）
            paste |= !s.v_down;
            s.v_down = false;
        }
    }
    if paste {
        let text = paste_text.or_else(screenrecorder_core::clipboard::read_text);
        let ann = ed.shot.as_ref().and_then(|s| s.clip.as_ref()).filter(|(m, _)| text.as_ref() == Some(m)).map(|(_, a)| a.clone());
        match ann {
            Some(a) => duplicate(ed, &a),
            None => ed.pending = Some(Act::PastePicture),
        }
    }
    if key(Modifiers::COMMAND, Key::D) {
        if let Some(a) = ed.selected().cloned() {
            duplicate(ed, &a);
        }
    }
    // 方向鍵：移動選取的標註
    for (k, dx, dy) in [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, -1.0), (Key::ArrowDown, 0.0, 1.0)] {
        let step = if key(Modifiers::SHIFT, k) {
            10.0
        } else if key(Modifiers::NONE, k) {
            1.0
        } else {
            continue;
        };
        if let Some(a) = ed.selected_mut() {
            a.x += dx * step;
            a.y += dy * step;
        }
    }
    // 字母鍵選工具（再按一次取消）
    use super::Tool;
    let tools = [AnnKind::Text, AnnKind::Arrow, AnnKind::Rect, AnnKind::Ellipse, AnnKind::Highlight, AnnKind::Pen, AnnKind::Step, AnnKind::Mosaic, AnnKind::Blur, AnnKind::Spotlight, AnnKind::Magnify]
        .into_iter()
        .map(Tool::Ann)
        .chain([Tool::Emoji]);
    for tool in tools {
        let Some(k) = tool_key(tool).and_then(|c| Key::from_name(&c.to_string())) else { continue };
        if ed.vw > 0.0 && key(Modifiers::NONE, k) {
            ed.tool = if ed.tool == Some(tool) { None } else { Some(tool) };
            if ed.tool.is_some() {
                ed.ann_sel = None;
                ed.tab = Tab::Ann;
            }
        }
    }
}

/// 複製一份標註（往右下移一點）並選取它
fn duplicate(ed: &mut Editor, a: &Ann) {
    let mut b = a.clone();
    b.id = ed.next_id;
    ed.next_id += 1;
    let off = (ed.vw.min(ed.vh) * 0.02).round().max(10.0);
    b.x += off;
    b.y += off;
    let id = b.id;
    ed.anns.push(b);
    ed.tool = None;
    ed.select_ann(Some(id));
}

// ───────────── 動作 ─────────────

/// 在找到的個資上加馬賽克（已經遮住的不重複加）；回傳要顯示的訊息
fn cover_pii(ed: &mut Editor, found: &[ocr::Found]) -> String {
    if found.is_empty() {
        return tr!("沒有找到 Email、電話、身分證字號或卡號", "No emails, phone numbers, ID numbers or card numbers were found").into();
    }
    let covered = |ed: &Editor, (x, y, w, h): (f64, f64, f64, f64)| {
        ed.anns.iter().filter(|a| a.kind.is_effect() && !a.invert).any(|a| {
            let ix = (a.x + a.w).min(x + w) - a.x.max(x);
            let iy = (a.y + a.h).min(y + h) - a.y.max(y);
            ix > 0.0 && iy > 0.0 && ix * iy >= w * h * 0.6
        })
    };
    let mut counts: Vec<(ocr::Pii, usize)> = vec![];
    for f in found {
        let (x, y, w, h) = f.rect;
        let x = x.clamp(0.0, ed.vw);
        let y = y.clamp(0.0, ed.vh);
        let (w, h) = (w.min(ed.vw - x), h.min(ed.vh - y));
        if w < 2.0 || h < 2.0 || covered(ed, (x, y, w, h)) {
            continue;
        }
        let mut a = ed.new_ann(AnnKind::Mosaic, x, y);
        (a.w, a.h) = (w, h);
        (a.shape, a.invert) = (Some(annotate::Shape::Rect), false);
        ed.anns.push(a);
        match counts.iter_mut().find(|(k, _)| *k == f.kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((f.kind, 1)),
        }
    }
    if counts.is_empty() {
        return tr!("找到的個資都已經遮住了", "The personal info found is already covered").into();
    }
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{} {n}", k.name())).collect();
    if screenrecorder_core::i18n::is_en() {
        format!("Covered {total} item{} ({}); please check for anything missed", if total == 1 { "" } else { "s" }, parts.join(", "))
    } else {
        format!("已遮住 {total} 處（{}）；請再確認有沒有漏掉的", parts.join("、"))
    }
}

/// 儲存、複製、釘在桌面、文字辨識、直接編輯開啟的那張
pub fn run(app: &mut UiApp, ed: &mut Editor, act: Act) {
    let (core, src, sp) = (app.core.clone(), source(ed), spec(ed));
    match act {
        Act::Save => {
            if unchanged(ed) || ed.saving {
                return;
            }
            ed.saving = true;
            let (replace, jpg) = (ed.replace_target.clone(), jpg(ed));
            app.spawn(async move { actions::shot_save(&core, &src, &sp, replace.as_deref(), jpg).await }, |app, r| match r {
                Ok(info) => {
                    app.editor = None;
                    saved_toast(app, &info);
                    app.shots_changed();
                }
                Err(e) => {
                    if let Some(ed) = &mut app.editor {
                        ed.saving = false;
                    }
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        Act::Copy => app.spawn(async move { actions::shot_copy(&src, &sp).await }, |app, r| match r {
            Ok((w, h)) => app.toast(trf!("已複製到剪貼簿（{w}×{h}）", "Copied to the clipboard ({w}×{h})"), false),
            Err(e) => app.toast(e.message().to_string(), true),
        }),
        Act::Pin => app.spawn(async move { actions::shot_pin(&src, &sp).await }, |app, r| match r {
            Ok(()) => app.toast(tr!("已釘在桌面，點兩下圖片即可關閉", "Pinned to the desktop. Double-click the image to close it"), false),
            Err(e) => app.toast(e.message().to_string(), true),
        }),
        Act::Ocr => super::super::ocr::start_spec(app, src, sp),
        Act::FindPii => {
            if ed.finding_pii {
                return;
            }
            ed.finding_pii = true;
            let rotate = sp.rotate;
            app.spawn(async move { actions::shot_find_pii(&src, rotate).await }, |app, r| {
                let Some(ed) = &mut app.editor else { return };
                ed.finding_pii = false;
                match r {
                    Ok(found) => {
                        let msg = cover_pii(ed, &found);
                        app.toast(msg, false);
                    }
                    Err(e) => crate::ui::ocr::error(app, e.message()),
                }
            });
        }
        Act::PickPicture { replace } => {
            app.spawn(async move { tokio::task::spawn_blocking(picture::pick_file).await.unwrap_or(Ok(None)) }, move |app, r| match r {
                Ok(Some(path)) => use_picture(app, &path.to_string_lossy(), replace),
                Ok(None) => {}
                Err(e) => app.toast(e, true),
            });
        }
        Act::PastePicture => {
            app.spawn(async move { tokio::task::spawn_blocking(picture::save_clipboard_image).await.unwrap_or_else(|e| Err(e.to_string())) }, |app, r| match r {
                Ok(path) => use_picture(app, &path.to_string_lossy(), false),
                Err(e) => app.toast(e, true),
            });
        }
        Act::AutoSubs | Act::ImportSrt | Act::CancelSubs => super::subs::run(app, ed, act),
        Act::LastPicture => {
            let path = ed.last_picture.clone();
            match ed.add_picture(&path) {
                Ok(()) => {}
                Err(e) => app.toast(e, true),
            }
        }
        Act::FindIdle => {
            if ed.finding_idle {
                return;
            }
            ed.finding_idle = true;
            let (ffmpeg, video, dur, audio) = (ed.ffmpeg.clone(), ed.entry.media.path.clone(), ed.duration, ed.entry.media.has_audio == Some(true));
            app.spawn(async move { screenrecorder_core::idle::find(&ffmpeg, &video, dur, audio).await }, |app, r| {
                let Some(ed) = &mut app.editor else { return };
                ed.finding_idle = false;
                match r {
                    Ok(found) => {
                        let msg = ed.remove_idle(&found);
                        app.toast(msg, false);
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                }
            });
        }
        Act::Grab => {
            let (video, t) = (ed.entry.media.path.clone(), ed.now());
            ed.pause();
            // 完成後由狀態更新顯示「已截圖」並更新清單
            app.spawn(async move { core.grab_frame(&video, t).await }, |app, r| {
                if let Err(e) = r {
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        Act::Plain => {
            let Some(path) = ed.shot.as_ref().and_then(|s| s.opened_edit.clone()) else { return };
            app.editor = None;
            open_with(app, path, false);
        }
        Act::AskPlain => {
            let Some(path) = ed.shot.as_ref().and_then(|s| s.opened_edit.clone()) else { return };
            let mut ask = crate::ui::dialogs::Ask::confirm(
                tr!("改成直接編輯這張？", "Edit this image directly?"),
                tr!("目前還沒儲存的修改會不見。", "Your unsaved changes will be lost."),
                tr!("放棄修改並切換", "Discard changes and switch"),
                move |app, _| {
                    app.editor = None;
                    open_with(app, path, false);
                },
            );
            ask.danger = true;
            app.ask = Some(ask);
        }
        Act::SaveAs => save_as(app, ed),
    }
}

/// 另存新檔：Windows 的另存新檔視窗（檔名預先填好、可選 PNG 或 JPG），存好後關閉編輯視窗；記住這次的資料夾
fn save_as(app: &mut UiApp, ed: &mut Editor) {
    if ed.saving {
        return;
    }
    let (core, src, sp) = (app.core.clone(), source(ed), spec(ed));
    let last = std::path::PathBuf::from(&app.s.shot_save_dir);
    let dir = if !app.s.shot_save_dir.is_empty() && last.is_dir() { Some(last) } else { std::path::Path::new(&src).parent().map(|p| p.to_path_buf()) };
    let name = match &ed.replace_target {
        Some(t) => file_name(t),
        None => file_name(&shot_edit::edited_path_as(std::path::Path::new(&src), if jpg(ed) { "jpg" } else { "png" }).display().to_string()),
    };
    // JPG 時另存新檔視窗預先選 JPG
    let jpg_first = jpg(ed);
    ed.saving = true;
    app.spawn(
        async move {
            let mut types = vec![(tr!("PNG 圖片", "PNG image"), "*.png"), (tr!("JPG 圖片（檔案較小）", "JPG image (smaller file)"), "*.jpg")];
            if jpg_first {
                types.reverse();
            }
            let picked = tokio::task::spawn_blocking(move || screenrecorder_core::filepick::save_file(tr!("另存新檔", "Save as"), dir, &name, &types)).await.unwrap_or_else(|e| Err(e.to_string()))?;
            let Some((path, kind)) = picked else { return Ok(None) };
            // 自己打的副檔名優先；沒有或不認得時依選的類型
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let chose_jpg = (kind == 1) != jpg_first;
            let path = if ["png", "jpg", "jpeg"].contains(&ext.as_str()) { path } else { path.with_extension(if chose_jpg { "jpg" } else { "png" }) };
            actions::shot_save_as(&core, &src, &sp, &path.display().to_string()).await.map(Some).map_err(|e| e.message().to_string())
        },
        |app, r: Result<Option<screenrecorder_core::types::ShotInfo>, String>| {
            if let Some(ed) = &mut app.editor {
                ed.saving = false;
            }
            match r {
                Ok(Some(info)) => {
                    if let Some(dir) = std::path::Path::new(&info.path).parent() {
                        app.s.shot_save_dir = dir.display().to_string();
                        app.save_settings();
                    }
                    app.editor = None;
                    saved_toast(app, &info);
                    app.shots_changed();
                }
                Ok(None) => {}
                Err(e) => app.toast(e, true),
            }
        },
    );
}

/// 存好的提示：[開啟] [在資料夾中顯示]
fn saved_toast(app: &mut UiApp, info: &screenrecorder_core::types::ShotInfo) {
    let text = trf!("已存成 {}{}", "Saved as {}{}", file_name(&info.path), if info.copied { tr!("，並複製到剪貼簿", " and copied to the clipboard") } else { "" });
    if crate::ui::dialogs::is_image(&info.path) {
        return app.toast_file(text, &info.path);
    }
    // JPG：用 Windows 預設的程式開啟
    let (p1, p2) = (info.path.clone(), info.path.clone());
    let open = crate::ui::ToastAction::new(tr!("開啟", "Open"), move |app: &mut UiApp| {
        if let Err(e) = actions::open(actions::OpenAction::Play, &p1) {
            app.toast(e.message().to_string(), true);
        }
    });
    let reveal = crate::ui::ToastAction::new(tr!("在資料夾中顯示", "Show in folder"), move |app: &mut UiApp| {
        if let Err(e) = actions::open(actions::OpenAction::Reveal, &p2) {
            app.toast(e.message().to_string(), true);
        }
    });
    app.toast_with(text, false, vec![open, reveal]);
}

/// 加上（或換成）這張圖片，並記住它（下次可以從選單直接再用）
pub fn use_picture(app: &mut UiApp, path: &str, replace: bool) {
    let Some(ed) = &mut app.editor else { return };
    let r = if replace { ed.replace_picture(path) } else { ed.add_picture(path) };
    match r {
        Ok(()) => {
            if app.s.last_picture != path {
                app.s.last_picture = path.to_string();
                app.save_settings();
            }
        }
        Err(e) => app.toast(e, true),
    }
}
