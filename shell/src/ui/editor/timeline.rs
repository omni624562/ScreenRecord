//! 時間軸（縮圖、保留 / 刪除的片段、頭尾把手、選取範圍、播放頭）、標註軌、放大時的捲軸。
//!
//! 時間軸：點一下跳到該時間；拖曳把手調整頭尾、拖曳上方橫條移動整段、拖曳播放頭跳到其他時間；在其他地方拖曳 = 選取要刪除的範圍。
//! 滾輪：放大 / 縮小（以滑鼠位置為中心）；Shift 或左右滾動 = 平移。
//! 標註軌：點一下選取（並跳到它出現的時間）；拖曳移動出現時間，拖曳兩端調整開始 / 結束；點空白處跳到該時間。

use super::{ann_color, hash_of, Drag, Edge, Editor, Tab, TlKind};
use crate::ui::theme::{self, Btn};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Layout, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions};
use screenrecorder_core::annotate;
use screenrecorder_core::edit::normalize_ranges;
use screenrecorder_core::format::video_clock;
use screenrecorder_core::player::{strip_frames, Frame};
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TL_H: f32 = 64.0;
/// 標註軌固定高度（不隨標註數量變高，影片才不會在放置標註時跳動）；列多時每列變矮
pub const AT_H: f32 = 70.0;
/// 還沒有標註、也不在「標註」分頁時，標註軌縮成一條，影片可以大一點
pub const AT_H_EMPTY: f32 = 26.0;

/// 標註軌的高度（切到「標註」分頁時就先變高，放標註時影片不會跳動）
pub fn ann_track_h(ed: &Editor) -> f32 {
    if ed.anns.is_empty() && ed.tab != Tab::Ann {
        AT_H_EMPTY
    } else {
        AT_H
    }
}
pub const SCROLL_H: f32 = 10.0;

const YELLOW: Color32 = Color32::from_rgb(0xf5, 0xb3, 0x01);
/// 錄影時加的標記（和錄影外框上的「・標記 N」同色）
const MARK: Color32 = Color32::from_rgb(0xff, 0x8a, 0x1f);
const FLAG_H: f32 = 22.0;
/// 加速的片段
const FAST: Color32 = Color32::from_rgb(0x00, 0x78, 0xff);

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

pub fn show(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    let _ = toast;
    let p = theme::pal(ui);
    let w = ui.available_width();
    let (a, b) = ed.view;
    let span = (b - a).max(1e-9);

    // ── 時間軸 ──
    let (rect, resp) = ui.allocate_exact_size(vec2(w, TL_H), Sense::click_and_drag());
    let x_of = |t: f64| rect.left() + ((t - a) / span) as f32 * rect.width();
    let dur = ed.duration;
    let time_at = |x: f32| (a + ((x - rect.left()) / rect.width()) as f64 * span).clamp(0.0, dur);
    let tile_w = update_strip(ed, ctx, rect);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), p.surface2);
    for (i, t) in ed.strip.tiles.iter().enumerate() {
        if let Some(t) = t {
            let r = Rect::from_min_size(pos2(rect.left() + i as f32 * tile_w, rect.top()), vec2(tile_w.ceil(), TL_H));
            painter.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha(235));
        }
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
        let label = format!("{}×", f.speed);
        let g = painter.layout_no_wrap(label, theme::font_bold(12.0), Color32::WHITE);
        if r.width() > g.size().x + 8.0 {
            let c = pos2(r.center().x, rect.top() + 21.0);
            painter.rect_filled(Rect::from_center_size(c, g.size() + vec2(8.0, 2.0)), CornerRadius::same(4), FAST);
            painter.galley(c - g.size() / 2.0, g, Color32::WHITE);
        }
    }
    if let Some((s0, s1)) = ed.sel {
        let r = Rect::from_min_max(pos2(x_of(s0), rect.top()), pos2(x_of(s1).max(x_of(s0) + 1.0), rect.bottom()));
        painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(214, 140, 18, 89));
        painter.vline(r.left(), r.y_range(), Stroke::new(2.0, p.warn));
        painter.vline(r.right(), r.y_range(), Stroke::new(2.0, p.warn));
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
    // 錄影時加的標記：底部的小旗子，點一下跳過去
    let flags: Vec<(f32, usize, f64)> = ed.markers.iter().enumerate().filter(|(_, &t)| t >= a && t <= b && t <= dur).map(|(i, &t)| (x_of(t), i, t)).collect();
    for &(x, i, _) in &flags {
        let (top, bot) = (rect.bottom() - FLAG_H, rect.bottom() - 1.0);
        painter.vline(x, top..=bot, Stroke::new(3.5, Color32::from_black_alpha(120)));
        painter.vline(x, top..=bot, Stroke::new(2.0, MARK));
        // 旗面寫第幾個標記
        let label = (i + 1).to_string();
        let fw = 9.0 + 6.0 * label.len() as f32;
        let flag = Rect::from_min_size(pos2(x, top), vec2(fw, 13.0));
        painter.rect(flag, CornerRadius { nw: 0, sw: 0, ne: 4, se: 4 }, MARK, Stroke::new(1.0, Color32::from_black_alpha(140)), egui::StrokeKind::Outside);
        painter.text(flag.center() + vec2(0.5, 0.0), Align2::CENTER_CENTER, label, theme::font_bold(10.0), Color32::WHITE);
    }
    // 跟著點擊放大：點擊的時間（小圓點）
    if ed.zoom_on() {
        for c in ed.clicks.iter().filter(|c| c[0] >= a && c[0] <= b) {
            painter.circle(pos2(x_of(c[0]), rect.bottom() - 5.0), 2.5, FAST, Stroke::new(1.0, Color32::WHITE));
        }
    }
    let flag_at = |pos: Pos2| flags.iter().find(|(x, _, _)| pos.y >= rect.bottom() - FLAG_H - 2.0 && pos.x >= x - 4.0 && pos.x <= x + 18.0).map(|&(_, i, t)| (i, t));
    // 播放頭
    let t_now = ed.now();
    let xp = x_of(t_now);
    painter.vline(xp, rect.y_range(), Stroke::new(3.0, Color32::from_black_alpha(110)));
    painter.vline(xp, rect.y_range(), Stroke::new(2.0, Color32::WHITE));
    painter.circle(pos2(xp, rect.top() + 5.0), 6.0, Color32::WHITE, Stroke::new(1.0, Color32::from_black_alpha(128)));

    let hit_kind = |pos: Pos2| -> TlKind {
        if pos.x >= xs - 4.0 && pos.x <= xs + 12.0 {
            TlKind::Start
        } else if pos.x >= xe - 12.0 && pos.x <= xe + 4.0 {
            TlKind::End
        } else if (pos.x - xp).abs() <= 7.0 {
            TlKind::Playhead
        } else if pos.y <= rect.top() + 9.0 && pos.x >= xs && pos.x <= xe {
            TlKind::Range
        } else {
            TlKind::Select
        }
    };
    let (pressed, down, pos, latest) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.pointer.latest_pos()));
    let mut tip: Option<String> = None;
    if resp.hovered() && matches!(ed.drag, Drag::None) {
        if let Some(pos) = pos {
            if let Some((_, _, t)) = restore_btns.iter().find(|(c, _, _)| c.distance(pos) <= 10.0) {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                tip = Some(t.clone());
            } else if let Some((i, t)) = flag_at(pos) {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                tip = Some(trf!(
                    "錄影時加的第 {} 個標記 {}・點一下跳過去・M 下一個、Shift+M 上一個",
                    "Marker {} at {} (added while recording) · click to jump there · M: next, Shift+M: previous",
                    i + 1,
                    video_clock(t)
                ));
            } else {
                let (cursor, t) = match hit_kind(pos) {
                    TlKind::Start => (CursorIcon::ResizeHorizontal, tr!("拖曳調整開頭", "Drag to adjust the start")),
                    TlKind::End => (CursorIcon::ResizeHorizontal, tr!("拖曳調整結尾", "Drag to adjust the end")),
                    TlKind::Playhead => (CursorIcon::Grab, tr!("拖曳跳到其他時間", "Drag to jump to another time")),
                    TlKind::Range => (CursorIcon::Grab, tr!("拖曳整段保留範圍（長度不變）", "Drag to move the whole kept range (the length stays the same)")),
                    TlKind::Select => (CursorIcon::Text, ""),
                };
                ui.ctx().set_cursor_icon(cursor);
                if !t.is_empty() {
                    tip = Some(t.into());
                }
            }
        }
    }
    if pressed && resp.hovered() && ed.duration > 0.0 {
        if let Some(pos) = pos {
            if let Some((_, i, _)) = restore_btns.iter().find(|(c, _, _)| c.distance(pos) <= 10.0) {
                ed.restore_removed(*i);
            } else if let Some((_, t)) = flag_at(pos) {
                ed.stop_preview();
                ed.player.pause();
                ed.seek(t);
            } else {
                ed.stop_preview();
                ed.player.pause();
                ed.drag = Drag::Timeline { kind: hit_kind(pos), x0: pos.x, t0: time_at(pos.x), moved: false, orig: (ed.spec.start, ed.spec.end) };
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
                TlKind::Playhead => ed.seek(t),
                TlKind::Select => {
                    if moved {
                        ed.sel = Some((t0.min(t), t0.max(t)));
                        ed.seek(t);
                    }
                }
            }
        } else {
            // 沒有拖曳的點擊：跳到該時間並取消選取
            if matches!(kind, TlKind::Select | TlKind::Range) && !moved {
                ed.sel = None;
                ed.seek(t);
            } else if kind == TlKind::Select && ed.sel.is_some() {
                // 選好要刪除的範圍：顯示「刪除這段」的按鈕
                ed.tab = Tab::Time;
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

    // ── 說明 ──
    ui.horizontal(|ui| {
        ui.set_height(20.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ed.zoomed() && Btn::new(tr!("顯示全部", "Show all")).ghost().small().show(ui).clicked() {
                ed.set_view(0.0, ed.duration);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let n = ed.markers.len();
                let hint = if n == 0 {
                    tr!(
                        "拖曳黃色把手剪頭尾・在時間軸上拖過一段可刪除・滾輪：時間軸放大縮小、影片上逐張移動",
                        "Drag the yellow handles to trim · drag across the timeline to remove a section · scroll: zoom the timeline, step frames on the video"
                    )
                    .to_string()
                } else if screenrecorder_core::i18n::is_en() {
                    format!(
                        "Orange flags: {n} marker{} added while recording (M: next) · drag the yellow handles to trim · drag across to remove · scroll to zoom",
                        if n == 1 { "" } else { "s" }
                    )
                } else {
                    format!("橘色旗子是錄影時加的 {n} 個標記（M 跳下一個）・拖曳黃色把手剪頭尾・拖過一段可刪除・滾輪放大縮小")
                };
                ui.add(egui::Label::new(theme::muted(ui, &hint).font(theme::font(12.0))).truncate()).on_hover_text(tr!(
                    "點時間軸跳到該時間；拖曳黃色把手剪掉頭尾，拖曳上方的黃色橫條移動整段；在時間軸上拖過一段可選取並刪除。在時間軸上轉滾輪放大 / 縮小（Shift + 滾輪左右移動），在影片上轉滾輪逐張前後移動（Shift 一次一秒）。錄影時按標記快速鍵或外框上的旗子，會在時間軸底下留下橘色旗子：點旗子或按 M / Shift+M 跳到下一個 / 上一個標記。",
                    "Click the timeline to jump to that time. Drag the yellow handles to trim the start and end, or drag the yellow bar on top to move the whole range; drag across the timeline to select a section and remove it. Scroll on the timeline to zoom in / out (Shift + scroll to move left and right); scroll on the video to step frame by frame (1 second at a time with Shift). Pressing the marker shortcut or the flag on the recording frame while recording leaves an orange flag below the timeline: click a flag or press M / Shift+M to jump to the next / previous marker.",
                ));
            });
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
        if lane_h >= 16.0 {
            let lp = painter.with_clip_rect(r.shrink2(vec2(6.0, 0.0)).intersect(rect));
            lp.text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, annotate::label(an), theme::font(11.5), fg);
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
                ed.ann_sel = Some(id);
                ed.tab = Tab::Ann;
                ed.drag = Drag::AnnBar { id, edge, x0: pos.x, t0: time_at(pos.x), moved: false, orig };
            }
        }
    } else if let Drag::AnnBar { id, edge, x0, t0, moved, orig } = ed.drag {
        let lx = latest.map(|p| p.x).unwrap_or(x0);
        if down {
            let moved = moved || (lx - x0).abs() > 3.0;
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
                        an.start = (orig.0 + d).clamp(0.0, an.end - 0.1);
                        seek_to = Some(an.start);
                    }
                    Edge::End => {
                        an.end = (orig.1 + d).clamp(an.start + 0.1, dur);
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
