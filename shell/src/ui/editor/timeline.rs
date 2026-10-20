//! 時間軸（縮圖、保留 / 刪除的片段、頭尾把手、選取範圍、播放頭）、標註軌、放大時的捲軸。
//!
//! 時間軸：上方的尺規點一下或拖曳移動播放頭；縮圖上點一下跳到那裡，拖曳是選取要刪除 / 加速的範圍；
//! 拖曳黃色把手調整頭尾、拖曳上方橫條移動整段。有分割點時點一下選取那一段。標記：點一下跳過去、按兩下改名、右鍵刪除。
//! 滾輪：放大 / 縮小（以滑鼠位置為中心）；Shift 或左右滾動 = 平移。
//! 標註軌：點一下選取（並跳到它出現的時間）；拖曳移動出現時間，拖曳兩端調整開始 / 結束；點空白處跳到該時間。

use super::{ann_color, hash_of, Drag, Edge, Editor, Tab, TlKind, VAct};
use crate::ui::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions};
use screenrecorder_core::annotate;
use screenrecorder_core::edit::normalize_ranges;
use screenrecorder_core::format::video_clock;
use screenrecorder_core::player::{strip_frames, Frame};
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TL_H: f32 = 64.0;
/// 縮圖上方的時間尺規（點一下或拖曳移動播放頭）
pub const RULER_H: f32 = 18.0;
/// 標註軌固定高度（不隨標註數量變高，影片才不會在放置標註時跳動）；列多時每列變矮
pub const AT_H: f32 = 70.0;
/// 還沒有標註、也不在「標註」分頁時，標註軌縮成一條，影片可以大一點
pub const AT_H_EMPTY: f32 = 26.0;

/// 標註軌的高度（切到「標註」分頁時就先變高，放標註時影片不會跳動）
pub fn ann_track_h(ed: &Editor) -> f32 {
    if ed.anns.is_empty() && !matches!(ed.tab, Tab::Ann | Tab::Subs) {
        AT_H_EMPTY
    } else {
        AT_H
    }
}
pub const SCROLL_H: f32 = 10.0;
/// 標註軌上比這個窄（像素）的一條，兩端的拖曳範圍會重疊：改成依拖的方向決定拉哪一端
const NARROW_BAR: f32 = 24.0;

const YELLOW: Color32 = Color32::from_rgb(0xf5, 0xb3, 0x01);
/// 錄影時加的標記（和錄影外框上的「・標記 N」同色）
const MARK: Color32 = Color32::from_rgb(0xff, 0x8a, 0x1f);
const FLAG_H: f32 = 22.0;
/// 加速的片段
const FAST: Color32 = Color32::from_rgb(0x00, 0x78, 0xff);
/// 分析找到的片段：不動又沒聲音（青綠色）、沒說話（紫色）
pub(crate) const IDLE_MARK: Color32 = Color32::from_rgb(0x14, 0xb8, 0xa6);
pub(crate) const QUIET_MARK: Color32 = Color32::from_rgb(0xa8, 0x55, 0xf7);
/// 錄影時麥克風靜音的時段
/// 片段順序的編號
const ORDER_MARK: Color32 = Color32::from_rgb(214, 120, 0);
/// 手動放大的色帶
const REGION_MARK: Color32 = Color32::from_rgb(0, 130, 230);
/// 旁白的色帶
const VOICE_MARK: Color32 = Color32::from_rgb(28, 150, 90);
/// 定格的線
const FREEZE_MARK: Color32 = Color32::from_rgb(150, 80, 220);
const MUTE_MARK: Color32 = Color32::from_rgb(0x64, 0x74, 0x8b);

/// 時間軸縮圖：在背景依序解出時間軸上各個時間點的小圖（放大後只解顯示的範圍）；
/// 一張一張出現；關閉、換影片或再次縮放時停止。
pub struct Strip {
    key: u64,
    due: Option<Instant>,
    cancel: Arc<AtomicBool>,
    gen: u64,
    inbox: Arc<Mutex<Vec<(u64, usize, Frame)>>>,
    tiles: Vec<Option<TextureHandle>>,
}

/// 聲音波形（開啟剪輯視窗後在背景讀一次）
#[derive(Default)]
pub struct Wave {
    started: bool,
    data: Arc<Mutex<Option<Arc<Vec<u8>>>>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Wave {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Wave {
    fn peaks(&mut self, ffmpeg: &std::path::Path, path: &str, ctx: &egui::Context) -> Option<Arc<Vec<u8>>> {
        if !self.started {
            self.started = true;
            let (ffmpeg, path, data, cancel, c) = (ffmpeg.to_path_buf(), path.to_string(), self.data.clone(), self.cancel.clone(), ctx.clone());
            std::thread::spawn(move || {
                if let Some(p) = screenrecorder_core::waveform::load(&ffmpeg, &path, &cancel) {
                    *data.lock().unwrap() = Some(Arc::new(p));
                    c.request_repaint();
                }
            });
        }
        self.data.lock().unwrap().clone()
    }
}

/// 縮圖下方的聲音波形（以中線上下對稱，越大聲越高）
fn draw_wave(painter: &egui::Painter, rect: Rect, peaks: &[u8], view: (f64, f64), color: Color32) {
    let band = Rect::from_min_max(pos2(rect.left(), rect.bottom() - rect.height() * 0.42), rect.max);
    painter.rect_filled(band, CornerRadius::ZERO, Color32::from_black_alpha(175));
    let (a, b) = view;
    let per_px = (b - a) / rect.width().max(1.0) as f64;
    let mid = band.center().y;
    let half = band.height() / 2.0 - 2.0;
    let mut x = band.left();
    while x < band.right() {
        let t = a + (x - rect.left()) as f64 * per_px;
        let v = screenrecorder_core::waveform::max_in(peaks, t, t + per_px * 2.0);
        if v > 0.02 {
            let h = (v * half).max(1.0);
            painter.line_segment([pos2(x + 0.5, mid - h), pos2(x + 0.5, mid + h)], Stroke::new(1.5, color));
        }
        x += 2.0;
    }
}

impl Default for Strip {
    fn default() -> Self {
        Strip { key: 0, due: None, cancel: Arc::new(AtomicBool::new(false)), gen: 0, inbox: Arc::default(), tiles: vec![] }
    }
}

impl Strip {
    pub fn reset(&mut self) {
        self.cancel();
        self.key = 0;
        self.tiles.clear();
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Strip {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn update_strip(ed: &mut Editor, ctx: &egui::Context, rect: Rect) -> f32 {
    let ar = if ed.vw > 0.0 && ed.vh > 0.0 { (ed.vw / ed.vh) as f32 } else { 16.0 / 9.0 };
    let tile_w = TL_H * ar;
    let n = (rect.width() / tile_w).ceil().max(1.0) as usize;
    let key = hash_of(&(ed.view.0.to_bits(), ed.view.1.to_bits(), n, ed.entry.media.path.as_str()));
    let st = &mut ed.strip;
    if key != st.key {
        st.key = key;
        st.due = Some(Instant::now() + if st.tiles.is_empty() { Duration::ZERO } else { Duration::from_millis(150) });
    }
    if let Some(due) = st.due {
        if Instant::now() >= due {
            st.due = None;
            st.cancel();
            st.cancel = Arc::new(AtomicBool::new(false));
            st.gen += 1;
            st.tiles = vec![None; n];
            let ppp = ctx.pixels_per_point();
            let h = ((TL_H * ppp).round() as u32 / 2 * 2).max(2);
            let w = ((tile_w * ppp).round() as u32 / 2 * 2).max(2);
            let (a, b) = ed.view;
            let last = (ed.duration - 0.05).max(0.0);
            let times: Vec<f64> = (0..n).map(|i| (a + (i as f64 + 0.5) / n as f64 * (b - a)).min(last)).collect();
            let (ffmpeg, path, cancel, inbox, gen, c) = (ed.ffmpeg.clone(), ed.entry.media.path.clone(), st.cancel.clone(), st.inbox.clone(), st.gen, ctx.clone());
            std::thread::spawn(move || {
                strip_frames(&ffmpeg, &path, &times, w, h, &cancel, |k, f| {
                    inbox.lock().unwrap().push((gen, k, f));
                    c.request_repaint();
                });
            });
        } else {
            ctx.request_repaint_after(due - Instant::now());
        }
    }
    let got: Vec<_> = std::mem::take(&mut *st.inbox.lock().unwrap());
    for (g, k, f) in got {
        if g == st.gen && k < st.tiles.len() {
            let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
            st.tiles[k] = Some(ctx.load_texture(format!("strip-{k}"), img, TextureOptions::LINEAR));
        }
    }
    tile_w
}

/// 剪掉的部分：暗底加紅色斜線
fn stripes(p: &egui::Painter, r: Rect, bg: Color32, line: Color32) {
    if !r.is_positive() {
        return;
    }
    p.rect_filled(r, CornerRadius::ZERO, bg);
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let mut x = r.left() - r.height();
    // 對齊到整個時間軸的格線，捲動時斜線不會跳
    x -= x.rem_euclid(12.0);
    while x < r.right() {
        p.line_segment([pos2(x, r.bottom()), pos2(x + r.height(), r.top())], Stroke::new(4.0, line));
        x += 12.0;
    }
}

/// 尺規的刻度間隔（秒）：讓標籤之間大約有 min_px 像素
fn tick_step(span: f64, width: f32, min_px: f32) -> f64 {
    let per_px = span / width.max(1.0) as f64;
    [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0].into_iter().find(|s| s / per_px >= min_px as f64).unwrap_or(7200.0)
}

/// 尺規上的時間：短的影片顯示到 0.1 秒
fn tick_label(t: f64, step: f64) -> String {
    let s = t.max(0.0);
    if step < 1.0 {
        format!("{}:{:04.1}", (s / 60.0).floor() as u64, s % 60.0)
    } else if s >= 3600.0 {
        format!("{}:{:02}:{:02}", (s / 3600.0) as u64, (s / 60.0) as u64 % 60, s as u64 % 60)
    } else {
        format!("{}:{:02}", (s / 60.0) as u64, s as u64 % 60)
    }
}

/// 時間軸上可以點的東西
enum Hit {
    /// 還原刪掉的片段（第幾段）
    Restore(usize),
    /// 標記（第幾個、時間）
    Flag(usize, f64),
    /// 跟著點擊放大的一次點擊（時間）
    Click(f64),
    /// 分割點（第幾個）
    Split(usize),
    /// 其他地方：拖曳的種類
    Drag(TlKind),
}

pub fn show(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    let p = theme::pal(ui);
    let w = ui.available_width();
    let (a, b) = ed.view;
    let span = (b - a).max(1e-9);

    // ── 尺規（點一下或拖曳移動播放頭）與縮圖 ──
    let (all, resp) = ui.allocate_exact_size(vec2(w, RULER_H + TL_H), Sense::click_and_drag());
    let ruler = Rect::from_min_size(all.min, vec2(w, RULER_H));
    let rect = Rect::from_min_max(pos2(all.left(), ruler.bottom()), all.max);
    let x_of = |t: f64| rect.left() + ((t - a) / span) as f32 * rect.width();
    let dur = ed.duration;
    let time_at = |x: f32| (a + ((x - rect.left()) / rect.width()) as f64 * span).clamp(0.0, dur);
    let tile_w = update_strip(ed, ctx, rect);
    let rp = ui.painter_at(ruler);
    let step = tick_step(span, w, 64.0);
    let mut t = (a / step).ceil() * step;
    while t <= b + 1e-9 {
        let x = x_of(t);
        rp.vline(x, ruler.bottom() - 5.0..=ruler.bottom(), Stroke::new(1.0, p.muted));
        rp.text(pos2(x + 3.0, ruler.center().y - 1.0), Align2::LEFT_CENTER, tick_label(t, step), theme::mono(10.5), p.muted);
        // 中間的小刻度
        let xm = x_of(t + step / 2.0);
        if xm < ruler.right() {
            rp.vline(xm, ruler.bottom() - 3.0..=ruler.bottom(), Stroke::new(1.0, p.border_strong));
        }
        t += step;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), p.surface2);
    for (i, t) in ed.strip.tiles.iter().enumerate() {
        if let Some(t) = t {
            let r = Rect::from_min_size(pos2(rect.left() + i as f32 * tile_w, rect.top()), vec2(tile_w.ceil(), TL_H));
            painter.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha(235));
        }
    }
    if let Some(peaks) = ed.wave.peaks(&ed.ffmpeg, &ed.entry.media.path, ctx) {
        draw_wave(&painter, rect, &peaks, ed.view, Color32::from_rgb(140, 205, 255));
    }
    let cut_bg = Color32::from_black_alpha(158);
    let cut_line = Color32::from_rgba_unmultiplied(229, 72, 77, 115);
    let full = |t0: f64, t1: f64| Rect::from_min_max(pos2(x_of(t0), rect.top()), pos2(x_of(t1), rect.bottom()));
    if ed.spec.start > 0.0 {
        stripes(&painter, full(0.0, ed.spec.start), cut_bg, cut_line);
    }
    if ed.spec.end < ed.duration {
        stripes(&painter, full(ed.spec.end, ed.duration), cut_bg, cut_line);
    }
    let removed = normalize_ranges(&ed.spec.removed, ed.duration);
    let mut restore_btns: Vec<(Pos2, usize, String)> = vec![];
    for (i, &(r0, r1)) in removed.iter().enumerate() {
        let r = full(r0, r1);
        stripes(&painter, r, cut_bg, cut_line);
        painter.vline(r.left(), r.y_range(), Stroke::new(1.0, p.rec));
        painter.vline(r.right(), r.y_range(), Stroke::new(1.0, p.rec));
        restore_btns.push((pos2(r.center().x, rect.top() + 14.0), i, trf!("還原這段（{} – {}）", "Restore this section ({} – {})", video_clock(r0), video_clock(r1))));
    }
    // 加速的片段：藍色，中間寫倍率
    for f in ed.spec.fast.iter().filter(|f| f.end() > a && f.start() < b) {
        let r = full(f.start(), f.end());
        painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(0, 120, 255, 70));
        painter.hline(r.x_range(), r.bottom() - 2.0, Stroke::new(3.0, FAST));
        let label = format!("{}×", screenrecorder_core::format::speed_label(f.speed));
        let g = painter.layout_no_wrap(label, theme::font_bold(12.0), Color32::WHITE);
        if r.width() > g.size().x + 8.0 {
            let c = pos2(r.center().x, rect.top() + 21.0);
            painter.rect_filled(Rect::from_center_size(c, g.size() + vec2(8.0, 2.0)), CornerRadius::same(4), FAST);
            painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
        }
    }
    // 旁白：下緣一條綠色帶（夠寬時寫「旁白」或語音的內容）
    for c in ed.spec.narration.clips.iter().filter(|c| c.end() > a && c.at() < b) {
        let r = full(c.at(), c.end().min(ed.duration)).shrink2(vec2(1.0, 0.0));
        let band = Rect::from_min_max(pos2(r.left(), r.bottom() - 14.0), pos2(r.right(), r.bottom() - 2.0));
        painter.rect_filled(band, CornerRadius::same(3), VOICE_MARK);
        let label = if c.text.is_empty() { tr!("旁白", "Narration").to_string() } else { c.text.chars().take(20).collect::<String>() };
        let g = painter.layout_no_wrap(label, theme::font_bold(10.0), Color32::WHITE);
        if band.width() > g.size().x + 6.0 {
            painter.galley(pos2(band.left() + 3.0, band.center().y - g.size().y / 2.0), g, Color32::WHITE);
        }
    }
    // 片段改了順序：每段開頭標上第幾段
    if ed.is_reordered() {
        for (i, (s0, s1)) in ed.ordered_keep().iter().enumerate().filter(|(_, r)| r.1 > a && r.0 < b) {
            let x = x_of(s0.max(a));
            let g = painter.layout_no_wrap(format!("{}", i + 1), theme::font_bold(11.0), Color32::WHITE);
            let c = pos2(x + 11.0, rect.bottom() - 12.0);
            if x_of(*s1) - x > 22.0 {
                painter.circle_filled(c, 9.0, ORDER_MARK);
                painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
            }
        }
    }
    // 手動放大：上緣一條藍色帶
    for r in ed.spec.zoom_regions.iter().filter(|r| r.end > a && r.start < b) {
        let rr = full(r.start, r.end).shrink2(vec2(1.0, 0.0));
        let band = Rect::from_min_max(pos2(rr.left(), rr.top() + 2.0), pos2(rr.right(), rr.top() + 14.0));
        painter.rect_filled(band, CornerRadius::same(3), REGION_MARK);
        let g = painter.layout_no_wrap(trf!("放大 {}×", "Zoom {}×", screenrecorder_core::format::speed_label((r.factor() * 10.0).round() / 10.0)), theme::font_bold(10.0), Color32::WHITE);
        if band.width() > g.size().x + 6.0 {
            painter.galley(pos2(band.left() + 3.0, band.center().y - g.size().y / 2.0), g, Color32::WHITE);
        }
    }
    // 定格：一條直線，上面寫停幾秒
    for f in ed.spec.freezes.iter().filter(|f| f.at() >= a && f.at() <= b) {
        let x = x_of(f.at());
        painter.vline(x, rect.y_range(), Stroke::new(2.0, FREEZE_MARK));
        let g = painter.layout_no_wrap(trf!("定格 {} 秒", "Freeze {}s", screenrecorder_core::format::speed_label(f.secs())), theme::font_bold(10.5), Color32::WHITE);
        let c = pos2(x, rect.top() + 21.0);
        painter.rect_filled(Rect::from_center_size(c, g.size() + vec2(8.0, 2.0)), CornerRadius::same(4), FREEZE_MARK);
        painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
    }
    // 錄影時麥克風靜音的時段：上緣一條灰色帶（夠寬時寫「麥克風靜音」）
    for &[m0, m1] in ed.mutes.iter().filter(|m| m[1] > a && m[0] < b) {
        let r = full(m0, m1).shrink2(vec2(1.0, 0.0));
        let band = Rect::from_min_max(pos2(r.left(), r.top() + 6.0), pos2(r.right(), r.top() + 22.0));
        painter.rect_filled(band, CornerRadius::ZERO, MUTE_MARK);
        for label in [tr!("🔇 麥克風靜音", "🔇 Mic muted"), "🔇"] {
            let g = painter.layout_no_wrap(label.to_string(), theme::font_bold(10.5), Color32::WHITE);
            if band.width() > g.size().x + 6.0 {
                painter.galley(band.center() - g.size() / 2.0, g, Color32::WHITE);
                break;
            }
        }
    }
    // 分析找到、還沒處理的片段：淡淡的底色，上緣一條色帶
    if let Some(found) = &ed.idle_found {
        for (list, col) in [(&found.idle, IDLE_MARK), (&found.quiet, QUIET_MARK)] {
            for &(f0, f1) in list.iter().filter(|(f0, f1)| *f1 > a && *f0 < b) {
                let r = full(f0, f1).shrink2(vec2(1.0, 0.0));
                painter.rect_filled(r, CornerRadius::ZERO, col.gamma_multiply(0.30));
                // 下緣一條色帶，夠寬時寫上是哪一種
                let band = Rect::from_min_max(pos2(r.left(), r.bottom() - 16.0), r.max);
                painter.rect_filled(band, CornerRadius::ZERO, col);
                let label = if col == IDLE_MARK { tr!("沒動靜", "Idle") } else { tr!("沒說話", "Quiet") };
                let g = painter.layout_no_wrap(label.to_string(), theme::font_bold(11.0), Color32::WHITE);
                if band.width() > g.size().x + 6.0 {
                    painter.galley(band.center() - g.size() / 2.0, g, Color32::WHITE);
                }
            }
        }
    }
    // 分割點：白色直線（尺規上一個小三角形）
    let splits: Vec<(usize, f32)> = ed.splits.iter().enumerate().filter(|(_, t)| **t >= a && **t <= b).map(|(i, t)| (i, x_of(*t))).collect();
    for &(_, x) in &splits {
        painter.vline(x, rect.y_range(), Stroke::new(3.5, Color32::from_black_alpha(120)));
        painter.vline(x, rect.y_range(), Stroke::new(1.5, Color32::WHITE));
        let tri = vec![pos2(x - 4.0, ruler.bottom() - 6.0), pos2(x + 4.0, ruler.bottom() - 6.0), pos2(x, ruler.bottom())];
        rp.add(egui::Shape::convex_polygon(tri, p.text, Stroke::NONE));
    }
    // 選取的範圍：實線外框、兩端把手、上方寫選了幾秒
    let sel_x = ed.sel.map(|(s0, s1)| (x_of(s0), x_of(s1).max(x_of(s0) + 1.0)));
    if let (Some((s0, s1)), Some((x0, x1))) = (ed.sel, sel_x) {
        let r = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
        painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(214, 140, 18, 70));
        painter.rect_stroke(r.shrink(1.0), CornerRadius::same(2), Stroke::new(2.0, p.warn), egui::StrokeKind::Inside);
        for x in [x0, x1] {
            let grip = Rect::from_center_size(pos2(x, rect.center().y), vec2(7.0, 24.0));
            painter.rect(grip, CornerRadius::same(3), p.warn, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Outside);
        }
        let g = painter.layout_no_wrap(trf!("已選取 {} 秒", "{} s selected", fmt_secs(s1 - s0)), theme::font_bold(11.5), Color32::WHITE);
        let c = pos2(((x0 + x1) / 2.0).max(rect.left() + g.size().x / 2.0 + 6.0).min(rect.right() - g.size().x / 2.0 - 6.0), rect.top() + 21.0);
        painter.rect_filled(Rect::from_center_size(c, g.size() + vec2(10.0, 4.0)), CornerRadius::same(4), p.warn);
        painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
    }
    // 保留範圍：上方的黃色橫條與頭尾把手
    let (xs, xe) = (x_of(ed.spec.start), x_of(ed.spec.end));
    painter.rect_filled(Rect::from_min_max(pos2(xs, rect.top()), pos2(xe, rect.top() + 9.0)), CornerRadius::ZERO, YELLOW.gamma_multiply(0.9));
    let bracket = |x: f32, dir: f32| {
        let pts = vec![pos2(x + dir * 8.0, rect.top() + 1.5), pos2(x, rect.top() + 1.5), pos2(x, rect.bottom() - 1.5), pos2(x + dir * 8.0, rect.bottom() - 1.5)];
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(4.5, Color32::from_black_alpha(90))));
        painter.add(egui::Shape::line(pts, Stroke::new(3.0, YELLOW)));
    };
    bracket(xs + 1.5, 1.0);
    bracket(xe - 1.5, -1.0);
    for (c, _, _) in &restore_btns {
        painter.circle_filled(*c + vec2(0.0, 1.0), 10.0, Color32::from_black_alpha(80));
        painter.circle_filled(*c, 10.0, p.rec);
        painter.text(*c, Align2::CENTER_CENTER, "×", theme::font_bold(13.0), Color32::WHITE);
    }
    // 標記：底部的小旗子（寫名稱或第幾個）；點一下跳過去、按兩下改名、右鍵刪除
    let flags: Vec<(Rect, usize, f64)> = ed
        .markers
        .iter()
        .enumerate()
        .filter(|(_, m)| m.t >= a && m.t <= b && m.t <= dur)
        .map(|(i, m)| {
            let x = x_of(m.t);
            let label = if m.name.trim().is_empty() { (i + 1).to_string() } else { m.name.trim().chars().take(10).collect::<String>() + if m.name.trim().chars().count() > 10 { "…" } else { "" } };
            let g = painter.layout_no_wrap(label, theme::font_bold(10.0), Color32::WHITE);
            let (top, bot) = (rect.bottom() - FLAG_H, rect.bottom() - 1.0);
            painter.vline(x, top..=bot, Stroke::new(3.5, Color32::from_black_alpha(120)));
            painter.vline(x, top..=bot, Stroke::new(2.0, MARK));
            let flag = Rect::from_min_size(pos2(x, top), vec2(g.size().x + 9.0, 13.0));
            painter.rect(flag, CornerRadius { nw: 0, sw: 0, ne: 4, se: 4 }, MARK, Stroke::new(1.0, Color32::from_black_alpha(140)), egui::StrokeKind::Outside);
            painter.galley(pos2(flag.left() + 4.5, flag.center().y - g.size().y / 2.0), g, Color32::WHITE);
            (Rect::from_min_max(pos2(x - 4.0, top - 2.0), pos2(flag.right() + 2.0, bot)), i, m.t)
        })
        .collect();
    // 跟著點擊放大：點擊的時間（藍點；設定成這次不放大的是空心灰點），點一下切換
    let dots: Vec<(Pos2, f64, bool)> =
        if ed.zoom_on() { ed.zoom_source().iter().filter(|c| c[0] >= a && c[0] <= b).map(|c| (pos2(x_of(c[0]), rect.bottom() - 7.0), c[0], ed.click_skipped(c[0]))).collect() } else { vec![] };
    for &(c, _, skipped) in &dots {
        if skipped {
            painter.circle(c, 4.5, Color32::from_black_alpha(90), Stroke::new(1.5, Color32::from_gray(200)));
        } else {
            painter.circle(c, 5.0, FAST, Stroke::new(1.5, Color32::WHITE));
        }
    }
    // 播放頭
    let t_now = ed.now();
    let xp = x_of(t_now);
    painter.vline(xp, rect.y_range(), Stroke::new(3.0, Color32::from_black_alpha(110)));
    painter.vline(xp, rect.y_range(), Stroke::new(2.0, Color32::WHITE));
    let knob = vec![pos2(xp - 6.0, ruler.top() + 4.0), pos2(xp + 6.0, ruler.top() + 4.0), pos2(xp + 6.0, ruler.bottom() - 5.0), pos2(xp, ruler.bottom()), pos2(xp - 6.0, ruler.bottom() - 5.0)];
    ui.painter_at(all).add(egui::Shape::convex_polygon(knob, p.accent, Stroke::new(1.0, Color32::WHITE)));

    let (pressed, secondary, down, pos, latest, dbl) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.secondary_pressed(),
            i.pointer.primary_down(),
            i.pointer.interact_pos(),
            i.pointer.latest_pos(),
            i.pointer.button_double_clicked(egui::PointerButton::Primary),
        )
    });
    let hit = |pos: Pos2| -> Hit {
        if ruler.contains(pos) {
            if let Some(&(i, _)) = splits.iter().find(|(_, x)| (pos.x - x).abs() <= 5.0) {
                return Hit::Split(i);
            }
            return Hit::Drag(TlKind::Scrub);
        }
        if let Some((_, i, _)) = restore_btns.iter().find(|(c, _, _)| c.distance(pos) <= 10.0) {
            return Hit::Restore(*i);
        }
        if let Some(&(_, i, t)) = flags.iter().find(|(r, _, _)| r.contains(pos)) {
            return Hit::Flag(i, t);
        }
        if let Some(&(_, t, _)) = dots.iter().find(|(c, _, _)| c.distance(pos) <= 7.0) {
            return Hit::Click(t);
        }
        if let Some((x0, x1)) = sel_x {
            if (pos.x - x0).abs() <= 6.0 {
                return Hit::Drag(TlKind::SelA);
            }
            if (pos.x - x1).abs() <= 6.0 {
                return Hit::Drag(TlKind::SelB);
            }
        }
        if pos.x >= xs - 4.0 && pos.x <= xs + 12.0 {
            Hit::Drag(TlKind::Start)
        } else if pos.x >= xe - 12.0 && pos.x <= xe + 4.0 {
            Hit::Drag(TlKind::End)
        } else if (pos.x - xp).abs() <= 7.0 {
            Hit::Drag(TlKind::Playhead)
        } else if pos.y <= rect.top() + 9.0 && pos.x >= xs && pos.x <= xe {
            Hit::Drag(TlKind::Range)
        } else if let Some(&(i, _)) = splits.iter().find(|(_, x)| (pos.x - x).abs() <= 4.0) {
            Hit::Split(i)
        } else {
            // 縮圖上拖曳就是選取一段（不用按 Shift）；點一下是跳到那裡
            Hit::Drag(TlKind::Select)
        }
    };
    let mut tip: Option<String> = None;
    if resp.hovered() && matches!(ed.drag, Drag::None) {
        if let Some(pos) = pos {
            let (cursor, t): (CursorIcon, String) = match hit(pos) {
                Hit::Restore(i) => (CursorIcon::PointingHand, restore_btns.get(i).map(|r| r.2.clone()).unwrap_or_default()),
                Hit::Flag(i, t) => (
                    CursorIcon::PointingHand,
                    trf!(
                        "{}（{}）・點一下跳過去・按兩下改名（輸出時當作章節名稱）・右鍵刪除・Shift+M 下一個",
                        "{} ({}) · click to jump there · double-click to rename (used as the chapter title) · right-click to delete · Shift+M: next",
                        ed.marker_name(i),
                        video_clock(t)
                    ),
                ),
                Hit::Click(t) => (
                    CursorIcon::PointingHand,
                    if ed.click_skipped(t) {
                        trf!("滑鼠點擊 {}・這次點擊不放大・點一下恢復放大", "Mouse click at {} · not zoomed · click to zoom again", video_clock(t))
                    } else {
                        trf!("滑鼠點擊 {}・點一下：這次點擊不放大", "Mouse click at {} · click to skip zooming here", video_clock(t))
                    },
                ),
                Hit::Split(i) => (CursorIcon::PointingHand, trf!("分割點 {}・右鍵移除", "Split at {} · right-click to remove", ed.splits.get(i).map(|t| video_clock(*t)).unwrap_or_default())),
                Hit::Drag(k) => match k {
                    TlKind::Start => (CursorIcon::ResizeHorizontal, tr!("拖曳調整開頭", "Drag to adjust the start").into()),
                    TlKind::End => (CursorIcon::ResizeHorizontal, tr!("拖曳調整結尾", "Drag to adjust the end").into()),
                    TlKind::SelA | TlKind::SelB => (CursorIcon::ResizeHorizontal, tr!("拖曳調整選取的範圍", "Drag to adjust the selection").into()),
                    TlKind::Playhead => (CursorIcon::Grab, tr!("拖曳跳到其他時間", "Drag to jump to another time").into()),
                    TlKind::Range => (CursorIcon::Grab, tr!("拖曳整段保留範圍（長度不變）", "Drag to move the whole kept range (the length stays the same)").into()),
                    TlKind::Select => (CursorIcon::Text, tr!("拖曳選取一段（刪除、加速）・點一下跳到這裡", "Drag to select a section (delete, speed up) · click to jump here").into()),
                    TlKind::Scrub if ruler.contains(pos) => (CursorIcon::PointingHand, tr!("點一下或拖曳移動播放頭", "Click or drag to move the playhead").into()),
                    TlKind::Scrub => (CursorIcon::PointingHand, String::new()),
                },
            };
            ui.ctx().set_cursor_icon(cursor);
            if !t.is_empty() {
                tip = Some(t);
            }
        }
    }
    // 右鍵：刪除標記、移除分割點
    if secondary && resp.hovered() {
        if let Some(pos) = pos {
            match hit(pos) {
                Hit::Flag(i, _) => {
                    let name = ed.marker_name(i);
                    ed.markers.remove(i);
                    *toast = Some((trf!("已刪除{}，可按 Ctrl+Z 復原", "Deleted {}. Press Ctrl+Z to undo", name), false));
                }
                Hit::Split(i) => {
                    ed.splits.remove(i);
                }
                _ => {}
            }
        }
    }
    if dbl && resp.hovered() {
        if let Some(Hit::Flag(i, _)) = pos.map(hit) {
            ed.drag = Drag::None;
            ed.vpending = Some(VAct::RenameMarker(i));
        }
    }
    if pressed && resp.hovered() && ed.duration > 0.0 {
        if let Some(pos) = pos {
            match hit(pos) {
                Hit::Restore(i) => ed.restore_removed(i),
                Hit::Flag(_, t) => {
                    ed.stop_preview();
                    ed.player.pause();
                    ed.seek(t);
                }
                Hit::Click(t) => ed.toggle_click(t),
                Hit::Split(_) => {}
                Hit::Drag(kind) => {
                    ed.stop_preview();
                    ed.player.pause();
                    let orig = match kind {
                        TlKind::SelA | TlKind::SelB => ed.sel.unwrap_or_default(),
                        _ => (ed.spec.start, ed.spec.end),
                    };
                    ed.drag = Drag::Timeline { kind, x0: pos.x, t0: time_at(pos.x), moved: false, orig };
                    if kind == TlKind::Scrub {
                        ed.seek(time_at(pos.x));
                    }
                }
            }
        }
    } else if let Drag::Timeline { kind, x0, t0, moved, orig } = ed.drag {
        let lx = latest.map(|p| p.x).unwrap_or(x0);
        let t = time_at(lx);
        if down {
            let moved = moved || (lx - x0).abs() > 4.0;
            ed.drag = Drag::Timeline { kind, x0, t0, moved, orig };
            match kind {
                TlKind::Start => {
                    ed.set_start(t);
                    // 畫面跟著顯示新的開頭
                    ed.seek(ed.spec.start);
                }
                TlKind::End => {
                    ed.set_end(t);
                    ed.seek(ed.spec.end);
                }
                TlKind::Range => {
                    if moved {
                        // 整段平移，長度不變
                        let len = orig.1 - orig.0;
                        let s = (orig.0 + (t - t0)).clamp(0.0, (ed.duration - len).max(0.0));
                        ed.spec.start = s;
                        ed.spec.end = s + len;
                        ed.seek(s);
                    }
                }
                TlKind::Playhead | TlKind::Scrub => ed.seek(t),
                TlKind::SelA | TlKind::SelB => {
                    let other = if kind == TlKind::SelA { orig.1 } else { orig.0 };
                    ed.set_sel(Some((t.min(other), t.max(other))));
                    ed.seek(t);
                }
                TlKind::Select => {
                    if moved {
                        ed.set_sel(Some((t0.min(t), t0.max(t))));
                        ed.seek(t);
                    }
                }
            }
        } else {
            // 沒有拖曳的點擊：跳到該時間；有分割時選取點到的那一段，否則取消選取
            if matches!(kind, TlKind::Select | TlKind::Scrub | TlKind::Range) && !moved {
                ed.seek(t);
                let seg = ed.segment_at(t);
                ed.set_sel(seg);
                // 在「加速」分頁時留在那裡（選好一段就能設定速度）
                if seg.is_some() && ed.tab != Tab::Speed {
                    ed.tab = Tab::Time;
                }
            } else if matches!(kind, TlKind::Select | TlKind::SelA | TlKind::SelB) && ed.sel.is_some() {
                // 選好要刪除的範圍：顯示「刪除這段」的按鈕（在「加速」分頁時顯示速度的按鈕）
                if ed.tab != Tab::Speed {
                    ed.tab = Tab::Time;
                }
            }
            ed.drag = Drag::None;
        }
    }
    if let Some(t) = tip {
        resp.clone().on_hover_text(t);
    }
    wheel(ed, ui, &resp, rect);

    // ── 標註軌 ──
    ui.add_space(4.0 - ui.spacing().item_spacing.y);
    ann_track(ed, ui);

    // ── 捲軸 ──
    ui.add_space(4.0 - ui.spacing().item_spacing.y);
    scrollbar(ed, ui);
}

/// 秒數（一位小數，去掉多餘的 0）
fn fmt_secs(s: f64) -> String {
    screenrecorder_core::format::num((s * 10.0).round() / 10.0)
}

/// 快速鍵說明（按「快速鍵」打開）
pub(super) fn shortcut_sheet(resp: &egui::Response) {
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(320.0);
        ui.label(RichText::new(tr!("剪輯的快速鍵", "Editing shortcuts")).font(theme::font_bold(13.5)));
        let rows: [(&str, &str, &str); 17] = [
            ("Space", "播放 / 暫停", "Play / pause"),
            ("← / →", "上一格 / 下一格", "Previous / next frame"),
            ("Shift+← / →", "往前 / 往後 1 秒", "Back / forward 1 second"),
            ("I / O", "播放頭設為開頭 / 結尾", "Set the start / end at the playhead"),
            ("S", "在播放頭分割", "Split at the playhead"),
            ("縮圖上拖曳", "選取一段", "Select a section"),
            ("Delete", "刪除選取的範圍或標註", "Delete the selected range or annotation"),
            ("D", "刪除選取的範圍", "Delete the selected range"),
            ("M", "在播放頭加標記", "Add a marker at the playhead"),
            ("Shift+M", "跳到下一個標記", "Jump to the next marker"),
            ("Ctrl+Shift+M", "跳到上一個標記", "Jump to the previous marker"),
            ("[ / ]", "選取的標註轉 15 度（Shift：1 度）", "Rotate the selected annotation 15° (Shift: 1°)"),
            ("Ctrl+Z", "復原", "Undo"),
            ("Ctrl+Y", "重做", "Redo"),
            ("Ctrl+S", "另存剪輯版 / 儲存修改", "Save the edited version / changes"),
            ("Esc", "取消選取；沒有選取時關閉", "Deselect; close when nothing is selected"),
            ("滾輪", "時間軸上：放大縮小；影片上：逐格移動", "On the timeline: zoom; on the video: step frames"),
        ];
        egui::Grid::new("ed-keys").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
            for (k, zh, en) in rows {
                let key = if screenrecorder_core::i18n::is_en() { k.replace("縮圖上拖曳", "Drag on thumbnails").replace("滾輪", "Wheel").replace("拖曳", "drag") } else { k.to_string() };
                ui.label(RichText::new(key).font(theme::mono(12.0)));
                ui.label(RichText::new(tr!(zh, en)).font(theme::font(12.5)));
                ui.end_row();
            }
        });
    });
}

/// 滾輪：放大 / 縮小（以滑鼠位置為中心）；Shift 或左右滾動 = 平移（時間軸與標註軌都可以）
fn wheel(ed: &mut Editor, ui: &egui::Ui, resp: &egui::Response, rect: Rect) {
    if !resp.hovered() || ed.duration <= 0.0 {
        return;
    }
    let (d, shift, pos) = ui.input(|i| (super::wheel_delta(i), i.modifiers.shift, i.pointer.hover_pos()));
    if d == egui::Vec2::ZERO {
        return;
    }
    let span = ed.view.1 - ed.view.0;
    let horizontal = d.x.abs() > d.y.abs();
    if shift || horizontal {
        let dd = -(if horizontal { d.x } else { d.y }) as f64 / rect.width().max(1.0) as f64;
        ed.set_view(ed.view.0 + dd * span, ed.view.1 + dd * span);
    } else if let Some(pos) = pos {
        let t = (ed.view.0 + ((pos.x - rect.left()) / rect.width()) as f64 * span).clamp(0.0, ed.duration);
        ed.zoom((-(d.y as f64) * 0.003).exp(), t);
    }
}

fn ann_track(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, ann_track_h(ed)), Sense::click_and_drag());
    let (a, b) = ed.view;
    let span = (b - a).max(1e-9);
    let x_of = |t: f64| rect.left() + ((t - a) / span) as f32 * rect.width();
    let dur = ed.duration;
    let time_at = |x: f32| (a + ((x - rect.left()) / rect.width()) as f64 * span).clamp(0.0, dur);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), p.surface2);
    wheel(ed, ui, &resp, rect);
    if ed.anns.is_empty() {
        let hint = if rect.height() < AT_H {
            tr!("標註軌：在右邊「標註」分頁加上文字、箭頭、馬賽克…，會在這裡顯示", "Annotation track: text, arrows, pixelation… added in the “Annotate” tab on the right show up here")
        } else {
            tr!("標註軌：加上的標註會在這裡顯示一條，拖曳可調整出現的時間", "Annotation track: each annotation you add shows up here as a bar; drag it to change when it appears")
        };
        painter.text(rect.center(), Align2::CENTER_CENTER, hint, theme::font(12.0), p.muted);
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                ed.seek(time_at(pos.x));
            }
        }
        return;
    }
    // 刪除的部分一樣壓暗，看得出標註會不會被剪掉
    let bg = Color32::from_black_alpha(46);
    let line = Color32::from_rgba_unmultiplied(229, 72, 77, 64);
    let seg = |t0: f64, t1: f64| Rect::from_min_max(pos2(x_of(t0), rect.top()), pos2(x_of(t1), rect.bottom()));
    if ed.spec.start > 0.0 {
        stripes(&painter, seg(0.0, ed.spec.start), bg, line);
    }
    if ed.spec.end < ed.duration {
        stripes(&painter, seg(ed.spec.end, ed.duration), bg, line);
    }
    for (r0, r1) in normalize_ranges(&ed.spec.removed, ed.duration) {
        stripes(&painter, seg(r0, r1), bg, line);
    }
    let keep = ed.keep();
    let lanes = ed.lane_freeze.clone().unwrap_or_else(|| ed.assign_lanes());
    let n = lanes.values().copied().max().unwrap_or(0) + 1;
    let lane_h = ((AT_H - 4.0) / n as f32).clamp(6.0, 22.0);
    let mut bars: Vec<(u64, Rect, String)> = vec![];
    for an in &ed.anns {
        let lane = lanes.get(&an.id).copied().unwrap_or(0);
        let c = ann_color(an);
        let fg = if !an.kind.is_effect() && (an.color == "#ffffff" || an.color == "#f5b301") { Color32::from_rgb(17, 17, 17) } else { Color32::WHITE };
        let gone = !Editor::in_output(an, &keep);
        let r = Rect::from_min_max(pos2(x_of(an.start), rect.top() + lane as f32 * lane_h + 2.0), pos2(x_of(an.end).max(x_of(an.start) + 3.0), rect.top() + (lane + 1) as f32 * lane_h));
        let fill = if gone { c.gamma_multiply(0.55) } else { c };
        painter.rect_filled(r, CornerRadius::same(4), fill);
        if fg != Color32::WHITE {
            // 白色、黃色的標註在淺色底上加細框
            painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, Color32::from_black_alpha(50)), egui::StrokeKind::Inside);
        }
        if gone {
            stripes(&painter.with_clip_rect(r), r, Color32::TRANSPARENT, Color32::from_white_alpha(40));
        }
        let on = ed.ann_sel == Some(an.id);
        if on {
            painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(2.0, p.accent), egui::StrokeKind::Outside);
            for er in [Rect::from_min_max(r.min, pos2(r.left() + 8.0, r.bottom())), Rect::from_min_max(pos2(r.right() - 8.0, r.top()), r.max)] {
                painter.rect_filled(er, CornerRadius::same(3), Color32::from_white_alpha(90));
            }
        }
        if lane_h >= 16.0 && r.width() > 24.0 {
            // 名稱放不下時結尾變成「…」
            let lp = painter.with_clip_rect(r.intersect(rect));
            let g = super::side::ellipsized(&lp, annotate::label(an), theme::font(11.5), fg, r.width() - 14.0);
            lp.galley(pos2(r.left() + 7.0, r.center().y - g.size().y / 2.0), g, fg);
        }
        let gone_note = if gone { tr!("（在刪除的片段中，不會出現在輸出影片）", " (in a removed section; won't appear in the output video)") } else { "" };
        let tip = trf!(
            "{}：{} – {}{}。拖曳移動，拖曳兩端調整長短",
            "{}: {} – {}{}. Drag to move, drag the ends to change the length",
            annotate::label(an),
            video_clock(an.start),
            video_clock(an.end),
            gone_note
        );
        bars.push((an.id, r, tip));
    }
    let xp = x_of(ed.now());
    painter.vline(xp, rect.y_range(), Stroke::new(2.0, p.text.gamma_multiply(0.7)));

    let (pressed, down, pos, latest) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.pointer.latest_pos()));
    let bar_at = |pos: Pos2| bars.iter().rev().find(|(_, r, _)| r.expand2(vec2(2.0, 0.0)).contains(pos));
    if resp.hovered() && matches!(ed.drag, Drag::None) {
        if let Some((_, r, tip)) = pos.and_then(bar_at) {
            let pos = pos.unwrap_or_default();
            let edge = pos.x - r.left() <= 8.0 || r.right() - pos.x <= 8.0;
            ui.ctx().set_cursor_icon(if edge { CursorIcon::ResizeHorizontal } else { CursorIcon::Grab });
            resp.clone().on_hover_text(tip.clone());
        }
    }
    if pressed && resp.hovered() && ed.duration > 0.0 {
        let Some(pos) = pos else { return };
        ed.stop_preview();
        ed.player.pause();
        match bar_at(pos) {
            None => {
                ed.ann_sel = None;
                ed.seek(time_at(pos.x));
            }
            Some((id, r, _)) => {
                let edge = if pos.x - r.left() <= 8.0 {
                    Edge::Start
                } else if r.right() - pos.x <= 8.0 {
                    Edge::End
                } else {
                    Edge::Body
                };
                let id = *id;
                let orig = ed.anns.iter().find(|x| x.id == id).map(|x| (x.start, x.end)).unwrap_or_default();
                ed.lane_freeze = Some(ed.assign_lanes());
                ed.select_ann(Some(id));
                ed.drag = Drag::AnnBar { id, edge, x0: pos.x, t0: time_at(pos.x), moved: false, orig };
            }
        }
    } else if let Drag::AnnBar { id, edge, x0, t0, moved, orig } = ed.drag {
        let lx = latest.map(|p| p.x).unwrap_or(x0);
        if down {
            let first = !moved && (lx - x0).abs() > 3.0;
            let moved = moved || first;
            // 很短的一條（兩端的拖曳範圍重疊）：依拖的方向決定，往右拉長結尾、往左拉長開頭
            let edge = if first && (((orig.1 - orig.0) / span) as f32 * rect.width()) < NARROW_BAR {
                if lx > x0 {
                    Edge::End
                } else {
                    Edge::Start
                }
            } else {
                edge
            };
            ed.drag = Drag::AnnBar { id, edge, x0, t0, moved, orig };
            if !moved {
                return;
            }
            let d = time_at(lx) - t0;
            let mut seek_to = None;
            if let Some(an) = ed.ann_mut(id) {
                match edge {
                    Edge::Body => {
                        let len = orig.1 - orig.0;
                        an.start = (orig.0 + d).clamp(0.0, (dur - len).max(0.0));
                        an.end = an.start + len;
                        seek_to = Some(an.start);
                    }
                    Edge::Start => {
                        an.start = (orig.0 + d).min(an.end - 0.1).max(0.0);
                        seek_to = Some(an.start);
                    }
                    Edge::End => {
                        an.end = (orig.1 + d).min(dur).max(an.start + 0.1);
                        seek_to = Some(an.end - 0.001);
                    }
                }
            }
            if let Some(t) = seek_to {
                ed.seek(t);
            }
        } else {
            // 只是點一下：跳到它出現的時間（已經在範圍內就不動）
            let now = ed.now();
            if let Some(an) = ed.anns.iter().find(|x| x.id == id) {
                if !moved && (now < an.start || now > an.end) {
                    let s = an.start;
                    ed.seek(s);
                }
            }
            ed.drag = Drag::None;
            ed.lane_freeze = None;
        }
    }
}

/// 放大時的捲軸：拖曳移動顯示的範圍，點捲軸其他地方跳過去
fn scrollbar(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, SCROLL_H), Sense::click_and_drag());
    if !ed.zoomed() || ed.duration <= 0.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(5), p.surface2);
    let d = ed.duration;
    let thumb = Rect::from_min_max(
        pos2(rect.left() + (ed.view.0 / d) as f32 * rect.width(), rect.top() + 1.0),
        pos2((rect.left() + (ed.view.1 / d) as f32 * rect.width()).max(rect.left() + (ed.view.0 / d) as f32 * rect.width() + 12.0), rect.bottom() - 1.0),
    );
    painter.rect_filled(thumb, CornerRadius::same(4), p.text.gamma_multiply(0.4));
    let resp = resp.on_hover_text(tr!("拖曳查看時間軸的其他部分", "Drag to see other parts of the timeline")).on_hover_cursor(CursorIcon::Grab);
    let (pressed, down, pos, latest) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.pointer.latest_pos()));
    let span = ed.view.1 - ed.view.0;
    if pressed && resp.hovered() {
        let Some(pos) = pos else { return };
        if !thumb.contains(pos) {
            let t = ((pos.x - rect.left()) / rect.width()) as f64 * d;
            ed.set_view(t - span / 2.0, t + span / 2.0);
        }
        ed.drag = Drag::Scroll { x0: pos.x, a0: ed.view.0 };
    } else if let Drag::Scroll { x0, a0 } = ed.drag {
        if down {
            if let Some(l) = latest {
                let a = a0 + ((l.x - x0) / rect.width()) as f64 * d;
                ed.set_view(a, a + span);
            }
        } else {
            ed.drag = Drag::None;
        }
    }
}
