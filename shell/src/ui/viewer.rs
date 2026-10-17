//! 內建檢視器（不必開 Windows 的相片 / 媒體播放器）：
//! - 截圖：適合視窗 / 原尺寸、滾輪縮放、拖曳移動、上一張 / 下一張、複製到剪貼簿
//! - 錄影（含加速版、GIF）：播放 / 暫停、拖曳進度、音量、播放速度、全螢幕、上一部 / 下一部、剪輯、製作加速版、壓縮
//! - 只有聲音的檔案（M4A）：播放聲音
//!
//! 都可以「用 Windows 開啟」、在資料夾中顯示、移到資源回收筒；視窗窄時不常用的按鈕收進「⋯」。

use super::dialogs::{base_name, is_default_name, short_date, Ask};
use super::theme::{self, Btn, Icon};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, Id, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use screenrecorder_core::actions;
use screenrecorder_core::format::{format_bytes, short_clock, speed_label};
use screenrecorder_core::library::{is_audio_name, is_image_name};
use screenrecorder_core::player::{MediaSpec, Player};
use screenrecorder_core::types::{LibraryEntry, LibraryFilter, LibraryQuery, LibrarySort, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 播放時解碼的最大尺寸
const DECODE_MAX: (u32, u32) = (1920, 1080);
const ZOOM_MIN: f32 = 0.05;
const ZOOM_MAX: f32 = 8.0;

/// 載入順序：切換很快時，丟掉較舊的載入結果
static LOAD_SEQ: AtomicU64 = AtomicU64::new(0);
/// 音量在開啟之間保留
static VOLUME: AtomicU64 = AtomicU64::new(100);
/// 播放速度（×100）在開啟之間保留
static RATE: AtomicU64 = AtomicU64::new(100);
/// 可選的播放速度
const RATES: [f64; 5] = [0.5, 1.0, 1.25, 1.5, 2.0];

pub struct Viewer {
    /// 上一個 / 下一個切換的清單（同類：截圖或錄影，最新的在前）
    list: Vec<LibraryEntry>,
    idx: usize,
    /// 圖片：材質、原始尺寸
    image: Option<(TextureHandle, Vec2)>,
    /// 縮放倍率（None：適合視窗）、平移（畫面中心的偏移，點）
    zoom: Option<f32>,
    pan: Vec2,
    /// 影片
    player: Option<Player>,
    video_tex: Option<TextureHandle>,
    video_size: Vec2,
    /// 拖曳進度列時暫停，放開後若原本在播放就繼續
    seeking: Option<bool>,
    volume: f32,
    muted: bool,
    loading: bool,
    error: Option<String>,
    close: bool,
    /// 影片從這裡開始播（搜尋說過的話時跳到那一句）
    start_at: Option<f64>,
    /// 全螢幕（只顯示畫面與下方的控制列）
    full: bool,
    /// 改名的對話框關掉後重新載入（改名前先放開檔案，Windows 上播放中的檔案不能改名）
    reload_after_ask: bool,
    /// 截圖：相關的截圖（同一個程式、時間接近、有共同的詞、長得像）
    related: Vec<MediaInfo>,
}

impl Viewer {
    pub(crate) fn cur(&self) -> &LibraryEntry {
        &self.list[self.idx]
    }

    fn is_image(&self) -> bool {
        is_image_name(&self.cur().media.name)
    }

    /// 只有聲音（M4A）
    fn is_audio(&self) -> bool {
        is_audio_name(&self.cur().media.name)
    }

    fn clear_media(&mut self) {
        self.image = None;
        self.player = None;
        self.video_tex = None;
        self.video_size = Vec2::ZERO;
        self.zoom = None;
        self.pan = Vec2::ZERO;
        self.seeking = None;
        self.error = None;
    }

    pub fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
    }

    fn volume_now(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }
}

/// 要換掉檔案之前（設封面、改影片資訊）：檢視器正在播這支就先放開（Windows 上播放中的檔案換不掉），回傳播到哪裡
pub fn release(app: &mut UiApp, path: &str) -> Option<f64> {
    let v = app.viewer.as_mut().filter(|v| v.cur().media.path == path)?;
    let t = v.player.as_ref().map(|p| p.time()).unwrap_or(0.0);
    v.clear_media();
    v.loading = true;
    Some(t)
}

/// 換好之後：檢視器還在這支就從 t 秒重新載入（release 有放開才會有 t）
pub fn reload(app: &mut UiApp, path: &str, t: Option<f64>) {
    let Some(t) = t else { return };
    let Some(v) = app.viewer.as_mut().filter(|v| v.cur().media.path == path) else { return };
    // load_current 會往前 0.5 秒開始播；太接近結尾時往前退（停在最後會沒有畫面）
    let end = v.cur().media.duration_sec.unwrap_or(f64::MAX);
    v.start_at = Some((t + 0.5).min(end - 1.0).max(0.0));
    load_current(app);
}

/// 開啟檢視器；同類的檔案（截圖或錄影）在背景載入，供上一個 / 下一個切換
/// 開啟影片並從 t 秒開始播放（搜尋到的說過的話）
pub fn open_at(app: &mut UiApp, entry: LibraryEntry, t: f64) {
    open(app, entry);
    if let Some(v) = &mut app.viewer {
        v.start_at = Some(t.max(0.0));
    }
}

pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    let shots = is_image_name(&entry.media.name);
    let path = entry.media.path.clone();
    app.viewer = Some(Viewer {
        list: vec![entry],
        idx: 0,
        image: None,
        zoom: None,
        pan: Vec2::ZERO,
        player: None,
        video_tex: None,
        video_size: Vec2::ZERO,
        seeking: None,
        volume: VOLUME.load(Ordering::Relaxed) as f32 / 100.0,
        muted: false,
        loading: true,
        error: None,
        close: false,
        start_at: None,
        full: false,
        reload_after_ask: false,
        related: vec![],
    });
    load_current(app);
    let core = app.core.clone();
    // 同一個資料夾裡的（全部錄影也可以看之前用過的資料夾）
    let dir = std::path::Path::new(&path).parent().map(|p| p.display().to_string()).filter(|d| !d.is_empty()).unwrap_or_else(|| app.s.out_dir(&app.env));
    let query = LibraryQuery { filter: if shots { LibraryFilter::Shot } else { LibraryFilter::All }, sort: LibrarySort::New, page_size: Some(200.0), audio: true, ..Default::default() };
    app.spawn(async move { actions::library(&core, &dir, &query).await }, move |app, page| {
        let Some(v) = app.viewer.as_mut() else { return };
        // 加速版 / GIF 不在清單裡（列在原片底下）：只看這一個
        if let Some(i) = page.items.iter().position(|e| e.media.path == path) {
            let cur = v.list[v.idx].clone();
            v.list = page.items;
            v.idx = i;
            // 保留已讀到的完整資訊（長度、fps）
            v.list[i] = cur;
        }
    });
}

/// 在背景找和這張相關的截圖（截圖資料夾、留在原處的舊截圖、這張所在的資料夾）
fn load_related(app: &mut UiApp, path: &str) {
    if let Some(v) = app.viewer.as_mut() {
        v.related.clear();
    }
    let mut dirs: Vec<String> = std::iter::once(app.s.shots_dir(&app.env)).chain(app.s.shot_extra(&app.env)).collect();
    if let Some(parent) = std::path::Path::new(path).parent().map(|p| p.display().to_string()) {
        if !dirs.iter().any(|d| d.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(parent.trim_end_matches(['\\', '/']))) {
            dirs.push(parent);
        }
    }
    let (core, p, q) = (app.core.clone(), path.to_string(), path.to_string());
    app.spawn(async move { actions::related_shots(&core, &q, dirs, 12).await }, move |app, list| {
        if let Some(v) = app.viewer.as_mut().filter(|v| v.cur().media.path == p) {
            v.related = list;
        }
    });
}

/// 載入目前這一個（圖片解碼、影片讀取資訊後開始播放）
fn load_current(app: &mut UiApp) {
    let Some(v) = app.viewer.as_mut() else { return };
    v.clear_media();
    v.loading = true;
    let e = v.cur().clone();
    let seq = LOAD_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    if is_image_name(&e.media.name) {
        load_related(app, &e.media.path);
        let max_side = app.ctx.input(|i| i.max_texture_side).max(2048);
        let path = e.media.path.clone();
        app.spawn(async move { tokio::task::spawn_blocking(move || decode_image(&path, max_side)).await.ok().flatten() }, move |app, img| {
            if LOAD_SEQ.load(Ordering::Relaxed) != seq {
                return;
            }
            let ctx = app.ctx.clone();
            let Some(v) = app.viewer.as_mut() else { return };
            v.loading = false;
            match img {
                // 放大時看得清每個像素；縮小時用 mipmap 避免細字糊成一團
                Some((img, size)) => {
                    let opt = TextureOptions { magnification: egui::TextureFilter::Nearest, mipmap_mode: Some(egui::TextureFilter::Linear), ..TextureOptions::LINEAR };
                    v.image = Some((ctx.load_texture("viewer-image", img, opt), size));
                }
                None => v.error = Some(tr!("無法讀取這張圖片", "Couldn't read this image").into()),
            }
        });
        return;
    }
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        v.loading = false;
        v.error = Some(tr!("找不到 FFmpeg，無法播放", "FFmpeg not found. Can't play this video.").into());
        return;
    };
    let core = app.core.clone();
    let has_info = e.media.duration_sec.is_some_and(|d| d > 0.0);
    app.spawn(
        async move {
            if has_info {
                return Some(e.media);
            }
            core.cache.probe(&ffmpeg, &e.media.path).await.ok()
        },
        move |app, media| {
            if LOAD_SEQ.load(Ordering::Relaxed) != seq {
                return;
            }
            let Some(ffmpeg) = app.core.ffmpeg_path() else { return };
            let ctx = app.ctx.clone();
            let Some(v) = app.viewer.as_mut() else { return };
            v.loading = false;
            let Some(m) = media.filter(|m| m.duration_sec.is_some_and(|d| d > 0.0)) else {
                v.error = Some(tr!("無法讀取這個影片", "Couldn't read this video").into());
                return;
            };
            v.list[v.idx].media = m.clone();
            let spec = MediaSpec { path: m.path.clone(), duration: m.duration_sec.unwrap_or(0.0), fps: m.fps.filter(|f| *f > 0.0).unwrap_or(30.0), has_audio: m.has_audio == Some(true) };
            let (w, h) = (m.width.unwrap_or(1280), m.height.unwrap_or(720));
            let c = ctx.clone();
            let wake = Arc::new(move || c.request_repaint());
            let player = if is_audio_name(&m.name) { Player::new_audio(ffmpeg, spec, wake) } else { Player::new(ffmpeg, spec, w, h, DECODE_MAX.0, DECODE_MAX.1, wake) };
            player.set_volume(v.volume_now());
            player.set_rate(RATE.load(Ordering::Relaxed) as f64 / 100.0);
            // 從說過的話開始：往前一點，聽得到整句
            player.seek(v.start_at.take().map(|t| (t - 0.5).max(0.0)).unwrap_or(0.0));
            player.play();
            v.video_size = vec2(w as f32, h as f32);
            v.player = Some(player);
        },
    );
}

/// 讀取截圖（PNG、JPG）；超過顯示卡材質上限時縮小
fn decode_image(path: &str, max_side: usize) -> Option<(egui::ColorImage, Vec2)> {
    let bytes = std::fs::read(path).ok()?;
    let pm = screenrecorder_core::picture::decode_pixmap(&bytes)?;
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let size = vec2(w as f32, h as f32);
    let k = w.max(h).div_ceil(max_side).max(1);
    if k == 1 {
        return Some((egui::ColorImage::from_rgba_premultiplied([w, h], pm.data()), size));
    }
    // 每 k×k 取平均
    let (nw, nh) = (w / k, h / k);
    let src = pm.data();
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for dy in 0..k {
                let row = (y * k + dy) * w;
                for dx in 0..k {
                    let i = (row + x * k + dx) * 4;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                }
            }
            let o = (y * nw + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / (k * k) as u32) as u8;
            }
        }
    }
    Some((egui::ColorImage::from_rgba_premultiplied([nw, nh], &out), size))
}

fn go(app: &mut UiApp, step: i64) {
    let Some(v) = app.viewer.as_mut() else { return };
    let n = v.list.len() as i64;
    let i = v.idx as i64 + step;
    if n <= 1 || i < 0 || i >= n {
        return;
    }
    v.idx = i as usize;
    load_current(app);
}

/// 移到資源回收筒（錄影連同底下的加速版、GIF），接著顯示下一個
fn delete_current(app: &mut UiApp) {
    let Some(v) = &app.viewer else { return };
    let e = v.cur().clone();
    let mut paths = vec![e.media.path.clone()];
    paths.extend(e.exports.iter().map(|x| x.media.path.clone()));
    let mut ask = Ask::confirm(
        if paths.len() > 1 {
            let n = paths.len() - 1;
            if screenrecorder_core::i18n::is_en() {
                format!("Move this recording and its {n} sped-up version{} to the Recycle Bin?", if n == 1 { "" } else { "s" })
            } else {
                format!("把這部錄影和底下的 {n} 個加速版移到資源回收筒？")
            }
        } else {
            tr!("把這個檔案移到資源回收筒？", "Move this file to the Recycle Bin?").into()
        },
        tr!("可從資源回收筒還原。", "You can restore files from the Recycle Bin."),
        tr!("移到資源回收筒", "Move to Recycle Bin"),
        move |app, _| {
            app.ask = None;
            // 先停止播放（Windows 上檔案使用中無法刪除）
            if let Some(v) = app.viewer.as_mut() {
                v.clear_media();
            }
            match actions::delete(&app.core, &paths) {
                Ok(n) => {
                    app.toast(
                        if screenrecorder_core::i18n::is_en() {
                            format!("Moved {n} file{} to the Recycle Bin", if n == 1 { "" } else { "s" })
                        } else {
                            format!("已將 {n} 個檔案移到資源回收筒")
                        },
                        false,
                    );
                    if let Some(d) = &mut app.library {
                        d.dirty = true;
                    }
                    app.load_recent();
                    let Some(v) = app.viewer.as_mut() else { return };
                    v.list.retain(|x| x.media.path != paths[0]);
                    if v.list.is_empty() {
                        app.viewer = None;
                        return;
                    }
                    v.idx = v.idx.min(v.list.len() - 1);
                }
                Err(err) => app.toast(err.message().to_string(), true),
            }
            load_current(app);
        },
    );
    ask.danger = true;
    ask.list = std::iter::once(e.media.name.clone()).chain(e.exports.iter().map(|x| x.media.name.clone())).collect();
    app.ask = Some(ask);
}

fn copy_image(app: &mut UiApp) {
    let Some(v) = &app.viewer else { return };
    let path = v.cur().media.path.clone();
    copy_png(app, path);
}

/// 截圖（PNG）複製到剪貼簿（圖片，可以直接貼到 LINE、Word）
pub fn copy_png(app: &mut UiApp, path: String) {
    app.toast(tr!("正在複製…", "Copying…"), false);
    app.spawn(async move { tokio::task::spawn_blocking(move || screenrecorder_core::clipboard::copy_png(std::path::Path::new(&path))).await.map(|r| r.2).unwrap_or(false) }, |app, ok| {
        if ok {
            app.toast(tr!("已複製到剪貼簿", "Copied to clipboard"), false);
        } else {
            app.toast(tr!("無法複製到剪貼簿", "Couldn't copy to clipboard"), true);
        }
    });
}

enum Act {
    None,
    Prev,
    Next,
    Entry(EntryAction),
    Copy,
    Delete,
    Ocr,
    Pin,
    /// 影片：目前這一格存成截圖
    Grab,
    /// 影片：從目前這一格做封面
    Cover,
    /// 影片：目前這一格寫進影片當縮圖（檔案總管看到的那一張）
    SetThumb,
    /// 影片：改標題、作者、說明等影片資訊
    Tags,
    Rename,
    /// 打開相關的截圖（第幾張）
    Related(usize),
}

/// 標題列的一個按鈕；視窗窄時依序從後面的開始收進「⋯」
struct HeadBtn {
    label: &'static str,
    icon: Option<Icon>,
    tip: String,
    act: fn() -> Act,
    /// 現在不能用的原因（轉檔進行中等）
    blocked: Option<String>,
}

/// 標題列上的按鈕（由左到右；越前面越重要）
fn head_buttons(app: &UiApp, e: &LibraryEntry, image: bool, audio: bool) -> Vec<HeadBtn> {
    let b = |label, icon, tip: &str, act: fn() -> Act| HeadBtn { label, icon, tip: tip.to_string(), act, blocked: None };
    let open_tip = if image { tr!("用 Windows 的「相片」開啟", "Open in the Windows Photos app") } else { tr!("用 Windows 的媒體播放器開啟", "Open in the Windows media player") };
    let open = b(tr!("用 Windows 開啟", "Open in Windows"), Some(Icon::Export), open_tip, || Act::Entry(EntryAction::External));
    if image {
        return vec![
            b(
                tr!("編輯", "Edit"),
                Some(Icon::Edit),
                tr!("加上標註、遮住個資、裁切（另存一張，原圖保留）", "Add annotations, redact personal info, crop (saved as a new image; the original is kept)"),
                || Act::Entry(EntryAction::Edit),
            ),
            b(
                tr!("複製", "Copy"),
                Some(Icon::Copy),
                tr!("複製到剪貼簿（Ctrl+C；可直接貼到 LINE、Word、信件）", "Copy to clipboard (Ctrl+C; paste it straight into LINE, Word or an email)"),
                || Act::Copy,
            ),
            b(tr!("文字辨識", "OCR"), None, tr!("把圖裡的文字轉成可以複製的文字", "Text recognition (OCR): turn the text in the image into text you can copy"), || Act::Ocr),
            b(
                tr!("釘在桌面", "Pin to desktop"),
                Some(Icon::Pin),
                tr!("把這張圖變成浮在最上層的小視窗，方便對照（點兩下或 Esc 關閉）", "Turn this image into a small always-on-top window for reference (double-click or Esc to close)"),
                || Act::Pin,
            ),
            open,
        ];
    }
    let mut list = vec![];
    if !audio && !is_export_name(&e.media.name) {
        let gate = |mut x: HeadBtn, a: EntryAction| {
            x.blocked = app.blocked(a);
            x
        };
        list.push(gate(b(tr!("剪輯", "Edit"), Some(Icon::Cut), tr!("剪掉頭尾、刪除片段、加上標註", "Trim, cut out parts, add annotations"), || Act::Entry(EntryAction::Edit)), EntryAction::Edit));
        list.push(gate(
            b(tr!("製作加速版 / GIF", "Make sped-up video / GIF"), Some(Icon::Fast), tr!("另存加速版、GIF 或 WebP 動圖", "Save a sped-up version, GIF or WebP"), || {
                Act::Entry(EntryAction::Export)
            }),
            EntryAction::Export,
        ));
        list.push(gate(
            b(tr!("壓縮", "Compress"), None, tr!("壓縮到 LINE、Email 傳得出去的大小（原速）", "Compress to a size you can send via LINE or email (original speed)"), || {
                Act::Entry(EntryAction::Compress)
            }),
            EntryAction::Compress,
        ));
    }
    if !audio && screenrecorder_core::cover::can_embed(&e.media.path) {
        list.push(b(
            tr!("設為封面", "Set as cover"),
            Some(Icon::Image),
            tr!(
                "把目前這一格設為這支影片的封面：檔案總管和多數播放器看到的縮圖就是這一格（影片內容不變，不會移到開頭）",
                "Make the current frame this video's cover: File Explorer and most players show it as the thumbnail (the video itself is unchanged; the frame isn't moved to the start)"
            ),
            || Act::SetThumb,
        ));
        list.push(b(
            tr!("影片資訊", "Details"),
            Some(Icon::Edit),
            tr!(
                "改寫在影片檔裡的標題、作者、說明等（檔案總管「內容 → 詳細資料」看得到；影片內容不變）",
                "Edit the title, author, description and more stored in the video file (shown in File Explorer under Properties → Details; the video itself is unchanged)"
            ),
            || Act::Tags,
        ));
    }
    if !audio {
        list.push(b(
            tr!("擷取這一格", "Grab frame"),
            Some(Icon::Camera),
            tr!("把目前這一格存成截圖（原尺寸 PNG），並複製到剪貼簿", "Save the current frame as a screenshot (full-size PNG) and copy it to the clipboard"),
            || Act::Grab,
        ));
        list.push(b(
            tr!("做 YouTube 封面圖", "Make YouTube cover"),
            None,
            tr!("用目前這一格加上標題，做成 YouTube 用的封面圖（1280×720）", "Use the current frame with a title to make a YouTube cover image (1280×720)"),
            || Act::Cover,
        ));
    }
    list.push(b(
        tr!("複製檔案", "Copy file"),
        Some(Icon::Copy),
        tr!("複製這個檔案，可以直接貼到 LINE、Teams、信件或資料夾", "Copy this file so you can paste it into LINE, Teams, an email or a folder"),
        || Act::Entry(EntryAction::CopyFile),
    ));
    list.push(open);
    list
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut v) = app.viewer.take() else { return };
    if let Some(f) = v.player.as_ref().and_then(|p| p.take_frame()) {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
        match &mut v.video_tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => v.video_tex = Some(ctx.load_texture("viewer-video", img, TextureOptions::LINEAR)),
        }
    }
    if let Some(p) = &v.player {
        let _ = p.take_ended();
        // 新的一格到了解碼器會叫畫面更新；這裡只讓時間順順地走
        if p.is_playing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
    // 改名的對話框關掉了：重新載入（改了名就是新的路徑）
    if v.reload_after_ask && app.ask.is_none() {
        v.reload_after_ask = false;
        app.viewer = Some(v);
        load_current(app);
        let Some(v2) = app.viewer.take() else { return };
        v = v2;
    }
    let mut act = Act::None;
    if app.ask.is_none() {
        // 全螢幕時 Esc 先離開全螢幕（不關掉檢視器）
        if v.full && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            set_full(&mut v, ctx, false);
        }
        keyboard(&mut v, ctx, &mut act);
    }
    let e = v.cur().clone();
    let buttons = head_buttons(app, &e, v.is_image(), v.is_audio());

    let screen = ctx.content_rect();
    let (size, frame, area) = if v.full {
        let area = egui::Modal::default_area(Id::new("viewer")).anchor(Align2::LEFT_TOP, vec2(0.0, 0.0));
        (screen.size(), theme::modal_frame(ctx).inner_margin(0).corner_radius(0).stroke(Stroke::NONE).shadow(egui::Shadow::NONE), area)
    } else {
        (vec2((screen.width() - 32.0).max(600.0), (screen.height() - 32.0).max(400.0)), theme::modal_frame(ctx).inner_margin(0), egui::Modal::default_area(Id::new("viewer")))
    };
    let rel_thumbs: Vec<Option<TextureHandle>> = v.related.iter().take(16).map(|m| app.thumb(&m.path, m.mtime)).collect();
    let modal = egui::Modal::new(Id::new("viewer")).area(area).frame(frame).show(ctx, |ui| {
        ui.set_width(size.x);
        ui.set_height(size.y);
        let p = theme::pal(ui);
        let image = v.is_image();
        let audio = v.is_audio();
        // 標題列（全螢幕時不顯示）
        if !v.full {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| header(&mut v, ui, &e, &buttons, &mut act));
        }
        // 畫面（截圖下方留一列相關的截圖）
        let bar_h = 52.0;
        let strip_h = if image && !v.full && !v.related.is_empty() { 104.0 } else { 0.0 };
        let full = ui.available_rect_before_wrap();
        let stage = Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - bar_h - strip_h));
        let resp = ui.allocate_rect(stage, Sense::click_and_drag());
        let painter = ui.painter_at(stage);
        painter.rect_filled(stage, CornerRadius::ZERO, Color32::from_gray(24));
        if image {
            image_stage(&mut v, ui, &painter, stage, &resp);
        } else {
            video_stage(&mut v, &painter, stage, &resp, audio);
        }
        // 點兩下畫面：全螢幕
        if !image && resp.double_clicked() {
            let on = !v.full;
            set_full(&mut v, ui.ctx(), on);
        }
        if v.loading && v.image.is_none() && v.video_tex.is_none() && !(audio && v.player.is_some()) {
            painter.text(stage.center(), Align2::CENTER_CENTER, tr!("讀取中…", "Loading…"), theme::font(14.0), Color32::from_gray(200));
        }
        if let Some(err) = &v.error {
            painter.text(stage.center(), Align2::CENTER_CENTER, err, theme::font(14.0), Color32::from_gray(220));
        }
        // 上一個 / 下一個
        if v.list.len() > 1 {
            let hover = ui.rect_contains_pointer(stage);
            for (step, icon, x) in [(-1i64, Icon::ChevL, stage.min.x + 34.0), (1, Icon::ChevR, stage.max.x - 34.0)] {
                let enabled = if step < 0 { v.idx > 0 } else { v.idx + 1 < v.list.len() };
                if !enabled {
                    continue;
                }
                let r = Rect::from_center_size(pos2(x, stage.center().y), vec2(44.0, 44.0));
                let b = ui.interact(r, Id::new(("viewer-nav", step)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                if hover || b.hovered() {
                    let a = if b.hovered() { 200 } else { 140 };
                    ui.painter().circle_filled(r.center(), 22.0, Color32::from_black_alpha(a));
                    theme::paint_icon(ui.painter(), Rect::from_center_size(r.center(), vec2(20.0, 20.0)), icon, Color32::WHITE);
                }
                b.clone().on_hover_text(match (image, step < 0) {
                    (true, true) => tr!("上一張（←）", "Previous image (←)"),
                    (true, false) => tr!("下一張（→）", "Next image (→)"),
                    (false, true) => tr!("上一部（PageUp）", "Previous video (PageUp)"),
                    (false, false) => tr!("下一部（PageDown）", "Next video (PageDown)"),
                });
                if b.clicked() {
                    act = if step < 0 { Act::Prev } else { Act::Next };
                }
            }
        }
        if strip_h > 0.0 {
            let strip = Rect::from_min_max(pos2(full.min.x, stage.max.y), pos2(full.max.x, stage.max.y + strip_h));
            if let Some(i) = related_strip(&v, ui, strip, &rel_thumbs) {
                act = Act::Related(i);
            }
        }
        // 下方控制列
        let bar = Rect::from_min_max(pos2(full.min.x, full.max.y - bar_h), full.max);
        ui.painter().hline(bar.x_range(), bar.min.y, Stroke::new(1.0, p.border));
        let bui = &mut ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(16.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
        if image {
            image_bar(&mut v, bui, stage);
        } else {
            video_bar(&mut v, bui, &mut act);
        }
    });
    if modal.should_close() && app.ask.is_none() {
        v.close = true;
    }
    if v.close {
        if v.full {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        return;
    }
    app.viewer = Some(v);
    run_act(app, act);
}

/// 相關的截圖：一列小縮圖（同一個程式、時間接近、有共同的詞、長得像）；回傳點了第幾張
fn related_strip(v: &Viewer, ui: &mut egui::Ui, strip: Rect, thumbs: &[Option<TextureHandle>]) -> Option<usize> {
    let p = theme::pal(ui);
    ui.painter().hline(strip.x_range(), strip.min.y, Stroke::new(1.0, p.border));
    ui.painter().text(
        pos2(strip.min.x + 16.0, strip.min.y + 12.0),
        Align2::LEFT_CENTER,
        tr!("相關的截圖（同一個程式、時間接近、內容相近）", "Related screenshots (same app, around the same time, similar content)"),
        theme::font(11.5),
        p.muted,
    );
    let (tw, th, gap) = (112.0, 63.0, 8.0);
    let mut x = strip.min.x + 16.0;
    let y = strip.min.y + 24.0;
    let mut pick = None;
    for (i, m) in v.related.iter().enumerate() {
        if x + tw > strip.max.x - 16.0 {
            break;
        }
        let r = Rect::from_min_size(pos2(x, y), vec2(tw, th));
        let resp = ui.interact(r, Id::new(("viewer-related", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(&m.name);
        ui.painter().rect_filled(r, CornerRadius::same(4), p.surface2);
        if let Some(Some(t)) = thumbs.get(i) {
            theme::paint_thumb(ui.painter(), r.shrink(2.0), t);
        }
        let border = if resp.hovered() { Stroke::new(2.0, p.accent) } else { Stroke::new(1.0, p.border) };
        ui.painter().rect_stroke(r, CornerRadius::same(4), border, egui::StrokeKind::Inside);
        if resp.clicked() {
            pick = Some(i);
        }
        x += tw + gap;
    }
    pick
}

/// 進入 / 離開全螢幕（整個視窗全螢幕，檢視器蓋滿）
fn set_full(v: &mut Viewer, ctx: &egui::Context, on: bool) {
    if v.full != on {
        v.full = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }
}

/// 標題列：左邊名稱與資訊（太長時截斷），右邊按鈕；視窗窄時不常用的按鈕收進「⋯ 更多」
fn header(v: &mut Viewer, ui: &mut egui::Ui, e: &LibraryEntry, buttons: &[HeadBtn], act: &mut Act) {
    let p = theme::pal(ui);
    let image = v.is_image();
    let audio = v.is_audio();
    let head = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 40.0));
    // 先算右邊放得下幾個按鈕（關閉、資源回收筒、資料夾、⋯ 固定在最右邊；左邊至少留 260 給名稱）
    let gap = ui.spacing().item_spacing.x;
    let fixed = 34.0 + 6.0 + 3.0 * (28.0 + gap) + 8.0;
    let mut room = head.width() - fixed - 260.0;
    let mut shown = 0;
    for b in buttons {
        let mut btn = Btn::new(b.label).small();
        if let Some(i) = b.icon {
            btn = btn.icon(i);
        }
        let w = btn.width(ui) + gap;
        if w > room {
            break;
        }
        room -= w;
        shown += 1;
    }
    let rui = &mut ui.new_child(egui::UiBuilder::new().max_rect(head).layout(Layout::right_to_left(Align::Center)));
    if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(rui).clicked() {
        v.close = true;
    }
    rui.add_space(6.0);
    if Btn::icon_only(Icon::Trash).ghost().small().tooltip(tr!("移到資源回收筒（Delete）", "Move to Recycle Bin (Delete)")).show(rui).clicked() {
        *act = Act::Delete;
    }
    if Btn::icon_only(Icon::Folder).ghost().small().tooltip(tr!("在資料夾中顯示", "Show in folder")).show(rui).clicked() {
        *act = Act::Entry(EntryAction::Reveal);
    }
    // 「⋯ 更多」：收起來的按鈕，加上改名、存成 M4A、複製章節、做成直式影片
    let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("更多", "More")).show(rui);
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(220.0);
        for b in &buttons[shown..] {
            let r = ui.add_enabled(b.blocked.is_none(), egui::Button::new(b.label)).on_hover_text(&b.tip);
            let r = match &b.blocked {
                Some(why) => r.on_disabled_hover_text(why),
                None => r,
            };
            if r.clicked() {
                *act = (b.act)();
            }
        }
        if shown < buttons.len() {
            ui.separator();
        }
        let export = is_export_name(&e.media.name);
        if !image && !audio && !export && e.media.has_audio != Some(false) && ui.button(tr!("存成 M4A（只留聲音）", "Save as M4A (audio only)")).clicked() {
            *act = Act::Entry(EntryAction::SaveAudio);
        }
        if !e.media.chapters.is_empty() && ui.button(tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)")).clicked() {
            *act = Act::Entry(EntryAction::CopyChapters);
        }
        if !image && !audio && !export {
            let r = ui.add_enabled(v_blocked_vertical(buttons), egui::Button::new(tr!("做成直式影片（Shorts / Reels）…", "Make a vertical video (Shorts / Reels)…")));
            if r.clicked() {
                *act = Act::Entry(EntryAction::Vertical);
            }
        }
        if !audio && !export && ui.button(tr!("重新命名…", "Rename…")).clicked() {
            *act = Act::Rename;
        }
    });
    for b in buttons[..shown].iter().rev() {
        let mut btn = Btn::new(b.label).small().enabled(b.blocked.is_none()).tooltip(b.blocked.clone().unwrap_or_else(|| b.tip.clone()));
        if let Some(i) = b.icon {
            btn = btn.icon(i);
        }
        if btn.show(rui).clicked() {
            *act = (b.act)();
        }
    }
    let right_w = rui.min_rect().width();
    let left = Rect::from_min_max(head.min, pos2(head.max.x - right_w - 16.0, head.max.y));
    let lui = &mut ui.new_child(egui::UiBuilder::new().max_rect(left).layout(Layout::top_down(Align::Min)));
    lui.spacing_mut().item_spacing.y = 2.0;
    let title = if is_default_name(&e.media.name) { short_date(&e.media.name, e.media.time_ms(), true) } else { base_name(&e.media.name) };
    lui.add(egui::Label::new(RichText::new(title).font(theme::font_bold(16.0))).truncate());
    let mut meta = vec![];
    if v.list.len() > 1 {
        meta.push(format!("{} / {}", v.idx + 1, v.list.len()));
    }
    if let (Some(w), Some(h)) = (e.media.width, e.media.height) {
        meta.push(format!("{w}×{h}"));
    }
    if !image {
        if let Some(d) = e.media.duration_sec {
            meta.push(short_clock(d));
        }
    }
    meta.push(format_bytes(e.media.bytes));
    meta.push(e.media.name.clone());
    let meta = meta.join(tr!("・", " · "));
    lui.add(egui::Label::new(RichText::new(&meta).font(theme::font(12.0)).color(p.muted)).truncate()).on_hover_text(&meta);
    ui.advance_cursor_after_rect(head);
}

/// 做成直式影片跟剪輯一樣：轉檔進行中、錄影中不能用
fn v_blocked_vertical(buttons: &[HeadBtn]) -> bool {
    buttons.iter().find(|b| b.label == tr!("剪輯", "Edit")).is_none_or(|b| b.blocked.is_none())
}

/// 處理按下的按鈕、選單、快速鍵
fn run_act(app: &mut UiApp, act: Act) {
    match act {
        Act::None => {}
        Act::Prev => go(app, -1),
        Act::Related(i) => {
            let Some(v) = app.viewer.as_mut() else { return };
            let Some(m) = v.related.get(i).cloned() else { return };
            match v.list.iter().position(|e| e.media.path == m.path) {
                Some(j) => v.idx = j,
                None => {
                    let at = v.idx + 1;
                    v.list.insert(at, LibraryEntry { media: m, exports: vec![], hits: vec![] });
                    v.idx = at;
                }
            }
            load_current(app);
        }
        Act::Next => go(app, 1),
        Act::Copy => copy_image(app),
        Act::Delete => delete_current(app),
        Act::Rename => {
            let Some(v) = app.viewer.as_mut() else { return };
            let e = v.cur().clone();
            // 先停止播放、放開檔案（Windows 上播放中的檔案不能改名），對話框關掉後重新載入
            if !v.is_image() {
                v.clear_media();
                v.reload_after_ask = true;
            }
            super::library_dialog::rename_entry(app, e);
        }
        Act::Cover => {
            let Some(v) = app.viewer.as_mut() else { return };
            let (entry, t) = (v.cur().clone(), v.player.as_ref().map(|p| p.time()));
            if let Some(p) = &mut v.player {
                p.pause();
            }
            super::cover::open(app, entry, t);
        }
        Act::SetThumb => {
            let Some(v) = app.viewer.as_ref() else { return };
            let path = v.cur().media.path.clone();
            // 先停止播放、放開檔案，寫好再從同一格重新載入
            let t = release(app, &path);
            let ff = app.core.ffmpeg_path();
            let p2 = path.clone();
            app.spawn(async move { screenrecorder_core::cover::set_thumbnail(ff.as_deref(), &p2, t.unwrap_or(0.0)).await }, move |app, r| {
                match r {
                    Ok(()) => {
                        app.thumbs.forget(&path);
                        app.toast(tr!("已設為封面：檔案總管看到的縮圖就是這一格", "Set as cover: File Explorer now shows this frame as the thumbnail"), false);
                        app.sync_edited(&path);
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                }
                reload(app, &path, t);
            });
        }
        Act::Tags => {
            let Some(v) = app.viewer.as_mut() else { return };
            let e = v.cur().clone();
            v.pause();
            super::video_tags::open(app, e);
        }
        Act::Grab => {
            let Some(v) = app.viewer.as_mut() else { return };
            let (path, t) = (v.cur().media.path.clone(), v.player.as_ref().map(|p| p.time()).unwrap_or(0.0));
            let core = app.core.clone();
            let shot_dir = app.s.shots_dir(&app.env);
            app.spawn(async move { core.grab_frame(&path, t, &shot_dir).await }, |app, r| match r {
                Ok(shot) => {
                    // 狀態更新的「已截圖」不用再顯示一次：這裡的提示有「開啟」「在資料夾中顯示」
                    app.last_shot_seq = app.last_shot_seq.max(shot.seq);
                    let copied = if shot.copied { tr!("，並複製到剪貼簿", " and copied to the clipboard") } else { "" };
                    app.toast_file(trf!("已擷取這一格：{}{copied}", "Frame saved: {}{copied}", super::dialogs::file_name(&shot.path)), &shot.path);
                    app.shots_changed();
                }
                Err(e) => app.toast(e.message().to_string(), true),
            });
        }
        Act::Ocr | Act::Pin => {
            let Some(path) = app.viewer.as_ref().map(|v| v.cur().media.path.clone()) else { return };
            if matches!(act, Act::Ocr) {
                super::ocr::start(app, path);
            } else {
                app.spawn(async move { screenrecorder_core::actions::shot_pin(&path, &Default::default()).await }, |app, r| {
                    if let Err(e) = r {
                        app.toast(e.message().to_string(), true);
                    }
                });
            }
        }
        Act::Entry(a) => {
            let blocked = app.blocked(a);
            let Some(v) = app.viewer.as_mut() else { return };
            let e = v.cur().clone();
            if matches!(a, EntryAction::Edit | EntryAction::Export | EntryAction::Compress | EntryAction::Vertical) && !super::dialogs::is_image(&e.media.path) {
                if let Some(why) = blocked {
                    return app.toast(why, true);
                }
                // 改到剪輯 / 製作視窗：關掉檢視器與清單（編輯截圖蓋在上面，關掉就回來）
                if v.full {
                    app.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
                app.viewer = None;
                app.library = None;
            } else if a == EntryAction::External {
                v.pause();
            }
            app.act(a, e);
        }
    }
}

/// 改名後：清單裡的路徑、檔名跟著改（底下的加速版也是），對話框關掉後用新的路徑重新載入
pub fn renamed(app: &mut UiApp, old: &str, new: &str) {
    let Some(v) = app.viewer.as_mut() else { return };
    let dir = std::path::Path::new(new).parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let (old_base, new_base) = (base_name(&super::dialogs::file_name(old)), base_name(&super::dialogs::file_name(new)));
    for e in v.list.iter_mut().filter(|e| e.media.path == old) {
        e.media.path = new.to_string();
        e.media.name = super::dialogs::file_name(new);
        for x in &mut e.exports {
            if let Some(rest) = x.media.name.strip_prefix(&old_base) {
                x.media.name = format!("{new_base}{rest}");
                x.media.path = dir.join(&x.media.name).display().to_string();
            }
        }
    }
}

fn keyboard(v: &mut Viewer, ctx: &egui::Context, act: &mut Act) {
    if ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
        return;
    }
    let image = v.is_image();
    // 全螢幕要在讀完輸入之後才切換（讀輸入時不能送視窗指令）
    let mut toggle_full = false;
    ctx.input(|i| {
        if i.key_pressed(Key::Delete) {
            *act = Act::Delete;
        }
        if image {
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::PageUp) {
                *act = Act::Prev;
            }
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::PageDown) {
                *act = Act::Next;
            }
            if i.key_pressed(Key::Num0) {
                v.zoom = None;
                v.pan = Vec2::ZERO;
            }
            if i.key_pressed(Key::Num1) {
                v.zoom = Some(1.0);
                v.pan = Vec2::ZERO;
            }
            // Ctrl+C：複製圖片
            let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy | egui::Event::Key { key: Key::C, pressed: true, modifiers: egui::Modifiers { command: true, .. }, .. }));
            if copy {
                *act = Act::Copy;
            }
            return;
        }
        if i.key_pressed(Key::PageUp) {
            *act = Act::Prev;
        }
        if i.key_pressed(Key::PageDown) {
            *act = Act::Next;
        }
        if i.key_pressed(Key::F) && !i.modifiers.any() {
            toggle_full = true;
        }
        let Some(p) = &v.player else { return };
        if i.key_pressed(Key::Space) {
            toggle_play(p);
        }
        let step = if i.modifiers.shift { 1.0 } else { 5.0 };
        if i.key_pressed(Key::ArrowLeft) {
            p.seek((p.time() - step).max(0.0));
        }
        if i.key_pressed(Key::ArrowRight) {
            p.seek(p.time() + step);
        }
        if i.key_pressed(Key::Home) {
            p.seek(0.0);
        }
        if i.key_pressed(Key::End) {
            p.seek((p.spec().duration - 0.1).max(0.0));
        }
        // M 靜音；↑↓ 音量（每次 10%）
        if p.spec().has_audio {
            if i.key_pressed(Key::M) {
                v.muted = !v.muted;
            }
            let dv = if i.key_pressed(Key::ArrowUp) {
                0.1
            } else if i.key_pressed(Key::ArrowDown) {
                -0.1
            } else {
                0.0
            };
            if dv != 0.0 {
                v.volume = ((v.volume + dv) * 10.0).round().clamp(0.0, 10.0) / 10.0;
                v.muted = false;
                VOLUME.store((v.volume * 100.0).round() as u64, Ordering::Relaxed);
            }
            p.set_volume(v.volume_now());
        }
    });
    if toggle_full {
        let on = !v.full;
        set_full(v, ctx, on);
    }
}

fn toggle_play(p: &Player) {
    if p.is_playing() {
        p.pause();
    } else {
        // 播完了：從頭播
        if p.time() >= p.spec().duration - 0.05 {
            p.seek(0.0);
        }
        p.play();
    }
}

/// 適合視窗時的倍率
fn fit_zoom(img: Vec2, area: Rect) -> f32 {
    ((area.width() - 24.0) / img.x).min((area.height() - 24.0) / img.y).clamp(ZOOM_MIN, 1.0)
}

fn image_stage(v: &mut Viewer, ui: &egui::Ui, painter: &egui::Painter, stage: Rect, resp: &egui::Response) {
    let Some((tex, size)) = v.image.clone() else { return };
    let fit = fit_zoom(size, stage);
    let z = v.zoom.unwrap_or(fit);
    // 滾輪：以游標為中心縮放
    if resp.hovered() {
        let scroll = ui.input(|i| super::editor::wheel_delta(i).y + i.zoom_delta().ln() * 300.0);
        if scroll.abs() > 0.1 {
            let nz = (z * (scroll / 300.0).exp()).clamp(ZOOM_MIN.min(fit), ZOOM_MAX);
            if let Some(m) = resp.hover_pos() {
                // 游標下的圖片位置不動
                let c = stage.center() + v.pan;
                v.pan += (m - c) * (1.0 - nz / z);
            }
            v.zoom = Some(nz);
        }
    }
    let z = v.zoom.unwrap_or(fit);
    let shown = size * z;
    if resp.dragged() && (shown.x > stage.width() || shown.y > stage.height()) {
        v.pan += resp.drag_delta();
    }
    if resp.double_clicked() {
        if v.zoom.is_some_and(|z| (z - 1.0).abs() < 0.001) || (v.zoom.is_none() && fit >= 1.0) {
            v.zoom = None;
            v.pan = Vec2::ZERO;
        } else {
            // 放大到原尺寸，點的地方留在游標下
            if let Some(m) = resp.interact_pointer_pos() {
                let c = stage.center() + v.pan;
                v.pan += (m - c) * (1.0 - 1.0 / z);
            }
            v.zoom = Some(1.0);
        }
    }
    let z = v.zoom.unwrap_or(fit);
    let shown = size * z;
    // 不讓圖片被拖出畫面
    let lim = ((shown - stage.size()) / 2.0).max(Vec2::ZERO);
    v.pan = v.pan.clamp(-lim, lim);
    if v.zoom.is_none() {
        v.pan = Vec2::ZERO;
    }
    let r = Rect::from_center_size(stage.center() + v.pan, shown);
    painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    if (shown.x > stage.width() || shown.y > stage.height()) && (resp.hovered() || resp.dragged()) {
        theme::hand_cursor(ui.ctx(), resp.dragged());
    }
}

fn image_bar(v: &mut Viewer, ui: &mut egui::Ui, stage: Rect) {
    let p = theme::pal(ui);
    let Some((_, size)) = v.image.clone() else { return };
    let fit = fit_zoom(size, stage);
    let z = v.zoom.unwrap_or(fit);
    ui.label(RichText::new(tr!("滾輪縮放・拖曳移動・點兩下切換原尺寸", "Scroll to zoom · Drag to pan · Double-click for actual size")).font(theme::font(12.0)).color(p.muted));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if Btn::new("+").small().tooltip(tr!("放大", "Zoom in")).show(ui).clicked() {
            v.zoom = Some((z * 1.25).min(ZOOM_MAX));
        }
        ui.label(RichText::new(format!("{:.0}%", z * 100.0)).font(theme::mono(12.5))).on_hover_text(tr!("目前的縮放", "Current zoom"));
        if Btn::new("−").small().tooltip(tr!("縮小", "Zoom out")).show(ui).clicked() {
            v.zoom = Some((z / 1.25).max(ZOOM_MIN.min(fit)));
        }
        ui.add_space(8.0);
        if Btn::new(tr!("原尺寸", "Actual size")).small().selected(v.zoom.is_some_and(|z| (z - 1.0).abs() < 0.001)).tooltip(tr!("1:1（按 1）", "1:1 (press 1)")).show(ui).clicked() {
            v.zoom = Some(1.0);
            v.pan = Vec2::ZERO;
        }
        if Btn::new(tr!("適合視窗", "Fit to window")).small().selected(v.zoom.is_none()).tooltip(tr!("整張放進視窗（按 0）", "Fit the whole image in the window (press 0)")).show(ui).clicked()
        {
            v.zoom = None;
            v.pan = Vec2::ZERO;
        }
    });
}

fn video_stage(v: &mut Viewer, painter: &egui::Painter, stage: Rect, resp: &egui::Response, audio: bool) {
    if audio {
        // 只有聲音：大大的喇叭與檔名
        let c = stage.center() - vec2(0.0, 16.0);
        painter.circle_filled(c, 56.0, Color32::from_gray(48));
        theme::paint_icon(painter, Rect::from_center_size(c, vec2(56.0, 56.0)), Icon::Speaker, Color32::from_gray(220));
        painter.text(c + vec2(0.0, 84.0), Align2::CENTER_CENTER, tr!("只有聲音", "Audio only"), theme::font(14.0), Color32::from_gray(200));
    } else if let Some(tex) = &v.video_tex {
        let src = if v.video_size.x > 0.0 { v.video_size } else { tex.size_vec2() };
        let k = ((stage.width() - 24.0) / src.x).min((stage.height() - 24.0) / src.y);
        let r = Rect::from_center_size(stage.center(), src * k);
        painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        return;
    }
    let Some(p) = &v.player else { return };
    // 點畫面：播放 / 暫停（點兩下：全螢幕）
    if resp.clicked() {
        toggle_play(p);
    }
    if !p.is_playing() && v.seeking.is_none() && !audio {
        painter.circle_filled(stage.center(), 34.0, Color32::from_black_alpha(150));
        theme::paint_icon(painter, Rect::from_center_size(stage.center() + vec2(3.0, 0.0), vec2(30.0, 30.0)), Icon::Play, Color32::WHITE);
    }
}

fn video_bar(v: &mut Viewer, ui: &mut egui::Ui, act: &mut Act) {
    let p = theme::pal(ui);
    let chapters = v.cur().media.chapters.clone();
    let full = v.full;
    let Some(player) = &v.player else { return };
    let playing = player.is_playing();
    let dur = player.spec().duration.max(0.001);
    let t = player.time().min(dur);
    let tip = if playing { tr!("暫停（空白鍵）", "Pause (Space)") } else { tr!("播放（空白鍵）", "Play (Space)") };
    if Btn::icon_only(if playing { Icon::Pause } else { Icon::Play }).small().tooltip(tip).show(ui).clicked() {
        toggle_play(player);
    }
    ui.label(RichText::new(format!("{} / {}", short_clock(t), short_clock(dur))).font(theme::mono(12.5)));
    // 速度、全螢幕、音量（右邊），進度列填滿中間
    let vol_w = 140.0;
    let chap_w = if chapters.is_empty() { 0.0 } else { 84.0 };
    let tools_w = 76.0 + 36.0;
    let track_w = (ui.available_width() - vol_w - chap_w - tools_w - 16.0).max(80.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(track_w, 28.0), Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 12.0, 6.0));
    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same(3), p.surface2);
    let frac = (t / dur) as f32;
    painter.rect_filled(Rect::from_min_max(track.min, pos2(track.min.x + track.width() * frac, track.max.y)), CornerRadius::same(3), p.accent);
    // 章節的分界（錄影時加的標記）
    for c in chapters.iter().filter(|c| c.start > 0.05 && c.start < dur) {
        let x = track.min.x + track.width() * (c.start / dur) as f32;
        painter.rect_filled(Rect::from_center_size(pos2(x, track.center().y), vec2(2.0, 12.0)), CornerRadius::ZERO, p.warn);
    }
    let knob = pos2(track.min.x + track.width() * frac, track.center().y);
    painter.circle(knob, if resp.hovered() || resp.dragged() { 8.0 } else { 6.0 }, p.surface, Stroke::new(2.0, p.accent));
    let to_t = |pos: Pos2| (((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64) * dur;
    if let Some(pos) = resp.hover_pos() {
        let at = to_t(pos);
        let when = match chapters.iter().rev().find(|c| c.start <= at) {
            Some(c) => format!("{}・{}", short_clock(at), c.title),
            None => short_clock(at),
        };
        let keys = tr!("←/→ 倒退/快轉 5 秒、空白鍵 播放/暫停", "←/→ back/forward 5 s, Space play/pause");
        resp.clone().on_hover_text_at_pointer(format!("{when}\n{keys}"));
    }
    if v.seeking.is_none() && resp.is_pointer_button_down_on() {
        v.seeking = Some(playing);
        player.pause();
    }
    if v.seeking.is_some() {
        if let Some(pos) = resp.interact_pointer_pos() {
            player.seek(to_t(pos));
        }
        if !resp.is_pointer_button_down_on() && !resp.dragged() && v.seeking.take() == Some(true) {
            player.play();
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    // 章節：跳到某一章、複製成 YouTube 章節
    if !chapters.is_empty() {
        let b = Btn::new(tr!("章節", "Chapters")).ghost().small().tooltip(tr!("跳到某一章（錄影時加的標記）", "Jump to a chapter (markers added while recording)")).show(ui);
        egui::Popup::menu(&b).show(|ui| {
            ui.set_min_width(200.0);
            for c in &chapters {
                if ui.button(format!("{}  {}", short_clock(c.start), c.title)).clicked() {
                    player.seek(c.start);
                }
            }
            ui.separator();
            if ui.button(tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)")).clicked() {
                *act = Act::Entry(EntryAction::CopyChapters);
            }
        });
    }
    // 播放速度「1×▾」
    ui.add_space(4.0);
    ui.scope(|ui| {
        theme::field_style(ui);
        ui.spacing_mut().interact_size.y = 28.0;
        let rate = player.rate();
        egui::ComboBox::from_id_salt("viewer-rate")
            .selected_text(RichText::new(format!("{}×", speed_label(rate))).font(theme::font(12.5)))
            .width(62.0)
            .show_ui(ui, |ui| {
                for r in RATES {
                    if ui.selectable_label((r - rate).abs() < 1e-6, format!("{}×", speed_label(r))).clicked() {
                        player.set_rate(r);
                        RATE.store((r * 100.0).round() as u64, Ordering::Relaxed);
                    }
                }
            })
            .response
            .on_hover_text(tr!("播放速度（聲音變速不變調）", "Playback speed (pitch preserved)"));
    });
    // 全螢幕
    let tip = if full { tr!("離開全螢幕（Esc、F）", "Exit full screen (Esc, F)") } else { tr!("全螢幕（F、點兩下畫面）", "Full screen (F, or double-click the video)") };
    if Btn::icon_only(Icon::Fullscreen).ghost().small().selected(full).tooltip(tip).show(ui).clicked() {
        let on = !v.full;
        set_full(v, ui.ctx(), on);
    }
    let Some(player) = &v.player else { return };
    // 音量
    ui.add_space(4.0);
    let has_audio = player.spec().has_audio;
    let icon_tip = if !has_audio {
        tr!("這個影片沒有聲音", "This video has no audio")
    } else if v.muted {
        tr!("取消靜音（M）", "Unmute (M)")
    } else {
        tr!("靜音（M）", "Mute (M)")
    };
    if Btn::icon_only(Icon::Speaker).ghost().small().selected(v.muted).enabled(has_audio).tooltip(icon_tip).show(ui).clicked() {
        v.muted = !v.muted;
        player.set_volume(v.volume_now());
    }
    // 音量表（與進度列同樣的樣式）
    let (rect, resp) = ui.allocate_exact_size(vec2(96.0, 28.0), if has_audio { Sense::click_and_drag() } else { Sense::hover() });
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 12.0, 4.0));
    let vol = v.volume_now();
    let color = if has_audio { p.accent } else { p.border_strong };
    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same(2), p.surface2);
    painter.rect_filled(Rect::from_min_max(track.min, pos2(track.min.x + track.width() * vol, track.max.y)), CornerRadius::same(2), color);
    painter.circle(pos2(track.min.x + track.width() * vol, track.center().y), if resp.hovered() || resp.dragged() { 7.0 } else { 5.5 }, p.surface, Stroke::new(2.0, color));
    if has_audio {
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on()) {
            v.volume = ((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0);
            v.muted = false;
            VOLUME.store((v.volume * 100.0).round() as u64, Ordering::Relaxed);
            player.set_volume(v.volume_now());
        }
        resp.on_hover_text(trf!("音量 {:.0}%（↑↓ 調整）", "Volume {:.0}% (↑↓ to adjust)", v.volume_now() * 100.0));
    }
}

/// 加速版、GIF（不能再剪輯或製作加速版）
fn is_export_name(name: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)(_\d+(\.\d+)?x\.mp4|\.gif|\.webp|\.webm)$").unwrap());
    RE.is_match(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_are_recognized() {
        assert!(is_export_name("Rec_2026-10-09_10-30-31_4x.mp4") && is_export_name("示範_13.69x.mp4") && is_export_name("Rec_X.gif"));
        assert!(!is_export_name("Rec_2026-10-09_10-30-31.mp4") && !is_export_name("Rec_X_cut.mp4") && !is_export_name("box.mp4"));
    }

    #[test]
    fn large_images_are_scaled_down_for_the_texture() {
        let dir = std::env::temp_dir().join(format!("viewer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.png");
        tiny_skia::Pixmap::new(5000, 1000).unwrap().save_png(&path).unwrap();
        let (img, size) = decode_image(path.to_str().unwrap(), 2048).unwrap();
        assert_eq!(size, vec2(5000.0, 1000.0));
        assert_eq!(img.size, [1666, 333]);
        let _ = std::fs::remove_file(&path);
    }
}
