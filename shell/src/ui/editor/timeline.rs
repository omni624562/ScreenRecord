//! 時間軸（縮圖、保留 / 刪除的片段、頭尾把手、選取範圍、播放頭）、標註軌、放大時的捲軸。
//!
//! 時間軸：上方的尺規點一下或拖曳移動播放頭；縮圖上點一下只跳到那裡，拖曳是選取要刪除 / 加速的範圍
//! （選好後範圍上方有「刪除」「速度」按鈕；碰到開頭或結尾的刪除就是剪掉頭尾）；按兩下選取分割好的那一段。
//! 拖曳黃色把手調整頭尾、拖曳上方橫條移動整段（照原樣排開時）。標記：點一下跳過去、按兩下改名。
//! 按右鍵：在那裡分割、刪除 / 加速 / 還原那一段、設為開頭 / 結尾、加標記（按在標記、分割點上時可以刪掉它）。
//! 滾輪：放大 / 縮小（以滑鼠位置為中心）；Shift 或左右滾動 = 平移。
//! 標註軌：點一下選取（並跳到它出現的時間）；拖曳移動出現時間，拖曳兩端調整開始 / 結束；點空白處跳到該時間。

use super::{ann_color, hash_of, Drag, Edge, Editor, Tab, TlKind, VAct};
use crate::ui::theme::{self, Btn};
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

/// 時間軸的橫向座標：原片的時間 ↔ 畫面上的 x。沒打開「顯示刪掉的部分」時，刪掉的片段收起來（寬度 0），
/// 時間軸只剩保留的部分，刪掉的地方是一條紅線；打開時照原片的時間平均排開
struct Axis {
    left: f32,
    width: f32,
    /// 顯示範圍（原片的時間）收起後的位置
    da: f64,
    db: f64,
    /// 收起來的片段（原片的時間，由小到大）；照原樣排開時是空的
    cuts: Vec<(f64, f64)>,
    dur: f64,
}

impl Axis {
    fn new(ed: &Editor, rect: Rect) -> Axis {
        let cuts = if ed.collapse_removed() {
            // 剪掉的頭尾也收起來；拖曳頭尾把手時照按下時的頭尾，時間軸才不會跟著滑鼠跑
            let (s, e) = match ed.drag {
                Drag::Timeline { kind: TlKind::Start | TlKind::End, orig, .. } => orig,
                _ => (ed.spec.start, ed.spec.end),
            };
            let mut list = ed.spec.removed.clone();
            list.push((0.0, s));
            list.push((e, ed.duration));
            normalize_ranges(&list, ed.duration)
        } else {
            vec![]
        };
        let mut ax = Axis { left: rect.left(), width: rect.width().max(1.0), da: 0.0, db: 1.0, cuts, dur: ed.duration };
        ax.da = ax.d(ed.view.0);
        ax.db = ax.d(ed.view.1).max(ax.da + 1e-9);
        ax
    }

    /// 原片的時間 → 收起後的時間（扣掉之前刪掉的長度）
    fn d(&self, t: f64) -> f64 {
        let mut out = t;
        for &(r0, r1) in &self.cuts {
            if t >= r1 {
                out -= r1 - r0;
            } else if t > r0 {
                out -= t - r0;
            }
        }
        out
    }

    /// 收起後的時間 → 原片的時間（剛好在刪掉的地方時是它後面保留的開頭）
    fn t(&self, d: f64) -> f64 {
        let mut t = d;
        for &(r0, r1) in &self.cuts {
            if r1 >= self.dur - 1e-9 && t >= r0 {
                // 剪掉的結尾後面沒有保留的部分：停在結尾
                return r0;
            }
            if t >= r0 {
                t += r1 - r0;
            } else {
                break;
            }
        }
        t
    }

    fn x(&self, t: f64) -> f32 {
        self.left + ((self.d(t) - self.da) / (self.db - self.da)) as f32 * self.width
    }

    /// 拖到時間軸外面時停在看得到的頭尾（收起來的頭尾不會一下子跳回來）
    fn time(&self, x: f32) -> f64 {
        let d = self.da + ((x - self.left) / self.width).clamp(0.0, 1.0) as f64 * (self.db - self.da);
        self.t(d).clamp(0.0, self.dur)
    }

    /// 一個像素是幾秒（收起後的時間）
    fn per_px(&self) -> f64 {
        (self.db - self.da) / self.width as f64
    }

    /// t 在收起來的片段裡（剛好是刪掉的開頭也算：那裡和後面保留的開頭疊在一起，刻度寫後面的時間）
    fn in_cut(&self, t: f64) -> bool {
        self.cuts.iter().any(|&(r0, r1)| t >= r0 && t < r1)
    }
}
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
                    *data.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(p));
                    c.request_repaint();
                }
            });
        }
        self.data.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// 縮圖下方的聲音波形（以中線上下對稱，越大聲越高）
fn draw_wave(painter: &egui::Painter, rect: Rect, peaks: &[u8], axis: &Axis, color: Color32) {
    let band = Rect::from_min_max(pos2(rect.left(), rect.bottom() - rect.height() * 0.42), rect.max);
    painter.rect_filled(band, CornerRadius::ZERO, Color32::from_black_alpha(175));
    let per_px = axis.per_px();
    let mid = band.center().y;
    let half = band.height() / 2.0 - 2.0;
    let mut x = band.left();
    while x < band.right() {
        let t = axis.time(x);
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

fn update_strip(ed: &mut Editor, ctx: &egui::Context, rect: Rect, axis: &Axis) -> f32 {
    let ar = if ed.vw > 0.0 && ed.vh > 0.0 { (ed.vw / ed.vh) as f32 } else { 16.0 / 9.0 };
    let tile_w = TL_H * ar;
    let n = (rect.width() / tile_w).ceil().max(1.0) as usize;
    let cuts: Vec<(u64, u64)> = axis.cuts.iter().map(|(a, b)| (a.to_bits(), b.to_bits())).collect();
    let key = hash_of(&(ed.view.0.to_bits(), ed.view.1.to_bits(), n, ed.entry.media.path.as_str(), cuts));
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
            let last = (ed.duration - 0.05).max(0.0);
            // 每一格中間的時間（刪掉的片段收起來時，跳過刪掉的部分）
            let times: Vec<f64> = (0..n).map(|i| axis.time(rect.left() + (i as f32 + 0.5) * tile_w).min(last)).collect();
            let (ffmpeg, path, cancel, inbox, gen, c) = (ed.ffmpeg.clone(), ed.entry.media.path.clone(), st.cancel.clone(), st.inbox.clone(), st.gen, ctx.clone());
            std::thread::spawn(move || {
                strip_frames(&ffmpeg, &path, &times, w, h, &cancel, |k, f| {
                    inbox.lock().unwrap_or_else(|e| e.into_inner()).push((gen, k, f));
                    c.request_repaint();
                });
            });
        } else {
            ctx.request_repaint_after(due - Instant::now());
        }
    }
    let got: Vec<_> = std::mem::take(&mut *st.inbox.lock().unwrap_or_else(|e| e.into_inner()));
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
/// 時間軸按右鍵時記下的：按的時間、按到的標記 / 分割點 / 刪掉的片段
#[derive(Debug, Clone, Copy)]
pub struct TlMenu {
    t: f64,
    flag: Option<usize>,
    split: Option<usize>,
    removed: Option<Cut>,
}

/// 剪掉的部分：中間刪掉的第幾段，或剪掉的開頭、結尾
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cut {
    Removed(usize),
    Head,
    Tail,
}

/// 右鍵選單選了什麼（選單關掉後才做）
enum MenuAct {
    Split(f64),
    /// 刪除 / 加速：那一段；None = 目前選取的（可能好幾段）
    Delete(Option<(f64, f64)>),
    Speed(Option<(f64, f64)>),
    Restore(Cut),
    Start(f64),
    End(f64),
    Marker(f64),
    DeleteMarker(usize),
    RenameMarker(usize),
    RemoveSplit(usize),
    Jump(f64),
}

/// 選取範圍上的按鈕
enum BarAct {
    Delete,
    Speed,
    Clear,
}

enum Hit {
    /// 還原剪掉的部分
    Restore(Cut),
    /// 選取範圍上的按鈕列（按鈕自己處理）
    Bar,
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

    // ── 尺規（點一下或拖曳移動播放頭）與縮圖 ──
    let (all, resp) = ui.allocate_exact_size(vec2(w, RULER_H + TL_H), Sense::click_and_drag());
    let ruler = Rect::from_min_size(all.min, vec2(w, RULER_H));
    let rect = Rect::from_min_max(pos2(all.left(), ruler.bottom()), all.max);
    let axis = Axis::new(ed, rect);
    let x_of = |t: f64| axis.x(t);
    let dur = ed.duration;
    let time_at = |x: f32| axis.time(x);
    let tile_w = update_strip(ed, ctx, rect, &axis);
    let rp = ui.painter_at(ruler);
    // 刻度寫原片的時間；刪掉的片段收起來時跳過刪掉的部分，標籤太擠的不寫
    let step = tick_step(axis.db - axis.da, w, 64.0);
    let mut t = (a / step).ceil() * step;
    let mut last_label = f32::NEG_INFINITY;
    while t <= b + 1e-9 {
        if !axis.in_cut(t) {
            let x = x_of(t);
            rp.vline(x, ruler.bottom() - 5.0..=ruler.bottom(), Stroke::new(1.0, p.muted));
            if x - last_label >= 44.0 {
                rp.text(pos2(x + 3.0, ruler.center().y - 1.0), Align2::LEFT_CENTER, tick_label(t, step), theme::mono(10.5), p.muted);
                last_label = x;
            }
        }
        // 中間的小刻度
        let tm = t + step / 2.0;
        let xm = x_of(tm);
        if xm < ruler.right() && !axis.in_cut(tm) {
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
        draw_wave(&painter, rect, &peaks, &axis, Color32::from_rgb(140, 205, 255));
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
    let mut restore_btns: Vec<(Pos2, Cut, String)> = vec![];
    let collapsed = ed.collapse_removed();
    // 剪掉的頭尾：和中間刪掉的一樣有還原鈕（收起來時在把手旁邊，排開時在斜線中間）
    let trimmed_tip = |head: bool, t0: f64, t1: f64| {
        let what = if head { tr!("開頭", "the start") } else { tr!("結尾", "the end") };
        trf!(
            "剪掉了{} {} – {}（{} 秒）・點一下還原・也可以拖曳黃色把手",
            "Trimmed {} {} – {} ({} s) · click to restore · or drag the yellow handle",
            what,
            video_clock(t0),
            video_clock(t1),
            fmt_secs(t1 - t0)
        )
    };
    if ed.spec.start > 0.05 {
        let (x0, x1) = (x_of(0.0), x_of(ed.spec.start));
        let x = if collapsed { x1 + 24.0 } else { (x0 + x1) / 2.0 };
        if collapsed || x1 - x0 > 24.0 {
            restore_btns.push((pos2(x, rect.top() + 14.0), Cut::Head, trimmed_tip(true, 0.0, ed.spec.start)));
        }
    }
    if ed.spec.end < ed.duration - 0.05 {
        let (x0, x1) = (x_of(ed.spec.end), x_of(ed.duration));
        let x = if collapsed { x0 - 24.0 } else { (x0 + x1) / 2.0 };
        if collapsed || x1 - x0 > 24.0 {
            restore_btns.push((pos2(x, rect.top() + 14.0), Cut::Tail, trimmed_tip(false, ed.spec.end, ed.duration)));
        }
    }
    for (i, &(r0, r1)) in removed.iter().enumerate() {
        let r = full(r0, r1);
        if collapsed {
            // 在剪掉的頭尾裡的：跟頭尾一起收起來了
            if r1 <= ed.spec.start || r0 >= ed.spec.end {
                continue;
            }
            // 收起來了：刪掉的地方一條紅線，上面的還原鈕
            painter.vline(r.left(), r.y_range(), Stroke::new(4.0, Color32::from_black_alpha(110)));
            painter.vline(r.left(), r.y_range(), Stroke::new(2.0, p.rec));
            restore_btns.push((
                pos2(r.left(), rect.top() + 14.0),
                Cut::Removed(i),
                trf!(
                    "這裡刪掉了 {} – {}（{} 秒）・點一下還原・按眼睛「顯示刪掉的部分」可以展開來看",
                    "{} – {} was removed here ({} s) · click to restore · turn on “Show removed parts” (the eye) to see it",
                    video_clock(r0),
                    video_clock(r1),
                    fmt_secs(r1 - r0)
                ),
            ));
        } else {
            stripes(&painter, r, cut_bg, cut_line);
            painter.vline(r.left(), r.y_range(), Stroke::new(1.0, p.rec));
            painter.vline(r.right(), r.y_range(), Stroke::new(1.0, p.rec));
            restore_btns.push((pos2(r.center().x, rect.top() + 14.0), Cut::Removed(i), trf!("還原這段（{} – {}）", "Restore this section ({} – {})", video_clock(r0), video_clock(r1))));
        }
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
    // Ctrl 多選的其他幾段：一樣的底色和外框（沒有把手）
    if ed.sel.is_some() {
        for &(s0, s1) in &ed.sel_more {
            let r = Rect::from_min_max(pos2(x_of(s0), rect.top()), pos2(x_of(s1).max(x_of(s0) + 1.0), rect.bottom()));
            painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(214, 140, 18, 70));
            painter.rect_stroke(r.shrink(1.0), CornerRadius::same(2), Stroke::new(2.0, p.warn), egui::StrokeKind::Inside);
        }
    }
    // 選取的範圍：實線外框、兩端把手；選好後上方一排按鈕（刪除、加速、取消），拖曳中寫選了幾秒
    let mut bar_act: Option<BarAct> = None;
    let mut sel_bar: Option<Rect> = None;
    let sel_x = ed.sel.map(|(s0, s1)| (x_of(s0), x_of(s1).max(x_of(s0) + 1.0)));
    if let (Some((s0, s1)), Some((x0, x1))) = (ed.sel, sel_x) {
        let r = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
        painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(214, 140, 18, 70));
        painter.rect_stroke(r.shrink(1.0), CornerRadius::same(2), Stroke::new(2.0, p.warn), egui::StrokeKind::Inside);
        for x in [x0, x1] {
            let grip = Rect::from_center_size(pos2(x, rect.center().y), vec2(7.0, 24.0));
            painter.rect(grip, CornerRadius::same(3), p.warn, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Outside);
        }
        let label = match ed.sel_summary() {
            Some((n, secs)) if n > 1 => trf!("{n} 段・共 {} 秒", "{n} sections · {} s", fmt_secs(secs)),
            _ => trf!("{} 秒", "{} s", fmt_secs(s1 - s0)),
        };
        let dragging = matches!(ed.drag, Drag::Timeline { kind: TlKind::Select | TlKind::SelA | TlKind::SelB, moved: true, .. });
        let g = painter.layout_no_wrap(label, theme::font_bold(11.5), Color32::WHITE);
        if dragging {
            // 拖曳中只寫選了幾秒
            let c = pos2(((x0 + x1) / 2.0).max(rect.left() + g.size().x / 2.0 + 6.0).min(rect.right() - g.size().x / 2.0 - 6.0), rect.top() + 21.0);
            painter.rect_filled(Rect::from_center_size(c, g.size() + vec2(10.0, 4.0)), CornerRadius::same(4), p.warn);
            painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
        } else {
            // 選好了：上方一排按鈕，直接刪除、加速或取消
            let n = ed.sels().len();
            let del = if n > 1 { Btn::new(trf!("刪除 {n} 段", "Delete {n}")) } else { Btn::new(tr!("刪除", "Delete")) }
                .icon(theme::Icon::Trash)
                .danger()
                .small()
                .height(24.0)
                .tooltip(tr!("刪掉選取的部分（Delete），可以按 Ctrl+Z 復原", "Remove the selection (Delete). Ctrl+Z to undo"));
            let fast = Btn::new(tr!("速度…", "Speed…"))
                .icon(theme::Icon::Fast)
                .small()
                .height(24.0)
                .tooltip(tr!("到「速度」分頁把選取的部分加速、放慢或定格", "Speed up, slow down or freeze the selection in the Speed tab"));
            let close = Btn::icon_only(theme::Icon::Close).ghost().small().height(24.0).tooltip(tr!("取消選取（Esc）", "Clear the selection (Esc)"));
            let gap = 4.0;
            let w = 8.0 + g.size().x + 8.0 + del.width(ui) + gap + fast.width(ui) + gap + close.width(ui) + 4.0;
            let cx = ((x0 + x1) / 2.0).max(rect.left() + w / 2.0 + 2.0).min(rect.right() - w / 2.0 - 2.0);
            let bar = Rect::from_center_size(pos2(cx, rect.top() + 4.0 + 15.0), vec2(w, 30.0));
            painter.rect(bar.expand(1.0), CornerRadius::same(6), Color32::from_black_alpha(60), Stroke::NONE, egui::StrokeKind::Outside);
            painter.rect(bar, CornerRadius::same(6), p.surface, Stroke::new(1.5, p.warn), egui::StrokeKind::Inside);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(8.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.spacing_mut().item_spacing.x = gap;
            child.label(RichText::new(g.text()).font(theme::font_bold(11.5)).color(p.warn));
            child.add_space(8.0 - gap);
            if del.show(&mut child).clicked() {
                bar_act = Some(BarAct::Delete);
            }
            if fast.show(&mut child).clicked() {
                bar_act = Some(BarAct::Speed);
            }
            if close.show(&mut child).clicked() {
                bar_act = Some(BarAct::Clear);
            }
            sel_bar = Some(bar);
        }
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
    // 還原鈕：紅底白色的「復原」箭頭
    for (c, _, _) in &restore_btns {
        painter.circle_filled(*c + vec2(0.0, 1.0), 10.0, Color32::from_black_alpha(80));
        painter.circle_filled(*c, 10.0, p.rec);
        theme::paint_icon(&painter, Rect::from_center_size(*c, vec2(12.0, 12.0)), theme::Icon::Undo, Color32::WHITE);
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

    let (pressed, ctrl, down, pos, latest, dbl) = ui.input(|i| {
        (i.pointer.primary_pressed(), i.modifiers.command, i.pointer.primary_down(), i.pointer.interact_pos(), i.pointer.latest_pos(), i.pointer.button_double_clicked(egui::PointerButton::Primary))
    });
    let hit = |pos: Pos2| -> Hit {
        if sel_bar.is_some_and(|r| r.contains(pos)) {
            return Hit::Bar;
        }
        if ruler.contains(pos) {
            if let Some(&(i, _)) = splits.iter().find(|(_, x)| (pos.x - x).abs() <= 5.0) {
                return Hit::Split(i);
            }
            return Hit::Drag(TlKind::Scrub);
        }
        if let Some((_, cut, _)) = restore_btns.iter().find(|(c, _, _)| c.distance(pos) <= 10.0) {
            return Hit::Restore(*cut);
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
        } else if !collapsed && pos.y <= rect.top() + 9.0 && pos.x >= xs && pos.x <= xe {
            Hit::Drag(TlKind::Range)
        } else if let Some(&(i, _)) = splits.iter().find(|(_, x)| (pos.x - x).abs() <= 4.0) {
            Hit::Split(i)
        } else {
            // 縮圖上拖曳就是選取一段（不用按 Shift）；點一下只跳到那裡，按兩下選取分割好的那一段
            Hit::Drag(TlKind::Select)
        }
    };
    let mut tip: Option<String> = None;
    if resp.hovered() && matches!(ed.drag, Drag::None) {
        if let Some(pos) = pos {
            let (cursor, t): (CursorIcon, String) = match hit(pos) {
                Hit::Bar => (CursorIcon::Default, String::new()),
                Hit::Restore(cut) => (
                    CursorIcon::PointingHand,
                    restore_btns.iter().find(|r| r.1 == cut).map(|r| r.2.clone()).unwrap_or_default(),
                ),
                Hit::Flag(i, t) => (
                    CursorIcon::PointingHand,
                    trf!(
                        "{}（{}）・點一下跳過去・按兩下改名（輸出時當作章節名稱）・按右鍵可以刪除・Shift+M 下一個",
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
                Hit::Split(i) => (CursorIcon::PointingHand, trf!("分割點 {}・按右鍵可以移除", "Split at {} · right-click to remove it", ed.splits.get(i).map(|t| video_clock(*t)).unwrap_or_default())),
                Hit::Drag(k) => match k {
                    TlKind::Start => (CursorIcon::ResizeHorizontal, tr!("拖曳調整開頭", "Drag to adjust the start").into()),
                    TlKind::End => (CursorIcon::ResizeHorizontal, tr!("拖曳調整結尾", "Drag to adjust the end").into()),
                    TlKind::SelA | TlKind::SelB => (CursorIcon::ResizeHorizontal, tr!("拖曳調整選取的範圍", "Drag to adjust the selection").into()),
                    TlKind::Playhead => (CursorIcon::Grab, tr!("拖曳跳到其他時間", "Drag to jump to another time").into()),
                    TlKind::Range => (CursorIcon::Grab, tr!("拖曳整段保留範圍（長度不變）", "Drag to move the whole kept range (the length stays the same)").into()),
                    TlKind::Select => (
                        CursorIcon::Text,
                        tr!(
                            "點一下跳到這裡・拖曳選取一段（刪除、改速度）・按兩下選取分割好的那一段・按住 Ctrl 可以一次選好幾段・按右鍵：分割、刪除、速度…",
                            "Click to jump here · drag to select a section (delete, change speed) · double-click to select a split section · hold Ctrl to select several · right-click: split, delete, speed…"
                        )
                        .into(),
                    ),
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
    // 右鍵（放開的那一下，選單也是這時打開）：記下按的地方，選單照那裡有什麼列出能做的事
    if resp.secondary_clicked() {
        if let Some(pos) = pos {
            let t = time_at(pos.x);
            let (flag, split, at_seam) = match hit(pos) {
                Hit::Flag(i, _) => (Some(i), None, None),
                Hit::Split(i) => (None, Some(i), None),
                Hit::Restore(cut) => (None, None, Some(cut)),
                _ => (None, None, None),
            };
            // 刪掉的片段收起來時按在還原鈕上；照原樣排開時按在斜線的範圍裡
            let removed = at_seam.or_else(|| {
                if collapsed {
                    None
                } else if t < ed.spec.start {
                    Some(Cut::Head)
                } else if t > ed.spec.end {
                    Some(Cut::Tail)
                } else {
                    removed.iter().position(|&(r0, r1)| t >= r0 && t <= r1).map(Cut::Removed)
                }
            });
            ed.tl_menu = Some(TlMenu { t, flag, split, removed });
        }
    }
    let mut menu_act: Option<MenuAct> = None;
    let menu = ed.tl_menu.map(|m| {
        // 要刪除 / 加速的：按在選取的範圍裡就是選取的那幾段，不然是分割點之間的那一段
        let sels = ed.sels();
        let target = if sels.iter().any(|&(s0, s1)| m.t >= s0 && m.t <= s1) { Some((None, sels)) } else { ed.segment_at(m.t).map(|r| (Some(r), vec![r])) };
        (m, target, m.flag.map(|i| ed.marker_name(i)))
    });
    let shown = egui::Popup::context_menu(&resp).show(|ui| {
        let Some((m, target, marker_name)) = menu else {
            ui.close();
            return;
        };
        {
            ui.set_min_width(220.0);
            ui.label(RichText::new(trf!("在 {}", "At {}", video_clock(m.t))).font(theme::font(12.0)).color(theme::pal(ui).muted));
            let mut item = |ui: &mut egui::Ui, label: String, act: Option<MenuAct>, why: &str| {
                let enabled = act.is_some();
                let r = ui.add_enabled(enabled, egui::Button::new(label));
                let r = if enabled || why.is_empty() { r } else { r.on_disabled_hover_text(why) };
                if r.clicked() {
                    menu_act = act;
                }
            };
            if let Some(cut) = m.removed {
                let label = match cut {
                    Cut::Head => tr!("還原剪掉的開頭", "Restore the trimmed start"),
                    Cut::Tail => tr!("還原剪掉的結尾", "Restore the trimmed end"),
                    Cut::Removed(_) => tr!("還原這段", "Restore this section"),
                };
                item(ui, label.into(), Some(MenuAct::Restore(cut)), "");
            } else {
                let why = tr!("先拖曳選取一段，或在兩邊分割", "Drag to select a section first, or split on both sides");
                item(ui, tr!("在這裡分割", "Split here").into(), Some(MenuAct::Split(m.t)), "");
                let (del, speed) = match &target {
                    Some((_, list)) if list.len() > 1 => {
                        (trf!("刪除選取的 {} 段", "Delete the {} selected sections", list.len()), trf!("調整選取的 {} 段的速度…", "Change the speed of the {} selected sections…", list.len()))
                    }
                    Some((_, list)) => {
                        let r = list[0];
                        (trf!("刪除這段（{} – {}）", "Delete this section ({} – {})", video_clock(r.0), video_clock(r.1)), tr!("調整這段的速度…", "Change this section's speed…").to_string())
                    }
                    None => (tr!("刪除這段", "Delete this section").to_string(), tr!("調整這段的速度…", "Change this section's speed…").to_string()),
                };
                item(ui, del, target.as_ref().map(|(r, _)| MenuAct::Delete(*r)), why);
                item(ui, speed, target.as_ref().map(|(r, _)| MenuAct::Speed(*r)), why);
            }
            ui.separator();
            item(ui, tr!("設為開頭（剪掉前面）", "Set as start (trim before)").into(), Some(MenuAct::Start(m.t)), "");
            item(ui, tr!("設為結尾（剪掉後面）", "Set as end (trim after)").into(), Some(MenuAct::End(m.t)), "");
            item(ui, tr!("在這裡加標記", "Add a marker here").into(), Some(MenuAct::Marker(m.t)), "");
            item(ui, tr!("跳到這裡", "Jump here").into(), Some(MenuAct::Jump(m.t)), "");
            if let (Some(i), Some(name)) = (m.flag, &marker_name) {
                ui.separator();
                item(ui, trf!("改名「{name}」…", "Rename “{name}”…"), Some(MenuAct::RenameMarker(i)), "");
                item(ui, trf!("刪除「{name}」", "Delete “{name}”"), Some(MenuAct::DeleteMarker(i)), "");
            }
            if let Some(i) = m.split {
                ui.separator();
                item(ui, tr!("移除這個分割點", "Remove this split").into(), Some(MenuAct::RemoveSplit(i)), "");
            }
        }
    });
    if shown.is_none() {
        ed.tl_menu = None;
    }
    if let Some(act) = menu_act {
        ed.tl_menu = None;
        ed.stop_preview();
        ed.player.pause();
        match act {
            MenuAct::Split(t) => *toast = Some(ed.split_at(t)),
            MenuAct::Delete(r) => {
                if let Some(r) = r {
                    ed.set_sel(Some(r));
                }
                ed.delete_selection(toast);
            }
            MenuAct::Speed(r) => {
                if let Some(r) = r {
                    ed.set_sel(Some(r));
                }
                ed.tab = Tab::Speed;
            }
            MenuAct::Restore(cut) => restore(ed, cut),
            MenuAct::Start(t) => {
                ed.set_start(t);
                ed.seek(ed.spec.start);
            }
            MenuAct::End(t) => {
                ed.set_end(t);
                ed.seek(ed.spec.end);
            }
            MenuAct::Marker(t) => *toast = Some(ed.add_marker_at(t)),
            MenuAct::DeleteMarker(i) => {
                let name = ed.marker_name(i);
                ed.markers.remove(i);
                *toast = Some((trf!("已刪除{}，可按 Ctrl+Z 復原", "Deleted {}. Press Ctrl+Z to undo", name), false));
            }
            MenuAct::RenameMarker(i) => ed.vpending = Some(VAct::RenameMarker(i)),
            MenuAct::RemoveSplit(i) => {
                if i < ed.splits.len() {
                    ed.splits.remove(i);
                }
            }
            MenuAct::Jump(t) => ed.seek(t),
        }
    }
    if let Some(act) = bar_act {
        ed.stop_preview();
        ed.player.pause();
        match act {
            BarAct::Delete => ed.delete_selection(toast),
            BarAct::Speed => ed.tab = Tab::Speed,
            BarAct::Clear => ed.set_sel(None),
        }
    }
    // 按兩下：旗子改名；縮圖上選取分割好的那一段（Ctrl 不算）
    let mut dbl_done = false;
    if dbl && resp.hovered() && !ctrl {
        match pos.map(hit) {
            Some(Hit::Flag(i, _)) => {
                ed.drag = Drag::None;
                ed.vpending = Some(VAct::RenameMarker(i));
            }
            Some(Hit::Drag(TlKind::Select)) => {
                ed.drag = Drag::None;
                ed.sel_add = false;
                dbl_done = true;
                let t = pos.map(|p| time_at(p.x)).unwrap_or_default();
                if let Some(seg) = ed.segment_at(t) {
                    ed.set_sel(Some(seg));
                    // 在「速度」分頁時留在那裡（選好一段就能設定速度）
                    if ed.tab != Tab::Speed {
                        ed.tab = Tab::Time;
                    }
                } else if ed.splits.is_empty() {
                    *toast = Some((
                        tr!("拖曳就能選取一段；先分割（S）的話，按兩下可以選取分割好的那一段", "Drag to select a section. After splitting (S), double-click a section to select it").into(),
                        false,
                    ));
                }
            }
            _ => {}
        }
    }
    if pressed && resp.hovered() && ed.duration > 0.0 && !dbl_done {
        if let Some(pos) = pos {
            match hit(pos) {
                Hit::Restore(cut) => restore(ed, cut),
                Hit::Bar => {}
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
                    // 貼齊用：按下時播放頭的位置（拖曳中播放頭會跟著動）
                    ed.press_playhead = ed.now();
                    // 按著 Ctrl：加選（拖曳是再加一段、只點一下是加選 / 取消那一段）；沒按 Ctrl 重新選
                    ed.sel_add = ctrl && kind == TlKind::Select;
                    if kind == TlKind::Select && !ctrl {
                        ed.sel_more.clear();
                    }
                    if kind == TlKind::Scrub {
                        ed.seek(time_at(pos.x));
                    }
                }
            }
        }
    } else if let Drag::Timeline { kind, x0, t0, moved, orig } = ed.drag {
        let lx = latest.map(|p| p.x).unwrap_or(x0);
        // 拖曳選取、調整頭尾或選取範圍時，靠近開頭結尾、按下時的播放頭、標記、分割點、刪掉的接縫就貼齊（10 像素內）
        let targets = snap_targets(ed, kind);
        let snap = |v: f64| -> (f64, Option<f64>) {
            let best = targets.iter().copied().map(|s| (s, (x_of(s) - x_of(v)).abs())).filter(|&(_, d)| d <= 10.0).min_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((s, _)) => (s, Some(s)),
                None => (v, None),
            }
        };
        let (t, snapped) = if targets.is_empty() { (time_at(lx), None) } else { snap(time_at(lx)) };
        ed.snap_at = snapped;
        if down {
            // 選取要拖遠一點才算，點一下時手抖不會變成選取
            let slop = if kind == TlKind::Select { 8.0 } else { 4.0 };
            let moved = moved || (lx - x0).abs() > slop;
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
                    // 調整最後選的那一段（其他多選的不動）
                    let other = if kind == TlKind::SelA { orig.1 } else { orig.0 };
                    ed.sel = Some((t.min(other), t.max(other)));
                    ed.seek(t);
                }
                TlKind::Select => {
                    if moved {
                        // Ctrl＋拖曳：原本選的留著，再加一段
                        if std::mem::take(&mut ed.sel_add) {
                            match ed.sel.take() {
                                Some(s) => ed.sel_more.push(s),
                                // 沒有選取時留下的舊多選不算
                                None => ed.sel_more.clear(),
                            }
                        }
                        ed.ann_sel = None;
                        // 按下的地方也貼齊（靠近開頭、結尾時按「刪除」就是剪掉頭尾）
                        let a = snap(t0).0;
                        ed.sel = Some((a.min(t), a.max(t)));
                        ed.seek(t);
                    }
                }
            }
        } else {
            // 沒有拖曳的點擊：跳到該時間並取消選取；按兩下選取分割好的那一段
            if kind == TlKind::Select && !moved && std::mem::take(&mut ed.sel_add) {
                // Ctrl＋點一段：加選，已經選了就取消（沒有分割點時點到的是選取的範圍）
                let hit_sel = ed.sels().into_iter().find(|&(s0, s1)| t >= s0 && t <= s1);
                if let Some(r) = hit_sel.or_else(|| ed.segment_at(t)) {
                    ed.toggle_sel(r);
                }
                if ed.sel.is_some() && ed.tab != Tab::Speed {
                    ed.tab = Tab::Time;
                }
            } else if matches!(kind, TlKind::Select | TlKind::Scrub | TlKind::Range) && !moved {
                ed.seek(t);
                ed.set_sel(None);
            } else if matches!(kind, TlKind::Select | TlKind::SelA | TlKind::SelB) && ed.sel.is_some() {
                // 選好要刪除的範圍：顯示「刪除這段」的按鈕（在「速度」分頁時顯示速度的按鈕）
                if ed.tab != Tab::Speed {
                    ed.tab = Tab::Time;
                }
            }
            ed.drag = Drag::None;
            ed.snap_at = None;
        }
    }
    // 貼齊時畫一條線，看得出貼到哪裡
    if let Some(sx) = ed.snap_at.filter(|_| matches!(ed.drag, Drag::Timeline { .. })).map(x_of) {
        painter.vline(sx, rect.y_range(), Stroke::new(2.0, p.accent));
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

/// 拖曳時可以貼齊的時間：選取時有開頭、結尾，調整頭尾時沒有（拖的就是它）；
/// 都有按下時的播放頭、標記、分割點、刪掉的片段的兩端
fn snap_targets(ed: &Editor, kind: TlKind) -> Vec<f64> {
    let mut out = match kind {
        TlKind::Select | TlKind::SelA | TlKind::SelB => vec![ed.spec.start, ed.spec.end],
        TlKind::Start | TlKind::End => vec![],
        _ => return vec![],
    };
    out.push(ed.press_playhead);
    out.extend(ed.markers.iter().map(|m| m.t));
    out.extend(ed.splits.iter().copied());
    for (a, b) in normalize_ranges(&ed.spec.removed, ed.duration) {
        out.extend([a, b]);
    }
    out
}

/// 還原剪掉的部分：中間刪掉的那一段，或把開頭 / 結尾放回來
fn restore(ed: &mut Editor, cut: Cut) {
    match cut {
        Cut::Removed(i) => ed.restore_removed(i),
        Cut::Head => ed.spec.start = 0.0,
        Cut::Tail => ed.spec.end = ed.duration,
    }
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
        let rows: [(&str, &str, &str); 21] = [
            ("Space", "播放 / 暫停", "Play / pause"),
            ("← / →", "上一格 / 下一格", "Previous / next frame"),
            ("Shift+← / →", "往前 / 往後 1 秒", "Back / forward 1 second"),
            ("I / O", "播放頭設為開頭 / 結尾", "Set the start / end at the playhead"),
            ("S", "在播放頭分割", "Split at the playhead"),
            ("J / K / L", "倒轉 / 停 / 播放（再按一次更快：2、4、8 倍）", "Rewind / stop / play (press again to go faster: 2×, 4×, 8×)"),
            ("縮圖上拖曳", "選取一段（上方有刪除、速度按鈕）", "Select a section (Delete and Speed buttons appear above it)"),
            ("按兩下", "選取分割好的那一段", "Select a split section"),
            ("Ctrl+點 / 拖曳", "再加選一段（可以一次刪除、改速度好幾段）", "Add another section (delete or change the speed of several at once)"),
            ("右鍵", "時間軸上：分割、刪除、速度、還原…", "On the timeline: split, delete, speed, restore…"),
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
                let key = if screenrecorder_core::i18n::is_en() {
                    k.replace("縮圖上拖曳", "Drag on thumbnails")
                        .replace("按兩下", "Double-click")
                        .replace("滾輪", "Wheel")
                        .replace("右鍵", "Right-click")
                        .replace("點", "click")
                        .replace("拖曳", "drag")
                } else {
                    k.to_string()
                };
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
        let t = Axis::new(ed, rect).time(pos.x);
        ed.zoom((-(d.y as f64) * 0.003).exp(), t);
    }
}

fn ann_track(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, ann_track_h(ed)), Sense::click_and_drag());
    // 和時間軸用同一個座標（刪掉的片段收起來時也一樣收起來）
    let axis = Axis::new(ed, rect);
    let span = axis.db - axis.da;
    let x_of = |t: f64| axis.x(t);
    let dur = ed.duration;
    let time_at = |x: f32| axis.time(x);
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
        if axis.cuts.is_empty() {
            stripes(&painter, seg(r0, r1), bg, line);
        } else {
            painter.vline(x_of(r0), rect.y_range(), Stroke::new(1.5, p.rec.gamma_multiply(0.7)));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_parts_fold_away() {
        // 0–10 秒，刪掉 2–4、6–7：顯示的長度 7 秒
        let mut ax = Axis { left: 0.0, width: 700.0, da: 0.0, db: 1.0, cuts: vec![(2.0, 4.0), (6.0, 7.0)], dur: 10.0 };
        ax.da = ax.d(0.0);
        ax.db = ax.d(10.0);
        assert_eq!(ax.db, 7.0);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        assert!(near(ax.x(1.0) as f64, 100.0));
        // 刪掉的片段寬度 0：開頭和結尾在同一個位置
        assert!(near(ax.x(2.0) as f64, ax.x(4.0) as f64));
        assert!(near(ax.x(5.0) as f64, 300.0));
        // 反過來：點在接縫上是後面保留的開頭
        assert!(near(ax.time(200.0), 4.0));
        assert!(near(ax.time(500.0), 8.0));
        assert!(ax.in_cut(2.0) && ax.in_cut(3.0) && !ax.in_cut(4.0));
    }

    #[test]
    fn trimmed_ends_fold_away() {
        // 0–10 秒，剪掉開頭 0–2、結尾 8–10：只顯示 2–8
        let mut ax = Axis { left: 0.0, width: 600.0, da: 0.0, db: 1.0, cuts: vec![(0.0, 2.0), (8.0, 10.0)], dur: 10.0 };
        ax.da = ax.d(0.0);
        ax.db = ax.d(10.0);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        assert!(near(ax.x(2.0) as f64, 0.0) && near(ax.x(8.0) as f64, 600.0));
        // 拖到時間軸外面停在看得到的頭尾，不會跳回剪掉的地方
        assert!(near(ax.time(-50.0), 2.0));
        assert!(near(ax.time(600.0), 8.0));
        assert!(near(ax.time(700.0), 8.0));
        assert!(near(ax.time(300.0), 5.0));
    }
}
