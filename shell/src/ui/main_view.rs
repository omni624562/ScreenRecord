//! 主畫面：
//! - 左：擷取範圍（單一螢幕 / 所有螢幕 / 自訂範圍）與預覽、下方一列錄影設定
//! - 右：錄影狀態、計時、控制按鈕、轉檔工作、事件紀錄、系統狀態
//! - 下：最近錄影（橫向分頁）

use super::dialogs::{base_name, date_labels, file_name, is_cut_name, is_default_name, Ask};
use super::settings::{FollowWindow, SourceType};
use super::settings_dialog::{self, Page};
use super::theme::{self, chip, segmented, switch, Btn, Icon, Tone};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::format::{clock, format_bytes, human_duration, output_size, speed_label, video_clock};
use screenrecorder_core::i18n::is_en;
use screenrecorder_core::types::{DownloadPhase, EncoderPreference, ExportFormat, ExportKind, ExportState, LibraryEntry, LogLevel, MethodPreference, RecorderState, Rect as DRect};
use screenrecorder_core::{tr, trf};
use std::time::Instant;

/// 主畫面自己的狀態（拖曳範圍、輸入中的文字）
#[derive(Default)]
pub struct MainState {
    drag: Option<RegionDrag>,
    region_text: [String; 4],
    region_focus: Option<usize>,
    /// 儲存位置改了的時間（停止輸入一下子後重新讀取最近錄影）
    pub dir_changed_at: Option<Instant>,
    /// 截圖中（按鈕停用，避免連按）
    pub shooting: bool,
    /// 「選擇視窗」的清單（按下時更新）
    windows: Vec<screenrecorder_core::winui::WindowInfo>,
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
    update_meter(app, ui.ctx());
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
        // 右欄：視窗窄時跟著變窄，左邊的預覽與設定列才放得下
        let rec_w = (inner.width() * 0.34).clamp(312.0, 360.0);
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
                    ui.label(RichText::new(tr!("找不到 ffmpeg.exe", "ffmpeg.exe not found")).font(theme::font_bold(14.0)).color(p.rec));
                    let text = match d.phase {
                        DownloadPhase::Downloading => {
                            let pct = d.total.map(|t| d.received as f64 / t.max(1) as f64 * 100.0);
                            let mut s = trf!("下載中 {}", "Downloading {}", format_bytes(d.received));
                            if let (Some(t), Some(pc)) = (d.total, pct) {
                                s += &trf!(" / {}（{}%）", " / {} ({}%)", format_bytes(t), pc.floor());
                            }
                            if let Some(sp) = d.speed {
                                s += &trf!("・{}/秒", " · {}/s", format_bytes(sp as u64));
                                if let Some(t) = d.total {
                                    s += &trf!("・剩約 {}", " · about {} left", human_duration((t.saturating_sub(d.received)) as f64 / sp.max(1.0)));
                                }
                            }
                            s
                        }
                        DownloadPhase::Verifying => tr!("比對 SHA-256 檢查碼…", "Verifying SHA-256 checksum…").into(),
                        DownloadPhase::Extracting => tr!("解壓縮並確認 ffmpeg.exe 可以執行…", "Extracting and checking that ffmpeg.exe runs…").into(),
                        DownloadPhase::Error | DownloadPhase::Canceled => d.message.clone().unwrap_or_else(|| tr!("下載失敗", "Download failed").into()),
                        _ => trf!(
                            "可以自動下載（gyan.dev 的 FFmpeg essentials，約 110 MB；下載後比對程式內建的 SHA-256，確認檔案完整且未遭竄改），或自行下載並把 bin\\ffmpeg.exe 放到 {}",
                            "Download it automatically (FFmpeg essentials from gyan.dev, about 110 MB; the download is checked against a built-in SHA-256 to make sure it is complete and untampered), or download it yourself and put bin\\ffmpeg.exe in {}",
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
                    if Btn::new(tr!("重新偵測", "Check again")).small().show(ui).clicked() {
                        app.refresh_env(|app| {
                            let (msg, err) = if app.env.ffmpeg.found {
                                (trf!("已找到 FFmpeg {}", "Found FFmpeg {}", app.env.ffmpeg.version.clone().unwrap_or_default()), false)
                            } else {
                                (tr!("仍然找不到 ffmpeg.exe", "ffmpeg.exe still not found").into(), true)
                            };
                            app.toast(msg, err);
                        });
                    }
                    if Btn::new(tr!("手動下載", "Manual download")).small().show(ui).clicked() {
                        let _ = actions::open_url("https://www.gyan.dev/ffmpeg/builds/");
                    }
                    if busy {
                        if Btn::new(tr!("取消", "Cancel")).small().show(ui).clicked() {
                            app.core.downloader.cancel();
                        }
                    } else {
                        let label = if matches!(d.phase, DownloadPhase::Error | DownloadPhase::Canceled) { tr!("重新下載", "Download again") } else { tr!("自動下載", "Auto download") };
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
            // 窄的時候「即時預覽」只留開關（滑鼠提示有說明），讓範圍的按鈕、座標放得下
            let narrow = ui.available_width() < 720.0;
            let mut st = app.s.source_type;
            // 窄的時候分頁名稱縮短，範圍的座標才放得下
            let tabs = if narrow {
                tr!(
                    [(SourceType::Monitor, "單螢幕"), (SourceType::All, "多螢幕"), (SourceType::Region, "自訂"), (SourceType::Audio, "錄音")],
                    [(SourceType::Monitor, "Screen"), (SourceType::All, "All"), (SourceType::Region, "Area"), (SourceType::Audio, "Audio")]
                )
            } else {
                tr!(
                    [(SourceType::Monitor, "單一螢幕"), (SourceType::All, "所有螢幕"), (SourceType::Region, "自訂範圍"), (SourceType::Audio, "只錄聲音")],
                    [(SourceType::Monitor, "Single screen"), (SourceType::All, "All screens"), (SourceType::Region, "Custom area"), (SourceType::Audio, "Audio only")]
                )
            };
            if segmented(ui, &mut st, &tabs, !locked) {
                app.s.source_type = st;
                app.save_settings();
            }
            // 右邊留給「即時預覽」與重新整理
            let room = ui.available_width() - if narrow { 100.0 } else { 160.0 };
            ui.scope(|ui| {
                ui.set_max_width(room.max(0.0));
                source_detail(app, ui, room);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Refresh).ghost().small().tooltip(tr!("重新擷取預覽並更新螢幕清單", "Refresh the preview and the screen list")).show(ui).clicked() {
                    app.preview.retry_live();
                    app.refresh_env(|_| {});
                }
                let mut live = app.s.live_preview;
                let audio = app.s.source_type == SourceType::Audio;
                if !audio
                    && switch(ui, &mut live, if narrow { "" } else { tr!("即時預覽", "Live preview") }, true)
                        .on_hover_text(tr!(
                            "即時預覽：即時顯示目前畫面（每秒 5 張；切到其他視窗或錄影中 2 張）",
                            "Live preview: shows the screen as it is now (5 frames per second; 2 while recording or when another window is active)"
                        ))
                        .changed()
                {
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

/// 範圍分頁旁的細節：螢幕按鈕 / 說明 / 座標輸入。room：可用的寬度（放不下時縮短文字、省略說明）
fn source_detail(app: &mut UiApp, ui: &mut Ui, room: f32) {
    let gap = ui.spacing().item_spacing.x;
    let text_w = |ui: &Ui, t: &str| ui.painter().layout_no_wrap(t.to_string(), theme::font(13.0), Color32::WHITE).size().x;
    let p = theme::pal(ui);
    let locked = app.locked();
    match app.s.source_type {
        SourceType::Monitor => {
            if app.env.monitors.is_empty() {
                ui.add(egui::Label::new(theme::muted(ui, tr!("找不到螢幕資訊，請改用「自訂範圍」", "No screen info found; use “Custom area” instead"))).truncate());
            }
            let monitors = app.env.monitors.clone();
            let full =
                |m: &screenrecorder_core::types::MonitorInfo| trf!("螢幕 {}  {}×{}{}", "Screen {}  {}×{}{}", m.display_number, m.width, m.height, if m.primary { tr!("・主", " · main") } else { "" });
            let boxed = app.s.region_in_monitor(&app.env).is_some();
            let extra = if boxed { Btn::new(tr!("整個螢幕", "Whole screen")).icon(Icon::Close).small().width(ui) } else { 0.0 };
            let buttons_w = |short: bool| -> f32 { monitors.iter().map(|m| Btn::new(if short { trf!("螢幕 {}", "Screen {}", m.display_number) } else { full(m) }).small().width(ui) + gap).sum() };
            // 放不下完整的「螢幕 1  1920×1080・主」時只寫「螢幕 1」（滑鼠提示有完整資訊）
            let short = buttons_w(false) + extra > room;
            let hint = tr!("可在預覽上拖曳框選範圍", "Drag on the preview to select an area");
            let show_hint = !boxed && !monitors.is_empty() && buttons_w(short) + text_w(ui, hint) + gap <= room;
            for m in monitors {
                let on = app.s.monitor_id.as_deref() == Some(&m.id);
                let label = if short { trf!("螢幕 {}", "Screen {}", m.display_number) } else { full(&m) };
                let tip = trf!("{}（{}）", "{} ({})", full(&m), m.adapter_name);
                if Btn::new(label).small().selected(on).enabled(!locked).tooltip(tip).show(ui).clicked() && !on {
                    app.s.monitor_id = Some(m.id.clone());
                    app.s.monitor_region = None;
                    app.save_settings();
                }
            }
            // 只錄螢幕的一部分：在預覽上拖曳框選
            if boxed {
                if Btn::new(tr!("整個螢幕", "Whole screen"))
                    .icon(Icon::Close)
                    .small()
                    .enabled(!locked)
                    .tooltip(tr!("取消框選的範圍，錄整個螢幕", "Clear the selected area and record the whole screen"))
                    .show(ui)
                    .clicked()
                {
                    app.s.monitor_region = None;
                    app.save_settings();
                }
            } else if show_hint {
                ui.label(theme::muted(ui, hint)).on_hover_text(tr!(
                    "只錄這個螢幕的一部分：在預覽圖上拖曳框選；拖曳紅框可移動，拉邊或角可調整大小",
                    "To record only part of this screen, drag on the preview to select an area. Drag the red frame to move it; drag its edges or corners to resize it."
                ));
            }
        }
        SourceType::All => {
            let n = app.env.monitors.len();
            let adapters: std::collections::HashSet<u32> = app.env.monitors.iter().map(|m| m.adapter).collect();
            let multi = adapters.len() > 1;
            let text = if n <= 1 {
                tr!("目前只有 1 個螢幕，與「單一螢幕」相同", "Only 1 screen connected; same as “Single screen”").to_string()
            } else {
                trf!(
                    "{n} 個螢幕拼成 {}×{}{}",
                    "{n} screens combined: {}×{}{}",
                    app.env.desktop.width,
                    app.env.desktop.height,
                    if multi { tr!("（不同顯示卡，將用 gdigrab）", " (different graphics cards; gdigrab will be used)") } else { "" }
                )
            };
            ui.add(egui::Label::new(RichText::new(&text).color(if multi { p.warn } else { p.muted }).font(theme::font(12.5))).truncate()).on_hover_text(&text);
        }
        SourceType::Audio => {
            let text = if app.s.audio_system || app.s.audio_mic {
                tr!("畫面不錄，只錄聲音", "Records audio only, no video")
            } else {
                tr!("請先打開「系統聲音」或「麥克風」", "Turn on “System audio” or “Microphone” first")
            };
            let warn = !(app.s.audio_system || app.s.audio_mic);
            ui.add(egui::Label::new(RichText::new(text).color(if warn { p.warn } else { p.muted }).font(theme::font(12.5))).truncate());
        }
        SourceType::Region => {
            let r = app.s.region;
            let vals = [r.x, r.y, r.width, r.height];
            for (i, label) in tr!(["X", "Y", "寬", "高"], ["X", "Y", "W", "H"]).iter().enumerate() {
                ui.label(theme::muted(ui, *label));
                if app.main.region_focus != Some(i) {
                    app.main.region_text[i] = vals[i].to_string();
                }
                let resp = ui.add_enabled(!locked, egui::TextEdit::singleline(&mut app.main.region_text[i]).desired_width(if room < 330.0 { 40.0 } else { 52.0 }));
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
                        app.s.follow_window = None;
                        app.save_settings();
                    }
                }
            }
            window_picker(app, ui, locked);
            // 放得下才顯示說明
            let hint = tr!("拖曳框選或移動紅框（可跨螢幕）", "Drag to select, or move the red frame (across screens)");
            if app.s.follow_window.is_none() && ui.available_width() >= text_w(ui, hint) {
                ui.label(theme::muted(ui, hint)).on_hover_text(tr!(
                    "在預覽圖上拖曳框選範圍（可跨螢幕）；拖曳紅框可移動，拉邊或角可調整大小",
                    "Drag on the preview to select an area (it can span screens). Drag the red frame to move it; drag its edges or corners to resize it."
                ));
            }
        }
    }
}

/// 「視窗」：選一個視窗，範圍設成它的位置，錄影時跟著它移動；已選時顯示視窗名稱，✕ 取消
fn window_picker(app: &mut UiApp, ui: &mut Ui, locked: bool) {
    let p = theme::pal(ui);
    if let Some(w) = app.s.follow_window.clone() {
        let text = trf!("只錄「{}」", "Only “{}”", w.title);
        let resp = ui.add(egui::Label::new(RichText::new(&text).color(p.accent).font(theme::font(12.5))).truncate()).on_hover_text(tr!(
            "錄影時範圍跟著這個視窗移動（視窗大小改變時維持原本的大小）",
            "While recording, the area follows this window (if the window is resized, the area keeps its original size)"
        ));
        let _ = resp;
        if !locked && Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("不跟著視窗，改回一般的自訂範圍", "Stop following the window and go back to a normal custom area")).show(ui).clicked()
        {
            app.s.follow_window = None;
        }
        return;
    }
    let tip = tr!("只錄某個視窗：範圍設成那個視窗的位置，錄影時跟著它移動", "Record one window only: the area is set to that window and follows it while recording");
    // 放不下文字時只顯示圖示
    let full = Btn::new(tr!("選擇視窗", "Choose window")).small().width(ui);
    let b = if ui.available_width() >= full { Btn::new(tr!("選擇視窗", "Choose window")).small() } else { Btn::icon_only(Icon::Window).ghost().small() }.enabled(!locked).tooltip(tip).show(ui);
    if b.clicked() {
        app.main.windows = screenrecorder_core::winui::app_windows();
    }
    egui::Popup::menu(&b).show(|ui| {
        ui.set_min_width(260.0);
        ui.set_max_width(420.0);
        if app.main.windows.is_empty() {
            ui.label(theme::muted(ui, if cfg!(windows) { tr!("找不到可以錄的視窗", "No windows to record") } else { tr!("只支援 Windows", "Windows only") }));
        }
        let mut chosen = None;
        for w in &app.main.windows {
            let label = format!("{}　{}×{}", w.title, w.rect.width, w.rect.height);
            if ui.add(egui::Button::new(egui::RichText::new(label)).truncate()).clicked() {
                chosen = Some(w.clone());
            }
        }
        if let Some(w) = chosen {
            // 寬高取偶數（編碼需要）
            let r = DRect { x: w.rect.x, y: w.rect.y, width: w.rect.width & !1, height: w.rect.height & !1 };
            app.s.region = r;
            app.s.follow_window = Some(FollowWindow { id: w.id, title: w.title.clone() });
            app.save_settings();
        }
    });
}

/// 預覽圖、螢幕框、自訂範圍的紅框
fn desk(app: &mut UiApp, ui: &mut Ui, area: Rect) {
    let p = theme::pal(ui);
    if app.s.source_type == SourceType::Audio {
        if app.preview.running() {
            app.preview.stop(&app.core);
        }
        return audio_panel(app, ui, area);
    }
    let view = view_rect(app);
    // 依範圍的比例放進可用的空間
    let ar = if view.width > 0 && view.height > 0 { view.width as f32 / view.height as f32 } else { 16.0 / 9.0 };
    let mut w = area.width();
    let mut h = w / ar;
    if h > area.height() {
        h = area.height();
        w = h * ar;
    }
    let rect = Rect::from_min_size(pos2(area.center().x - w / 2.0, area.min.y), vec2(w, h));
    // 即時預覽：視窗看得到時才擷取；切到其他視窗或錄影中放慢；大小跟著預覽實際顯示的像素
    let key = if app.s.source_type == SourceType::Monitor { app.s.monitor_id.clone().unwrap_or_default() } else { String::new() };
    let focused = ui.ctx().input(|i| i.viewport().focused) != Some(false);
    let fps = match (app.s.live_preview, app.locked() || !focused) {
        (false, _) => 0,
        (true, true) => 2,
        (true, false) => 5,
    };
    let width = super::preview::live_width(w, ui.ctx().pixels_per_point());
    // 開著剪輯、製作、全部錄影等視窗時暫停即時預覽（被蓋住看不到，不浪費 CPU / GPU），保留最後一張畫面
    let covered = app.editor.is_some() || app.export_dlg.is_some() || app.library.is_some() || app.viewer.is_some();
    if covered {
        if app.preview.running() {
            app.preview.stop(&app.core);
        }
    } else if app.env_ready {
        let (core, rt, ctx) = (app.core.clone(), app.rt.clone(), ui.ctx().clone());
        app.preview.ensure(&core, &rt, &ctx, &key, fps, width, app.env.ffmpeg.found);
    }
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
                super::preview::PreviewState::Loading { live: true } => tr!("正在開啟即時預覽…", "Starting live preview…").into(),
                super::preview::PreviewState::Loading { live: false } => tr!("正在擷取預覽…", "Capturing preview…").into(),
                _ => {
                    if app.env_ready {
                        tr!("尚無預覽", "No preview yet").into()
                    } else {
                        tr!("正在偵測螢幕與 FFmpeg…", "Detecting screens and FFmpeg…").into()
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
        let label = trf!("螢幕 {}  {} × {}", "Screen {}  {} × {}", m.display_number, m.width, m.height);
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

/// 只錄聲音：預覽區改成說明（錄哪些聲音、存成什麼）
fn audio_panel(app: &mut UiApp, ui: &mut Ui, area: Rect) {
    let p = theme::pal(ui);
    ui.painter().rect_filled(area, CornerRadius::same(theme::RADIUS_SM), p.surface2);
    let c = area.center();
    let s = (area.height() / 260.0).clamp(0.6, 1.2);
    theme::paint_icon(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0 * s), vec2(64.0 * s, 64.0 * s)), Icon::Mic, p.accent);
    ui.painter().text(c - vec2(0.0, 12.0 * s), egui::Align2::CENTER_CENTER, tr!("只錄聲音", "Audio only"), theme::font_bold(20.0 * s), p.text);
    let parts: Vec<&str> = [app.s.audio_system.then_some(tr!("系統聲音（電腦播放的聲音）", "System audio (sound played by the computer)")), app.s.audio_mic.then_some(tr!("麥克風", "Microphone"))]
        .into_iter()
        .flatten()
        .collect();
    let (line, warn) = if parts.is_empty() {
        (tr!("還沒選要錄的聲音：請在下方「聲音」打開系統聲音或麥克風", "No audio selected: turn on system audio or the microphone under “Audio” below").to_string(), true)
    } else {
        (trf!("會錄：{}", "Will record: {}", parts.join(tr!("、", ", "))), false)
    };
    ui.painter().text(c + vec2(0.0, 20.0 * s), egui::Align2::CENTER_CENTER, line, theme::font(13.5), if warn { p.warn } else { p.text });
    let tips = tr!(
        ["畫面不會錄下來，存成 MP4（畫面是一張「只錄聲音」的卡片，檔案幾乎只有聲音的大小）", "一樣可以暫停、加標記、剪輯、降噪；在錄影的「更多」選單選「存成 M4A」就只留下聲音"],
        [
            "No video is recorded. Saved as MP4 with an “Audio only” card, so the file stays small",
            "You can still pause, add markers, edit and reduce noise. “More” → “Save as M4A” keeps just the audio"
        ]
    );
    for (i, t) in tips.iter().enumerate() {
        ui.painter().text(c + vec2(0.0, (48.0 + i as f32 * 22.0) * s), egui::Align2::CENTER_CENTER, *t, theme::font(12.0), p.muted);
    }
    ui.allocate_rect(area, Sense::hover());
}

/// 目前的框選範圍：自訂範圍，或單一螢幕裡框選的部分（沒有時是整個螢幕）
fn region_target(app: &UiApp) -> Option<DRect> {
    match app.s.source_type {
        SourceType::Region => Some(app.s.region),
        SourceType::Monitor => app.s.region_in_monitor(&app.env),
        SourceType::All | SourceType::Audio => None,
    }
}

fn set_region(app: &mut UiApp, r: DRect) {
    match app.s.source_type {
        SourceType::Region => {
            if app.s.region != r {
                app.s.follow_window = None;
            }
            app.s.region = r
        }
        SourceType::Monitor => app.s.monitor_region = Some(r),
        SourceType::All | SourceType::Audio => {}
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
        let tol = 8.0;
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
        // 從按下的位置判斷（開始拖曳時游標已經移動了幾點；用移動後的位置會離開控制點，變成重新框選，範圍先變小）
        if let Some(pos) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos()) {
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

// ───────────── 錄影設定摘要（下方一列；點一下開啟「設定」視窗的那一頁） ─────────────

fn settings_bar(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let locked = app.locked();
    ui.spacing_mut().item_spacing.x = 2.0;
    // 右邊：「設定」按鈕，前面是輸出大小
    let set_btn =
        Btn::new(tr!("設定", "Settings")).icon(Icon::Settings).small().tooltip(tr!("錄影、聲音、儲存位置、快速鍵與其他設定", "Recording, audio, save location, shortcuts and other settings"));
    let set_w = set_btn.width(ui) + super::presets::width(app, ui) + 4.0;
    let out = app.s.source_rect(&app.env).map(|r| output_size(r.width, r.height, app.s.scale as f64));
    let big = out.is_some_and(|(w, h)| (w as f64) * (h as f64) > 3840.0 * 2160.0 * 1.05);
    let out_text = out.map(|(w, h)| trf!("輸出 {w}×{h}{}", "Output {w}×{h}{}", if big { tr!("（很大，建議 50%）", " (very large; 50% recommended)") } else { "" }));
    let out_w = out_text.as_ref().map(|t| ui.painter().layout_no_wrap(t.clone(), theme::font(12.5), p.muted).size().x + 16.0).unwrap_or(0.0);
    // 左邊：錄影、聲音、儲存位置的摘要，依剩下的寬度取捨
    let room = ui.available_width() - set_w - 8.0;
    let show_out = room - out_w > 360.0;
    let mut room = if show_out { room - out_w } else { room };
    let rec = settings_dialog::record_summary(app);
    let (audio, audio_icon) = settings_dialog::audio_summary(app);
    let item = |text: &str, icon: Icon| Btn::new(text).icon(icon).ghost().small();
    let measure = |ui: &Ui, text: &str, icon: Icon| item(text, icon).width(ui) + 2.0;
    let mut open: Option<Page> = None;
    // 錄影：太窄時只寫 FPS
    let rec_full = rec.join(tr!("・", " · "));
    let rec_text = if measure(ui, &rec_full, Icon::Camera) + 200.0 <= room { rec_full.clone() } else { rec[0].clone() };
    room -= measure(ui, &rec_text, Icon::Camera);
    if item(&rec_text, Icon::Camera).enabled(!locked).tooltip(trf!("錄影：{rec_full}", "Recording: {rec_full}")).show(ui).clicked() {
        open = Some(Page::Record);
    }
    // 聲音：太窄時只剩圖示
    let audio_text = if measure(ui, &audio, audio_icon) + 120.0 <= room { audio.clone() } else { String::new() };
    room -= measure(ui, &audio_text, audio_icon);
    if item(&audio_text, audio_icon).enabled(!locked).tooltip(trf!("錄製聲音：{audio}", "Audio: {audio}")).show(ui).clicked() {
        open = Some(Page::Audio);
    }
    // 儲存位置：縮短路徑（保留結尾）
    let dir = app.s.out_dir(&app.env);
    let fits = |t: &str| measure(ui, t, Icon::Folder) <= room;
    let short: String = if fits(&dir) {
        dir.clone()
    } else {
        let chars: Vec<char> = dir.chars().collect();
        (1..chars.len()).map(|i| format!("…{}", chars[i..].iter().collect::<String>())).find(|t| fits(t)).unwrap_or_default()
    };
    if item(&short, Icon::Folder).enabled(!locked).tooltip(trf!("儲存位置：{dir}", "Save location: {dir}")).show(ui).clicked() {
        open = Some(Page::Save);
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if set_btn.show(ui).clicked() {
            open = Some(Page::Record);
        }
        super::presets::button(app, ui);
        if let Some(t) = out_text.filter(|_| show_out) {
            ui.add_space(8.0);
            ui.label(RichText::new(t).font(theme::font(12.5)).color(if big { p.warn } else { p.muted }));
        }
    });
    if let Some(page) = open {
        settings_dialog::open(app, page);
    }
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
                RecorderState::Idle => (tr!("待命", "Ready"), Tone::Plain),
                RecorderState::Countdown => (tr!("倒數中", "Countdown"), Tone::Warn),
                RecorderState::Recording => (tr!("● 錄影中", "● Recording"), Tone::Bad),
                RecorderState::Paused => (tr!("已暫停", "Paused"), Tone::Warn),
                RecorderState::Stopping => (tr!("處理中", "Processing"), Tone::Accent),
            };
            chip(ui, text, tone, false);
            let mut busy = r.busy.clone().unwrap_or_default();
            if busy.is_empty() && r.max_ms > 0 && active {
                busy = trf!("剩餘 {}", "{} left", clock((r.max_ms as f64 - live_recorded_ms(app)).max(0.0)));
            }
            // 右上角：講稿小視窗
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let open = app.notes.is_some();
                let tip = tr!(
                    "講稿小視窗：浮在最上層、不會被錄進影片；可以用大字自動往上捲（提詞），開始錄影時自動開始",
                    "Script window: stays on top and isn't recorded. It can show your script in large text that scrolls up automatically (teleprompter), starting when you record."
                );
                if Btn::new(if open { tr!("關閉講稿", "Close script") } else { tr!("講稿", "Script") }).icon(Icon::Edit).ghost().small().tooltip(tip).show(ui).clicked() {
                    super::notes::toggle(app);
                }
                if !active
                    && Btn::new(tr!("排程", "Schedule"))
                        .icon(Icon::Clock)
                        .ghost()
                        .small()
                        .tooltip(tr!("指定時間自動開始錄影，也可以設定錄幾分鐘後自動停止", "Start recording automatically at a set time, optionally stopping after a set length"))
                        .show(ui)
                        .clicked()
                {
                    super::schedule::open(app);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(theme::muted(ui, busy)).truncate());
                });
            });
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
                trf!(
                    "{}{}・{}",
                    "{}{} · {}",
                    r.method.map(method_name).unwrap_or("—"),
                    r.tiles.filter(|t| *t > 1).map(|t| format!(" ×{t}")).unwrap_or_default(),
                    r.encoder.as_deref().map(encoder_name).unwrap_or_else(|| "—".into())
                ),
                r.audio.clone().unwrap_or_else(|| tr!("不錄聲音", "No audio").into()),
            )
        } else {
            let o = app.s.source_rect(&app.env).map(|s| output_size(s.width, s.height, app.s.scale as f64));
            let enc = match app.s.encoder {
                EncoderPreference::Gpu => app.env.ffmpeg.hw_encoders.as_ref().and_then(|h| h.first().map(|e| encoder_name(e))).unwrap_or_else(|| tr!("顯示卡編碼", "GPU encoding").into()),
                EncoderPreference::Cpu => tr!("CPU 編碼", "CPU encoding").into(),
                EncoderPreference::Auto => tr!("自動", "Auto").into(),
            };
            let m = match app.s.method {
                MethodPreference::Auto => tr!("自動", "Auto"),
                MethodPreference::Ddagrab => tr!("顯示卡擷取", "GPU capture"),
                MethodPreference::Gdigrab => tr!("相容模式", "Compatibility mode"),
            };
            let parts: Vec<&str> = [app.s.audio_system.then_some(tr!("系統聲音", "System audio")), app.s.audio_mic.then_some(tr!("麥克風", "Microphone"))].into_iter().flatten().collect();
            (
                if app.s.source_type == SourceType::Audio { tr!("只錄聲音", "Audio only").into() } else { o.map(|(w, h)| format!("{w}×{h} · {}fps", app.s.fps)).unwrap_or_else(|| "—".into()) },
                match (app.env.ffmpeg.encoder.is_some(), app.s.method == MethodPreference::Auto && app.s.encoder == EncoderPreference::Auto) {
                    (false, _) => "—".into(),
                    (true, true) => tr!("自動（依電腦選最順的方式）", "Auto (best for this PC)").into(),
                    (true, false) => trf!("{m}・{enc}", "{m} · {enc}"),
                },
                if parts.is_empty() { tr!("不錄聲音", "No audio").into() } else { parts.join(" + ") },
            )
        };
        // 名稱一欄固定寬度；值太長（例如音訊裝置名稱）時截斷，不撐寬面板（滑鼠移上去看完整內容）
        let gap_y = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 6.0;
        let keys = tr!(["影片長度", "檔案大小", "輸出", "錄影方式", "聲音"], ["Duration", "File size", "Output", "Method", "Audio"]);
        let key_w = keys.iter().map(|k| ui.painter().layout_no_wrap(k.to_string(), theme::font(13.0), p.muted).size().x).fold(0.0, f32::max);
        let values = [video_clock(r.video_sec), if active || r.bytes > 0 { format_bytes(r.bytes) } else { "—".into() }, out, method, audio];
        for (i, (k, v)) in keys.into_iter().zip(values).enumerate() {
            // 最後一列是「聲音」
            let audio_row = i == 4;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let (cell, _) = ui.allocate_exact_size(vec2(key_w, 18.0), Sense::hover());
                ui.painter().text(cell.left_center(), egui::Align2::LEFT_CENTER, k, theme::font(13.0), p.muted);
                if audio_row && !active {
                    if let Some(l) = app.meter.as_ref().map(|m| m.levels()) {
                        // 音量表：每個來源一條，旁邊寫名稱
                        let meters: Vec<(&str, f32)> = [(tr!("系統", "System"), l.system), (tr!("麥克風", "Mic"), l.mic)].into_iter().filter_map(|(n, v)| v.map(|v| (n, v))).collect();
                        let resp = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (n, v) in &meters {
                                ui.label(RichText::new(*n).font(theme::font(12.5)));
                                level_bar(ui, *v);
                            }
                            if l.error.is_some() {
                                ui.label(RichText::new("⚠").color(p.warn));
                            }
                        });
                        let tip = match &l.error {
                            Some(e) => trf!("打不開：{e}", "Couldn't open: {e}"),
                            None => tr!("錄影前先講幾句話：麥克風的條會跳動，就表示收得到聲音", "Say a few words before recording: if the microphone bar moves, your voice is being picked up").into(),
                        };
                        resp.response.on_hover_text(tip);
                        return;
                    }
                    if app.s.audio_system || app.s.audio_mic {
                        // 右邊留給「測試音量」
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if Btn::new(tr!("測試音量", "Test levels"))
                                .ghost()
                                .small()
                                .tooltip(tr!(
                                    "量 20 秒的音量：講幾句話看麥克風的條有沒有跳動，確定錄得到聲音",
                                    "Shows levels for 20 seconds: say a few words and check that the microphone bar moves, so you know audio is being recorded"
                                ))
                                .show(ui)
                                .clicked()
                            {
                                app.meter_until = Some(Instant::now() + std::time::Duration::from_secs(20));
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(&v).font(theme::font(13.0))).truncate()).on_hover_text(&v);
                            });
                        });
                        return;
                    }
                }
                ui.add(egui::Label::new(RichText::new(&v).font(theme::font(13.0))).truncate()).on_hover_text(&v);
            });
        }
        ui.spacing_mut().item_spacing.y = gap_y;
        ui.add_space(10.0);
        // 控制按鈕
        ui.horizontal(|ui| {
            if !active {
                let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && !app.exporting();
                let tip = if app.exporting() { tr!("轉檔進行中，完成後才能錄影", "A conversion is in progress; you can record when it finishes") } else { "" };
                // 截圖按鈕（含小箭頭）的寬度：至少 118，英文字較長時跟著加寬
                let shot_w = (Btn::new(shot_label()).icon(Icon::Camera).width(ui) + 28.0).max(118.0);
                if Btn::new(tr!("開始錄影", "Start recording"))
                    .kind(theme::Kind::Record)
                    .enabled(can)
                    .tooltip(tip)
                    .min_width(ui.available_width() - shot_w - ui.spacing().item_spacing.x)
                    .show(ui)
                    .clicked()
                {
                    let config = app.s.record_config(&app.env);
                    let core = app.core.clone();
                    app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
                }
                shot_button(app, ui, shot_w);
            } else {
                let w = ui.available_width();
                let marking = matches!(r.state, RecorderState::Recording | RecorderState::Paused);
                let pw = if marking { w * 0.34 } else { w * 0.45 };
                if r.state == RecorderState::Recording && Btn::new(tr!("暫停", "Pause")).icon(Icon::Pause).enabled(r.busy.is_none()).min_width(pw).show(ui).clicked() {
                    let core = app.core.clone();
                    app.guarded(async move { core.recorder.pause().await }, |_, _| {});
                }
                if r.state == RecorderState::Paused && Btn::new(tr!("繼續", "Resume")).icon(Icon::Play).enabled(r.busy.is_none()).min_width(pw).show(ui).clicked() {
                    let core = app.core.clone();
                    app.guarded(async move { core.recorder.resume().await }, |_, _| {});
                }
                if marking {
                    let mk = if app.keys.label(4).is_empty() { String::new() } else { trf!("（{}）", " ({})", app.keys.label(4)) };
                    let label = if r.markers > 0 { trf!("標記 {}", "Mark ({})", r.markers) } else { tr!("標記", "Mark").into() };
                    if Btn::new(label)
                        .min_width(w * 0.26)
                        .tooltip(trf!("記下現在的位置，剪輯時可以直接跳過去{mk}", "Mark the current position so you can jump straight to it when editing{mk}"))
                        .show(ui)
                        .clicked()
                    {
                        if let Err(e) = app.core.recorder.add_marker() {
                            app.toast(e.message().to_string(), true);
                        }
                    }
                }
                let stop_label = if r.state == RecorderState::Countdown {
                    if r.countdown_covers_ui == Some(false) {
                        trf!("{} 秒後開始・取消", "Starts in {} s · Cancel", countdown_left(app))
                    } else {
                        tr!("取消倒數", "Cancel countdown").into()
                    }
                } else {
                    tr!("停止", "Stop").into()
                };
                if Btn::new(stop_label).icon(Icon::Stop).danger().enabled(r.state != RecorderState::Stopping).min_width(ui.available_width()).show(ui).clicked() {
                    let core = app.core.clone();
                    app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
                }
            }
        });
        // 排程錄影：什麼時候開始、還有多久、取消
        if !active {
            super::schedule::strip(app, ui);
        }
        // 步驟截圖進行中：顯示幾步，可以完成
        if let Some(n) = app.status.steps {
            ui.add_space(6.0);
            egui::Frame::new().fill(p.accent.gamma_multiply(0.1)).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let text = if is_en() { format!("Step capture · {n} step{} captured", if n == 1 { "" } else { "s" }) } else { format!("步驟截圖中・已截 {n} 步") };
                    ui.label(RichText::new(text).color(p.accent).font(theme::font_bold(13.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if Btn::new(tr!("完成", "Finish"))
                            .primary()
                            .small()
                            .tooltip(tr!("停止並做成教學文件（HTML），用瀏覽器開啟", "Stop and create the step-by-step guide (HTML), then open it in your browser"))
                            .show(ui)
                            .clicked()
                        {
                            let core = app.core.clone();
                            app.spawn(async move { core.steps_finish().await }, |app, r| match r {
                                Ok(Some(path)) => {
                                    app.toast(
                                        tr!(
                                            "教學文件做好了，已用瀏覽器開啟；文字可以直接修改，再列印成 PDF",
                                            "The step-by-step guide is ready and open in your browser. You can edit the text, then print it to PDF."
                                        ),
                                        false,
                                    );
                                    screenrecorder_core::desktop::open_with_explorer(&path, false);
                                }
                                Ok(None) => app.toast(tr!("沒有截到任何步驟", "No steps were captured"), false),
                                Err(e) => app.toast(e.message().to_string(), true),
                            });
                        }
                    });
                });
            });
        }
        // 提醒
        let mut msgs = vec![];
        if let Some(t) = &r.retrying {
            msgs.push(t.clone());
        }
        if r.slow {
            msgs.push(trf!(
                "電腦跟不上：實際約 {} fps（設定 {}），建議調低解析度或 FPS",
                "The computer can't keep up: about {} fps (set to {}). Try a lower resolution or FPS",
                r.actual_fps.map(|f| format!("{f:.1}")).unwrap_or_else(|| "?".into()),
                r.fps
            ));
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
                        let saved = trf!("錄影已儲存・{}・{}", "Recording saved · {} · {}", video_clock(res.video_sec), format_bytes(res.bytes.unwrap_or(0)));
                        ui.label(RichText::new(saved).font(theme::font_bold(13.5)).color(p.ok));
                        ui.label(RichText::new(file_name(path)).font(theme::mono(12.0))).on_hover_text(path);
                        action_buttons(app, ui, path, true, true);
                    }
                    _ => {
                        ui.label(RichText::new(tr!("錄影未完成", "Recording incomplete")).font(theme::font_bold(13.5)).color(p.rec));
                        ui.add(egui::Label::new(RichText::new(&res.message).font(theme::font(12.5))).wrap());
                    }
                }
            });
        }
        // 事件紀錄 + 系統狀態（固定在底部）
        let sys_h = BOTTOM_BAR_H;
        // 轉檔工作、結果佔掉空間時：放不下就不顯示事件紀錄，系統狀態不被蓋住
        let log_h = ui.available_height() - sys_h - 8.0 - 32.0;
        if log_h >= 40.0 && r.log.is_empty() && !active {
            // 還沒有事件：顯示快速鍵小抄
            ui.add_space(10.0);
            ui.label(RichText::new(tr!("快速鍵", "Shortcuts")).font(theme::font_bold(12.5)).color(p.muted));
            egui::ScrollArea::vertical().max_height(log_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                for (i, name) in screenrecorder_core::types::hotkey_names().iter().enumerate() {
                    let key = app.keys.label(i);
                    if key.is_empty() {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(*name).font(theme::font(12.5))).truncate());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(key).font(theme::mono(12.0)).color(p.muted));
                        });
                    });
                }
                ui.add_space(4.0);
                let tip = tr!(
                    "「截圖」旁的小箭頭還有框選、長截圖、步驟截圖；快速鍵可在「設定 → 快速鍵」更換。",
                    "The small arrow next to “Screenshot” offers select area, scrolling screenshot and step capture. Change shortcuts in “Settings → Shortcuts”."
                );
                ui.add(egui::Label::new(theme::muted(ui, tip).font(theme::font(12.0))).wrap());
            });
        } else if log_h >= 40.0 {
            ui.add_space(10.0);
            ui.label(RichText::new(tr!("事件紀錄", "Event log")).font(theme::font_bold(12.5)).color(p.muted));
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

/// 「截圖」按鈕的文字
fn shot_label() -> &'static str {
    tr!("截圖", "Screenshot")
}

/// 截圖：與錄影相同的範圍，存成 PNG 並複製到剪貼簿（完成後由狀態更新顯示提示、更新清單）
fn shot_button(app: &mut UiApp, ui: &mut Ui, w: f32) {
    let hotkey = if app.env.hotkeys.is_some_and(|h| h.shot) && !app.keys.label(2).is_empty() { trf!("（{}）", " ({})", app.keys.label(2)) } else { String::new() };
    let tip = trf!(
        "把目前的擷取範圍存成 PNG 並複製到剪貼簿{hotkey}\n旁邊的小箭頭（或按右鍵）：框選、延遲截圖、長截圖、步驟截圖、取色器、尺規",
        "Save the current capture area as PNG and copy it to the clipboard{hotkey}\nSmall arrow (or right-click): select area, delayed screenshot, scrolling screenshot, step capture, color picker, ruler"
    );
    let can = app.env.ffmpeg.found && !app.main.shooting;
    let arrow_w = 26.0;
    let (resp, arrow) = ui
        .scope(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let resp = Btn::new(shot_label()).icon(Icon::Camera).enabled(can).tooltip(tip).min_width(w - arrow_w - 2.0).height(40.0).show(ui);
            let arrow = Btn::icon_only(Icon::ChevD)
                .enabled(can)
                .tooltip(tr!(
                    "更多截圖方式：框選範圍或視窗、延遲截圖、長截圖、步驟截圖、取色器、尺規",
                    "More screenshot options: select an area or window, delayed screenshot, scrolling screenshot, step capture, color picker, ruler"
                ))
                .min_width(arrow_w)
                .height(40.0)
                .show(ui);
            (resp, arrow)
        })
        .inner;
    // 選單（小箭頭或按右鍵）：在螢幕上框選、等幾秒再框選（先打開要截的選單）、長截圖、步驟截圖
    let mut snip: Option<u64> = None;
    let mut long = false;
    let mut steps = false;
    let steps_on = app.status.steps.is_some();
    let mut qr = false;
    // 取色器（false）或尺規（true）
    let mut tool: Option<bool> = None;
    let menu = |ui: &mut Ui, snip: &mut Option<u64>, long: &mut bool, steps: &mut bool, qr: &mut bool, tool: &mut Option<bool>| {
        ui.set_min_width(190.0);
        if ui.button(tr!("框選範圍或視窗…", "Select area or window…")).clicked() {
            *snip = Some(0);
        }
        ui.separator();
        for sec in [3u64, 5, 10] {
            if ui.button(trf!("{sec} 秒後框選", "Select area in {sec} s")).clicked() {
                *snip = Some(sec);
            }
        }
        ui.separator();
        if ui
            .button(tr!("長截圖（捲動）…", "Scrolling screenshot…"))
            .on_hover_text(tr!(
                "框選要捲動的內容（例如網頁），自動往下捲並接成一張長圖；按 Esc 停止",
                "Select the content to scroll (e.g. a web page); it scrolls down automatically and stitches everything into one long image. Press Esc to stop."
            ))
            .clicked()
        {
            *long = true;
        }
        if ui
            .button(tr!("讀取 QR 碼…", "Read QR code…"))
            .on_hover_text(tr!("框選畫面上的 QR 碼，讀出內容（網址可以直接開啟）", "Select a QR code on the screen to read it (links can be opened directly)"))
            .clicked()
        {
            *qr = true;
        }
        if !steps_on
            && ui
                .button(tr!("步驟截圖（做成教學文件）", "Step capture (step-by-step guide)"))
                .on_hover_text(tr!(
                    "開始後每點一下滑鼠就截一張，標出點的位置；完成時做成一份圖文並茂的教學文件",
                    "Once started, every mouse click takes a screenshot with the click marked. When you finish, it becomes an illustrated step-by-step guide."
                ))
                .clicked()
        {
            *steps = true;
        }
        ui.separator();
        if ui
            .button(tr!("取色器…", "Color picker…"))
            .on_hover_text(tr!(
                "點一下畫面上的任何地方，色碼（例如 #1D4ED8）就複製到剪貼簿；有放大鏡可以對準",
                "Click anywhere on the screen to copy its color code (e.g. #1D4ED8) to the clipboard. A magnifier helps you aim."
            ))
            .clicked()
        {
            *tool = Some(false);
        }
        if ui
            .button(tr!("尺規（量距離）…", "Ruler (measure distance)…"))
            .on_hover_text(tr!("在畫面上拖曳，量出兩點之間有幾個像素與角度", "Drag on the screen to measure the distance in pixels and the angle between two points"))
            .clicked()
        {
            *tool = Some(true);
        }
    };
    egui::Popup::context_menu(&resp).show(|ui| menu(ui, &mut snip, &mut long, &mut steps, &mut qr, &mut tool));
    egui::Popup::menu(&arrow).show(|ui| menu(ui, &mut snip, &mut long, &mut steps, &mut qr, &mut tool));
    if let Some(ruler) = tool.filter(|_| can) {
        app.main.shooting = true;
        let config = app.s.record_config(&app.env);
        let core = app.core.clone();
        app.spawn(async move { core.screen_tool(&config, ruler).await }, |app, r| {
            app.main.shooting = false;
            match r {
                Ok(Some(hex)) => app.toast(trf!("已複製色碼 {hex}", "Copied color code {hex}"), false),
                Ok(None) => {}
                Err(e) => app.toast(e.message().to_string(), true),
            }
        });
    }
    if qr && can {
        app.main.shooting = true;
        let config = app.s.record_config(&app.env);
        let core = app.core.clone();
        app.spawn(async move { core.qr_snip(&config).await }, |app, r| {
            app.main.shooting = false;
            match r {
                Ok(Some(list)) => app.show_qr(list),
                Ok(None) => {}
                Err(e) => app.toast(e.message().to_string(), true),
            }
        });
    }
    if steps {
        let dir = app.s.record_config(&app.env).output_dir;
        match app.core.steps_start(&dir) {
            Ok(()) => {
                app.toast(tr!("步驟截圖開始：之後每點一下滑鼠就截一張，做完後按「完成」", "Step capture started: every mouse click now takes a screenshot. Click “Finish” when you're done."), false)
            }
            Err(e) => app.toast(e.message().to_string(), true),
        }
    }
    if long && can {
        app.main.shooting = true;
        let config = app.s.record_config(&app.env);
        let core = app.core.clone();
        app.spawn(async move { core.long_shot(&config).await }, |app, r| {
            app.main.shooting = false;
            if let Err(e) = r {
                app.toast(e.message().to_string(), true);
            }
        });
    }
    if let Some(sec) = snip.filter(|_| can) {
        app.main.shooting = true;
        let config = app.s.record_config(&app.env);
        let core = app.core.clone();
        if sec > 0 {
            app.toast(trf!("{sec} 秒後框選：現在先打開要截的選單或提示", "Selecting in {sec} s: open the menu or tooltip you want to capture now"), false);
        }
        app.spawn(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(sec)).await;
                core.snip_begin(&config).await
            },
            |app, r| {
                app.main.shooting = false;
                if let Err(e) = r {
                    app.toast(e.message().to_string(), true);
                }
            },
        );
    }
    if resp.clicked() {
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
        if Btn::new(tr!("播放", "Play")).icon(Icon::Play).small().show(ui).clicked() {
            app.act_path(EntryAction::Play, path.to_string());
        }
        if Btn::new(tr!("顯示", "Show in folder")).icon(Icon::Folder).small().show(ui).clicked() {
            app.act_path(EntryAction::Reveal, path.to_string());
        }
        if edit && Btn::new(tr!("剪輯", "Edit")).icon(Icon::Cut).small().show(ui).clicked() {
            app.act_path(EntryAction::Edit, path.to_string());
        }
        if export && Btn::new(tr!("製作加速版 / GIF", "Speed up / GIF")).icon(Icon::Export).small().show(ui).clicked() {
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
    let gif_speed = if e.speed > 1.0 { format!(" {}×", speed_label(e.speed)) } else { String::new() };
    let label = match e.kind {
        ExportKind::Cut => tr!("剪輯", "edited video").to_string(),
        ExportKind::Merge => tr!("合併錄影", "merged video").to_string(),
        ExportKind::Gif => trf!("製作 GIF{gif_speed}", "GIF{gif_speed}"),
        ExportKind::Webp => trf!("製作 WebP 動圖{gif_speed}", "WebP{gif_speed}"),
        ExportKind::Speed => trf!("製作 {}× 加速版", "{}× sped-up video", speed_label(e.speed)),
    };
    let title = match e.state {
        ExportState::Running => trf!("{label}中", "Making {label}…"),
        ExportState::Done => trf!("{label}完成", "Done: {label}"),
        ExportState::Error => trf!("{label}失敗", "Failed: {label}"),
        ExportState::Canceled => trf!("{label}已取消", "Canceled: {label}"),
    };
    let pct = (e.progress * 100.0).floor();
    egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).font(theme::font_bold(13.5)));
            let meta = match e.state {
                ExportState::Running => format!("{pct}%{}", e.eta_sec.map(|s| trf!("・剩 {}", " · {} left", human_duration(s))).unwrap_or_default()),
                ExportState::Done => trf!("{}・{}", "{} · {}", video_clock(e.expected_sec), format_bytes(e.bytes.unwrap_or(0))),
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
                        if Btn::new(tr!("取消", "Cancel")).small().show(ui).clicked() {
                            let _ = app.core.exporter.cancel();
                        }
                    });
                });
            }
            ExportState::Done => {
                let cut = e.kind == ExportKind::Cut;
                action_buttons(app, ui, &e.output, cut, cut);
                if Btn::new(tr!("關閉", "Close")).ghost().small().show(ui).clicked() {
                    app.dismissed_job = Some(e.id);
                }
            }
            _ => {
                ui.add(egui::Label::new(RichText::new(e.message.clone().unwrap_or_default()).font(theme::font(12.5))).wrap());
                if Btn::new(tr!("關閉", "Close")).ghost().small().show(ui).clicked() {
                    app.dismissed_job = Some(e.id);
                }
            }
        }
    });
}

/// 系統狀態：新版本、FFmpeg、擷取方式、編碼器（正常的合併成一個「已就緒」）
/// 錄影前的音量表：主畫面看得到、沒在錄影、沒開其他視窗時才開著（錄影中、隱藏時關掉，不佔用麥克風）
fn update_meter(app: &mut UiApp, ctx: &egui::Context) {
    let idle = app.status.recorder.state == RecorderState::Idle && app.status.recorder.busy.is_none();
    let covered = app.editor.is_some() || app.viewer.is_some() || app.library.is_some() || app.export_dlg.is_some();
    let testing = app.meter_until.is_some_and(|t| Instant::now() < t);
    if !testing {
        app.meter_until = None;
    }
    let want = (testing && app.visible && idle && !covered && (app.s.audio_system || app.s.audio_mic)).then(|| (app.s.audio_system, app.s.audio_mic.then(|| app.s.mic_id.clone())));
    if app.meter.as_ref().map(|m| m.key.clone()) != want {
        app.meter = want.map(|(sys, mic)| screenrecorder_core::meter::Meter::start(std::sync::Arc::new(screenrecorder_core::audio::open_wasapi), sys, mic));
    }
    if app.meter.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}

/// 音量表（0～1）：綠色，接近滿格時變橘色
fn level_bar(ui: &mut Ui, v: f32) {
    let p = theme::pal(ui);
    let (r, _) = ui.allocate_exact_size(vec2(46.0, 8.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(4), p.surface2);
    // 人耳對音量是對數感受：-50 dB～0 dB 對應到整條
    let db = 20.0 * v.max(1e-5).log10();
    let k = ((db + 50.0) / 50.0).clamp(0.0, 1.0);
    if k > 0.01 {
        let mut f = r;
        f.set_width(r.width() * k);
        ui.painter().rect_filled(f, CornerRadius::same(4), if k > 0.9 { p.warn } else { p.ok });
    }
}

/// 擷取方式的白話名稱
fn method_name(m: screenrecorder_core::types::CaptureMethod) -> &'static str {
    match m.as_str() {
        "ddagrab" => tr!("顯示卡擷取", "GPU capture"),
        _ => tr!("相容模式", "Compatibility mode"),
    }
}

/// 編碼器的白話名稱：libx264 → CPU 編碼、h264_nvenc → NVIDIA 顯示卡…
fn encoder_name(e: &str) -> String {
    let e = e.to_lowercase();
    let gpu = |brand: &str| trf!("{brand} 顯示卡編碼", "{brand} GPU encoding");
    if e.contains("nvenc") {
        gpu("NVIDIA")
    } else if e.contains("qsv") {
        gpu("Intel")
    } else if e.contains("amf") {
        gpu("AMD")
    } else if e.contains("mf") {
        tr!("顯示卡編碼", "GPU encoding").into()
    } else {
        tr!("CPU 編碼", "CPU encoding").into()
    }
}

fn sys_status(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    ui.label(RichText::new(tr!("系統狀態", "System status")).font(theme::font(12.0)).color(p.muted));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ff = app.env.ffmpeg.clone();
        let mut chips: Vec<(String, Tone, String)> = vec![];
        let mut ok: Vec<String> = vec![];
        let ver = ff.version.clone().unwrap_or_default().split('-').next().unwrap_or("").to_string();
        if !app.env_ready {
            chips.push((tr!("偵測中…", "Detecting…").into(), Tone::Plain, String::new()));
        } else if ff.found {
            ok.push(trf!("FFmpeg {ver}（{}）", "FFmpeg {ver} ({})", ff.path.clone().unwrap_or_default()));
            if !ff.has_ddagrab {
                chips.push((
                    tr!("相容模式擷取", "Compatibility capture").into(),
                    Tone::Warn,
                    tr!(
                        "這個 FFmpeg 不支援顯示卡擷取（ddagrab），改用相容模式（gdigrab），大畫面時比較吃 CPU",
                        "This FFmpeg doesn't support GPU capture (ddagrab), so compatibility mode (gdigrab) is used. It uses more CPU on large screens."
                    )
                    .into(),
                ));
            } else {
                match ff.ddagrab_works {
                    None => chips.push((tr!("ddagrab 測試中…", "Testing ddagrab…").into(), Tone::Plain, String::new())),
                    Some(true) => ok.push(tr!("擷取：顯示卡擷取（ddagrab）", "Capture: GPU capture (ddagrab)").into()),
                    Some(false) => chips.push((
                        tr!("改用相容模式擷取", "Using compatibility capture").into(),
                        Tone::Warn,
                        trf!(
                            "顯示卡擷取（ddagrab）在這台電腦不能用，改用相容模式（gdigrab）\n{}",
                            "GPU capture (ddagrab) doesn't work on this PC, so compatibility mode (gdigrab) is used\n{}",
                            ff.ddagrab_error.clone().unwrap_or_default()
                        ),
                    )),
                }
            }
            if let Some(g) = &ff.gpu_convert {
                ok.push(trf!("畫面處理：{g}", "Video processing: {g}"));
            }
            match &ff.encoder {
                Some(e) => ok.push(trf!("編碼：{}（{e}）", "Encoding: {} ({e})", encoder_name(e))),
                None => chips.push((tr!("無 H.264 編碼器", "No H.264 encoder").into(), Tone::Bad, String::new())),
            }
        } else {
            chips.push((tr!("找不到 FFmpeg", "FFmpeg not found").into(), Tone::Bad, String::new()));
        }
        if !ok.is_empty() {
            let problems = chips.iter().any(|c| matches!(c.1, Tone::Warn | Tone::Bad));
            chips.push((if problems { format!("FFmpeg {ver}") } else { tr!("已就緒", "Ready").into() }, Tone::Ok, ok.join("\n")));
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
            // (文字, 更新中)
            let (label, busy) = match &app.status.install {
                Some(i) if i.phase == screenrecorder_core::selfupdate::InstallPhase::Downloading => {
                    if let Some(total) = i.total.filter(|t| *t > 0) {
                        (trf!("下載新版 {}%", "Downloading update {}%", (i.received as f64 / total as f64 * 100.0).floor()), true)
                    } else {
                        (trf!("下載新版 {}", "Downloading update {}", format_bytes(i.received)), true)
                    }
                }
                Some(i) if i.phase == screenrecorder_core::selfupdate::InstallPhase::Verifying => (tr!("驗證新版…", "Verifying update…").into(), true),
                Some(i) if i.phase == screenrecorder_core::selfupdate::InstallPhase::Restarting => (tr!("重新啟動中…", "Restarting…").into(), true),
                _ => (trf!("有新版本 v{}", "Update available: v{}", u.version), false),
            };
            let r = chip(ui, &label, Tone::Accent, !busy).on_hover_text(if auto {
                tr!("點一下更新（自動下載並重新啟動）", "Click to update (downloads and restarts automatically)")
            } else {
                tr!("點一下前往下載頁面", "Click to open the download page")
            });
            if r.clicked() && !busy {
                if !auto {
                    let _ = actions::open_url(&u.url);
                } else {
                    let size = u.size.map(|s| trf!("（約 {}）", " (about {})", format_bytes(s))).unwrap_or_default();
                    app.ask = Some(Ask::confirm(
                        trf!("更新到 v{}", "Update to v{}", u.version),
                        trf!(
                            "會下載新版{size}、核對檔案後自動重新啟動程式；設定與錄影都會保留。更新內容可在系統匣選單的「更新說明」查看。",
                            "The new version{size} will be downloaded and verified, then the app restarts automatically. Your settings and recordings are kept. See what's new under “Release notes” in the system tray menu."
                        ),
                        tr!("立即更新", "Update now"),
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
                // 英文的「Recent recordings」分成兩行
                ui.set_width(if is_en() { 96.0 } else { 84.0 });
                ui.label(RichText::new(tr!("最近錄影", "Recent recordings")).font(theme::font_bold(14.0)));
                if let Some(r) = &app.recent {
                    if r.total > 0 {
                        ui.label(theme::muted(ui, trf!("{} / {} 頁", "Page {} / {}", app.recent_page, r.pages.max(1))));
                    }
                }
            });
            let pages = app.recent.as_ref().map(|r| r.pages).unwrap_or(1);
            if Btn::icon_only(Icon::ChevL).ghost().small().enabled(app.recent_page > 1).tooltip(tr!("較新的錄影", "Newer recordings")).show(ui).clicked() {
                app.recent_page -= 1;
                app.load_recent();
            }
            // 一頁放幾張依寬度決定
            // 右邊「全部錄影」「全部截圖」：一樣寬，圖示與文字靠左對齊
            let rec_btn = Btn::new(tr!("全部錄影", "All recordings")).icon(Icon::List).small().left();
            let shot_btn = Btn::new(tr!("全部截圖", "All screenshots")).icon(Icon::Camera).small().left();
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
                        // 路徑很長時截斷（滑鼠提示有完整內容），不蓋到右邊的按鈕
                        let text = trf!("「{dir}」還沒有錄影，按「開始錄影」試試看。", "No recordings in “{dir}” yet. Click “Start recording” to try it.");
                        ui.add(egui::Label::new(theme::muted(ui, &text)).truncate()).on_hover_text(&text);
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
                if Btn::icon_only(Icon::ChevR).ghost().small().enabled(app.recent_page < pages).tooltip(tr!("較舊的錄影", "Older recordings")).show(ui).clicked() {
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
    if is_cut_name(&e.media.name) {
        tags.push((tr!("剪輯版", "Edited").into(), Tone::Warn, String::new()));
    }
    if !e.exports.is_empty() {
        let all = |f: ExportFormat| e.exports.iter().all(|x| x.format == Some(f));
        let kind = if all(ExportFormat::Gif) {
            "GIF"
        } else if all(ExportFormat::Webp) {
            "WebP"
        } else if e.exports.iter().all(|x| x.format.is_some_and(|f| f.is_animation())) {
            tr!("動圖", "Anim")
        } else {
            tr!("加速", "Sped-up")
        };
        let list: Vec<String> = e.exports.iter().map(super::library_dialog::export_tag).collect();
        tags.push((format!("{kind} {}", e.exports.len()), Tone::Accent, trf!("已製作 {}", "Made: {}", list.join(tr!("、", ", ")))));
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
        // 有聲音：灰色小喇叭（不用彩色標籤）
        if e.media.has_audio == Some(true) {
            super::library_dialog::audio_mark(ui);
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
            (EntryAction::Play, Icon::Play, tr!("播放", "Play")),
            (EntryAction::Reveal, Icon::Folder, tr!("在資料夾中顯示", "Show in folder")),
            (EntryAction::Edit, Icon::Cut, tr!("剪輯", "Edit")),
            (EntryAction::Export, Icon::Export, tr!("製作加速版 / GIF", "Make sped-up video / GIF")),
        ];
        // 平常淡灰色，滑鼠移到卡片上才變深
        let hot = ui.rect_contains_pointer(rect);
        for (act, icon, tip) in acts {
            if Btn::icon_only(icon).ghost().small().quiet(!hot).tooltip(tip).show(ui).clicked() {
                app.known.insert(e.media.path.clone(), e.clone());
                app.act(act, e.clone());
            }
        }
        let more = Btn::icon_only(Icon::More).ghost().small().quiet(!hot).tooltip(tr!("更多", "More")).show(ui);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(180.0);
            if ui.button(tr!("複製檔案（貼到 LINE、資料夾）", "Copy file (paste into LINE or a folder)")).clicked() {
                app.act(EntryAction::CopyFile, e.clone());
            }
            if e.media.has_audio != Some(false) && ui.button(tr!("存成 M4A（只留聲音）", "Save as M4A (audio only)")).clicked() {
                app.act(EntryAction::SaveAudio, e.clone());
            }
            if !e.media.chapters.is_empty() && ui.button(tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)")).clicked() {
                app.act(EntryAction::CopyChapters, e.clone());
            }
        });
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
    let hint = trf!(
        "即將開始錄影{}{}",
        "Recording is about to start{}{}",
        if app.s.hide_ui { tr!("，這個視窗若在錄影範圍內會自動縮小", ". This window will minimize if it's inside the recording area") } else { "" },
        if app.env.hotkeys.is_some_and(|h| h.record) && !app.keys.label(0).is_empty() { trf!("，或按 {} 取消", ". Press {} to cancel", app.keys.label(0)) } else { String::new() }
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
            if Btn::new(tr!("取消", "Cancel")).show(ui).clicked() {
                let core = app.core.clone();
                app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
            }
        });
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
}
