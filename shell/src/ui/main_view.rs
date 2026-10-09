//! 主畫面：
//! - 左：擷取範圍（單一螢幕 / 所有螢幕 / 自訂範圍）與預覽、下方一列錄影設定
//! - 右：錄影狀態、計時、控制按鈕、轉檔工作、事件紀錄、系統狀態
//! - 下：最近錄影（橫向分頁）

use super::dialogs::{base_name, date_labels, file_name, is_cut_name, is_default_name, Ask};
use super::settings::{SourceType, FPS_CHOICES, MAX_PRESETS};
use super::theme::{self, chip, segmented, switch, Btn, Icon, Tone};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::format::{clock, format_bytes, human_duration, output_size, speed_label, video_clock};
use screenrecorder_core::types::{
    DownloadPhase, EncoderPreference, ExportFormat, ExportKind, ExportState, LibraryEntry, LogLevel, MethodPreference, RecorderState, Rect as DRect, HOTKEY_PAUSE_LABEL, HOTKEY_RECORD_LABEL,
    HOTKEY_SHOT_LABEL, HOTKEY_SNIP_LABEL,
};
use std::time::Instant;

/// 主畫面自己的狀態（拖曳範圍、輸入中的文字）
#[derive(Default)]
pub struct MainState {
    drag: Option<RegionDrag>,
    max_custom_open: bool,
    max_text: String,
    region_text: [String; 4],
    region_focus: Option<usize>,
    dir_text: Option<String>,
    dir_changed_at: Option<Instant>,
    /// 截圖中（按鈕停用，避免連按）
    pub shooting: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    New,
    Move,
    Resize { n: bool, s: bool, e: bool, w: bool },
}

struct RegionDrag {
    mode: DragMode,
    from: (i32, i32),
    orig: DRect,
}

pub fn show(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let full = ui.max_rect();
    ui.painter().rect_filled(full, CornerRadius::ZERO, p.bg);
    let inner = full.shrink(16.0);
    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
        banners(app, ui);
        let rest = ui.available_rect_before_wrap();
        let recent_h = 118.0;
        let gap = 14.0;
        let top = Rect::from_min_max(rest.min, pos2(rest.max.x, rest.max.y - recent_h - gap));
        let bottom = Rect::from_min_max(pos2(rest.min.x, rest.max.y - recent_h), rest.max);
        let rec_w = 360.0;
        let left = Rect::from_min_max(top.min, pos2(top.max.x - rec_w - gap, top.max.y));
        let right = Rect::from_min_max(pos2(top.max.x - rec_w, top.min.y), top.max);
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| capture_panel(app, ui));
        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| rec_panel(app, ui));
        ui.scope_builder(UiBuilder::new().max_rect(bottom), |ui| recent_strip(app, ui));
    });
    // 儲存位置停止輸入 0.6 秒後重新讀取最近錄影
    if app.main.dir_changed_at.is_some_and(|t| t.elapsed().as_millis() > 600) {
        app.main.dir_changed_at = None;
        app.recent_page = 1;
        app.load_recent();
    }
}

// ───────────── 上方提示（找不到 FFmpeg、環境警告） ─────────────

fn banners(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    if app.env_ready && !app.env.ffmpeg.found {
        let d = app.status.download.clone();
        let busy = matches!(d.phase, DownloadPhase::Downloading | DownloadPhase::Verifying | DownloadPhase::Extracting);
        egui::Frame::new().fill(p.rec_soft).stroke(Stroke::new(1.0, p.rec.gamma_multiply(0.4))).corner_radius(CornerRadius::same(theme::RADIUS)).inner_margin(12.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 360.0);
                    ui.label(RichText::new("找不到 ffmpeg.exe").font(theme::font_bold(14.0)).color(p.rec));
                    let text = match d.phase {
                        DownloadPhase::Downloading => {
                            let pct = d.total.map(|t| d.received as f64 / t.max(1) as f64 * 100.0);
                            let mut s = format!("下載中 {}", format_bytes(d.received));
                            if let (Some(t), Some(pc)) = (d.total, pct) {
                                s += &format!(" / {}（{}%）", format_bytes(t), pc.floor());
                            }
                            if let Some(sp) = d.speed {
                                s += &format!("・{}/秒", format_bytes(sp as u64));
                                if let Some(t) = d.total {
                                    s += &format!("・剩約 {}", human_duration((t.saturating_sub(d.received)) as f64 / sp.max(1.0)));
                                }
                            }
                            s
                        }
                        DownloadPhase::Verifying => "比對 SHA-256 校驗碼…".into(),
                        DownloadPhase::Extracting => "解壓縮並確認 ffmpeg.exe 可以執行…".into(),
                        DownloadPhase::Error | DownloadPhase::Canceled => d.message.clone().unwrap_or_else(|| "下載失敗".into()),
                        _ => format!(
                            "可以自動下載（gyan.dev 的 FFmpeg essentials，約 110 MB；下載後比對程式內建的 SHA-256，確認檔案完整且未被替換），或自行下載並把 bin\\ffmpeg.exe 放到 {}",
                            app.env.app_dir
                        ),
                    };
                    ui.add(egui::Label::new(RichText::new(text).color(p.text)).wrap());
                    if busy {
                        let frac = if d.phase == DownloadPhase::Downloading { d.total.map(|t| d.received as f32 / t.max(1) as f32).unwrap_or(0.0) } else { 1.0 };
                        theme::progress(ui, frac, p.rec, 6.0);
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::new("重新偵測").small().show(ui).clicked() {
                        app.refresh_env(|app| {
                            let (msg, err) = if app.env.ffmpeg.found {
                                (format!("已找到 FFmpeg {}", app.env.ffmpeg.version.clone().unwrap_or_default()), false)
                            } else {
                                ("仍然找不到 ffmpeg.exe".into(), true)
                            };
                            app.toast(msg, err);
                        });
                    }
                    if Btn::new("手動下載").small().show(ui).clicked() {
                        let _ = actions::open_url("https://www.gyan.dev/ffmpeg/builds/");
                    }
                    if busy {
                        if Btn::new("取消").small().show(ui).clicked() {
                            app.core.downloader.cancel();
                        }
                    } else {
                        let label = if matches!(d.phase, DownloadPhase::Error | DownloadPhase::Canceled) { "重新下載" } else { "自動下載" };
                        if Btn::new(label).primary().small().show(ui).clicked() {
                            if let Err(e) = actions::ffmpeg_download(&app.core) {
                                app.toast(e.message().to_string(), true);
                            }
                        }
                    }
                });
            });
        });
        ui.add_space(10.0);
    }
    let warns: Vec<String> = [app.env.ffmpeg.found.then(|| app.env.ffmpeg.error.clone()).flatten(), app.env.monitor_error.clone()].into_iter().flatten().collect();
    if !warns.is_empty() {
        egui::Frame::new().fill(p.warn_soft).corner_radius(CornerRadius::same(theme::RADIUS)).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(warns.join("　")).color(p.warn));
        });
        ui.add_space(10.0);
    }
}

// ───────────── 擷取範圍 ─────────────

/// 預覽區顯示的範圍：單一螢幕模式只顯示選到的那一台，其餘顯示整個桌面（自訂範圍要能跨螢幕框選）
fn view_rect(app: &UiApp) -> DRect {
    match app.s.source_type {
        SourceType::Monitor => app.s.selected_monitor(&app.env).map(|m| DRect { x: m.x, y: m.y, width: m.width, height: m.height }).unwrap_or(app.env.desktop),
        _ => app.env.desktop,
    }
}

/// 左右兩欄底部那一列（設定列、系統狀態）的高度與上方細線的間距：兩邊相同，細線才會對齊
const BOTTOM_BAR_H: f32 = 40.0;
const BOTTOM_LINE_GAP: f32 = 5.0;

fn capture_panel(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    theme::card(ui).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let locked = app.locked();
        // 範圍分頁、細節、即時預覽、重新整理
        ui.horizontal(|ui| {
            let mut st = app.s.source_type;
            if segmented(ui, &mut st, &[(SourceType::Monitor, "單一螢幕"), (SourceType::All, "所有螢幕"), (SourceType::Region, "自訂範圍")], !locked) {
                app.s.source_type = st;
                app.save_settings();
            }
            source_detail(app, ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Refresh).ghost().small().tooltip("重新擷取預覽並更新螢幕清單").show(ui).clicked() {
                    app.preview.retry_live();
                    app.refresh_env(|_| {});
                }
                let mut live = app.s.live_preview;
                if switch(ui, &mut live, "即時預覽", true).on_hover_text("即時顯示目前畫面（每秒 5 張，錄影中 2 張）").changed() {
                    app.s.live_preview = live;
                    app.preview.retry_live();
                    app.save_settings();
                }
            });
        });
        ui.add_space(10.0);
        let bar_h = BOTTOM_BAR_H;
        let desk_area = Rect::from_min_max(ui.cursor().min, pos2(ui.max_rect().max.x, ui.max_rect().max.y - bar_h - 10.0));
        desk(app, ui, desk_area);
        let bar = Rect::from_min_max(pos2(desk_area.min.x, ui.max_rect().max.y - bar_h), ui.max_rect().max);
        ui.scope_builder(UiBuilder::new().max_rect(bar).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.painter().hline(bar.x_range(), bar.min.y - BOTTOM_LINE_GAP, Stroke::new(1.0, p.border));
            settings_bar(app, ui);
        });
    });
}

/// 範圍分頁旁的細節：螢幕按鈕 / 說明 / 座標輸入
fn source_detail(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let locked = app.locked();
    match app.s.source_type {
        SourceType::Monitor => {
            if app.env.monitors.is_empty() {
                ui.label(theme::muted(ui, "找不到螢幕資訊，請改用「自訂範圍」"));
            }
            for m in app.env.monitors.clone() {
                let on = app.s.monitor_id.as_deref() == Some(&m.id);
                let label = format!("螢幕 {}  {}×{}{}", m.display_number, m.width, m.height, if m.primary { "・主" } else { "" });
                if Btn::new(label).small().selected(on).enabled(!locked).tooltip(&m.adapter_name).show(ui).clicked() && !on {
                    app.s.monitor_id = Some(m.id.clone());
                    app.s.monitor_region = None;
                    app.save_settings();
                }
            }
            // 只錄螢幕的一部分：在預覽上拖曳框選
            if app.s.region_in_monitor(&app.env).is_some() {
                if Btn::new("整個螢幕").icon(Icon::Close).small().enabled(!locked).tooltip("取消框選的範圍，錄整個螢幕").show(ui).clicked() {
                    app.s.monitor_region = None;
                    app.save_settings();
                }
            } else if !app.env.monitors.is_empty() {
                ui.label(theme::muted(ui, "可在預覽上拖曳框選範圍")).on_hover_text("只錄這個螢幕的一部分：在預覽圖上拖曳框選；拖曳紅框可移動，拉邊或角可調整大小");
            }
        }
        SourceType::All => {
            let n = app.env.monitors.len();
            let adapters: std::collections::HashSet<u32> = app.env.monitors.iter().map(|m| m.adapter).collect();
            let multi = adapters.len() > 1;
            let text = if n <= 1 {
                "目前只有 1 個螢幕，與「單一螢幕」相同".to_string()
            } else {
                format!("{n} 個螢幕拼成 {}×{}{}", app.env.desktop.width, app.env.desktop.height, if multi { "（不同顯示卡，將用 gdigrab）" } else { "" })
            };
            ui.label(RichText::new(text).color(if multi { p.warn } else { p.muted }).font(theme::font(12.5)));
        }
        SourceType::Region => {
            let r = app.s.region;
            let vals = [r.x, r.y, r.width, r.height];
            for (i, label) in ["X", "Y", "寬", "高"].iter().enumerate() {
                ui.label(theme::muted(ui, *label));
                if app.main.region_focus != Some(i) {
                    app.main.region_text[i] = vals[i].to_string();
                }
                let resp = ui.add_enabled(!locked, egui::TextEdit::singleline(&mut app.main.region_text[i]).desired_width(52.0));
                if resp.has_focus() {
                    app.main.region_focus = Some(i);
                } else if app.main.region_focus == Some(i) {
                    app.main.region_focus = None;
                }
                if resp.changed() {
                    if let Ok(v) = app.main.region_text[i].trim().parse::<f64>() {
                        let v = v.round() as i32;
                        match i {
                            0 => app.s.region.x = v,
                            1 => app.s.region.y = v,
                            2 => app.s.region.width = v,
                            _ => app.s.region.height = v,
                        }
                        app.save_settings();
                    }
                }
            }
            ui.label(theme::muted(ui, "拖曳框選或移動紅框（可跨螢幕）")).on_hover_text("在預覽圖上拖曳框選範圍（可跨螢幕）；拖曳紅框可移動，拉邊或角可調整大小");
        }
    }
}

/// 預覽圖、螢幕框、自訂範圍的紅框
fn desk(app: &mut UiApp, ui: &mut Ui, area: Rect) {
    let p = theme::pal(ui);
    let view = view_rect(app);
    // 即時預覽：視窗看得到時才擷取；錄影中放慢
    let key = if app.s.source_type == SourceType::Monitor { app.s.monitor_id.clone().unwrap_or_default() } else { String::new() };
    let fps = if app.s.live_preview {
        if app.locked() {
            2
        } else {
            5
        }
    } else {
        0
    };
    // 開著剪輯、製作、全部錄影等視窗時暫停即時預覽（被蓋住看不到，不浪費 CPU / GPU），保留最後一張畫面
    let covered = app.editor.is_some() || app.export_dlg.is_some() || app.library.is_some() || app.viewer.is_some();
    if covered {
        if app.preview.running() {
            app.preview.stop(&app.core);
        }
    } else if app.env_ready {
        let (core, rt, ctx) = (app.core.clone(), app.rt.clone(), ui.ctx().clone());
        app.preview.ensure(&core, &rt, &ctx, &key, fps, app.env.ffmpeg.found);
    }
    // 依範圍的比例放進可用的空間
    let ar = if view.width > 0 && view.height > 0 { view.width as f32 / view.height as f32 } else { 16.0 / 9.0 };
    let mut w = area.width();
    let mut h = w / ar;
    if h > area.height() {
        h = area.height();
        w = h * ar;
    }
    let rect = Rect::from_min_size(pos2(area.center().x - w / 2.0, area.min.y), vec2(w, h));
    let painter = ui.painter_at(area);
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_SM), p.surface2);
    let tex = app.preview.texture(ui.ctx()).cloned();
    match tex {
        Some(t) => {
            painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            let msg = match app.preview.state() {
                super::preview::PreviewState::Failed(m) => m,
                super::preview::PreviewState::Loading { live: true } => "正在連接即時預覽…".into(),
                super::preview::PreviewState::Loading { live: false } => "正在擷取預覽…".into(),
                _ => {
                    if app.env_ready {
                        "尚無預覽".into()
                    } else {
                        "正在偵測螢幕與 FFmpeg…".into()
                    }
                }
            };
            theme::centered_text(&painter, rect, &msg, theme::font(13.0), p.muted);
        }
    }
    if view.width <= 0 {
        return;
    }
    let to_screen = |x: i32, y: i32| pos2(rect.min.x + (x - view.x) as f32 / view.width as f32 * rect.width(), rect.min.y + (y - view.y) as f32 / view.height as f32 * rect.height());
    let locked = app.locked();
    // 螢幕框
    let shown: Vec<_> = if app.s.source_type == SourceType::Monitor { app.s.selected_monitor(&app.env).cloned().into_iter().collect() } else { app.env.monitors.clone() };
    for m in &shown {
        let r = Rect::from_min_max(to_screen(m.x, m.y), to_screen(m.x + m.width, m.y + m.height));
        let selected = app.s.source_type == SourceType::All || (app.s.source_type == SourceType::Monitor && app.s.monitor_id.as_deref() == Some(&m.id));
        // 有框選範圍時，紅框只標範圍
        let boxed = app.s.source_type == SourceType::Region || region_target(app).is_some() || app.main.drag.is_some();
        let col = if selected && !boxed { p.rec } else { Color32::from_white_alpha(140) };
        painter.rect_stroke(r.shrink(1.0), CornerRadius::same(4), Stroke::new(if selected { 2.0 } else { 1.0 }, col), StrokeKind::Inside);
        let label = format!("螢幕 {}  {} × {}", m.display_number, m.width, m.height);
        let g = painter.layout_no_wrap(label, theme::font_bold(12.0), Color32::WHITE);
        let tag = Rect::from_min_size(r.min + vec2(8.0, 8.0), g.size() + vec2(12.0, 6.0));
        painter.rect_filled(tag, CornerRadius::same(5), if selected && !boxed { p.rec } else { Color32::from_black_alpha(140) });
        painter.galley(tag.min + vec2(6.0, 3.0), g, Color32::WHITE);
    }
    // 自訂範圍
    let desk_resp = ui.interact(rect, Id::new("desk"), if !locked && app.s.source_type != SourceType::All { Sense::click_and_drag() } else { Sense::click() });
    if app.s.source_type == SourceType::Monitor && desk_resp.clicked() && !locked {
        if let Some(pos) = desk_resp.interact_pointer_pos() {
            // 點螢幕框選擇那一台
            let x = view.x + ((pos.x - rect.min.x) / rect.width() * view.width as f32) as i32;
            let y = view.y + ((pos.y - rect.min.y) / rect.height() * view.height as f32) as i32;
            if let Some(m) = app.env.monitors.iter().find(|m| x >= m.x && x < m.x + m.width && y >= m.y && y < m.y + m.height) {
                app.s.monitor_id = Some(m.id.clone());
                app.save_settings();
            }
        }
    }
    if app.s.source_type == SourceType::All {
        return;
    }
    if !locked {
        region_drag(app, ui, &desk_resp, rect, view);
    }
    let Some(r) = region_target(app) else { return };
    let rr = Rect::from_min_max(to_screen(r.x, r.y), to_screen(r.x + r.width, r.y + r.height));
    // 範圍外變暗
    let dim = Color32::from_black_alpha(110);
    for part in [
        Rect::from_min_max(rect.min, pos2(rect.max.x, rr.min.y)),
        Rect::from_min_max(pos2(rect.min.x, rr.max.y), rect.max),
        Rect::from_min_max(pos2(rect.min.x, rr.min.y), pos2(rr.min.x, rr.max.y)),
        Rect::from_min_max(pos2(rr.max.x, rr.min.y), pos2(rect.max.x, rr.max.y)),
    ] {
        let part = part.intersect(rect);
        if part.is_positive() {
            painter.rect_filled(part, CornerRadius::ZERO, dim);
        }
    }
    painter.rect_stroke(rr, CornerRadius::ZERO, Stroke::new(2.0, p.rec), StrokeKind::Middle);
    if !locked {
        for h in handle_points(rr) {
            painter.rect(Rect::from_center_size(h, vec2(9.0, 9.0)), CornerRadius::same(2), Color32::WHITE, Stroke::new(1.5, p.rec), StrokeKind::Middle);
        }
    }
    let g = painter.layout_no_wrap(format!("{} × {}", r.width, r.height), theme::font_bold(12.0), Color32::WHITE);
    let tag = Rect::from_min_size(rr.min + vec2(0.0, -24.0).max(rect.min - rr.min), g.size() + vec2(12.0, 6.0));
    painter.rect_filled(tag, CornerRadius::same(5), p.rec);
    painter.galley(tag.min + vec2(6.0, 3.0), g, Color32::WHITE);
}

fn handle_points(r: Rect) -> [Pos2; 8] {
    let c = r.center();
    [r.left_top(), pos2(c.x, r.top()), r.right_top(), pos2(r.right(), c.y), r.right_bottom(), pos2(c.x, r.bottom()), r.left_bottom(), pos2(r.left(), c.y)]
}

/// 目前的框選範圍：自訂範圍，或單一螢幕裡框選的部分（沒有時是整個螢幕）
fn region_target(app: &UiApp) -> Option<DRect> {
    match app.s.source_type {
        SourceType::Region => Some(app.s.region),
        SourceType::Monitor => app.s.region_in_monitor(&app.env),
        SourceType::All => None,
    }
}

fn set_region(app: &mut UiApp, r: DRect) {
    match app.s.source_type {
        SourceType::Region => app.s.region = r,
        SourceType::Monitor => app.s.monitor_region = Some(r),
        SourceType::All => {}
    }
}

/// 框選範圍：拖曳框選新範圍；拖曳紅框內部移動，拖曳邊與角調整大小。
/// 自訂範圍限制在整個桌面內（可跨螢幕），單一螢幕限制在那個螢幕內（view）
fn region_drag(app: &mut UiApp, ui: &Ui, resp: &egui::Response, rect: Rect, view: DRect) {
    const MIN: i32 = 16;
    let to_desk = |pos: Pos2, clamp: bool| {
        let mut fx = (pos.x - rect.min.x) / rect.width();
        let mut fy = (pos.y - rect.min.y) / rect.height();
        if clamp {
            fx = fx.clamp(0.0, 1.0);
            fy = fy.clamp(0.0, 1.0);
        }
        (view.x + (fx * view.width as f32).round() as i32, view.y + (fy * view.height as f32).round() as i32)
    };
    let cur = region_target(app);
    let r = cur.unwrap_or(DRect { x: view.x, y: view.y, width: 0, height: 0 });
    let rr = Rect::from_min_max(
        pos2(rect.min.x + (r.x - view.x) as f32 / view.width as f32 * rect.width(), rect.min.y + (r.y - view.y) as f32 / view.height as f32 * rect.height()),
        pos2(rect.min.x + (r.x + r.width - view.x) as f32 / view.width as f32 * rect.width(), rect.min.y + (r.y + r.height - view.y) as f32 / view.height as f32 * rect.height()),
    );
    // 滑鼠游標：依位置顯示移動 / 調整大小
    let mode_at = |pos: Pos2| -> DragMode {
        let tol = 7.0;
        let near = |a: f32, b: f32| (a - b).abs() <= tol;
        let inside_y = pos.y >= rr.top() - tol && pos.y <= rr.bottom() + tol;
        let inside_x = pos.x >= rr.left() - tol && pos.x <= rr.right() + tol;
        let (n, s) = (near(pos.y, rr.top()) && inside_x, near(pos.y, rr.bottom()) && inside_x);
        let (w, e) = (near(pos.x, rr.left()) && inside_y, near(pos.x, rr.right()) && inside_y);
        if cur.is_none() {
            DragMode::New
        } else if n || s || e || w {
            DragMode::Resize { n, s, e, w }
        } else if rr.contains(pos) {
            DragMode::Move
        } else {
            DragMode::New
        }
    };
    if let Some(pos) = resp.hover_pos() {
        let icon = match app.main.drag.as_ref().map(|d| d.mode).unwrap_or_else(|| mode_at(pos)) {
            DragMode::Move => egui::CursorIcon::Move,
            DragMode::Resize { n, s, e, w } if (n && w) || (s && e) => egui::CursorIcon::ResizeNwSe,
            DragMode::Resize { n, s, e, w } if (n && e) || (s && w) => egui::CursorIcon::ResizeNeSw,
            DragMode::Resize { n, s, .. } if n || s => egui::CursorIcon::ResizeVertical,
            DragMode::Resize { .. } => egui::CursorIcon::ResizeHorizontal,
            DragMode::New => egui::CursorIcon::Crosshair,
        };
        ui.ctx().set_cursor_icon(icon);
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let mode = mode_at(pos);
            app.main.drag = Some(RegionDrag { mode, from: to_desk(pos, mode == DragMode::New), orig: r });
        }
    }
    if let (Some(d), Some(pos)) = (&app.main.drag, resp.interact_pointer_pos()) {
        let dk = if app.s.source_type == SourceType::Monitor { view } else { app.env.desktop };
        let o = d.orig;
        let next = match d.mode {
            DragMode::New => {
                let pt = to_desk(pos, true);
                DRect { x: d.from.0.min(pt.0), y: d.from.1.min(pt.1), width: (pt.0 - d.from.0).abs().max(1), height: (pt.1 - d.from.1).abs().max(1) }
            }
            DragMode::Move => {
                let pt = to_desk(pos, false);
                let (dx, dy) = (pt.0 - d.from.0, pt.1 - d.from.1);
                DRect { x: (o.x + dx).clamp(dk.x, dk.x + dk.width - o.width), y: (o.y + dy).clamp(dk.y, dk.y + dk.height - o.height), ..o }
            }
            DragMode::Resize { n, s, e, w } => {
                let pt = to_desk(pos, false);
                let (dx, dy) = (pt.0 - d.from.0, pt.1 - d.from.1);
                let (mut l, mut t, mut rt, mut b) = (o.x, o.y, o.x + o.width, o.y + o.height);
                if w {
                    l = (o.x + dx).clamp(dk.x, rt - MIN);
                }
                if e {
                    rt = (rt + dx).clamp(l + MIN, dk.x + dk.width);
                }
                if n {
                    t = (o.y + dy).clamp(dk.y, b - MIN);
                }
                if s {
                    b = (b + dy).clamp(t + MIN, dk.y + dk.height);
                }
                DRect { x: l, y: t, width: rt - l, height: b - t }
            }
        };
        set_region(app, next);
    }
    if resp.drag_stopped() && app.main.drag.take().is_some() {
        if let Some(mut r) = region_target(app).or(app.s.monitor_region) {
            r.width = r.width.max(MIN).min(dk_of(app, view).width);
            r.height = r.height.max(MIN).min(dk_of(app, view).height);
            let b = dk_of(app, view);
            r.x = r.x.clamp(b.x, b.x + b.width - r.width);
            r.y = r.y.clamp(b.y, b.y + b.height - r.height);
            set_region(app, r);
        }
        app.save_settings();
    }
}

/// 框選範圍的界線：單一螢幕是那個螢幕，自訂範圍是整個桌面
fn dk_of(app: &UiApp, view: DRect) -> DRect {
    if app.s.source_type == SourceType::Monitor {
        view
    } else {
        app.env.desktop
    }
}

// ───────────── 錄影設定（下方一列 + 下拉面板） ─────────────

fn combo<T: PartialEq + Copy>(ui: &mut Ui, id: &str, label: &str, value: &mut T, items: &[(T, String)], enabled: bool, width: f32) -> bool {
    let mut changed = false;
    if !label.is_empty() {
        ui.label(theme::muted(ui, label));
    }
    let text = items.iter().find(|(v, _)| v == value).map(|(_, t)| t.clone()).unwrap_or_default();
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id).selected_text(text).width(width).show_ui(ui, |ui| {
            for (v, t) in items {
                if ui.selectable_label(v == value, t).clicked() && v != value {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// 下拉按鈕：點一下開關面板，點外面關閉
fn drop_button(ui: &mut Ui, id: &str, text: String, icon: Icon, enabled: bool, width: f32, contents: impl FnOnce(&mut Ui)) {
    drop_button_tip(ui, id, text, None, icon, enabled, width, contents);
}

/// 下拉按鈕；text 為空時只有圖示（tip 顯示在滑鼠提示）
#[allow(clippy::too_many_arguments)]
fn drop_button_tip(ui: &mut Ui, id: &str, text: String, tip: Option<String>, icon: Icon, enabled: bool, width: f32, contents: impl FnOnce(&mut Ui)) {
    let mut b = Btn::new(text).icon(icon).trailing(Icon::Down).small().enabled(enabled);
    if let Some(t) = tip {
        b = b.tooltip(t);
    }
    let resp = b.show(ui);
    egui::Popup::from_toggle_button_response(&resp).id(Id::new(id)).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(width).show(|ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 8.0;
        contents(ui);
    });
}

fn settings_bar(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let locked = app.locked();
    // 視窗較窄時：聲音只寫來源、「更多設定」只剩圖示、不顯示輸出大小，整列才放得下
    let compact = ui.available_width() < 800.0;
    let mut fps_items: Vec<(f64, String)> = FPS_CHOICES.iter().map(|f| (*f, format!("{f}"))).collect();
    if !FPS_CHOICES.contains(&app.s.fps) {
        fps_items.push((app.s.fps, format!("{}", app.s.fps)));
    }
    if combo(ui, "fps", "FPS", &mut app.s.fps, &fps_items, !locked, 56.0) {
        app.save_settings();
    }
    ui.add_space(4.0);
    let scales: Vec<(u32, String)> = [100, 75, 50, 25].iter().map(|s| (*s, format!("{s}%"))).collect();
    if combo(ui, "scale", if compact { "" } else { "解析度" }, &mut app.s.scale, &scales, !locked, 64.0) {
        app.save_settings();
    }
    ui.add_space(4.0);
    let mut dm = app.s.draw_mouse;
    if switch(ui, &mut dm, "游標", !locked).changed() {
        app.s.draw_mouse = dm;
        app.save_settings();
    }
    ui.add_space(4.0);
    // 聲音
    let label = match (app.s.audio_system, app.s.audio_mic) {
        (true, true) => "系統＋麥克風",
        (true, false) => "系統",
        (false, true) => "麥克風",
        _ => "不錄",
    };
    let icon = if app.s.audio_mic && !app.s.audio_system { Icon::Mic } else { Icon::Speaker };
    let audio_text = if compact { label.to_string() } else { format!("聲音：{label}") };
    drop_button_tip(ui, "audio", audio_text, Some(format!("錄製聲音：{label}")), icon, !locked, 300.0, |ui| audio_panel(app, ui));
    // 更多設定
    let mut more = vec!["更多設定".to_string()];
    if app.s.max_minutes > 0.0 {
        more.push(format!("最長 {}", human_duration(app.s.max_minutes * 60.0)));
    }
    if app.s.countdown_sec != 3 {
        more.push(if app.s.countdown_sec > 0 { format!("倒數 {} 秒", app.s.countdown_sec) } else { "不倒數".into() });
    }
    match app.s.method {
        MethodPreference::Ddagrab => more.push("ddagrab".into()),
        MethodPreference::Gdigrab => more.push("gdigrab".into()),
        _ => {}
    }
    match app.s.encoder {
        EncoderPreference::Gpu => more.push("GPU 編碼".into()),
        EncoderPreference::Cpu => more.push("CPU 編碼".into()),
        _ => {}
    }
    let more_text = if compact { String::new() } else { more.join("・") };
    drop_button_tip(ui, "more", more_text, Some(more.join("・")), Icon::Settings, !locked, 340.0, |ui| more_panel(app, ui));
    // 儲存位置
    let dir = app.s.out_dir(&app.env);
    // 依剩下的寬度縮短路徑（保留結尾），整列不會超出卡片；右邊留給「輸出 3840×1080」
    let out_w = if !compact && app.s.source_rect(&app.env).is_some() { 110.0 } else { 0.0 };
    let room = (ui.available_width() - out_w - 60.0).max(40.0);
    let fits = |t: &str| ui.painter().layout_no_wrap(t.to_string(), theme::font(13.0), p.text).size().x <= room;
    let short: String = if fits(&dir) {
        dir.clone()
    } else {
        let chars: Vec<char> = dir.chars().collect();
        (1..chars.len()).map(|i| format!("…{}", chars[i..].iter().collect::<String>())).find(|t| fits(t)).unwrap_or_else(|| "…".into())
    };
    ui.scope(|ui| {
        drop_button(ui, "dir", short, Icon::Folder, !locked, 420.0, |ui| dir_panel(app, ui));
    })
    .response
    .on_hover_text(format!("儲存位置：{dir}"));
    // 輸出大小
    if let Some(r) = app.s.source_rect(&app.env).filter(|_| !compact) {
        let (w, h) = output_size(r.width, r.height, app.s.scale as f64);
        let big = (w as f64) * (h as f64) > 3840.0 * 2160.0 * 1.05;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format!("輸出 {w}×{h}{}", if big { "（很大，建議 50%）" } else { "" })).font(theme::font(12.5)).color(if big { p.warn } else { p.muted }));
        });
    }
}

fn audio_panel(app: &mut UiApp, ui: &mut Ui) {
    let a = app.env.audio.clone();
    let mut sys = app.s.audio_system;
    let render = a.render.clone().map(|r| format!("（{r}）")).unwrap_or_else(|| "（找不到播放裝置）".into());
    if switch(ui, &mut sys, format!("系統聲音{render}"), true).changed() {
        app.s.audio_system = sys;
        app.save_settings();
    }
    let mut mic = app.s.audio_mic;
    if switch(ui, &mut mic, "麥克風", true).changed() {
        app.s.audio_mic = mic;
        app.save_settings();
    }
    if !app.s.mic_id.is_empty() && !a.captures.iter().any(|d| d.id == app.s.mic_id) {
        app.s.mic_id.clear();
    }
    let def = a.captures.iter().find(|d| d.is_default).map(|d| format!("預設麥克風（{}）", d.name)).unwrap_or_else(|| "預設麥克風".into());
    let current = if app.s.mic_id.is_empty() { def.clone() } else { a.captures.iter().find(|d| d.id == app.s.mic_id).map(|d| d.name.clone()).unwrap_or_default() };
    ui.add_enabled_ui(app.s.audio_mic, |ui| {
        egui::ComboBox::from_id_salt("mic").selected_text(current).width(ui.available_width()).show_ui(ui, |ui| {
            if ui.selectable_label(app.s.mic_id.is_empty(), &def).clicked() {
                app.s.mic_id.clear();
                app.save_settings();
            }
            for d in &a.captures {
                if ui.selectable_label(app.s.mic_id == d.id, &d.name).clicked() {
                    app.s.mic_id = d.id.clone();
                    app.save_settings();
                }
            }
        });
    });
    let hint = a.error.clone().unwrap_or_else(|| {
        if app.s.audio_system || app.s.audio_mic {
            "聲音與畫面以同一個時鐘對齊。系統聲音會受 Windows 音量影響。".into()
        } else if a.captures.is_empty() {
            "找不到麥克風。".into()
        } else {
            String::new()
        }
    });
    if !hint.is_empty() {
        ui.add(egui::Label::new(theme::muted(ui, hint)).wrap());
    }
}

fn more_panel(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    // 最長錄影時間
    ui.horizontal(|ui| {
        let custom = app.main.max_custom_open || !MAX_PRESETS.contains(&app.s.max_minutes);
        let mut sel: f64 = if custom { -1.0 } else { app.s.max_minutes };
        let items = vec![(0.0, "不限".to_string()), (30.0, "30 分鐘".into()), (60.0, "1 小時".into()), (120.0, "2 小時".into()), (-1.0, "自訂…".into())];
        if combo(ui, "maxPreset", "最長錄影時間", &mut sel, &items, true, 96.0) {
            app.main.max_custom_open = sel < 0.0;
            if sel >= 0.0 {
                app.s.max_minutes = sel;
                app.save_settings();
            } else {
                app.main.max_text = if app.s.max_minutes > 0.0 { app.s.max_minutes.to_string() } else { String::new() };
            }
        }
        if custom {
            if !app.main.max_custom_open && app.main.max_text.is_empty() {
                app.main.max_text = app.s.max_minutes.to_string();
            }
            let r = ui.add(egui::TextEdit::singleline(&mut app.main.max_text).desired_width(56.0).hint_text("分鐘"));
            ui.label(theme::muted(ui, "分鐘"));
            if r.changed() {
                let v: f64 = app.main.max_text.trim().parse().unwrap_or(0.0);
                app.s.max_minutes = if v.is_finite() && v > 0.0 { v.round().min(screenrecorder_core::format::MAX_MINUTES_MAX) } else { 0.0 };
                app.save_settings();
            }
        }
    });
    let mut cd = app.s.countdown_sec;
    ui.horizontal(|ui| {
        let items: Vec<(u32, String)> = [(0, "不倒數".to_string()), (3, "3 秒".into()), (5, "5 秒".into()), (10, "10 秒".into())].to_vec();
        if combo(ui, "countdown", "開始前倒數", &mut cd, &items, true, 96.0) {
            app.s.countdown_sec = cd;
            app.save_settings();
        }
    });
    let mut hide = app.s.hide_ui;
    if ui.checkbox(&mut hide, "開始錄影時縮小這個視窗").on_hover_text("視窗不在錄影範圍內時不縮小").changed() {
        app.s.hide_ui = hide;
        app.save_settings();
    }
    ui.separator();
    // 快捷鍵
    match app.env.hotkeys {
        Some(hk) => {
            for (keys, what, ok) in
                [(HOTKEY_RECORD_LABEL, "開始 / 停止錄影", hk.record), (HOTKEY_PAUSE_LABEL, "暫停 / 繼續", hk.pause), (HOTKEY_SHOT_LABEL, "截圖", hk.shot), (HOTKEY_SNIP_LABEL, "框選截圖", hk.snip)]
            {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(keys).font(theme::mono(12.5)).background_color(p.surface2));
                    ui.label(what);
                    if !ok {
                        ui.label(RichText::new("（已被其他程式使用）").color(p.warn));
                    }
                });
            }
        }
        None => {
            ui.label(theme::muted(ui, "全域快捷鍵需要系統匣常駐時才能使用"));
        }
    }
    ui.horizontal(|ui| {
        let mut on = app.update.enabled;
        if ui.checkbox(&mut on, "自動檢查新版本").changed() {
            app.core.set_check_updates(on);
            app.update = actions::update_state(&app.core);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new("立即檢查").ghost().small().show(ui).clicked() {
                let core = app.core.clone();
                app.spawn(async move { actions::check_update(&core).await }, |app, r| match r {
                    Ok(u) => {
                        app.update = actions::update_state(&app.core);
                        app.env.update = u.clone();
                        match u {
                            Some(u) => app.toast(format!("有新版本 v{}，點「系統狀態」裡的「有新版本」即可更新", u.version), false),
                            None => app.toast(format!("目前已是最新版本（v{}）", app.env.app_version), false),
                        }
                    }
                    Err(e) => {
                        app.update.error = Some(e.message().to_string());
                        app.toast(e.message().to_string(), true);
                    }
                });
            }
        });
    });
    if let Some(e) = &app.update.error {
        ui.label(RichText::new(format!("無法檢查新版本：{}", e.trim_start_matches("無法檢查新版本："))).color(p.warn).font(theme::font(12.0)));
    }
    ui.separator();
    ui.label(RichText::new("進階").font(theme::font_bold(12.5)).color(p.muted));
    let mut method = app.s.method;
    ui.horizontal(|ui| {
        let items = vec![
            (MethodPreference::Auto, "自動（建議）".to_string()),
            (MethodPreference::Ddagrab, "ddagrab（Desktop Duplication）".into()),
            (MethodPreference::Gdigrab, "gdigrab（GDI，相容性最高）".into()),
        ];
        if combo(ui, "method", "擷取方式", &mut method, &items, true, 210.0) {
            app.s.method = method;
            app.save_settings();
        }
    });
    let ff = app.env.ffmpeg.clone();
    let hw = ff.hw_encoders.clone();
    if app.s.encoder == EncoderPreference::Gpu && hw.as_ref().is_some_and(|h| h.is_empty()) {
        // 這台電腦沒有可用的 GPU：改回自動並存檔，系統匣「開始錄影」才不會拿到無效的設定
        app.s.encoder = EncoderPreference::Auto;
        app.save_settings();
    }
    let gpu_label = match &hw {
        None => "GPU（偵測中…）".to_string(),
        Some(h) if !h.is_empty() => format!("GPU（{}）", h.join("、")),
        _ => "GPU（這台電腦沒有可用的）".into(),
    };
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, "編碼器"));
        let text = match app.s.encoder {
            EncoderPreference::Auto => "自動（建議）".to_string(),
            EncoderPreference::Cpu => "CPU（libx264，畫質最穩）".into(),
            EncoderPreference::Gpu => gpu_label.clone(),
        };
        egui::ComboBox::from_id_salt("encoder").selected_text(text).width(210.0).show_ui(ui, |ui| {
            for (v, t, en) in [
                (EncoderPreference::Auto, "自動（建議）".to_string(), true),
                (EncoderPreference::Cpu, "CPU（libx264，畫質最穩）".into(), true),
                (EncoderPreference::Gpu, gpu_label.clone(), hw.as_ref().is_some_and(|h| !h.is_empty())),
            ] {
                if ui.add_enabled(en, egui::Button::selectable(app.s.encoder == v, t)).clicked() {
                    app.s.encoder = v;
                    app.save_settings();
                }
            }
        });
    });
    let hint = if app.s.encoder != EncoderPreference::Auto {
        String::new()
    } else {
        match &hw {
            Some(h) if !h.is_empty() => {
                if ff.prefer_gpu == Some(true) {
                    format!("先前偵測到 CPU 編碼跟不上，目前會使用 GPU（{}）。", h[0])
                } else {
                    format!("平常用 CPU；畫面大或電腦跟不上時改用 GPU（{}）。", h[0])
                }
            }
            _ => "這台電腦只能用 CPU 編碼。".into(),
        }
    };
    if !hint.is_empty() {
        ui.add(egui::Label::new(theme::muted(ui, hint)).wrap());
    }
    if app.s.encoder == EncoderPreference::Auto && ff.prefer_gpu == Some(true) && hw.as_ref().is_some_and(|h| !h.is_empty()) && Btn::new("重設：恢復平常用 CPU 編碼").ghost().small().show(ui).clicked()
    {
        app.core.reset_learned_gpu();
        app.env.ffmpeg.prefer_gpu = Some(false);
        app.toast("已重設，之後的錄影平常會用 CPU 編碼", false);
    }
}

fn dir_panel(app: &mut UiApp, ui: &mut Ui) {
    ui.label(theme::muted(ui, "儲存位置"));
    let text = app.main.dir_text.get_or_insert_with(|| app.s.output_dir.clone());
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
    if r.changed() {
        app.s.output_dir = text.clone();
        app.save_settings();
        app.main.dir_changed_at = Some(Instant::now());
    }
    if !r.has_focus() {
        app.main.dir_text = None;
    }
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, "檔名：Rec_年-月-日_時-分-秒.mp4"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new("在檔案總管開啟").small().show(ui).clicked() {
                if let Err(e) = actions::open(actions::OpenAction::Folder, &app.s.out_dir(&app.env)) {
                    app.toast(e.message().to_string(), true);
                }
            }
        });
    });
}

// ───────────── 錄影狀態 ─────────────

fn live_recorded_ms(app: &UiApp) -> f64 {
    let r = &app.status.recorder;
    if r.state == RecorderState::Recording {
        r.recorded_ms as f64 + app.status_at.elapsed().as_millis() as f64
    } else {
        r.recorded_ms as f64
    }
}

fn countdown_left(app: &UiApp) -> u64 {
    let left = app.status.recorder.countdown_ms.unwrap_or(0) as f64 - app.status_at.elapsed().as_millis() as f64;
    (left.max(0.0) / 1000.0).ceil().max(1.0) as u64
}

fn rec_panel(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    theme::card(ui).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let r = app.status.recorder.clone();
        let active = r.state != RecorderState::Idle;
        // 狀態
        ui.horizontal(|ui| {
            let (text, tone) = match r.state {
                RecorderState::Idle => ("待命", Tone::Plain),
                RecorderState::Countdown => ("倒數中", Tone::Warn),
                RecorderState::Recording => ("● 錄影中", Tone::Bad),
                RecorderState::Paused => ("已暫停", Tone::Warn),
                RecorderState::Stopping => ("處理中", Tone::Accent),
            };
            chip(ui, text, tone, false);
            let mut busy = r.busy.clone().unwrap_or_default();
            if busy.is_empty() && r.max_ms > 0 && active {
                busy = format!("剩餘 {}", clock((r.max_ms as f64 - live_recorded_ms(app)).max(0.0)));
            }
            ui.label(theme::muted(ui, busy));
        });
        ui.add_space(4.0);
        let ms = live_recorded_ms(app);
        ui.label(RichText::new(clock(ms)).font(theme::mono(46.0)).color(if r.state == RecorderState::Recording { p.rec } else { p.text }));
        if r.max_ms > 0 && active && r.state != RecorderState::Countdown {
            theme::progress(ui, (ms / r.max_ms as f64) as f32, p.rec, 4.0);
        }
        ui.add_space(6.0);
        // 統計
        let (out, method, audio) = if active {
            (
                format!("{}×{} · {}fps", r.out_width, r.out_height, r.fps),
                format!(
                    "{}{} / {}",
                    r.method.map(|m| m.as_str()).unwrap_or("—"),
                    r.tiles.filter(|t| *t > 1).map(|t| format!(" ×{t}")).unwrap_or_default(),
                    r.encoder.clone().unwrap_or_else(|| "—".into())
                ),
                r.audio.clone().unwrap_or_else(|| "不錄聲音".into()),
            )
        } else {
            let o = app.s.source_rect(&app.env).map(|s| output_size(s.width, s.height, app.s.scale as f64));
            let enc = match app.s.encoder {
                EncoderPreference::Gpu => app.env.ffmpeg.hw_encoders.as_ref().and_then(|h| h.first().cloned()).unwrap_or_else(|| "GPU".into()),
                EncoderPreference::Cpu => app.env.ffmpeg.encoder.clone().unwrap_or_else(|| "CPU".into()),
                EncoderPreference::Auto => "自動".into(),
            };
            let m = match app.s.method {
                MethodPreference::Auto => "自動",
                MethodPreference::Ddagrab => "ddagrab",
                MethodPreference::Gdigrab => "gdigrab",
            };
            let parts: Vec<&str> = [app.s.audio_system.then_some("系統聲音"), app.s.audio_mic.then_some("麥克風")].into_iter().flatten().collect();
            (
                o.map(|(w, h)| format!("{w}×{h} · {}fps", app.s.fps)).unwrap_or_else(|| "—".into()),
                if app.env.ffmpeg.encoder.is_some() { format!("{m} / {enc}") } else { "—".into() },
                if parts.is_empty() { "不錄聲音".into() } else { parts.join(" + ") },
            )
        };
        egui::Grid::new("stats").num_columns(2).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
            for (k, v) in
                [("影片長度", video_clock(r.video_sec)), ("檔案大小", if active || r.bytes > 0 { format_bytes(r.bytes) } else { "—".into() }), ("輸出", out), ("擷取 / 編碼", method), ("聲音", audio)]
            {
                ui.label(theme::muted(ui, k));
                ui.label(RichText::new(v).font(theme::font(13.0)));
                ui.end_row();
            }
        });
        ui.add_space(10.0);
        // 控制按鈕
        ui.horizontal(|ui| {
            if !active {
                let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && !app.exporting();
                let tip = if app.exporting() { "轉檔進行中，完成後才能錄影" } else { "" };
                let shot_w = 92.0;
                if Btn::new("開始錄影").kind(theme::Kind::Record).enabled(can).tooltip(tip).min_width(ui.available_width() - shot_w - ui.spacing().item_spacing.x).show(ui).clicked() {
                    let config = app.s.record_config(&app.env);
                    let core = app.core.clone();
                    app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
                }
                shot_button(app, ui, shot_w);
            } else {
                let w = ui.available_width();
                if r.state == RecorderState::Recording && Btn::new("暫停").icon(Icon::Pause).enabled(r.busy.is_none()).min_width(w * 0.45).show(ui).clicked() {
                    let core = app.core.clone();
                    app.guarded(async move { core.recorder.pause().await }, |_, _| {});
                }
                if r.state == RecorderState::Paused && Btn::new("繼續").icon(Icon::Play).enabled(r.busy.is_none()).min_width(w * 0.45).show(ui).clicked() {
                    let core = app.core.clone();
                    app.guarded(async move { core.recorder.resume().await }, |_, _| {});
                }
                let stop_label = if r.state == RecorderState::Countdown {
                    if r.countdown_covers_ui == Some(false) {
                        format!("{} 秒後開始・取消", countdown_left(app))
                    } else {
                        "取消倒數".into()
                    }
                } else {
                    "停止".into()
                };
                if Btn::new(stop_label).icon(Icon::Stop).danger().enabled(r.state != RecorderState::Stopping).min_width(ui.available_width()).show(ui).clicked() {
                    let core = app.core.clone();
                    app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
                }
            }
        });
        // 提醒
        let mut msgs = vec![];
        if let Some(t) = &r.retrying {
            msgs.push(t.clone());
        }
        if r.slow {
            msgs.push(format!("電腦跟不上：實際約 {} fps（設定 {}），建議調低解析度或 FPS", r.actual_fps.map(|f| format!("{f:.1}")).unwrap_or_else(|| "?".into()), r.fps));
        }
        if !msgs.is_empty() {
            ui.add_space(6.0);
            egui::Frame::new().fill(p.warn_soft).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(msgs.join("　")).color(p.warn)).wrap());
            });
        }
        job_card(app, ui);
        // 錄影結果
        if let Some(res) = r.result.clone().filter(|_| !active) {
            ui.add_space(8.0);
            let (fill, stroke) = if res.ok { (p.ok_soft, p.ok) } else { (p.rec_soft, p.rec) };
            egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke.gamma_multiply(0.4))).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                match (&res.path, res.ok) {
                    (Some(path), true) => {
                        ui.label(RichText::new(format!("錄影已儲存・{}・{}", video_clock(res.video_sec), format_bytes(res.bytes.unwrap_or(0)))).font(theme::font_bold(13.5)).color(p.ok));
                        ui.label(RichText::new(file_name(path)).font(theme::mono(12.0))).on_hover_text(path);
                        action_buttons(app, ui, path, true, true);
                    }
                    _ => {
                        ui.label(RichText::new("錄影未完成").font(theme::font_bold(13.5)).color(p.rec));
                        ui.add(egui::Label::new(RichText::new(&res.message).font(theme::font(12.5))).wrap());
                    }
                }
            });
        }
        // 事件紀錄 + 系統狀態（固定在底部）
        let sys_h = BOTTOM_BAR_H;
        // 轉檔工作、結果佔掉空間時：放不下就不顯示事件紀錄，系統狀態不被蓋住
        let log_h = ui.available_height() - sys_h - 8.0 - 32.0;
        if log_h >= 40.0 {
            ui.add_space(10.0);
            ui.label(RichText::new("事件紀錄").font(theme::font_bold(12.5)).color(p.muted));
            egui::ScrollArea::vertical().max_height(log_h).auto_shrink([false, true]).show(ui, |ui| {
                for l in r.log.iter().rev().take(20) {
                    let time = chrono::DateTime::from_timestamp_millis(l.t as i64).map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string()).unwrap_or_default();
                    let color = match l.level {
                        LogLevel::Info => p.text,
                        LogLevel::Warn => p.warn,
                        LogLevel::Error => p.rec,
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(time).font(theme::mono(11.5)).color(p.muted));
                        ui.add(egui::Label::new(RichText::new(&l.text).font(theme::font(12.5)).color(color)).truncate()).on_hover_text(&l.text);
                    });
                }
            });
        }
        let bottom = Rect::from_min_max(pos2(ui.max_rect().min.x, ui.max_rect().max.y - sys_h), ui.max_rect().max);
        ui.painter().hline(bottom.x_range(), bottom.min.y - BOTTOM_LINE_GAP, Stroke::new(1.0, p.border));
        ui.scope_builder(UiBuilder::new().max_rect(bottom).layout(Layout::left_to_right(Align::Center)), |ui| sys_status(app, ui));
    });
}

/// 截圖：與錄影相同的範圍，存成 PNG 並複製到剪貼簿（完成後由狀態更新顯示提示、更新清單）
fn shot_button(app: &mut UiApp, ui: &mut Ui, w: f32) {
    let hotkey = if app.env.hotkeys.is_some_and(|h| h.shot) { format!("（{HOTKEY_SHOT_LABEL}）") } else { String::new() };
    let tip = format!("截取目前的擷取範圍，存成 PNG 並複製到剪貼簿{hotkey}");
    let can = app.env.ffmpeg.found && !app.main.shooting;
    if Btn::new("截圖").icon(Icon::Camera).enabled(can).tooltip(tip).min_width(w).height(40.0).show(ui).clicked() {
        app.main.shooting = true;
        let config = app.s.record_config(&app.env);
        let core = app.core.clone();
        app.spawn(async move { core.screenshot(&config).await }, |app, r| {
            app.main.shooting = false;
            if let Err(e) = r {
                app.toast(e.message().to_string(), true);
            }
        });
    }
}

/// 播放 / 顯示 / 剪輯 / 製作加速版的按鈕
fn action_buttons(app: &mut UiApp, ui: &mut Ui, path: &str, edit: bool, export: bool) {
    ui.horizontal_wrapped(|ui| {
        if Btn::new("播放").icon(Icon::Play).small().show(ui).clicked() {
            app.act_path(EntryAction::Play, path.to_string());
        }
        if Btn::new("顯示").icon(Icon::Folder).small().show(ui).clicked() {
            app.act_path(EntryAction::Reveal, path.to_string());
        }
        if edit && Btn::new("剪輯").icon(Icon::Cut).small().show(ui).clicked() {
            app.act_path(EntryAction::Edit, path.to_string());
        }
        if export && Btn::new("製作加速版 / GIF").icon(Icon::Export).small().show(ui).clicked() {
            app.act_path(EntryAction::Export, path.to_string());
        }
    });
}

/// 「工作」卡：製作加速版 / GIF、剪輯的進度
fn job_card(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let Some(e) = app.status.export.clone() else {
        return;
    };
    if app.dismissed_job == Some(e.id) && e.state != ExportState::Running {
        return;
    }
    ui.add_space(8.0);
    let label = match e.kind {
        ExportKind::Cut => "剪輯".to_string(),
        ExportKind::Gif => format!("製作 GIF{}", if e.speed > 1.0 { format!(" {}×", speed_label(e.speed)) } else { String::new() }),
        ExportKind::Speed => format!("製作 {}× 加速版", speed_label(e.speed)),
    };
    let title = match e.state {
        ExportState::Running => format!("{label}中"),
        ExportState::Done => format!("{label}完成"),
        ExportState::Error => format!("{label}失敗"),
        ExportState::Canceled => format!("{label}已取消"),
    };
    let pct = (e.progress * 100.0).floor();
    egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).font(theme::font_bold(13.5)));
            let meta = match e.state {
                ExportState::Running => format!("{pct}%{}", e.eta_sec.map(|s| format!("・剩 {}", human_duration(s))).unwrap_or_default()),
                ExportState::Done => format!("{}・{}", video_clock(e.expected_sec), format_bytes(e.bytes.unwrap_or(0))),
                _ => String::new(),
            };
            ui.label(theme::muted(ui, meta));
        });
        match e.state {
            ExportState::Running => {
                theme::progress(ui, e.progress as f32, p.accent, 6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(theme::muted(ui, file_name(&e.output))).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if Btn::new("取消").small().show(ui).clicked() {
                            let _ = app.core.exporter.cancel();
                        }
                    });
                });
            }
            ExportState::Done => {
                let cut = e.kind == ExportKind::Cut;
                action_buttons(app, ui, &e.output, cut, cut);
                if Btn::new("關閉").ghost().small().show(ui).clicked() {
                    app.dismissed_job = Some(e.id);
                }
            }
            _ => {
                ui.add(egui::Label::new(RichText::new(e.message.clone().unwrap_or_default()).font(theme::font(12.5))).wrap());
                if Btn::new("關閉").ghost().small().show(ui).clicked() {
                    app.dismissed_job = Some(e.id);
                }
            }
        }
    });
}

/// 系統狀態：新版本、FFmpeg、擷取方式、編碼器（正常的合併成一個「已就緒」）
fn sys_status(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    ui.label(RichText::new("系統狀態").font(theme::font(12.0)).color(p.muted));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ff = app.env.ffmpeg.clone();
        let mut chips: Vec<(String, Tone, String)> = vec![];
        let mut ok: Vec<String> = vec![];
        let ver = ff.version.clone().unwrap_or_default().split('-').next().unwrap_or("").to_string();
        if !app.env_ready {
            chips.push(("偵測中…".into(), Tone::Plain, String::new()));
        } else if ff.found {
            ok.push(format!("FFmpeg {ver}（{}）", ff.path.clone().unwrap_or_default()));
            if !ff.has_ddagrab {
                chips.push(("gdigrab".into(), Tone::Warn, "此 FFmpeg 不含 ddagrab，將使用 gdigrab".into()));
            } else {
                match ff.ddagrab_works {
                    None => chips.push(("ddagrab 測試中…".into(), Tone::Plain, String::new())),
                    Some(true) => ok.push("擷取：ddagrab（GPU 擷取）".into()),
                    Some(false) => chips.push(("ddagrab 不可用 → gdigrab".into(), Tone::Warn, ff.ddagrab_error.clone().unwrap_or_default())),
                }
            }
            match &ff.encoder {
                Some(e) => ok.push(format!("編碼：{e}")),
                None => chips.push(("無 H.264 編碼器".into(), Tone::Bad, String::new())),
            }
        } else {
            chips.push(("找不到 FFmpeg".into(), Tone::Bad, String::new()));
        }
        if !ok.is_empty() {
            let problems = chips.iter().any(|c| matches!(c.1, Tone::Warn | Tone::Bad));
            chips.push((if problems { format!("FFmpeg {ver}") } else { "已就緒".into() }, Tone::Ok, ok.join("\n")));
        }
        // 右到左：最後加的在最左邊
        for (text, tone, tip) in chips.iter().rev() {
            let r = chip(ui, text, *tone, false);
            if !tip.is_empty() {
                r.on_hover_text(tip);
            }
        }
        if let Some(u) = app.env.update.clone() {
            let auto = u.download_url.is_some() && u.sha256.is_some();
            let label = match &app.status.install {
                Some(i) if i.phase == screenrecorder_core::selfupdate::InstallPhase::Downloading => {
                    if let Some(total) = i.total.filter(|t| *t > 0) {
                        format!("下載新版 {}%", (i.received as f64 / total as f64 * 100.0).floor())
                    } else {
                        format!("下載新版 {}", format_bytes(i.received))
                    }
                }
                Some(i) if i.phase == screenrecorder_core::selfupdate::InstallPhase::Verifying => "驗證新版…".into(),
                Some(i) if i.phase == screenrecorder_core::selfupdate::InstallPhase::Restarting => "重新啟動中…".into(),
                _ => format!("有新版本 v{}", u.version),
            };
            let busy = !label.starts_with("有新版本");
            let r = chip(ui, &label, Tone::Accent, !busy).on_hover_text(if auto { "點一下更新（自動下載並重新啟動）" } else { "點一下前往下載頁面" });
            if r.clicked() && !busy {
                if !auto {
                    let _ = actions::open_url(&u.url);
                } else {
                    let size = u.size.map(|s| format!("（約 {}）", format_bytes(s))).unwrap_or_default();
                    app.ask = Some(Ask::confirm(
                        format!("更新到 v{}", u.version),
                        format!("會下載新版{size}、核對檔案後自動重新啟動程式；設定與錄影都會保留。更新內容可在系統匣選單的「更新說明」查看。"),
                        "立即更新",
                        |app, _| {
                            app.ask = None;
                            if let Err(e) = app.core.start_self_update() {
                                app.toast(e.message().to_string(), true);
                            }
                        },
                    ));
                }
            }
        }
    });
}

// ───────────── 最近錄影 ─────────────

/// 「全部錄影」「全部截圖」按鈕的最小寬度
const LIB_BTN_W: f32 = 120.0;

fn recent_strip(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    theme::card(ui).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.horizontal_centered(|ui| {
            ui.vertical(|ui| {
                ui.set_width(84.0);
                ui.label(RichText::new("最近錄影").font(theme::font_bold(14.0)));
                if let Some(r) = &app.recent {
                    if r.total > 0 {
                        ui.label(theme::muted(ui, format!("{} / {} 頁", app.recent_page, r.pages.max(1))));
                    }
                }
            });
            let pages = app.recent.as_ref().map(|r| r.pages).unwrap_or(1);
            if Btn::icon_only(Icon::ChevL).ghost().small().enabled(app.recent_page > 1).tooltip("較新的錄影").show(ui).clicked() {
                app.recent_page -= 1;
                app.load_recent();
            }
            // 一頁放幾張依寬度決定
            // 右邊「全部錄影」「全部截圖」：一樣寬，圖示與文字靠左對齊
            let rec_btn = Btn::new("全部錄影").icon(Icon::List).small().left();
            let shot_btn = Btn::new("全部截圖").icon(Icon::Camera).small().left();
            let btn_w = rec_btn.width(ui).max(shot_btn.width(ui)).max(LIB_BTN_W);
            let lib_w = btn_w + 8.0;
            let cards_w = ui.available_width() - lib_w - 44.0;
            let card_w = 280.0;
            let per = ((cards_w + 10.0) / (card_w + 10.0)).floor().clamp(1.0, 8.0) as usize;
            // 卡片平均分配寬度（填滿這一列）
            let card_w = ((cards_w - 8.0 * (per as f32 - 1.0)) / per as f32).clamp(card_w, 360.0);
            if per != app.recent_per_page {
                app.recent_per_page = per;
                app.load_recent();
            }
            let items = app.recent.as_ref().map(|r| r.items.clone()).unwrap_or_default();
            ui.allocate_ui(vec2(cards_w, ui.available_height()), |ui| {
                ui.horizontal_centered(|ui| {
                    if items.is_empty() {
                        let dir = app.s.out_dir(&app.env);
                        ui.label(theme::muted(ui, format!("「{dir}」還沒有錄影，按「開始錄影」試試看。")));
                    }
                    let dates = date_labels(items.iter().map(|e| (e.media.name.as_str(), e.media.mtime)));
                    for e in &items {
                        recent_card(app, ui, e, dates.get(&e.media.name).cloned().unwrap_or_default(), card_w);
                    }
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 錄影與截圖分開看
                ui.allocate_ui_with_layout(vec2(btn_w, 64.0), Layout::top_down(Align::Max), |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if rec_btn.min_width(btn_w).show(ui).clicked() {
                        app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video));
                    }
                    if shot_btn.min_width(btn_w).show(ui).clicked() {
                        app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot));
                    }
                });
                if Btn::icon_only(Icon::ChevR).ghost().small().enabled(app.recent_page < pages).tooltip("較舊的錄影").show(ui).clicked() {
                    app.recent_page += 1;
                    app.load_recent();
                }
            });
        });
        let _ = p;
    });
}

fn recent_card(app: &mut UiApp, ui: &mut Ui, e: &LibraryEntry, date: String, w: f32) {
    let p = theme::pal(ui);
    let h = 80.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect(rect, CornerRadius::same(theme::RADIUS_SM), if resp.hovered() { p.surface2 } else { p.surface }, Stroke::new(1.0, p.border), StrokeKind::Inside);
    let thumb = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2((h - 12.0) * 16.0 / 9.0, h - 12.0));
    painter.rect_filled(thumb, CornerRadius::same(5), p.surface2);
    if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
        theme::paint_thumb(&painter, thumb, &t);
    }
    let x = thumb.max.x + 10.0;
    let meta = format!("{}・{}", e.media.duration_sec.map(video_clock).unwrap_or_else(|| "—".into()), format_bytes(e.media.bytes));
    painter.text(pos2(x, rect.min.y + 28.0), egui::Align2::LEFT_TOP, meta, theme::font(12.0), p.muted);
    // 標籤
    let mut tags: Vec<(String, Tone, String)> = vec![];
    if e.media.has_audio == Some(true) {
        tags.push(("聲音".into(), Tone::Ok, String::new()));
    }
    if is_cut_name(&e.media.name) {
        tags.push(("剪輯版".into(), Tone::Warn, String::new()));
    }
    if !e.exports.is_empty() {
        let all_gif = e.exports.iter().all(|x| x.format == Some(ExportFormat::Gif));
        let list: Vec<String> = e.exports.iter().map(super::library_dialog::export_tag).collect();
        tags.push((format!("{} {}", if all_gif { "GIF" } else { "加速" }, e.exports.len()), Tone::Accent, format!("已製作 {}", list.join("、"))));
    }
    // 第一列：日期（或名稱）與標籤
    let top = Rect::from_min_max(pos2(x, rect.min.y + 5.0), pos2(rect.max.x - 6.0, rect.min.y + 27.0));
    {
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(top).layout(Layout::right_to_left(Align::Center)));
        ui.spacing_mut().item_spacing.x = 3.0;
        for (t, tone, tip) in tags.iter().rev() {
            let r = chip(ui, t, *tone, false);
            if !tip.is_empty() {
                r.on_hover_text(tip);
            }
        }
        let title = if is_default_name(&e.media.name) { date } else { base_name(&e.media.name) };
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(title).font(theme::font_bold(13.0))).truncate());
        });
    }
    // 下方：操作按鈕
    let row = Rect::from_min_max(pos2(x - 6.0, rect.max.y - 32.0), pos2(rect.max.x - 4.0, rect.max.y - 4.0));
    {
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing.x = 2.0;
        let acts = [
            (EntryAction::Play, Icon::Play, "播放"),
            (EntryAction::Reveal, Icon::Folder, "在資料夾中顯示"),
            (EntryAction::Edit, Icon::Cut, "剪輯"),
            (EntryAction::Export, Icon::Export, "製作加速版 / GIF"),
        ];
        for (act, icon, tip) in acts {
            if Btn::icon_only(icon).ghost().small().tooltip(tip).show(ui).clicked() {
                app.known.insert(e.media.path.clone(), e.clone());
                app.act(act, e.clone());
            }
        }
    }
    resp.on_hover_text(&e.media.name);
}

// ───────────── 倒數 ─────────────

/// 操作視窗在錄影範圍內：全畫面倒數（接著縮小）；在別的螢幕：不擋畫面，只在取消按鈕上倒數
pub fn countdown_overlay(app: &mut UiApp, ctx: &egui::Context) {
    let r = &app.status.recorder;
    if r.state != RecorderState::Countdown || r.countdown_covers_ui == Some(false) {
        return;
    }
    let n = countdown_left(app);
    let hint = format!(
        "即將開始錄影{}{}",
        if app.s.hide_ui { "，這個視窗若在錄影範圍內會自動縮小" } else { "" },
        if app.env.hotkeys.is_some_and(|h| h.record) { format!("，或按 {HOTKEY_RECORD_LABEL} 取消") } else { String::new() }
    );
    egui::Area::new(Id::new("countdown")).fixed_pos(pos2(0.0, 0.0)).order(egui::Order::Foreground).show(ctx, |ui| {
        let screen = ctx.content_rect();
        ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(170));
        ui.allocate_rect(screen, Sense::click());
        ui.scope_builder(UiBuilder::new().max_rect(screen).layout(Layout::top_down(Align::Center)), |ui| {
            ui.add_space(screen.height() * 0.3);
            ui.label(RichText::new(n.to_string()).font(theme::font_bold(140.0)).color(Color32::WHITE));
            ui.label(RichText::new(hint).color(Color32::from_white_alpha(220)));
            ui.add_space(16.0);
            if Btn::new("取消").show(ui).clicked() {
                let core = app.core.clone();
                app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
            }
        });
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
}
