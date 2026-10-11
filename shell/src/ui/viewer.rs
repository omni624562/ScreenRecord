//! 內建檢視器（不必開 Windows 的相片 / 媒體播放器）：
//! - 截圖：適合視窗 / 原尺寸、滾輪縮放、拖曳移動、上一張 / 下一張、複製到剪貼簿
//! - 錄影（含加速版、GIF）：播放 / 暫停、拖曳進度、音量、上一部 / 下一部、剪輯、製作加速版
//!
//! 都可以「用 Windows 開啟」、在資料夾中顯示、移到資源回收筒。

use super::dialogs::{base_name, is_default_name, short_date, Ask};
use super::theme::{self, Btn, Icon};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, Id, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use screenrecorder_core::actions;
use screenrecorder_core::format::{format_bytes, video_clock};
use screenrecorder_core::library::is_image_name;
use screenrecorder_core::player::{MediaSpec, Player};
use screenrecorder_core::types::{LibraryEntry, LibraryFilter, LibraryQuery, LibrarySort};
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
}

impl Viewer {
    fn cur(&self) -> &LibraryEntry {
        &self.list[self.idx]
    }

    fn is_image(&self) -> bool {
        is_image_name(&self.cur().media.name)
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

/// 開啟檢視器；同類的檔案（截圖或錄影）在背景載入，供上一個 / 下一個切換
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
    });
    load_current(app);
    let core = app.core.clone();
    let dir = app.s.out_dir(&app.env);
    let query = LibraryQuery { filter: if shots { LibraryFilter::Shot } else { LibraryFilter::All }, sort: LibrarySort::New, page_size: Some(200.0), ..Default::default() };
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

/// 載入目前這一個（圖片解碼、影片讀取資訊後開始播放）
fn load_current(app: &mut UiApp) {
    let Some(v) = app.viewer.as_mut() else { return };
    v.clear_media();
    v.loading = true;
    let e = v.cur().clone();
    let seq = LOAD_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    if is_image_name(&e.media.name) {
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
            let player = Player::new(ffmpeg, spec, w, h, DECODE_MAX.0, DECODE_MAX.1, Arc::new(move || c.request_repaint()));
            player.set_volume(v.volume_now());
            player.seek(0.0);
            player.play();
            v.video_size = vec2(w as f32, h as f32);
            v.player = Some(player);
        },
    );
}

/// 讀取 PNG；超過顯示卡材質上限時縮小
fn decode_image(path: &str, max_side: usize) -> Option<(egui::ColorImage, Vec2)> {
    let bytes = std::fs::read(path).ok()?;
    let pm = tiny_skia::Pixmap::decode_png(&bytes).ok()?;
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
    Qr,
    /// 影片：目前這一格存成截圖
    Grab,
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
    let mut act = Act::None;
    if app.ask.is_none() {
        keyboard(&mut v, ctx, &mut act);
    }

    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 32.0).max(600.0), (screen.height() - 32.0).max(400.0));
    let modal = egui::Modal::new(Id::new("viewer")).frame(theme::modal_frame(ctx).inner_margin(0)).show(ctx, |ui| {
        ui.set_width(size.x);
        ui.set_height(size.y);
        let p = theme::pal(ui);
        let image = v.is_image();
        let e = v.cur().clone();
        // 標題列
        egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let title = if is_default_name(&e.media.name) { short_date(&e.media.name, e.media.mtime, true) } else { base_name(&e.media.name) };
                    ui.label(RichText::new(title).font(theme::font_bold(16.0)));
                    let mut meta = vec![];
                    if v.list.len() > 1 {
                        meta.push(format!("{} / {}", v.idx + 1, v.list.len()));
                    }
                    if let (Some(w), Some(h)) = (e.media.width, e.media.height) {
                        meta.push(format!("{w}×{h}"));
                    }
                    if !image {
                        if let Some(d) = e.media.duration_sec {
                            meta.push(video_clock(d));
                        }
                    }
                    meta.push(format_bytes(e.media.bytes));
                    meta.push(e.media.name.clone());
                    ui.label(RichText::new(meta.join(tr!("・", " · "))).font(theme::font(12.0)).color(p.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                        v.close = true;
                    }
                    ui.add_space(6.0);
                    if Btn::icon_only(Icon::Trash).ghost().small().tooltip(tr!("移到資源回收筒", "Move to Recycle Bin")).show(ui).clicked() {
                        act = Act::Delete;
                    }
                    if Btn::icon_only(Icon::Folder).ghost().small().tooltip(tr!("在資料夾中顯示", "Show in folder")).show(ui).clicked() {
                        act = Act::Entry(EntryAction::Reveal);
                    }
                    let other = if image {
                        tr!("用 Windows 的「相片」開啟", "Open in the Windows Photos app")
                    } else {
                        tr!("用 Windows 的媒體播放器開啟", "Open in the Windows media player")
                    };
                    if Btn::new(tr!("用 Windows 開啟", "Open in Windows")).icon(Icon::Export).small().tooltip(other).show(ui).clicked() {
                        act = Act::Entry(EntryAction::External);
                    }
                    if image {
                        if Btn::new(tr!("釘在桌面", "Pin to desktop"))
                            .small()
                            .tooltip(tr!(
                                "把這張圖變成浮在最上層的小視窗，方便對照（點兩下或 Esc 關閉）",
                                "Turn this image into a small always-on-top window for reference (double-click or Esc to close)"
                            ))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Pin;
                        }
                        if Btn::new(tr!("文字辨識", "OCR"))
                            .small()
                            .tooltip(tr!("把圖裡的文字轉成可以複製的文字", "Text recognition (OCR): turn the text in the image into text you can copy"))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Ocr;
                        }
                        if Btn::new(tr!("讀取 QR 碼", "Read QR code"))
                            .small()
                            .tooltip(tr!("找出圖裡的 QR 碼，讀出內容（網址可以直接開啟）", "Find a QR code in the image and read its content (links can be opened directly)"))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Qr;
                        }
                        if Btn::new(tr!("複製", "Copy"))
                            .small()
                            .tooltip(tr!("複製到剪貼簿（可直接貼到 LINE、Word、信件）", "Copy to clipboard (paste it straight into LINE, Word or an email)"))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Copy;
                        }
                        if Btn::new(tr!("編輯", "Edit"))
                            .icon(Icon::Edit)
                            .small()
                            .tooltip(tr!("加上標註、遮住個資、裁切（另存一張，原圖保留）", "Add annotations, redact personal info, crop (saved as a new image; the original is kept)"))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Entry(EntryAction::Edit);
                        }
                    } else {
                        if Btn::new(tr!("擷取這一格", "Grab frame"))
                            .icon(Icon::Camera)
                            .small()
                            .tooltip(tr!("把目前這一格存成截圖（原尺寸 PNG），並複製到剪貼簿", "Save the current frame as a screenshot (full-size PNG) and copy it to the clipboard"))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Grab;
                        }
                        if Btn::new(tr!("複製檔案", "Copy file"))
                            .small()
                            .tooltip(tr!("複製這個影片檔，可以直接貼到 LINE、Teams、信件或資料夾", "Copy this video file so you can paste it into LINE, Teams, an email or a folder"))
                            .show(ui)
                            .clicked()
                        {
                            act = Act::Entry(EntryAction::CopyFile);
                        }
                    }
                    if !image && !is_export_name(&e.media.name) {
                        if Btn::new(tr!("製作加速版 / GIF", "Make sped-up video / GIF")).icon(Icon::Export).small().show(ui).clicked() {
                            act = Act::Entry(EntryAction::Export);
                        }
                        if Btn::new(tr!("剪輯", "Edit")).icon(Icon::Cut).small().show(ui).clicked() {
                            act = Act::Entry(EntryAction::Edit);
                        }
                    }
                });
            });
        });
        // 畫面
        let bar_h = 52.0;
        let full = ui.available_rect_before_wrap();
        let stage = Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - bar_h));
        let resp = ui.allocate_rect(stage, Sense::click_and_drag());
        let painter = ui.painter_at(stage);
        painter.rect_filled(stage, CornerRadius::ZERO, Color32::from_gray(24));
        if image {
            image_stage(&mut v, ui, &painter, stage, &resp);
        } else {
            video_stage(&mut v, &painter, stage, &resp);
        }
        if v.loading && v.image.is_none() && v.video_tex.is_none() {
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
        // 下方控制列
        let bar = Rect::from_min_max(pos2(full.min.x, stage.max.y), full.max);
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
        return;
    }
    app.viewer = Some(v);
    match act {
        Act::None => {}
        Act::Prev => go(app, -1),
        Act::Next => go(app, 1),
        Act::Copy => copy_image(app),
        Act::Delete => delete_current(app),
        Act::Grab => {
            let Some(v) = app.viewer.as_mut() else { return };
            let (path, t) = (v.cur().media.path.clone(), v.player.as_ref().map(|p| p.time()).unwrap_or(0.0));
            let core = app.core.clone();
            app.spawn(async move { core.grab_frame(&path, t).await }, |app, r| {
                if let Err(e) = r {
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        Act::Qr => {
            let Some(path) = app.viewer.as_ref().map(|v| v.cur().media.path.clone()) else { return };
            app.spawn(async move { screenrecorder_core::actions::shot_qr(&path).await }, |app, r| match r {
                Ok(list) => app.show_qr(list),
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
            let Some(v) = app.viewer.as_mut() else { return };
            let e = v.cur().clone();
            if matches!(a, EntryAction::Edit | EntryAction::Export) && !super::dialogs::is_image(&e.media.path) {
                // 改到剪輯 / 製作視窗：關掉檢視器與清單（編輯截圖蓋在上面，關掉就回來）
                app.viewer = None;
                app.library = None;
            } else if a == EntryAction::External {
                v.pause();
            }
            app.act(a, e);
        }
    }
}

fn keyboard(v: &mut Viewer, ctx: &egui::Context, act: &mut Act) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let image = v.is_image();
    ctx.input(|i| {
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
            return;
        }
        if i.key_pressed(Key::PageUp) {
            *act = Act::Prev;
        }
        if i.key_pressed(Key::PageDown) {
            *act = Act::Next;
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
    });
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
    if shown.x > stage.width() || shown.y > stage.height() {
        resp.clone().on_hover_cursor(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
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

fn video_stage(v: &mut Viewer, painter: &egui::Painter, stage: Rect, resp: &egui::Response) {
    let Some(tex) = &v.video_tex else { return };
    let src = if v.video_size.x > 0.0 { v.video_size } else { tex.size_vec2() };
    let k = ((stage.width() - 24.0) / src.x).min((stage.height() - 24.0) / src.y);
    let r = Rect::from_center_size(stage.center(), src * k);
    painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    let Some(p) = &v.player else { return };
    // 點畫面：播放 / 暫停
    if resp.clicked() {
        toggle_play(p);
    }
    if !p.is_playing() && v.seeking.is_none() {
        painter.circle_filled(stage.center(), 34.0, Color32::from_black_alpha(150));
        theme::paint_icon(painter, Rect::from_center_size(stage.center() + vec2(3.0, 0.0), vec2(30.0, 30.0)), Icon::Play, Color32::WHITE);
    }
}

fn video_bar(v: &mut Viewer, ui: &mut egui::Ui, act: &mut Act) {
    let p = theme::pal(ui);
    let chapters = v.cur().media.chapters.clone();
    let Some(player) = &v.player else { return };
    let playing = player.is_playing();
    let dur = player.spec().duration.max(0.001);
    let t = player.time().min(dur);
    let tip = if playing { tr!("暫停（空白鍵）", "Pause (Space)") } else { tr!("播放（空白鍵）", "Play (Space)") };
    if Btn::icon_only(if playing { Icon::Pause } else { Icon::Play }).small().tooltip(tip).show(ui).clicked() {
        toggle_play(player);
    }
    ui.label(RichText::new(format!("{} / {}", video_clock(t), video_clock(dur))).font(theme::mono(12.5)));
    // 音量（右邊），進度列填滿中間
    let vol_w = 140.0;
    let chap_w = if chapters.is_empty() { 0.0 } else { 84.0 };
    let track_w = (ui.available_width() - vol_w - chap_w - 16.0).max(80.0);
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
        let text = match chapters.iter().rev().find(|c| c.start <= at) {
            Some(c) => format!("{}・{}", video_clock(at), c.title),
            None => video_clock(at),
        };
        resp.clone().on_hover_text_at_pointer(text);
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
                if ui.button(format!("{}  {}", video_clock(c.start), c.title)).clicked() {
                    player.seek(c.start);
                }
            }
            ui.separator();
            if ui.button(tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)")).clicked() {
                *act = Act::Entry(EntryAction::CopyChapters);
            }
        });
    }
    // 音量
    ui.add_space(8.0);
    let has_audio = player.spec().has_audio;
    let icon_tip = if !has_audio {
        tr!("這個影片沒有聲音", "This video has no audio")
    } else if v.muted {
        tr!("取消靜音", "Unmute")
    } else {
        tr!("靜音", "Mute")
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
        resp.on_hover_text(trf!("音量 {:.0}%", "Volume {:.0}%", v.volume_now() * 100.0));
    }
}

/// 加速版、GIF（不能再剪輯或製作加速版）
fn is_export_name(name: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)(_\d+(\.\d+)?x\.mp4|\.gif)$").unwrap());
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
