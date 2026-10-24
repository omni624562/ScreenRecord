//! 主畫面內播放：按「最近錄影」卡片的播放鍵，影片（或錄音）就在即時預覽的位置播放，看完按「回到錄影畫面」、Esc 或點範圍分頁回到即時預覽。
//! 外框與標籤用藍色（錄影範圍是紅色），一看就知道是在播放。只放基本控制（播放、進度、速度、音量、全螢幕）；
//! 要用其他功能（章節、擷取這一格、設為封面…）按「在播放器開啟」，從目前的位置接著播。
//! 開始錄影（含倒數）時自動停止播放。有 mpv 播放元件時用 mpv 播放（見 playback.rs），沒有時用 FFmpeg 解碼。

use super::playback::{self, Playback, Skipper};
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, UiBuilder, Vec2};
use screenrecorder_core::format::{short_clock, speed_label};
use screenrecorder_core::library::is_audio_name;
use screenrecorder_core::mpv::{MpvPlayer, Start};
use screenrecorder_core::player::{MediaSpec, Player};
use screenrecorder_core::types::LibraryEntry;
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 解碼的最大尺寸（預覽區不大，720p 就夠清楚）
const DECODE_MAX: (u32, u32) = (1280, 720);
/// 可選的播放速度
const RATES: [f64; 5] = [0.5, 1.0, 1.25, 1.5, 2.0];
/// 控制列的高度
const BAR_H: f32 = 36.0;
/// 載入順序：切換很快時丟掉舊的結果
static SEQ: AtomicU64 = AtomicU64::new(0);
/// mpv 播不了的檔案：改用 FFmpeg 播放
static NO_MPV: Mutex<Option<String>> = Mutex::new(None);

pub struct Inline {
    pub entry: LibraryEntry,
    /// 按了全螢幕：在播放器開啟時直接全螢幕
    want_full: bool,
    player: Option<Playback>,
    tex: Option<TextureHandle>,
    /// mpv 畫好第一張畫面之後：共用材質（UiApp::inline_surface）的 id
    mpv_tex: Option<egui::TextureId>,
    /// mpv 要畫多大（上一次顯示的大小，像素）
    video_px: [u32; 2],
    size: Vec2,
    /// 拖曳進度列時暫停，放開後若原本在播放就繼續
    seeking: Option<bool>,
    /// 後退 / 快轉（按住加速）
    skip: Skipper,
    volume: f32,
    muted: bool,
    loading: bool,
    error: Option<String>,
    /// 錄音的波形（每秒 waveform::PER_SEC 個值）
    wave: Option<Vec<u8>>,
    /// 只有聲音（M4A、只錄聲音的錄影、沒有畫面的檔案）
    sound_only: bool,
    cancel: Arc<AtomicBool>,
}

impl Inline {
    fn audio(&self) -> bool {
        self.sound_only
    }

    fn volume_now(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    /// 正在播的是這個檔案（卡片標出來）
    pub fn is(&self, path: &str) -> bool {
        self.entry.media.path == path
    }
}

impl Drop for Inline {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 在主畫面播放（截圖仍用檢視器）
pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    if screenrecorder_core::library::is_image_name(&entry.media.name) || app.locked() {
        return super::viewer::open(app, entry);
    }
    if app.inline.as_ref().is_some_and(|i| i.is(&entry.media.path)) {
        // 同一支：從頭再播
        if let Some(p) = app.inline.as_ref().and_then(|i| i.player.as_ref()) {
            p.seek(0.0);
            p.play();
        }
        return;
    }
    let volume = app.inline.as_ref().map_or(1.0, |i| i.volume);
    let cancel = Arc::new(AtomicBool::new(false));
    app.inline = Some(Inline {
        entry: entry.clone(),
        want_full: false,
        player: None,
        tex: None,
        mpv_tex: None,
        video_px: [0, 0],
        size: Vec2::ZERO,
        seeking: None,
        skip: Skipper::default(),
        volume,
        muted: false,
        loading: true,
        error: None,
        wave: None,
        sound_only: sound_only(&entry),
        cancel: cancel.clone(),
    });
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        if let Some(i) = &mut app.inline {
            i.loading = false;
            i.error = Some(tr!("找不到 FFmpeg，無法播放", "FFmpeg not found. Can't play this.").into());
        }
        return;
    };
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let core = app.core.clone();
    let has_info = entry.media.duration_sec.is_some_and(|d| d > 0.0);
    let media = entry.media.clone();
    app.spawn(
        async move {
            if has_info {
                Some(media)
            } else {
                core.cache.probe(&ffmpeg, &media.path).await.ok()
            }
        },
        move |app, media| {
            if SEQ.load(Ordering::Relaxed) != seq {
                return;
            }
            let Some(ffmpeg) = app.core.ffmpeg_path() else { return };
            let ctx = app.ctx.clone();
            let gl = app.proc_address.clone().filter(|_| app.s.use_mpv && app.gl.is_some() && screenrecorder_core::mpv::available());
            let software_gl = app.gl.as_ref().is_some_and(|g| playback::software_gl(g));
            let Some(i) = app.inline.as_mut() else { return };
            i.loading = false;
            let Some(m) = media.filter(|m| m.duration_sec.is_some_and(|d| d > 0.0)) else {
                i.error = Some(tr!("無法讀取這個檔案", "Couldn't read this file").into());
                return;
            };
            i.entry.media = m.clone();
            let spec = MediaSpec { path: m.path.clone(), duration: m.duration_sec.unwrap_or(0.0), fps: m.fps.filter(|f| *f > 0.0).unwrap_or(30.0), has_audio: m.has_audio == Some(true) };
            let (w, h) = (m.width.unwrap_or(1280), m.height.unwrap_or(720));
            let c = ctx.clone();
            let wake = Arc::new(move || c.request_repaint());
            let audio = is_audio_name(&m.name) || screenrecorder_core::after_record::is_audio_recording(&m.path) || m.width.is_none();
            i.sound_only = audio;
            i.size = vec2(w as f32, h as f32);
            // 有 libmpv 就用 mpv 播；聲音、mpv 播不了的檔案用 FFmpeg
            let no_mpv = NO_MPV.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(m.path.as_str());
            let mpv = gl.filter(|_| !audio && !no_mpv).and_then(|gl| {
                MpvPlayer::new(&m.path, spec.duration, Start { at: 0.0, rate: 1.0, volume: i.volume_now(), paused: false, ..Default::default() }, wake.clone(), &gl, software_gl)
                    .map_err(|e| screenrecorder_core::info!("[主畫面播放] 無法用 mpv 播放，改用 FFmpeg：{e}"))
                    .ok()
            });
            if let Some(p) = mpv {
                i.player = Some(Playback::Mpv { p, has_audio: spec.has_audio });
            } else {
                let player = if audio { Player::new_audio(ffmpeg.clone(), spec, wake) } else { Player::new(ffmpeg.clone(), spec, w, h, DECODE_MAX.0, DECODE_MAX.1, wake) };
                player.set_volume(i.volume_now());
                player.play();
                i.player = Some(Playback::Ff(player));
            }
            // 錄音：在背景讀波形
            if audio {
                let (path, cancel) = (m.path.clone(), i.cancel.clone());
                app.spawn(async move { tokio::task::spawn_blocking(move || screenrecorder_core::waveform::load(&ffmpeg, &path, &cancel)).await.ok().flatten() }, move |app, wave| {
                    if let (Some(i), Some(mut w)) = (app.inline.as_mut(), wave) {
                        if SEQ.load(Ordering::Relaxed) == seq {
                            screenrecorder_core::waveform::normalize(&mut w);
                            i.wave = Some(w);
                        }
                    }
                });
            }
        },
    );
}

/// 只有聲音的檔案：M4A、只錄聲音的錄影（錄音_…）
fn sound_only(e: &LibraryEntry) -> bool {
    is_audio_name(&e.media.name) || screenrecorder_core::after_record::is_audio_recording(&e.media.path)
}

/// 停止播放，回到即時預覽
pub fn close(app: &mut UiApp) {
    app.inline = None;
}

/// 在播放器開啟（從目前的位置接著播）
fn to_viewer(app: &mut UiApp) {
    let Some(i) = app.inline.take() else { return };
    let t = i.player.as_ref().map_or(0.0, |p| p.time());
    let playing = i.player.as_ref().is_some_and(|p| p.is_playing());
    let entry = i.entry.clone();
    drop(i);
    super::viewer::open_at(app, entry, t + 0.5);
    // 在主畫面暫停著就暫停著開
    super::viewer::set_autoplay(app, playing);
}

/// 在播放器接著播，直接全螢幕
fn to_full(app: &mut UiApp, ctx: &egui::Context) {
    to_viewer(app);
    if let Some(v) = app.viewer.as_mut() {
        super::viewer::set_full(v, ctx, true);
    }
}

/// 每一格：錄影開始了、或被其他視窗蓋住時停下來；收下解出來的畫面
pub fn tick(app: &mut UiApp, ctx: &egui::Context) {
    if app.inline.is_none() {
        return;
    }
    // 開始錄影（含倒數）：停止播放，回到即時預覽
    if app.locked() {
        return close(app);
    }
    let covered = app.editor.is_some() || app.export_dlg.is_some() || app.library.is_some() || app.viewer.is_some();
    let Some(i) = app.inline.as_mut() else { return };
    if covered {
        if let Some(p) = &i.player {
            p.pause();
        }
    }
    if let Some(f) = i.player.as_ref().and_then(|p| if let Playback::Ff(p) = p { p.take_frame() } else { None }) {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
        match &mut i.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => i.tex = Some(ctx.load_texture("inline-video", img, TextureOptions::LINEAR)),
        }
    }
    if let Some(p) = &i.player {
        let _ = p.take_ended();
        if p.is_playing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
}

/// 每一格畫面開始時（OpenGL context 是目前的）：mpv 有新畫面就畫到材質；mpv 播不了就改用 FFmpeg 重新開
pub fn render(app: &mut UiApp, frame: &mut eframe::Frame) {
    let Some(i) = app.inline.as_mut() else { return };
    let Some(Playback::Mpv { p, .. }) = i.player.as_mut() else { return };
    let size = if i.video_px[0] > 0 {
        i.video_px
    } else {
        let (w, h) = screenrecorder_core::player::fit_size(i.size.x as u32, i.size.y as u32, DECODE_MAX.0, DECODE_MAX.1);
        [w, h]
    };
    match playback::render_mpv(app.gl.as_ref(), &mut app.inline_surface, frame, p, size) {
        Ok(Some(id)) => i.mpv_tex = Some(id),
        Ok(None) => {}
        Err(e) => {
            screenrecorder_core::info!("[主畫面播放] mpv 無法播放，改用 FFmpeg：{e}");
            let entry = i.entry.clone();
            *NO_MPV.lock().unwrap_or_else(|e| e.into_inner()) = Some(entry.media.path.clone());
            app.inline = None;
            open(app, entry);
        }
    }
}

fn toggle(p: &Playback) {
    if p.is_playing() {
        p.pause();
    } else {
        if p.time() >= p.duration() - 0.05 {
            p.seek(0.0);
        }
        p.play();
    }
}

/// 鍵盤：空白鍵播放 / 暫停、←→ 後退 / 快轉 5 秒（按住加速）、M 靜音、F 全螢幕（播放器）、Esc 回到即時預覽
fn keyboard(app: &mut UiApp, ctx: &egui::Context) {
    if let Some(i) = app.inline.as_mut() {
        i.skip.no_keys();
    }
    if app.ask.is_some() || ctx.memory(|m| m.focused().is_some()) || app.editor.is_some() || app.library.is_some() || app.viewer.is_some() || app.export_dlg.is_some() {
        return;
    }
    let none = egui::Modifiers::NONE;
    // ←→ 按住的方向先記下來（播放列一起處理），再把按鍵事件吃掉
    if let Some(il) = app.inline.as_mut() {
        if let Some(p) = &il.player {
            ctx.input(|i| {
                if !i.modifiers.any() {
                    il.skip.keys(i, p)
                }
            });
        }
    }
    let (esc, space, m, f) = ctx.input_mut(|i| {
        i.consume_key(none, Key::ArrowLeft);
        i.consume_key(none, Key::ArrowRight);
        (i.consume_key(none, Key::Escape), i.consume_key(none, Key::Space), i.consume_key(none, Key::M), i.consume_key(none, Key::F))
    });
    if esc {
        return close(app);
    }
    // F：和播放畫面一樣是全螢幕（在播放器全螢幕接著播）
    if f {
        return to_full(app, ctx);
    }
    let Some(i) = app.inline.as_mut() else { return };
    if m {
        i.muted = !i.muted;
        let v = i.volume_now();
        if let Some(p) = &i.player {
            p.set_volume(v);
        }
    }
    let Some(p) = &i.player else { return };
    if space {
        toggle(p);
    }
}

/// 畫在預覽區（area：整個預覽區，控制列放在下面）
pub fn show(app: &mut UiApp, ui: &mut egui::Ui, area: Rect) {
    keyboard(app, ui.ctx());
    let Some(i) = app.inline.as_mut() else { return };
    let p = theme::pal(ui);
    let stage = Rect::from_min_max(area.min, pos2(area.max.x, area.max.y - BAR_H - 6.0));
    let painter = ui.painter_at(area);
    painter.rect_filled(stage, CornerRadius::same(theme::RADIUS_SM), Color32::from_gray(18));
    let audio = i.audio();
    // 畫面：依影片的比例放進去
    let mut shown = stage;
    if audio {
        wave_view(i, &painter, stage, p);
    } else if let Some((id, size, uv)) = i.tex.as_ref().map(|t| (t.id(), t.size_vec2(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)))).or(i.mpv_tex.map(|id| (id, i.size, playback::mpv_uv()))) {
        let src = if i.size.x > 0.0 { i.size } else { size };
        let k = (stage.width() / src.x).min(stage.height() / src.y);
        // 對齊螢幕像素；mpv 照顯示的大小（像素）畫
        let (r, px) = playback::pixel_rect(Rect::from_center_size(stage.center(), src * k), ui.ctx().pixels_per_point());
        shown = r;
        painter.image(id, shown, uv, Color32::WHITE);
        i.video_px = px;
    }
    let msg = if let Some(e) = &i.error {
        Some(e.clone())
    } else if i.loading || (!audio && i.tex.is_none() && i.mpv_tex.is_none()) {
        Some(tr!("讀取中…", "Loading…").to_string())
    } else {
        None
    };
    if let Some(m) = msg {
        theme::centered_text(&painter, stage, &m, theme::font(13.0), p.muted);
    }
    // 藍色外框與「播放中」標籤（和紅色的錄影範圍分開）
    painter.rect_stroke(shown.shrink(1.0), CornerRadius::same(4), Stroke::new(2.0, p.accent), StrokeKind::Inside);
    let name = super::dialogs::base_name(&i.entry.media.name);
    let label = trf!("▶ 播放中：{}", "▶ Playing: {}", name);
    let g = painter.layout(label, theme::font_bold(12.0), Color32::WHITE, (shown.width() - 120.0).max(80.0));
    let tag = Rect::from_min_size(shown.min + vec2(8.0, 8.0), g.size() + vec2(12.0, 6.0));
    painter.rect_filled(tag, CornerRadius::same(5), p.accent);
    painter.galley(tag.min + vec2(6.0, 3.0), g, Color32::WHITE);
    // 點畫面：播放 / 暫停；按兩下：在播放器開啟。要比上面的按鈕先登記，按鈕才收得到點擊（後登記的蓋在上面）
    let resp = ui.interact(stage, egui::Id::new("inline-stage"), Sense::click());
    // 右上角：回到錄影畫面（即時預覽），有文字比較好找
    let close_r = Rect::from_min_max(pos2(shown.max.x - 220.0, shown.min.y + 6.0), pos2(shown.max.x - 6.0, shown.min.y + 38.0));
    let mut close_now = false;
    let mut btn_r = close_r;
    ui.scope_builder(UiBuilder::new().max_rect(close_r).layout(Layout::right_to_left(Align::Min)), |ui| {
        let r = Btn::new(tr!("回到錄影畫面", "Back to recording"))
            .icon(Icon::Video)
            .small()
            .tooltip(tr!("停止播放，回到即時預覽，可以接著錄影（Esc；切換上面的範圍分頁也可以）", "Stop playing and go back to the live preview to record (Esc, or pick a capture tab above)"))
            .show(ui);
        btn_r = r.rect;
        if r.clicked() {
            close_now = true;
        }
    });
    let close_r = btn_r;
    let mut open_viewer = resp.double_clicked();
    if let Some(pl) = &i.player {
        if resp.clicked() && !close_r.contains(resp.interact_pointer_pos().unwrap_or_default()) {
            toggle(pl);
        }
        if !pl.is_playing() && i.seeking.is_none() {
            painter.circle_filled(stage.center(), 30.0, Color32::from_black_alpha(150));
            theme::paint_icon(&painter, Rect::from_center_size(stage.center() + vec2(3.0, 0.0), vec2(26.0, 26.0)), Icon::Play, Color32::WHITE);
        }
    }
    // 控制列
    let bar = Rect::from_min_max(pos2(area.min.x, area.max.y - BAR_H), area.max);
    ui.scope_builder(UiBuilder::new().max_rect(bar).layout(Layout::left_to_right(Align::Center)), |ui| {
        if controls(i, ui) {
            open_viewer = true;
        }
    });
    if close_now {
        close(app);
    } else if open_viewer {
        if app.inline.as_ref().is_some_and(|i| i.want_full) {
            to_full(app, ui.ctx());
        } else {
            to_viewer(app);
        }
    }
}

/// 錄音：波形與播放的位置
fn wave_view(i: &Inline, painter: &egui::Painter, stage: Rect, p: &theme::Pal) {
    let Some(w) = &i.wave else {
        let c = stage.center();
        painter.circle_filled(c, 40.0, Color32::from_gray(48));
        theme::paint_icon(painter, Rect::from_center_size(c, vec2(40.0, 40.0)), Icon::Speaker, Color32::from_gray(220));
        return;
    };
    let r = stage.shrink2(vec2(24.0, stage.height() * 0.22));
    let n = (r.width() / 3.0).max(1.0) as usize;
    let dur = i.player.as_ref().map_or(1.0, |pl| pl.duration().max(0.001));
    let t = i.player.as_ref().map_or(0.0, |pl| pl.time());
    let per = w.len() as f64 / n as f64;
    for k in 0..n {
        let a = (k as f64 * per) as usize;
        let b = (((k + 1) as f64 * per) as usize).clamp(a + 1, w.len().max(a + 1));
        let v = w.get(a..b.min(w.len())).and_then(|s| s.iter().max()).copied().unwrap_or(0) as f32 / 255.0;
        let x = r.min.x + k as f32 * 3.0 + 1.0;
        let hh = (v * r.height()).max(2.0);
        let played = (k as f64 / n as f64) <= t / dur;
        let col = if played { p.accent } else { Color32::from_gray(90) };
        painter.rect_filled(Rect::from_center_size(pos2(x, r.center().y), vec2(2.0, hh)), CornerRadius::same(1), col);
    }
}

/// 控制列：播放、時間、進度、速度、音量、在播放器開啟；回傳 true = 要在播放器開啟
fn controls(i: &mut Inline, ui: &mut egui::Ui) -> bool {
    let p = theme::pal(ui);
    let mut open_viewer = false;
    let Some(player) = &i.player else {
        ui.label(theme::muted(ui, tr!("讀取中…", "Loading…")));
        return false;
    };
    let playing = player.is_playing();
    let dur = player.duration().max(0.001);
    let t = player.time().min(dur);
    let tip = if playing { tr!("暫停（空白鍵）", "Pause (Space)") } else { tr!("播放（空白鍵）", "Play (Space)") };
    if Btn::icon_only(if playing { Icon::Pause } else { Icon::Play }).small().tooltip(tip).show(ui).clicked() {
        toggle(player);
    }
    i.skip.buttons(ui, player);
    ui.label(RichText::new(format!("{} / {}", short_clock(t), short_clock(dur))).font(theme::mono(12.0)));
    let right_w = 62.0 + 34.0 + 70.0 + 34.0 + 24.0;
    let track_w = (ui.available_width() - right_w).max(60.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(track_w, 26.0), Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 12.0, 5.0));
    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same(3), p.surface2);
    let frac = (t / dur) as f32;
    painter.rect_filled(Rect::from_min_max(track.min, pos2(track.min.x + track.width() * frac, track.max.y)), CornerRadius::same(3), p.accent);
    painter.circle(pos2(track.min.x + track.width() * frac, track.center().y), if resp.hovered() || resp.dragged() { 7.0 } else { 5.5 }, p.surface, Stroke::new(2.0, p.accent));
    let to_t = |pos: Pos2| (((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64) * dur;
    if let Some(pos) = resp.hover_pos() {
        resp.clone().on_hover_text_at_pointer(short_clock(to_t(pos)));
    }
    if i.seeking.is_none() && resp.is_pointer_button_down_on() {
        i.seeking = Some(playing);
        player.pause();
    }
    if i.seeking.is_some() {
        if let Some(pos) = resp.interact_pointer_pos() {
            player.scrub(to_t(pos));
        }
        if !resp.is_pointer_button_down_on() && !resp.dragged() {
            player.end_scrub();
            if i.seeking.take() == Some(true) {
                player.play();
            }
        }
    }
    // 播放速度
    ui.scope(|ui| {
        theme::field_style(ui);
        ui.spacing_mut().interact_size.y = 26.0;
        let rate = player.rate();
        egui::ComboBox::from_id_salt("inline-rate").selected_text(RichText::new(format!("{}×", speed_label(rate))).font(theme::font(12.0))).width(52.0).show_ui(ui, |ui| {
            for r in RATES {
                if ui.selectable_label((r - rate).abs() < 1e-6, format!("{}×", speed_label(r))).clicked() {
                    player.set_rate(r);
                }
            }
        });
    });
    // 音量
    let has_audio = player.has_audio();
    let tip = if !has_audio {
        tr!("沒有聲音", "No audio")
    } else if i.muted {
        tr!("取消靜音（M）", "Unmute (M)")
    } else {
        tr!("靜音（M）", "Mute (M)")
    };
    if Btn::icon_only(Icon::Speaker).ghost().small().selected(i.muted).enabled(has_audio).tooltip(tip).show(ui).clicked() {
        i.muted = !i.muted;
        player.set_volume(i.volume_now());
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(64.0, 26.0), if has_audio { Sense::click_and_drag() } else { Sense::hover() });
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 10.0, 4.0));
    let vol = i.volume_now();
    let color = if has_audio { p.accent } else { p.border_strong };
    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same(2), p.surface2);
    painter.rect_filled(Rect::from_min_max(track.min, pos2(track.min.x + track.width() * vol, track.max.y)), CornerRadius::same(2), color);
    if has_audio {
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on()) {
            i.volume = ((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0);
            i.muted = false;
            player.set_volume(i.volume_now());
        }
        resp.on_hover_text(trf!("音量 {:.0}%", "Volume {:.0}%", vol * 100.0));
    }
    // 全螢幕（F）；在播放器開啟（其他功能都在那裡）
    let mut full = false;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if Btn::icon_only(Icon::Fullscreen).ghost().small().tooltip(tr!("全螢幕（F）", "Full screen (F)")).show(ui).clicked() {
            full = true;
        }
        if Btn::icon_only(Icon::Window)
            .ghost()
            .small()
            .tooltip(tr!("在播放器開啟（按兩下畫面也可以）：章節、擷取這一格、設為封面等", "Open in the player (or double-click the video): chapters, frame capture, set as cover…"))
            .show(ui)
            .clicked()
        {
            open_viewer = true;
        }
    });
    if full {
        i.want_full = true;
    }
    open_viewer || full
}
