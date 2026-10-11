//! 講稿小視窗（提詞）：浮在最上層，設成不會被錄進影片或截圖（Windows 10 2004 以後）。
//! 「編輯」寫講稿；「提詞」大字往上捲，讀的位置固定在上方三分之一（有一條提示線）。
//! 開始錄影時自動開始捲、暫停或停止時停下（可在「⋯」關掉）。
//! 講稿存在資料夾的 notes.txt；字級、速度、不透明度、位置存在設定。
//! 跟著操作視窗：操作視窗收到系統匣時一起收起來，再打開時回來。

use super::settings::{NOTES_FONT, NOTES_OPACITY_MIN};
use super::theme::{self, segmented, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Key, Rect, RichText, Sense, Stroke, ViewportBuilder, ViewportId};
use screenrecorder_core::types::RecorderState;
use screenrecorder_core::winui;
use screenrecorder_core::{tr, trf};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Notes {
    text: String,
    /// 改了講稿：這個時間後存檔
    save_at: Option<Instant>,
    prompt: bool,
    /// 提詞捲到哪裡（點）
    scroll: f32,
    playing: bool,
    last_tick: Option<Instant>,
    /// 已經套用到視窗的不透明度（找不到視窗時是 None，下一格再試）
    styled: Option<u8>,
    /// 錄影狀態（判斷開始 / 暫停 / 停止）
    rec: RecorderState,
    /// 位置與大小（點）：關閉時存進設定
    rect: Option<Rect>,
}

fn file() -> PathBuf {
    screenrecorder_core::paths::data_dir().join("notes.txt")
}

/// 開啟或關閉
pub fn toggle(app: &mut UiApp) {
    if app.notes.take().is_some() {
        return;
    }
    let text = std::fs::read_to_string(file()).unwrap_or_default();
    // 沒有講稿時先編輯；有講稿時直接提詞
    let prompt = !text.trim().is_empty();
    app.notes = Some(Notes { text, save_at: None, prompt, scroll: 0.0, playing: false, last_tick: None, styled: None, rec: app.status.recorder.state, rect: None });
}

/// 測試用：notes:play（開始捲動）、notes:edit（編輯）
#[cfg(debug_assertions)]
pub fn dev(app: &mut UiApp, cmd: &str) {
    if app.notes.is_none() {
        toggle(app);
    }
    if let Some(n) = &mut app.notes {
        match cmd {
            "play" => (n.prompt, n.playing) = (true, true),
            "edit" => (n.prompt, n.playing) = (false, false),
            _ => {}
        }
    }
}

fn save_text(n: &mut Notes) {
    n.save_at = None;
    let f = file();
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&f, &n.text) {
        screenrecorder_core::error!("無法儲存講稿：{e}");
    }
}

/// 關閉：存講稿與位置
fn close(app: &mut UiApp, mut n: Notes) {
    if n.save_at.is_some() {
        save_text(&mut n);
    }
    if let Some(r) = n.rect {
        app.s.notes_rect = Some([r.min.x.round() as i32, r.min.y.round() as i32, r.width().round() as i32, r.height().round() as i32]);
        app.save_settings();
    }
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut n) = app.notes.take() else { return };
    // 開始錄影：自動開始捲；暫停、停止：停下
    let state = app.status.recorder.state;
    if state != n.rec {
        if app.s.notes_auto && !n.text.trim().is_empty() {
            match state {
                RecorderState::Recording => {
                    n.prompt = true;
                    n.playing = true;
                }
                RecorderState::Paused | RecorderState::Idle | RecorderState::Stopping => n.playing = false,
                RecorderState::Countdown => n.prompt = true,
            }
        }
        n.rec = state;
    }
    if n.save_at.is_some_and(|t| Instant::now() >= t) {
        save_text(&mut n);
    }
    let [x, y, w, h] = app.s.notes_rect.unwrap_or_else(|| {
        // 預設：主螢幕上方中間（靠近鏡頭，讀稿時眼睛不會飄走）
        let m = app.env.monitors.iter().find(|m| m.primary).or(app.env.monitors.first());
        let ppp = ctx.pixels_per_point();
        let (mx, my, mw) = m.map(|m| (m.x as f32 / ppp, m.y as f32 / ppp, m.width as f32 / ppp)).unwrap_or((0.0, 0.0, 1280.0));
        [(mx + (mw - 460.0) / 2.0) as i32, (my + 40.0) as i32, 460, 280]
    });
    let builder = ViewportBuilder::default()
        .with_title(winui::notes_title())
        .with_always_on_top()
        .with_taskbar(false)
        .with_position(pos2(x as f32, y as f32))
        .with_inner_size(vec2(w as f32, h as f32))
        .with_min_inner_size(vec2(280.0, 160.0));
    let mut closed = false;
    let s = &mut app.s;
    let mut changed = false;
    ctx.show_viewport_immediate(ViewportId::from_hash_of("notes"), builder, |ui, _| {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            closed = true;
        }
        n.rect = ctx.input(|i| match (i.viewport().outer_rect, i.viewport().inner_rect) {
            (Some(o), Some(inner)) => Some(Rect::from_min_size(o.min, inner.size())),
            _ => n.rect,
        });
        // 不被擷取、不透明度：視窗建立後才找得到
        let alpha = (s.notes_opacity.clamp(NOTES_OPACITY_MIN, 100) as f32 * 2.55).round() as u8;
        if n.styled != Some(alpha) && winui::style_notes_window(alpha) {
            n.styled = Some(alpha);
        }
        changed |= body(ui, &mut n, s);
    });
    if changed {
        app.save_settings();
    }
    if closed {
        close(app, n);
    } else {
        app.notes = Some(n);
    }
}

/// 視窗內容；回傳設定是否改變
fn body(ui: &mut egui::Ui, n: &mut Notes, s: &mut super::settings::UiSettings) -> bool {
    let p = theme::pal(ui);
    let mut changed = false;
    let full = ui.ctx().content_rect();
    let bar_h = 40.0;
    let bar = Rect::from_min_size(full.min, vec2(full.width(), bar_h));
    // 工具列用一般的底色；提詞時下面是深色底、淺色大字
    ui.painter().rect_filled(full, 0.0, p.bg);
    if n.prompt {
        ui.painter().rect_filled(Rect::from_min_max(pos2(full.min.x, bar.max.y), full.max), 0.0, Color32::from_rgb(17, 17, 20));
    }
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, p.border));
    // 工具列
    ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(8.0, 4.0))), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut mode = n.prompt;
            if segmented(ui, &mut mode, &[(false, tr!("編輯", "Edit")), (true, tr!("提詞", "Prompt"))], true) {
                n.prompt = mode;
                n.playing = false;
            }
            if n.prompt {
                let (icon, tip) = if n.playing { (Icon::Pause, tr!("暫停（空白鍵）", "Pause (Space)")) } else { (Icon::Play, tr!("開始捲動（空白鍵）", "Start scrolling (Space)")) };
                if Btn::icon_only(icon).small().tooltip(tip).show(ui).clicked() {
                    n.playing = !n.playing;
                }
                if Btn::icon_only(Icon::Undo).ghost().small().tooltip(tr!("回到開頭（Home）", "Back to the top (Home)")).show(ui).clicked() {
                    n.scroll = 0.0;
                }
                if Btn::new("−").ghost().small().tooltip(tr!("慢一點（↓）", "Slower (↓)")).show(ui).clicked() && s.notes_speed > 1 {
                    s.notes_speed -= 1;
                    changed = true;
                }
                ui.label(RichText::new(trf!("速度 {}", "Speed {}", s.notes_speed)).font(theme::font(12.5)));
                if Btn::new("+").ghost().small().tooltip(tr!("快一點（↑）", "Faster (↑)")).show(ui).clicked() && s.notes_speed < 10 {
                    s.notes_speed += 1;
                    changed = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("字級、不透明度、自動捲動", "Text size, opacity, auto-scroll")).show(ui);
                egui::Popup::menu(&more).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.set_min_width(240.0);
                    ui.horizontal(|ui| {
                        ui.label(tr!("提詞字級", "Text size"));
                        if ui.button("A−").clicked() && s.notes_font > NOTES_FONT.0 {
                            s.notes_font = (s.notes_font - 4).max(NOTES_FONT.0);
                            changed = true;
                        }
                        ui.label(s.notes_font.to_string());
                        if ui.button("A+").clicked() && s.notes_font < NOTES_FONT.1 {
                            s.notes_font = (s.notes_font + 4).min(NOTES_FONT.1);
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr!("不透明度", "Opacity"));
                        changed |= ui.add(egui::Slider::new(&mut s.notes_opacity, NOTES_OPACITY_MIN..=100).suffix("%")).changed();
                    });
                    changed |= ui.checkbox(&mut s.notes_auto, tr!("開始錄影時自動捲動，暫停時停下", "Scroll automatically while recording, stop when paused")).changed();
                    ui.separator();
                    ui.label(theme::muted(
                        ui,
                        if cfg!(windows) {
                            tr!("這個視窗不會被錄進影片或截圖（Windows 10 2004 以後）", "This window isn't captured in recordings or screenshots (Windows 10 2004 or later)")
                        } else {
                            tr!("只有 Windows 能設定不被錄進影片", "Only Windows can keep this window out of recordings")
                        },
                    ));
                });
            });
        });
    });
    let area = Rect::from_min_max(pos2(full.min.x, bar.max.y), full.max);
    if n.prompt {
        prompt(ui, n, s, area, &mut changed);
    } else {
        ui.scope_builder(egui::UiBuilder::new().max_rect(area.shrink(8.0)), |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let hint =
                    tr!("在這裡貼上或打講稿，切到「提詞」就會用大字顯示、自動往上捲", "Paste or type your script here. Switch to “Prompt” to show it in large text that scrolls up automatically.");
                let edit = egui::TextEdit::multiline(&mut n.text).hint_text(hint).desired_width(f32::INFINITY).desired_rows(8).font(FontId::proportional(15.0));
                if ui.add_sized(ui.available_size(), edit).changed() {
                    n.save_at = Some(Instant::now() + Duration::from_millis(800));
                }
            });
        });
    }
    changed
}

/// 提詞：大字往上捲；讀的位置在上方三分之一
fn prompt(ui: &mut egui::Ui, n: &mut Notes, s: &mut super::settings::UiSettings, area: Rect, changed: &mut bool) {
    let resp = ui.allocate_rect(area, Sense::click());
    let painter = ui.painter_at(area);
    let font = s.notes_font as f32;
    let pad = 16.0;
    let galley = painter.layout(n.text.clone(), FontId::proportional(font), Color32::from_gray(240), (area.width() - pad * 2.0).max(40.0));
    let guide = area.min.y + area.height() / 3.0;
    let max_scroll = galley.size().y.max(0.0);
    // 鍵盤：空白鍵播放 / 暫停，Home 回到開頭，↑↓ 速度
    if ui.ctx().input(|i| i.key_pressed(Key::Space)) || resp.clicked() {
        n.playing = !n.playing;
    }
    if ui.ctx().input(|i| i.key_pressed(Key::Home)) {
        n.scroll = 0.0;
    }
    if ui.ctx().input(|i| i.key_pressed(Key::ArrowUp)) && s.notes_speed < 10 {
        s.notes_speed += 1;
        *changed = true;
    }
    if ui.ctx().input(|i| i.key_pressed(Key::ArrowDown)) && s.notes_speed > 1 {
        s.notes_speed -= 1;
        *changed = true;
    }
    // 滾輪：手動調整位置
    if resp.hovered() {
        let dy = ui.ctx().input(|i| i.smooth_scroll_delta.y);
        n.scroll -= dy;
    }
    let now = Instant::now();
    if n.playing {
        let dt = n.last_tick.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0).min(0.2);
        // 速度 1～10：每秒捲過約 0.15～1.5 行
        n.scroll += dt * s.notes_speed as f32 * 0.15 * font * 1.3;
        n.last_tick = Some(now);
        if n.scroll >= max_scroll {
            n.playing = false;
        }
        ui.ctx().request_repaint();
    } else {
        n.last_tick = None;
    }
    n.scroll = n.scroll.clamp(0.0, max_scroll);
    if n.text.trim().is_empty() {
        painter.text(
            area.center(),
            Align2::CENTER_CENTER,
            tr!("還沒有講稿：切到「編輯」貼上講稿", "No script yet: switch to “Edit” and paste it"),
            FontId::proportional(15.0),
            Color32::from_gray(170),
        );
        return;
    }
    painter.galley(pos2(area.min.x + pad, guide - n.scroll), galley, Color32::from_gray(240));
    // 讀的位置：左右兩邊的小三角形與淡淡的線
    let c = Color32::from_rgb(250, 204, 21);
    painter.line_segment([pos2(area.min.x + 10.0, guide), pos2(area.max.x - 10.0, guide)], Stroke::new(1.0, c.gamma_multiply(0.35)));
    painter.add(egui::Shape::convex_polygon(vec![pos2(area.min.x, guide - 6.0), pos2(area.min.x + 8.0, guide), pos2(area.min.x, guide + 6.0)], c, Stroke::NONE));
    painter.add(egui::Shape::convex_polygon(vec![pos2(area.max.x, guide - 6.0), pos2(area.max.x - 8.0, guide), pos2(area.max.x, guide + 6.0)], c, Stroke::NONE));
    // 上方讀過的部分變暗
    painter.rect_filled(Rect::from_min_max(area.min, pos2(area.max.x, guide - font * 0.9)), 0.0, Color32::from_black_alpha(110));
}
