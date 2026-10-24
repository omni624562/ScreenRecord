//! 內建檢視器（不必開 Windows 的相片 / 媒體播放器）：
//! - 截圖：適合視窗 / 原尺寸、滾輪縮放、拖曳移動、上一張 / 下一張、複製到剪貼簿
//! - 錄影（含加速版、GIF）：播放 / 暫停、拖曳進度、音量、播放速度、全螢幕、上一部 / 下一部、剪輯、製作加速版、壓縮。
//!   有 mpv 播放元件時用 mpv 播放（見 playback.rs），沒有時用 FFmpeg 解碼
//! - 只有聲音的檔案（M4A）：播放聲音
//!
//! 都可以「用 Windows 開啟」、在資料夾中顯示、移到資源回收筒；視窗窄時不常用的按鈕收進「⋯」。

use super::dialogs::{base_name, is_default_name, short_date};
use super::entry_menu::{Node, Place};
use super::playback::{Playback, Skipper};
use super::theme::{self, Btn, Icon};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, Id, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use screenrecorder_core::actions;
use screenrecorder_core::components::{JobStatus, Kind};
use screenrecorder_core::format::{format_bytes, short_clock, speed_label};
use screenrecorder_core::library::{is_audio_name, is_image_name};
use screenrecorder_core::mpv::{MpvPlayer, Start};
use screenrecorder_core::player::{MediaSpec, Player};
use screenrecorder_core::types::{LibraryEntry, LibraryFilter, LibraryQuery, LibrarySort, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 播放時解碼的最大尺寸
const DECODE_MAX: (u32, u32) = (1920, 1080);

/// FFmpeg 播放時解出的畫面最大多大：至少 1080p，視窗的實際像素比較大時（2K、4K 螢幕、Windows 縮放 150%）照視窗的像素，
/// 不然 2K / 4K 的錄影先縮成 1080p 再放大顯示，字會糊
fn decode_max(ctx: &egui::Context) -> (u32, u32) {
    let px = ctx.content_rect().size() * ctx.pixels_per_point();
    ((px.x.round() as u32).max(DECODE_MAX.0), (px.y.round() as u32).max(DECODE_MAX.1))
}
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
    pub(super) list: Vec<LibraryEntry>,
    idx: usize,
    /// 圖片：材質、原始尺寸
    image: Option<(TextureHandle, Vec2)>,
    /// 縮放倍率（None：適合視窗）、平移（畫面中心的偏移，點）
    zoom: Option<f32>,
    pan: Vec2,
    /// 影片
    player: Option<Playback>,
    video_tex: Option<TextureHandle>,
    /// mpv 畫好第一張畫面之後：共用材質（UiApp::video_surface）的 id
    mpv_tex: Option<egui::TextureId>,
    /// mpv 要畫多大（上一次顯示的大小，像素）
    video_px: [u32; 2],
    /// 這個檔案 mpv 播不了：改用 FFmpeg 播放
    no_mpv: Option<String>,
    /// 按了提示的「下載」：下載好就改用 mpv 重新載入
    mpv_waiting: bool,
    /// 這次開啟時關掉了下載 mpv 的提示
    mpv_hint_closed: bool,
    video_size: Vec2,
    /// 拖曳進度列時暫停，放開後若原本在播放就繼續
    seeking: Option<bool>,
    /// 後退 / 快轉（按住加速）
    skip: Skipper,
    /// 控制列進度列右邊那些按鈕實際用掉的寬度（上一格量的；進度列用剩下的，不再寫死估計值）
    tools_w: f32,
    volume: f32,
    muted: bool,
    loading: bool,
    error: Option<String>,
    close: bool,
    /// 影片從這裡開始播（搜尋說過的話時跳到那一句）
    start_at: Option<f64>,
    /// 影片開好就播放；按兩下卡片、按清單開啟時不播，停在第一個畫面等按播放
    autoplay: bool,
    /// 全螢幕（只顯示畫面與下方的控制列）
    full: bool,
    /// 重新命名的對話框關掉後重新載入（重新命名前先放開檔案，Windows 上播放中的檔案不能重新命名）
    reload_after_ask: bool,
    /// 截圖：相關的截圖（同一個程式、時間接近、有共同的詞、長得像）
    related: Vec<MediaInfo>,
    /// 串流的 Jellyfin 影片：看到哪裡回報給 Jellyfin、字幕
    pub(crate) remote: Option<Remote>,
    /// 從瀏覽視窗開的同一排影片（和 list 一一對應）：上一部 / 下一部、看完自動接下一支
    queue: Vec<Remote>,
    /// Jellyfin 的時間軸預覽縮圖：第幾張大圖 → 材質（None = 讀取中或讀不到）；要讀的那一張
    tp_tiles: std::collections::HashMap<u32, Option<TextureHandle>>,
    tp_want: std::cell::Cell<Option<u32>>,
}

/// 正在串流的 Jellyfin 影片
#[derive(Clone)]
pub struct Remote {
    pub id: String,
    /// 看到哪裡記在哪個使用者
    pub user: String,
    /// 外掛字幕的網址
    pub subs: Vec<String>,
    pub subs_on: bool,
    /// 原檔的檔名（下載到這台電腦用）
    pub file_name: String,
    /// 上次記下看到哪裡的時候
    saved: Option<std::time::Instant>,
    /// 上次看到一半的地方（從那裡接著看；0 = 從頭）
    pub resume: f64,
    /// Jellyfin 做好的時間軸預覽縮圖
    pub trickplay: Option<screenrecorder_core::jellyfin::Trickplay>,
    /// 在這個使用者的我的最愛裡（星號）
    pub favorite: bool,
}

impl Remote {
    fn cur_is(&self, path: &str) -> bool {
        screenrecorder_core::jellyfin::Jellyfin::item_of_stream(path).as_deref() == Some(self.id.as_str())
    }

    pub fn new(id: &str, user: &str, subs: Vec<String>, file_name: &str) -> Remote {
        // 第一次記是看了 30 秒之後（剛開始時播放位置還是 0，會蓋掉上次看到的地方）
        Remote {
            id: id.to_string(),
            user: user.to_string(),
            subs,
            subs_on: true,
            file_name: file_name.to_string(),
            saved: Some(std::time::Instant::now()),
            resume: 0.0,
            trickplay: None,
            favorite: false,
        }
    }
}

impl Viewer {
    pub(crate) fn cur(&self) -> &LibraryEntry {
        &self.list[self.idx]
    }

    /// 現在播到幾秒（還沒開始播時 0）
    pub(crate) fn time(&self) -> f64 {
        self.player.as_ref().map_or(0.0, |p| p.time())
    }

    fn is_image(&self) -> bool {
        is_image_name(&self.cur().media.name)
    }

    /// 只有聲音（M4A）
    fn is_audio(&self) -> bool {
        is_audio_name(&self.cur().media.name)
    }

    pub(crate) fn clear_media(&mut self) {
        self.image = None;
        self.player = None;
        self.video_tex = None;
        self.mpv_tex = None;
        self.video_px = [0, 0];
        self.video_size = Vec2::ZERO;
        self.zoom = None;
        self.pan = Vec2::ZERO;
        self.seeking = None;
        self.skip.reset();
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
    // 換好重新載入時照原本的：播放中就接著播
    v.autoplay = v.player.as_ref().is_some_and(|p| p.is_playing());
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
        v.autoplay = true;
    }
}

/// 剪輯、製作完回來：重新開啟同一支，停在原本看到的地方（不自動播放）
pub fn reopen(app: &mut UiApp, entry: LibraryEntry, t: f64) {
    open(app, entry);
    if let Some(v) = &mut app.viewer {
        v.start_at = (t > 0.5).then_some(t + 0.5);
    }
}

/// 影片開好之後要不要直接播放（在 open / open_at 之後呼叫）
pub fn set_autoplay(app: &mut UiApp, on: bool) {
    if let Some(v) = &mut app.viewer {
        v.autoplay = on;
    }
}

/// 串流的影片（Jellyfin 上的，不是這台電腦的檔案）：不能刪除、重新命名，也沒有同一個資料夾的其他影片
pub fn is_remote(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

/// 串流播放 Jellyfin 的影片（start：從哪裡開始，上次看到一半的）
pub fn open_remote(app: &mut UiApp, entry: LibraryEntry, remote: Remote, start: Option<f64>) {
    open_with(app, entry, Some(remote), start);
    set_autoplay(app, true);
}

/// 串流播放瀏覽視窗同一排裡的第 idx 支：上一部 / 下一部在這一排裡切換，看完自動接下一支
pub fn open_remote_list(app: &mut UiApp, list: Vec<(LibraryEntry, Remote)>, idx: usize, start: Option<f64>) {
    let Some((entry, remote)) = list.get(idx).cloned() else { return };
    open_remote(app, entry, remote, start);
    if let Some(v) = app.viewer.as_mut() {
        let (entries, remotes): (Vec<_>, Vec<_>) = list.into_iter().unzip();
        v.list = entries;
        v.queue = remotes;
        v.idx = idx;
    }
}

pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    open_with(app, entry, None, None);
}

fn open_with(app: &mut UiApp, entry: LibraryEntry, remote: Option<Remote>, start: Option<f64>) {
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
        mpv_tex: None,
        video_px: [0, 0],
        no_mpv: None,
        mpv_waiting: false,
        mpv_hint_closed: false,
        video_size: Vec2::ZERO,
        seeking: None,
        skip: Skipper::default(),
        tools_w: 0.0,
        volume: VOLUME.load(Ordering::Relaxed) as f32 / 100.0,
        muted: false,
        loading: true,
        error: None,
        close: false,
        start_at: start,
        autoplay: remote.is_some(),
        full: false,
        reload_after_ask: false,
        related: vec![],
        remote,
        queue: vec![],
        tp_tiles: Default::default(),
        tp_want: Default::default(),
    });
    load_current(app);
    if is_remote(&path) {
        return;
    }
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
            let gl = app.proc_address.clone().filter(|_| app.s.use_mpv && app.gl.is_some() && screenrecorder_core::mpv::available());
            let software_gl = app.gl.as_ref().is_some_and(|g| super::playback::software_gl(g));
            let ctx = app.ctx.clone();
            let max = decode_max(&ctx);
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
            let rate = RATE.load(Ordering::Relaxed) as f64 / 100.0;
            // 從說過的話開始：往前一點，聽得到整句
            let start = v.start_at.take().map(|t| (t - 0.5).max(0.0)).unwrap_or(0.0);
            // Jellyfin 的字幕（mpv 才疊得上去）
            let subs = v.remote.as_ref().filter(|r| r.subs_on && r.cur_is(&m.path)).map(|r| r.subs.clone()).unwrap_or_default();
            v.video_size = vec2(w as f32, h as f32);
            // 有 libmpv 就用 mpv 播（跟 Windows 的播放器一樣順）；只有聲音的檔案、mpv 播不了的檔案用 FFmpeg
            if let Some(gl) = gl.filter(|_| !is_audio_name(&m.name) && v.no_mpv.as_deref() != Some(m.path.as_str())) {
                match MpvPlayer::new(&m.path, spec.duration, Start { at: start, rate, volume: v.volume_now(), paused: !v.autoplay, subs }, wake.clone(), &gl, software_gl) {
                    Ok(p) => {
                        v.player = Some(Playback::Mpv { p, has_audio: spec.has_audio });
                        return;
                    }
                    Err(e) => screenrecorder_core::info!("[檢視器] 無法用 mpv 播放，改用 FFmpeg：{e}"),
                }
            }
            let player = if is_audio_name(&m.name) { Player::new_audio(ffmpeg, spec, wake) } else { Player::new(ffmpeg, spec, w, h, max.0, max.1, wake) };
            player.set_volume(v.volume_now());
            player.set_rate(rate);
            player.seek(start);
            if v.autoplay {
                player.play();
            }
            v.player = Some(Playback::Ff(player));
        },
    );
}

/// 每一格畫面開始時（介面執行緒、OpenGL context 是目前的）：mpv 有新畫面就畫到共用的材質；
/// mpv 播不了（格式不支援、顯示卡不支援）就改用 FFmpeg 重新載入
pub fn render(app: &mut UiApp, frame: &mut eframe::Frame) {
    let Some(v) = app.viewer.as_mut() else { return };
    let Some(Playback::Mpv { p, .. }) = v.player.as_mut() else { return };
    // 還沒顯示過：先照影片大小（最大 1920 × 1080）
    let size = if v.video_px[0] > 0 {
        v.video_px
    } else {
        let (w, h) = screenrecorder_core::player::fit_size(v.video_size.x as u32, v.video_size.y as u32, DECODE_MAX.0, DECODE_MAX.1);
        [w, h]
    };
    match super::playback::render_mpv(app.gl.as_ref(), &mut app.video_surface, frame, p, size) {
        Ok(Some(id)) => v.mpv_tex = Some(id),
        Ok(None) => {}
        Err(e) => {
            screenrecorder_core::info!("[檢視器] mpv 無法播放，改用 FFmpeg：{e}");
            let t = v.player.as_ref().map(|p| p.time());
            v.no_mpv = Some(v.cur().media.path.clone());
            let path = v.cur().media.path.clone();
            v.clear_media();
            v.loading = true;
            reload(app, &path, t);
        }
    }
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
    let playing = app.viewer.as_ref().and_then(|v| v.player.as_ref()).is_some_and(|p| p.is_playing());
    go_to(app, step, playing);
}

/// 上一個 / 下一個（autoplay：開好就播放）。Jellyfin 的影片：先記下這支看到哪裡，換成那一支的字幕、預覽縮圖，
/// 上次看到一半的從那裡接著看
fn go_to(app: &mut UiApp, step: i64, autoplay: bool) {
    let Some(v) = app.viewer.as_mut() else { return };
    let n = v.list.len() as i64;
    let i = v.idx as i64 + step;
    if n <= 1 || i < 0 || i >= n {
        return;
    }
    if !v.queue.is_empty() {
        let close = std::mem::replace(&mut v.close, true);
        let position = remote_position(v);
        v.close = close;
        if let Some((id, user, at, played)) = position {
            save_position(app, id, user, at, played);
        }
    }
    let Some(v) = app.viewer.as_mut() else { return };
    v.idx = i as usize;
    if let Some(r) = v.queue.get(v.idx).cloned() {
        let dur = v.cur().media.duration_sec.unwrap_or(0.0);
        v.start_at = (r.resume > 5.0 && r.resume < dur - 10.0).then_some(r.resume + 0.5);
        v.remote = Some(Remote { saved: Some(std::time::Instant::now()), ..r });
        v.tp_tiles.clear();
        v.tp_want.set(None);
    }
    // 上一個 / 下一個：正在播就接著播下一個，暫停中就停在第一個畫面
    v.autoplay = autoplay;
    load_current(app);
}

/// 移到資源回收筒（錄影連同底下的加速版、GIF），接著顯示下一個
fn delete_current(app: &mut UiApp) {
    let Some(v) = &app.viewer else { return };
    let e = v.cur().clone();
    // 串流的 Jellyfin 影片：從 Jellyfin 刪除
    if is_remote(&e.media.path) {
        if let Some(id) = screenrecorder_core::jellyfin::Jellyfin::item_of_stream(&e.media.path) {
            super::jellyfin_browse::confirm_delete(app, id, base_name(&e.media.name));
        }
        return;
    }
    let mut paths = vec![e.media.path.clone()];
    paths.extend(e.exports.iter().map(|x| x.media.path.clone()));
    let title = if paths.len() > 1 {
        let n = paths.len() - 1;
        if screenrecorder_core::i18n::is_en() {
            format!("Move this recording and its {n} sped-up version{} to the Recycle Bin?", if n == 1 { "" } else { "s" })
        } else {
            format!("把這部錄影和底下的 {n} 個加速版移到資源回收筒？")
        }
    } else {
        tr!("把這個檔案移到資源回收筒？", "Move this file to the Recycle Bin?").into()
    };
    app.confirm_trash(title, tr!("可從資源回收筒還原。", "You can restore files from the Recycle Bin."), paths, deleted);
}

/// 刪好之後換到下一個；沒刪成（檔案使用中等）就重新載入這一個
fn deleted(app: &mut UiApp, paths: &[String], ok: bool) {
    if ok {
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
    load_current(app);
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

#[derive(Clone, Copy)]
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
    /// 影片：目前這一格寫進影片當縮圖（檔案總管看到的那一張）
    SetThumb,
    /// 影片：改標題、作者、說明等影片資訊
    Tags,
    Rename,
    /// 開啟相關的截圖（第幾張）
    Related(usize),
    /// 串流的 Jellyfin 影片：下載到這台電腦
    Download,
    /// 串流的 Jellyfin 影片：先下載，下載好接著剪輯、製作
    DownloadThen(EntryAction),
    /// 串流的 Jellyfin 影片：目前這一格設成 Jellyfin 上的封面
    JfCover,
    /// 串流的 Jellyfin 影片：改 Jellyfin 上的影片資訊
    JfInfo,
    /// 串流的 Jellyfin 影片：用這一格框出臉，設成人物照片
    JfFace,
    /// 串流的 Jellyfin 影片：加進或拿出我的最愛
    JfFav,
}

/// 標題列的一個按鈕；視窗窄時依序從後面的開始收進「⋯」
struct HeadBtn {
    label: String,
    icon: Option<Icon>,
    tip: String,
    act: Act,
    /// 現在不能用的原因（轉檔進行中等）
    blocked: Option<String>,
    /// 最主要的動作（藍底）
    primary: bool,
    /// 只顯示圖示（滑鼠移上去顯示名稱）：常用但不是最主要的動作，不用點進選單也按得到
    icon_only: bool,
    /// 前面畫一條分隔線（新的一組）
    group: bool,
    /// 有子項目時是「▾」選單按鈕；收進「⋯」時直接列出子項目
    items: Vec<HeadBtn>,
}

fn head_btn(label: &str, icon: Option<Icon>, tip: &str, act: Act) -> HeadBtn {
    HeadBtn { label: label.to_string(), icon, tip: tip.to_string(), act, blocked: None, primary: false, icon_only: false, group: false, items: vec![] }
}

/// 只有圖示的按鈕（滑鼠移上去顯示名稱與說明）
fn head_icon(label: &str, icon: Icon, tip: &str, act: Act) -> HeadBtn {
    HeadBtn { icon_only: true, ..head_btn(label, Some(icon), tip, act) }
}

/// 檢視器標題列上的按鈕（由左到右，越前面越常用；放不下的從後面開始收進「⋯」）。
/// 錄影：和主畫面卡片、全部錄影同一份清單（entry_menu），常用的（剪輯、製作、傳到媒體庫）有文字放最前面，
/// 其他照清單的分組和順序排成一排小圖示；資料夾、刪除固定在右上角
fn head_buttons(app: &UiApp, e: &LibraryEntry, image: bool, remote: Option<&Remote>) -> Vec<HeadBtn> {
    let b = head_btn;
    // 串流的 Jellyfin 影片：和這台電腦上的錄影一樣的排法；要用到檔案的（剪輯、製作）先下載回來再接著做
    if is_remote(&e.media.path) {
        return remote_buttons(app, remote);
    }
    if image {
        let mut edit = b(
            tr!("編輯", "Edit"),
            Some(Icon::Edit),
            tr!("加上標註、遮住個資、裁切（另存一張，原圖保留）", "Add annotations, redact personal info, crop (saved as a new image; the original is kept)"),
            Act::Entry(EntryAction::Edit),
        );
        edit.primary = true;
        let mut open = head_icon(tr!("用 Windows 開啟", "Open in Windows"), Icon::Export, tr!("用 Windows 的「相片」開啟", "Open in the Windows Photos app"), Act::Entry(EntryAction::External));
        open.group = true;
        return vec![
            edit,
            b(
                tr!("複製", "Copy"),
                Some(Icon::Copy),
                tr!("複製到剪貼簿（Ctrl+C；可直接貼到 LINE、Word、信件）", "Copy to clipboard (Ctrl+C; paste it straight into LINE, Word or an email)"),
                Act::Copy,
            ),
            b(tr!("文字辨識", "OCR"), Some(Icon::Ocr), tr!("把圖裡的文字轉成可以複製的文字", "Text recognition (OCR): turn the text in the image into text you can copy"), Act::Ocr),
            b(
                tr!("釘在桌面", "Pin to desktop"),
                Some(Icon::Pin),
                tr!("把這張圖變成浮在最上層的小視窗，方便對照（按兩下或 Esc 關閉）", "Turn this image into a small always-on-top window for reference (double-click or Esc to close)"),
                Act::Pin,
            ),
            open,
        ];
    }
    // 有自己做法的：設為封面用現在這一格、重新命名和影片資訊先放開檔案、刪除後換到下一個
    let act_of = |a: EntryAction| match a {
        EntryAction::SetThumb => Act::SetThumb,
        EntryAction::Tags => Act::Tags,
        EntryAction::Rename => Act::Rename,
        EntryAction::Trash => Act::Delete,
        a => Act::Entry(a),
    };
    let one = |it: &super::entry_menu::Item| HeadBtn { blocked: it.blocked.clone(), primary: it.action == EntryAction::Edit, ..head_btn(&it.label, it.icon, &it.tip, act_of(it.action)) };
    let node = |n: &Node| match n {
        Node::One(it) => one(it),
        Node::Sub { label, icon, tip, items } => HeadBtn { items: items.iter().map(one).collect(), ..head_btn(label, Some(*icon), tip, Act::None) },
    };
    let groups = super::entry_menu::groups(app, e, Place::Viewer);
    let mut list: Vec<HeadBtn> = groups.iter().flatten().filter(|n| n.quick()).map(node).collect();
    for g in &groups {
        let mut first = true;
        for n in g.iter().filter(|n| !n.quick()) {
            // 旋轉：向左轉、向右轉各一個圖示，一按就轉（180°、另存轉好的在卡片的「⋯」）
            if let Node::Sub { items, .. } = n {
                if items.iter().any(|i| i.action == EntryAction::RotateLeft) {
                    for it in items.iter().filter(|i| matches!(i.action, EntryAction::RotateLeft | EntryAction::RotateRight)) {
                        list.push(HeadBtn { icon_only: true, group: std::mem::take(&mut first), ..one(it) });
                    }
                    continue;
                }
            }
            list.push(HeadBtn { icon_only: true, group: std::mem::take(&mut first), ..node(n) });
        }
    }
    list
}

/// 串流 Jellyfin 影片時標題列的按鈕：剪輯、製作（先下載）、下載｜設為封面、影片資訊｜星號
fn remote_buttons(app: &UiApp, remote: Option<&Remote>) -> Vec<HeadBtn> {
    let blocked = app.blocked(EntryAction::Export);
    let then = tr!("（先下載到這台電腦的儲存位置，下載好就接著開啟）", " (downloads it to your save folder first, then opens)");
    let mut edit =
        head_btn(tr!("剪輯", "Edit"), Some(Icon::Cut), &format!("{}{then}", tr!("剪掉不要的片段、加上標註、字幕", "Cut parts, add annotations and subtitles")), Act::DownloadThen(EntryAction::Edit));
    edit.primary = true;
    edit.blocked = app.blocked(EntryAction::Edit);
    let make_items = [
        (EntryAction::Export, tr!("加速版 / GIF / WebP…", "Sped-up / GIF / WebP…")),
        (EntryAction::Compress, tr!("壓縮…", "Compress…")),
        (EntryAction::Vertical, tr!("直式影片（Shorts / Reels）…", "Vertical video (Shorts / Reels)…")),
    ]
    .into_iter()
    .map(|(a, label)| HeadBtn { blocked: blocked.clone(), ..head_btn(label, None, tr!("先下載到這台電腦，下載好就開啟", "Downloads it first, then opens"), Act::DownloadThen(a)) })
    .collect();
    let make = HeadBtn {
        items: make_items,
        ..head_btn(
            tr!("製作", "Make"),
            Some(Icon::Fast),
            &format!("{}{then}", tr!("另存加速版、GIF、WebP、直式影片，或壓縮", "Save a sped-up version, GIF, WebP or vertical video, or compress")),
            Act::None,
        )
    };
    let down = head_btn(tr!("下載到這台電腦", "Download"), Some(Icon::Download), tr!("把原檔下載到儲存位置", "Download the original to your save folder"), Act::Download);
    let mut cover = head_icon(tr!("設為封面", "Set as cover"), Icon::Image, tr!("把正在看的這一格設成 Jellyfin 上的封面", "Use the frame you're watching as the cover on Jellyfin"), Act::JfCover);
    cover.group = true;
    let face = head_icon(
        tr!("人物照片", "Person photo"),
        Icon::Person,
        tr!("用正在看的這一格框出臉，設成 Jellyfin 上人物的照片", "Frame a face in the picture you're watching and set it as a person's photo on Jellyfin"),
        Act::JfFace,
    );
    let info = head_icon(tr!("影片資訊", "Details"), Icon::Info, tr!("改 Jellyfin 上的標題、說明、人物、標籤、年份", "Change the title, description, people, tags and year on Jellyfin"), Act::JfInfo);
    let fav_on = remote.is_some_and(|r| r.favorite);
    let mut fav = if fav_on {
        head_icon(tr!("移除星號", "Remove star"), Icon::StarOn, tr!("從 Jellyfin 的我的最愛移除", "Remove from your Jellyfin favorites"), Act::JfFav)
    } else {
        head_icon(tr!("加上星號", "Star"), Icon::Star, tr!("加進 Jellyfin 的我的最愛", "Add to your Jellyfin favorites"), Act::JfFav)
    };
    fav.group = true;
    vec![edit, make, down, cover, face, info, fav]
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut v) = app.viewer.take() else { return };
    if let Some(f) = v.player.as_ref().and_then(|p| if let Playback::Ff(p) = p { p.take_frame() } else { None }) {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
        match &mut v.video_tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => v.video_tex = Some(ctx.load_texture("viewer-video", img, TextureOptions::LINEAR)),
        }
    }
    // Jellyfin 的影片看完了：記成看過，接著播同一排的下一支
    let mut auto_next = false;
    if let Some(p) = &v.player {
        auto_next = p.take_ended() && !v.queue.is_empty() && v.idx + 1 < v.list.len();
        // 新的一格到了解碼器會叫畫面更新；這裡只讓時間順順地走
        if p.is_playing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
    // 重新命名的對話框關掉了：重新載入（改了名就是新的路徑）
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
    let buttons = head_buttons(app, &e, v.is_image(), v.remote.as_ref());

    let screen = ctx.content_rect();
    let (size, frame, area) = if v.full {
        let area = egui::Modal::default_area(Id::new("viewer")).anchor(Align2::LEFT_TOP, vec2(0.0, 0.0));
        (screen.size(), theme::modal_frame(ctx).inner_margin(0).corner_radius(0).stroke(Stroke::NONE).shadow(egui::Shadow::NONE), area)
    } else {
        (vec2((screen.width() - 32.0).max(600.0), (screen.height() - 32.0).max(400.0)), theme::modal_frame(ctx).inner_margin(0), egui::Modal::default_area(Id::new("viewer")))
    };
    let rel_thumbs: Vec<Option<TextureHandle>> = v.related.iter().take(16).map(|m| app.thumb(&m.path, m.mtime)).collect();
    // 大影片（大於 1080p）用 FFmpeg 播放時：提示下載 mpv 播放元件（播放更順）
    let mpv_job = app.core.component_status(Kind::Mpv);
    if v.mpv_waiting && !mpv_job.running {
        v.mpv_waiting = false;
        if app.core.component_installed(Kind::Mpv) {
            let (path, t) = (v.cur().media.path.clone(), v.player.as_ref().map(|p| p.time()));
            app.viewer = Some(v);
            if let Some(v) = app.viewer.as_mut() {
                v.clear_media();
                v.loading = true;
            }
            reload(app, &path, t);
            return;
        }
    }
    let show_hint = (cfg!(windows) || std::env::var("SCREENRECORDER_DEV").is_ok_and(|d| d.contains("mpvhint")))
        && matches!(v.player, Some(Playback::Ff(_)))
        && screenrecorder_core::player::big(v.video_size.x as u32, v.video_size.y as u32)
        && !v.mpv_hint_closed
        && !app.s.mpv_hint_off
        && app.s.use_mpv
        && !app.core.component_installed(Kind::Mpv);
    let (mut hint_download, mut hint_close, mut hint_never) = (false, false, false);
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
            if show_hint {
                mpv_hint(ui, stage, &mpv_job, &mut hint_download, &mut hint_close, &mut hint_never);
            }
        }
        // 按兩下畫面：全螢幕
        if !image && resp.double_clicked() {
            let on = !v.full;
            set_full(&mut v, ui.ctx(), on);
        }
        if v.loading && v.image.is_none() && v.video_tex.is_none() && v.mpv_tex.is_none() && !(audio && v.player.is_some()) {
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
        // 提示放在控制列（和相關截圖列）上方的右邊，不蓋到播放按鈕、進度列
        super::dialogs::toast_spot(ui.ctx(), pos2(bar.max.x, stage.max.y));
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
    if hint_download {
        v.mpv_waiting = true;
        app.core.component_download(Kind::Mpv);
    }
    if hint_close || hint_never {
        v.mpv_hint_closed = true;
    }
    if hint_never {
        app.s.mpv_hint_off = true;
        app.save_settings();
    }
    let position = remote_position(&mut v);
    if let Some((id, user, at, played)) = position {
        save_position(app, id, user, at, played);
    }
    if v.close {
        if v.full {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        return;
    }
    let want = v.tp_want.take().filter(|i| !v.tp_tiles.contains_key(i));
    app.viewer = Some(v);
    if let Some(index) = want {
        load_tile(app, index);
    }
    if auto_next {
        go_to(app, 1, true);
        if let Some(v) = &app.viewer {
            app.toast(trf!("接著播下一支：{}", "Playing the next one: {}", base_name(&v.cur().media.name)), false);
        }
        return;
    }
    run_act(app, act);
}

/// 讀 Jellyfin 的時間軸預覽縮圖的第 index 張大圖（背景下載、轉成材質）
fn load_tile(app: &mut UiApp, index: u32) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(v) = app.viewer.as_mut() else { return };
    let Some((id, tp)) = v.remote.as_ref().and_then(|r| Some((r.id.clone(), r.trickplay.clone()?))) else { return };
    v.tp_tiles.insert(index, None);
    let ctx = app.ctx.clone();
    let key = id.clone();
    app.spawn(
        async move {
            let img = tokio::task::spawn_blocking(move || {
                let bytes = jf.trickplay_tile(&key, &tp, index)?;
                let pm = screenrecorder_core::picture::decode_pixmap(&bytes)?;
                Some(egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
            })
            .await
            .ok()
            .flatten();
            img.map(|i| ctx.load_texture("jf-trickplay", i, TextureOptions::LINEAR))
        },
        move |app, tex| {
            if let Some(v) = app.viewer.as_mut().filter(|v| v.remote.as_ref().is_some_and(|r| r.id == id)) {
                v.tp_tiles.insert(index, tex);
            }
        },
    );
}

/// Jellyfin 串流：要不要記下看到哪裡（播放中每 30 秒一次、關掉時）；回傳（影片, 使用者, 秒, 看完了沒）
fn remote_position(v: &mut Viewer) -> Option<(String, String, f64, bool)> {
    let close = v.close;
    let (Some(r), Some(pl)) = (v.remote.as_mut(), v.player.as_ref()) else { return None };
    if r.user.is_empty() {
        return None;
    }
    // 還沒開始看（不到 5 秒）：不記
    let (at, dur) = (pl.time(), pl.duration());
    let due = pl.is_playing() && r.saved.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(30));
    if at <= 5.0 || !(due || close) {
        return None;
    }
    r.saved = Some(std::time::Instant::now());
    Some((r.id.clone(), r.user.clone(), at, dur > 0.0 && at >= dur * 0.9))
}

/// 在背景記下看到哪裡（連不到 Jellyfin 時只寫進記錄檔）；瀏覽視窗的進度條跟著更新
fn save_position(app: &mut UiApp, id: String, user: String, at: f64, played: bool) {
    let Some(jf) = app.jellyfin() else { return };
    super::jellyfin_browse::note_position(app, &id, at, played);
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.save_position(&user, &id, at, played)).await.unwrap_or_else(|e| Err(e.to_string())) }, |_, r| {
        if let Err(e) = r {
            screenrecorder_core::info!("[Jellyfin] 記下看到哪裡失敗：{e}");
        }
    });
}

/// 畫面上方的提示：下載 mpv 播放元件讓大影片播放更順（下載中顯示進度）
fn mpv_hint(ui: &mut egui::Ui, stage: Rect, job: &JobStatus, download: &mut bool, close: &mut bool, never: &mut bool) {
    let w = (stage.width() - 32.0).min(560.0);
    let r = Rect::from_min_size(pos2(stage.center().x - w / 2.0, stage.min.y + 12.0), vec2(w, 40.0));
    ui.painter().rect_filled(r, CornerRadius::same(8), Color32::from_black_alpha(200));
    ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink2(vec2(12.0, 6.0))).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if job.running {
            ui.label(RichText::new(trf!("正在下載 mpv 播放元件 {}%…", "Downloading the mpv component {}%…", (job.progress * 100.0).floor() as u32)).font(theme::font(12.5)).color(Color32::WHITE));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        let text = match &job.error {
            Some(e) => trf!("下載失敗：{}", "Download failed: {}", e),
            None => trf!("大影片卡卡的？下載 mpv 播放元件（約 {} MB）播放會更順", "Large video stuttering? Download the mpv component (about {} MB) for smoother playback", Kind::Mpv.mb()),
        };
        ui.add(egui::Label::new(RichText::new(text).font(theme::font(12.5)).color(Color32::WHITE)).truncate());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("這次先不要", "Not now")).show(ui).clicked() {
                *close = true;
            }
            if Btn::new(tr!("不再提示", "Don't ask again")).ghost().small().show(ui).clicked() {
                *never = true;
            }
            let label = if job.error.is_some() { tr!("重試", "Retry") } else { tr!("下載", "Download") };
            if Btn::new(label).primary().small().icon(Icon::Download).show(ui).clicked() {
                *download = true;
            }
        });
    });
}

/// 相關的截圖：一列小縮圖（同一個程式、時間接近、有共同的詞、長得像）；回傳點了第幾張
fn related_strip(v: &Viewer, ui: &mut egui::Ui, strip: Rect, thumbs: &[Option<TextureHandle>]) -> Option<usize> {
    let p = theme::pal(ui);
    ui.painter().hline(strip.x_range(), strip.min.y, Stroke::new(1.0, p.border));
    ui.painter().text(
        pos2(strip.min.x + 16.0, strip.min.y + 12.0),
        Align2::LEFT_CENTER,
        tr!("相關的截圖（同一個程式、時間接近、內容相近）", "Related screenshots (same app, around the same time, similar content)"),
        theme::font(12.0),
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
pub(super) fn set_full(v: &mut Viewer, ctx: &egui::Context, on: bool) {
    if v.full != on {
        v.full = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }
}

/// 標題列按鈕的樣子（量寬度、畫出來都用這個）
fn head_style(b: &HeadBtn) -> Btn {
    let tip = match (&b.blocked, b.icon_only) {
        (Some(why), _) => why.clone(),
        (None, true) if b.tip.is_empty() => b.label.to_string(),
        (None, true) => format!("{}\n{}", b.label, b.tip),
        (None, false) => b.tip.clone(),
    };
    let mut btn = if b.icon_only { Btn::icon_only(b.icon.unwrap_or(Icon::More)).ghost() } else { Btn::new(b.label.as_str()) };
    btn = btn.small().enabled(b.blocked.is_none()).tooltip(tip);
    if let (Some(i), false) = (b.icon, b.icon_only) {
        btn = btn.icon(i);
    }
    if b.primary {
        btn = btn.primary();
    }
    if !b.items.is_empty() {
        btn = btn.menu();
    }
    btn
}

/// 選單裡的一個項目（不能用時說明原因）；回傳是否按下
fn menu_entry(ui: &mut egui::Ui, b: &HeadBtn) -> bool {
    let r = ui.add_enabled(b.blocked.is_none(), egui::Button::new(b.label.as_str()));
    let r = if b.tip.is_empty() { r } else { r.on_hover_text(&b.tip) };
    let r = match &b.blocked {
        Some(why) => r.on_disabled_hover_text(why),
        None => r,
    };
    r.clicked()
}

/// 標題列裡的直線分隔
const SEP_W: f32 = 9.0;
fn head_sep(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(SEP_W, 20.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, theme::pal(ui).border));
}

/// 標題列：左邊名稱與資訊（太長時截斷），右邊按鈕都直接按得到；視窗窄放不下的才收進「⋯ 更多」
fn header(v: &mut Viewer, ui: &mut egui::Ui, e: &LibraryEntry, buttons: &[HeadBtn], act: &mut Act) {
    let p = theme::pal(ui);
    let image = v.is_image();
    let head = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 40.0));
    // 先算右邊放得下幾個按鈕（關閉、資源回收筒、資料夾固定在最右邊；左邊至少留 170 給名稱，太長時截斷）
    let gap = ui.spacing().item_spacing.x;
    let fixed = 34.0 + 16.0 + 2.0 * (28.0 + gap) + SEP_W + gap + 8.0;
    let mut room = head.width() - fixed - 170.0;
    let width = |b: &HeadBtn| head_style(b).width(ui).max(if b.icon_only { 28.0 } else { 0.0 }) + gap + if b.group { SEP_W + gap } else { 0.0 };
    let all: f32 = buttons.iter().map(width).sum();
    let mut shown = buttons.len();
    if all > room {
        // 放不下：留一個「⋯」的位置，從後面開始收
        room -= 28.0 + SEP_W + 2.0 * gap;
        shown = 0;
        for b in buttons {
            let w = width(b);
            if w > room {
                break;
            }
            room -= w;
            shown += 1;
        }
    }
    let rui = &mut ui.new_child(egui::UiBuilder::new().max_rect(head).layout(Layout::right_to_left(Align::Center)));
    if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(rui).clicked() {
        v.close = true;
    }
    // 刪除離關閉遠一點，不容易按錯
    rui.add_space(16.0);
    let remote = is_remote(&e.media.path);
    let trash_tip = if remote { tr!("從 Jellyfin 刪除（Delete）", "Delete from Jellyfin (Delete)") } else { tr!("移到資源回收筒（Delete）", "Move to Recycle Bin (Delete)") };
    if Btn::icon_only(Icon::Trash).ghost().small().tooltip(trash_tip).show(rui).clicked() {
        *act = Act::Delete;
    }
    if !remote && Btn::icon_only(Icon::Folder).ghost().small().tooltip(tr!("在資料夾中顯示", "Show in folder")).show(rui).clicked() {
        *act = Act::Entry(EntryAction::Reveal);
    }
    head_sep(rui);
    if shown < buttons.len() {
        // 「⋯ 更多」：視窗太窄放不下的按鈕（「製作」直接列出它的項目）
        let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("更多", "More")).show(rui);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(220.0);
            for (i, b) in buttons[shown..].iter().enumerate() {
                if b.group && i > 0 {
                    ui.separator();
                }
                let items: Vec<&HeadBtn> = if b.items.is_empty() { vec![b] } else { b.items.iter().collect() };
                for it in items {
                    if menu_entry(ui, it) {
                        *act = it.act;
                    }
                }
            }
        });
        head_sep(rui);
    }
    for b in buttons[..shown].iter().rev() {
        let r = head_style(b).show(rui);
        if b.items.is_empty() {
            if r.clicked() {
                *act = b.act;
            }
        } else {
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(200.0);
                for it in &b.items {
                    if menu_entry(ui, it) {
                        *act = it.act;
                    }
                }
            });
        }
        if b.group {
            head_sep(rui);
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
    if remote {
        meta.push(tr!("在 Jellyfin 上串流播放", "Streaming from Jellyfin").to_string());
    } else {
        meta.push(format_bytes(e.media.bytes));
        meta.push(e.media.name.clone());
    }
    let meta = meta.join(tr!("・", " · "));
    lui.add(egui::Label::new(RichText::new(&meta).font(theme::font(12.0)).color(p.muted)).truncate()).on_hover_text(&meta);
    ui.advance_cursor_after_rect(head);
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
        Act::Download => {
            let Some(r) = app.viewer.as_ref().and_then(|v| v.remote.as_ref()) else { return };
            let item = (r.id.clone(), r.file_name.clone());
            super::jellyfin_browse::download(app, vec![item]);
        }
        Act::DownloadThen(a) => {
            let Some(v) = app.viewer.as_mut() else { return };
            let Some(r) = v.remote.as_ref() else { return };
            let item = (r.id.clone(), r.file_name.clone());
            v.pause();
            super::jellyfin_browse::download_then(app, item, a);
        }
        Act::JfCover => {
            let Some(v) = app.viewer.as_ref() else { return };
            let (Some(r), Some(jf), Some(ff)) = (v.remote.as_ref(), app.jellyfin(), app.core.ffmpeg_path()) else { return };
            let (id, url, t) = (r.id.clone(), v.cur().media.path.clone(), v.player.as_ref().map(|p| p.time()).unwrap_or(0.0));
            app.toast(tr!("正在把這一格設成 Jellyfin 上的封面…", "Setting this frame as the cover on Jellyfin…"), false);
            let id2 = id.clone();
            app.spawn(
                async move {
                    let pm = screenrecorder_core::cover::frame_at(&ff, &url, t).await.map_err(|e| e.message().to_string())?;
                    let jpeg = screenrecorder_core::shot_edit::encode_jpeg(&pm, 90)?;
                    tokio::task::spawn_blocking(move || jf.set_cover(&id, &jpeg)).await.unwrap_or_else(|e| Err(e.to_string()))
                },
                move |app, r| match r {
                    Ok(()) => {
                        super::jellyfin_browse::edited(app, &id2, "");
                        app.toast(tr!("已設為 Jellyfin 上的封面", "Set as the cover on Jellyfin"), false);
                    }
                    Err(e) => app.toast(trf!("沒有設成封面：{e}", "Couldn't set the cover: {e}"), true),
                },
            );
        }
        Act::JfFace => {
            let Some(v) = app.viewer.as_mut() else { return };
            let (Some(r), Some(ff)) = (v.remote.as_ref(), app.core.ffmpeg_path()) else { return };
            let (id, user, url, t) = (r.id.clone(), r.user.clone(), v.cur().media.path.clone(), v.player.as_ref().map(|p| p.time()).unwrap_or(0.0));
            v.pause();
            let people = super::jellyfin_browse::people_of(app, &id);
            app.spawn(async move { screenrecorder_core::cover::frame_at(&ff, &url, t).await.map_err(|e| e.message().to_string()) }, move |app, r| match r {
                Ok(pm) => super::jellyfin_face::open(app, pm, id, user, people),
                Err(e) => app.toast(e, true),
            });
        }
        Act::JfInfo => {
            let Some(v) = app.viewer.as_mut() else { return };
            let Some(r) = v.remote.as_ref() else { return };
            let (id, user, name) = (r.id.clone(), r.user.clone(), base_name(&v.cur().media.name));
            v.pause();
            let (people, tags) = super::jellyfin_browse::known_people_tags(app);
            super::jellyfin_edit::open(app, id, user, name, people, tags);
        }
        Act::JfFav => {
            let Some(r) = app.viewer.as_ref().and_then(|v| v.remote.as_ref()) else { return };
            let Some(jf) = app.jellyfin() else { return };
            let (id, user, on) = (r.id.clone(), r.user.clone(), !r.favorite);
            let id2 = id.clone();
            app.spawn(async move { tokio::task::spawn_blocking(move || jf.set_favorite_item(&user, &id, on)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| match r {
                Ok(()) => {
                    if let Some(r) = app.viewer.as_mut().and_then(|v| v.remote.as_mut()).filter(|r| r.id == id2) {
                        r.favorite = on;
                    }
                    super::jellyfin_browse::set_favorite(app, &id2, on);
                    app.toast(
                        if on {
                            tr!("已加進 Jellyfin 的我的最愛", "Added to your Jellyfin favorites")
                        } else {
                            tr!("已從 Jellyfin 的我的最愛移除", "Removed from your Jellyfin favorites")
                        },
                        false,
                    );
                }
                Err(e) => app.toast(trf!("沒有改到 Jellyfin 的我的最愛：{e}", "Couldn't change your Jellyfin favorites: {e}"), true),
            });
        }
        Act::Delete => delete_current(app),
        Act::Rename => {
            let Some(v) = app.viewer.as_mut() else { return };
            let e = v.cur().clone();
            // 先停止播放、放開檔案（Windows 上播放中的檔案不能重新命名），對話框關掉後重新載入
            if !v.is_image() {
                v.clear_media();
                v.reload_after_ask = true;
            }
            super::library_dialog::rename_entry(app, e);
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
                        app.sync_edited(&path, screenrecorder_core::jellyfin::Edit::Cover);
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
                // 改到剪輯 / 製作視窗：先收起檢視器與清單，剪輯、製作視窗關掉後回到這裡（編輯截圖蓋在上面，關掉就回來）
                if v.full {
                    app.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
                app.leave_for_edit();
            } else if a == EntryAction::External {
                v.pause();
            }
            app.act(a, e);
        }
    }
}

/// 重新命名後：清單裡的路徑、檔名跟著改（底下的加速版也是），對話框關掉後用新的路徑重新載入
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
    v.skip.no_keys();
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
        // Shift+←→ 每次 1 秒；沒按 Shift 時和播放列的按鈕一樣（按住加速）
        if i.modifiers.shift {
            if i.key_pressed(Key::ArrowLeft) {
                p.seek((p.time() - 1.0).max(0.0));
            }
            if i.key_pressed(Key::ArrowRight) {
                p.seek(p.time() + 1.0);
            }
        } else {
            v.skip.keys(i, p);
        }
        if i.key_pressed(Key::Home) {
            p.seek(0.0);
        }
        if i.key_pressed(Key::End) {
            p.seek((p.duration() - 0.1).max(0.0));
        }
        // M 靜音；↑↓ 音量（每次 10%）
        if p.has_audio() {
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

/// 播放的快速鍵（按鍵盤圖示開啟；和剪輯視窗一樣的鍵做一樣的事）
fn key_sheet(resp: &egui::Response) {
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(300.0);
        ui.label(RichText::new(tr!("播放的快速鍵", "Playback shortcuts")).font(theme::font_bold(13.5)));
        let rows: [(&str, &str, &str); 9] = [
            ("Space", "播放 / 暫停", "Play / pause"),
            ("← / →", "後退 / 快轉 5 秒（按住越跳越快）", "Back / forward 5 s (hold to go faster)"),
            ("Shift+← / →", "後退 / 快轉 1 秒", "Back / forward 1 s"),
            ("Home / End", "跳到開頭 / 結尾", "Jump to the start / end"),
            ("↑ / ↓", "音量大 / 小", "Volume up / down"),
            ("M", "靜音", "Mute"),
            ("F", "全螢幕", "Full screen"),
            ("Page Up / Down", "上一個 / 下一個", "Previous / next"),
            ("Esc", "離開全螢幕；不是全螢幕時關閉", "Exit full screen; close otherwise"),
        ];
        egui::Grid::new("viewer-keys").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
            for (k, zh, en) in rows {
                ui.label(RichText::new(k).font(theme::mono(12.0)));
                ui.label(RichText::new(tr!(zh, en)).font(theme::font(12.5)));
                ui.end_row();
            }
        });
    });
}

fn toggle_play(p: &Playback) {
    if p.is_playing() {
        p.pause();
    } else {
        // 播完了：從頭播
        if p.time() >= p.duration() - 0.05 {
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
    ui.label(RichText::new(tr!("滾輪縮放・拖曳移動・按兩下切換原尺寸", "Scroll to zoom · Drag to pan · Double-click for actual size")).font(theme::font(12.0)).color(p.muted));
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
    } else if let Some((id, size, uv)) =
        v.video_tex.as_ref().map(|t| (t.id(), t.size_vec2(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)))).or(v.mpv_tex.map(|id| (id, v.video_size, super::playback::mpv_uv())))
    {
        let src = if v.video_size.x > 0.0 { v.video_size } else { size };
        let k = ((stage.width() - 24.0) / src.x).min((stage.height() - 24.0) / src.y);
        // 對齊螢幕像素；mpv 照顯示的大小（像素）畫，縮放由 mpv 在顯示卡上做
        let (r, px) = super::playback::pixel_rect(Rect::from_center_size(stage.center(), src * k), painter.ctx().pixels_per_point());
        painter.image(id, r, uv, Color32::WHITE);
        v.video_px = px;
    } else {
        return;
    }
    let Some(p) = &v.player else { return };
    // 點畫面：播放 / 暫停（按兩下：全螢幕）
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
    let remote = v.remote.is_some();
    let full = v.full;
    let audio = v.is_audio();
    let has_subs = v.remote.as_ref().is_some_and(|r| !r.subs.is_empty()) && matches!(v.player, Some(Playback::Mpv { .. }));
    let Some(player) = &v.player else { return };
    let playing = player.is_playing();
    let dur = player.duration().max(0.001);
    let t = player.time().min(dur);
    let tip = if playing { tr!("暫停（空白鍵）", "Pause (Space)") } else { tr!("播放（空白鍵）", "Play (Space)") };
    // 播放｜後退、快轉（並排；按下就跳，按住加速）
    if Btn::icon_only(if playing { Icon::Pause } else { Icon::Play }).small().tooltip(tip).show(ui).clicked() {
        toggle_play(player);
    }
    v.skip.buttons(ui, player);
    ui.label(RichText::new(format!("{} / {}", short_clock(t), short_clock(dur))).font(theme::mono(12.5)));
    // 章節、字幕、擷取、速度、快速鍵、全螢幕、音量（右邊），進度列填滿中間：
    // 右邊用掉多少寬度是上一格畫完量的（第一格先估），增減按鈕都不會把音量擠出畫面
    let guess = 380.0 + if chapters.is_empty() { 0.0 } else { 84.0 } + if has_subs { 36.0 } else { 0.0 };
    let tools_w = if v.tools_w > 0.0 { v.tools_w } else { guess };
    let track_w = (ui.available_width() - tools_w - 4.0).max(80.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(track_w, 28.0), Sense::click_and_drag());
    let tools_from = rect.max.x;
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
        let keys = tr!("←/→ 後退/快轉 5 秒（按住加速）、空白鍵 播放/暫停", "←/→ back/forward 5 s (hold to go faster), Space play/pause");
        // Jellyfin 做好時間軸預覽縮圖的：滑鼠移到進度列上看得到那一秒的畫面
        let thumb = v.remote.as_ref().and_then(|r| r.trickplay.as_ref()).and_then(|tp| {
            let (index, uv) = tp.at(at);
            match v.tp_tiles.get(&index) {
                Some(Some(tex)) => Some((tex.id(), uv, tp.width as f32 / tp.height.max(1) as f32)),
                Some(None) => None,
                None => {
                    v.tp_want.set(Some(index));
                    None
                }
            }
        });
        match thumb {
            Some((tex, uv, aspect)) => {
                resp.clone().on_hover_ui_at_pointer(|ui| {
                    let size = vec2(220.0, 220.0 / aspect);
                    ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(tex, size)).uv(Rect::from_min_max(pos2(uv[0], uv[1]), pos2(uv[2], uv[3]))).corner_radius(4));
                    ui.label(RichText::new(&when).font(theme::font_bold(12.5)));
                });
            }
            None => {
                resp.clone().on_hover_text_at_pointer(format!("{when}\n{keys}"));
            }
        }
    }
    if v.seeking.is_none() && resp.is_pointer_button_down_on() {
        v.seeking = Some(playing);
        player.pause();
    }
    if v.seeking.is_some() {
        if let Some(pos) = resp.interact_pointer_pos() {
            player.scrub(to_t(pos));
        }
        if !resp.is_pointer_button_down_on() && !resp.dragged() {
            player.end_scrub();
            if v.seeking.take() == Some(true) {
                player.play();
            }
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
            if !remote {
                ui.separator();
                if ui.button(tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)")).clicked() {
                    *act = Act::Entry(EntryAction::CopyChapters);
                }
            }
        });
    }
    // Jellyfin 影片的字幕：開或關
    if has_subs {
        let on = v.remote.as_ref().is_some_and(|r| r.subs_on);
        if Btn::icon_only(Icon::Captions).ghost().small().selected(on).tooltip(if on { tr!("關掉字幕", "Hide subtitles") } else { tr!("顯示字幕", "Show subtitles") }).show(ui).clicked() {
            player.show_subs(!on);
            if let Some(r) = v.remote.as_mut() {
                r.subs_on = !on;
            }
        }
    }
    // 擷取這一格（跟目前的畫面有關，放在進度列旁邊；全螢幕時也按得到）
    if !audio
        && Btn::icon_only(Icon::Camera)
            .ghost()
            .small()
            .tooltip(tr!("擷取這一格：存成截圖（原尺寸 PNG）並複製到剪貼簿", "Grab this frame: save it as a screenshot (full-size PNG) and copy it to the clipboard"))
            .show(ui)
            .clicked()
    {
        *act = Act::Grab;
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
    // 快速鍵一覽
    let keys = Btn::icon_only(Icon::Keyboard).ghost().small().tooltip(tr!("播放的快速鍵", "Playback shortcuts")).show(ui);
    key_sheet(&keys);
    // 全螢幕
    let tip = if full { tr!("離開全螢幕（Esc、F）", "Exit full screen (Esc, F)") } else { tr!("全螢幕（F、按兩下畫面）", "Full screen (F, or double-click the video)") };
    if Btn::icon_only(Icon::Fullscreen).ghost().small().selected(full).tooltip(tip).show(ui).clicked() {
        let on = !v.full;
        set_full(v, ui.ctx(), on);
    }
    let Some(player) = &v.player else { return };
    // 音量
    ui.add_space(4.0);
    let has_audio = player.has_audio();
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
    // 記下右邊實際用掉的寬度（和這一格用的不同時再畫一次，下一格就對了）
    let used = ui.min_rect().max.x.max(ui.cursor().min.x) - tools_from;
    if (used - v.tools_w).abs() > 0.5 {
        v.tools_w = used;
        ui.ctx().request_repaint();
    }
}

/// 加速版、GIF（不能再剪輯或製作加速版）
pub(crate) fn is_export_name(name: &str) -> bool {
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
