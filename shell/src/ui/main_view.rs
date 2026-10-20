//! 主畫面：
//! - 左：擷取範圍（單一螢幕 / 所有螢幕 / 自訂範圍）與預覽、下方一列錄影設定
//! - 右：錄影狀態、計時、控制按鈕、轉檔工作、事件紀錄、系統狀態
//! - 下：最近錄影（橫向分頁）

use super::dialogs::{base_name, date_labels, file_name, is_cut_name, is_default_name, Ask};
use super::entry_menu::Place;
use super::settings::{FollowWindow, SourceType};
use super::settings_dialog::{self, Page};
use super::theme::{self, chip, segmented, switch, Btn, Icon, Tone};
use super::tour::{self, Spot};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::audiopipe::{SILENT_MIC, SILENT_SYSTEM};
use screenrecorder_core::format::{clock, format_bytes, human_duration, output_size, short_clock, speed_label, video_clock};
use screenrecorder_core::i18n::is_en;
use screenrecorder_core::types::{DownloadPhase, EncoderPreference, ExportKind, ExportState, LibraryEntry, LogLevel, MethodPreference, RecorderState, Rect as DRect};
use screenrecorder_core::{tr, trf};
use std::time::Instant;

/// 主畫面自己的狀態（拖曳範圍、輸入中的文字）
#[derive(Default)]
pub struct MainState {
    drag: Option<RegionDrag>,
    /// 自訂範圍：展開「精確數值」（X、Y、寬、高）
    precise: bool,
    /// 儲存位置改了的時間（停止輸入一下子後重新讀取最近錄影）
    pub dir_changed_at: Option<Instant>,
    /// 截圖中（按鈕停用，避免連按）
    pub shooting: bool,
    /// 「選擇視窗」的清單（按下時更新）
    windows: Vec<screenrecorder_core::winui::WindowInfo>,
    /// 事件紀錄展開時看到的最後一筆的時間：收起時，之後的警告與錯誤標在標題旁
    log_seen: u64,
    /// 按 ✕ 關掉的錄影結果（路徑或訊息）
    dismissed_result: Option<String>,
    /// 錄影失敗的卡片：展開「詳細資訊」
    result_detail: bool,
    /// 按 ✕ 關掉的「已自動處理」結果
    dismissed_after: Option<u64>,
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
        ffmpeg_banner(app, ui);
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

/// 第一次使用（找不到 FFmpeg）：一個大的「下載並開始使用」（和設定 → 外部元件的下載是同一個）；
/// 下載不了時到外部元件設定 Proxy 或選擇自己下載的檔案
fn ffmpeg_banner(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let d = app.status.download.clone();
    let busy = matches!(d.phase, DownloadPhase::Downloading | DownloadPhase::Verifying | DownloadPhase::Extracting);
    let failed = matches!(d.phase, DownloadPhase::Error | DownloadPhase::Canceled);
    let (fill, stroke) = if failed { (p.rec_soft, p.rec) } else { (p.accent_soft, p.accent) };
    let banner = egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke.gamma_multiply(0.4))).corner_radius(CornerRadius::same(theme::RADIUS)).inner_margin(14.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width() - 240.0);
                ui.label(
                    RichText::new(tr!("第一次使用：需要先下載錄影元件（約 110 MB）", "First time here: the recording component needs to be downloaded (about 110 MB)"))
                        .font(theme::font_bold(15.0))
                        .color(p.text),
                );
                ui.add_space(2.0);
                let (text, color) = match d.phase {
                    DownloadPhase::Downloading => {
                        let pct = d.total.map(|t| d.received as f64 / t.max(1) as f64 * 100.0);
                        let mut s = trf!("下載中 {}", "Downloading {}", format_bytes(d.received));
                        if let (Some(t), Some(pc)) = (d.total, pct) {
                            s += &trf!(" / {}（{}%）", " / {} ({}%)", format_bytes(t), pc.floor());
                        }
                        if let Some(sp) = d.speed {
                            if let Some(t) = d.total {
                                s += &trf!("・剩約 {}", " · about {} left", human_duration((t.saturating_sub(d.received)) as f64 / sp.max(1.0)));
                            }
                        }
                        (s, p.text)
                    }
                    DownloadPhase::Verifying => (tr!("確認檔案完整…", "Checking the download…").into(), p.text),
                    DownloadPhase::Extracting => (tr!("解壓縮並確認可以使用…", "Unpacking and checking that it works…").into(), p.text),
                    DownloadPhase::Error | DownloadPhase::Canceled => (d.message.clone().unwrap_or_else(|| tr!("下載失敗", "Download failed").into()), p.rec),
                    _ => (
                        tr!(
                            "錄影、截圖都要用到免費的 FFmpeg 錄影元件。按右邊的按鈕自動下載，只要一次，完成後就可以開始錄影。",
                            "Recording and screenshots use the free FFmpeg component. Click the button on the right to download it once; then you can start recording."
                        )
                        .into(),
                        p.muted,
                    ),
                };
                ui.add(egui::Label::new(RichText::new(text).font(theme::font(13.0)).color(color)).wrap());
                if busy {
                    ui.add_space(2.0);
                    let frac = if d.phase == DownloadPhase::Downloading { d.total.map(|t| d.received as f32 / t.max(1) as f32).unwrap_or(0.0) } else { 1.0 };
                    theme::progress(ui, frac, p.accent, 6.0);
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if busy {
                    if Btn::new(tr!("取消下載", "Cancel download")).show(ui).clicked() {
                        app.core.downloader.cancel();
                    }
                } else {
                    let label = if failed { tr!("重新下載", "Download again") } else { tr!("下載並開始使用", "Download and get started") };
                    if Btn::new(label).primary().height(40.0).min_width(180.0).show(ui).clicked() {
                        if let Err(e) = actions::ffmpeg_download(&app.core) {
                            app.toast(e.message().to_string(), true);
                        }
                    }
                }
            });
        });
        // 下載不了（公司網路、沒有網路）：設定 → 外部元件可以設定 Proxy，或選擇自己下載的檔案
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(tr!("無法下載，或已經有 FFmpeg？", "Download not working, or already have FFmpeg?")).font(theme::font(12.5)).color(p.muted));
            if Btn::new(tr!("到「設定 → 外部元件」", "Open Settings → Components"))
                .small()
                .ghost()
                .tooltip(tr!("設定 Proxy、用瀏覽器下載後選擇檔案，或重新偵測", "Set a proxy, choose a file downloaded in your browser, or detect again"))
                .show(ui)
                .clicked()
            {
                settings_dialog::open(app, settings_dialog::Page::Components);
            }
        });
    });
    tour::mark(ui, Spot::Download, banner.response.rect);
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
        // 範圍分頁、細節、即時預覽、重新整理：分頁名稱與按鈕的文字不縮短，放不下時細節換到第二行
        let tabs = tr!(
            [(SourceType::Monitor, "單一螢幕"), (SourceType::All, "所有螢幕"), (SourceType::Region, "自訂範圍"), (SourceType::Audio, "只錄聲音")],
            [(SourceType::Monitor, "Single screen"), (SourceType::All, "All screens"), (SourceType::Region, "Custom area"), (SourceType::Audio, "Audio only")]
        );
        let gap = ui.spacing().item_spacing.x;
        let text_w = |ui: &Ui, t: &str, size: f32| ui.painter().layout_no_wrap(t.to_string(), theme::font(size), Color32::WHITE).size().x;
        let tabs_w = tabs.iter().map(|(_, t)| text_w(ui, t, 13.5) + 22.0 + 2.0).sum::<f32>() + 8.0;
        let audio = app.s.source_type == SourceType::Audio;
        let live_label = tr!("即時預覽", "Live preview");
        let right_w = 28.0 + gap + if audio { 0.0 } else { 34.0 + 8.0 + text_w(ui, live_label, 14.0) + gap };
        let one_row = tabs_w + gap + detail_width(app, ui) + gap + right_w <= ui.available_width();
        ui.horizontal(|ui| {
            let mut st = app.s.source_type;
            // 播放中點範圍分頁：停止播放，回到錄影畫面（點的是不同的分頁時也一起切換）
            if segmented(ui, &mut st, &tabs, !locked)
                || (app.inline.is_some() && ui.min_rect().contains(ui.ctx().input(|i| i.pointer.interact_pos()).unwrap_or_default()) && ui.ctx().input(|i| i.pointer.primary_clicked()))
            {
                app.inline = None;
                if st != app.s.source_type {
                    app.s.source_type = st;
                    app.save_settings();
                }
            }
            tour::mark(ui, Spot::Source, ui.min_rect());
            if one_row {
                let room = ui.available_width() - right_w - gap;
                ui.scope(|ui| {
                    ui.set_max_width(room.max(0.0));
                    source_detail(app, ui, room);
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Refresh).ghost().small().tooltip(tr!("重新擷取預覽並更新螢幕清單", "Refresh the preview and the screen list")).show(ui).clicked() {
                    app.preview.retry_live();
                    app.refresh_env(|_| {});
                }
                let mut live = app.s.live_preview;
                if !audio
                    && switch(ui, &mut live, live_label, true)
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
        if !one_row {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let room = ui.available_width();
                source_detail(app, ui, room);
            });
        }
        // 自訂範圍的精確數值（展開時）
        if app.s.source_type == SourceType::Region && app.main.precise && app.s.follow_window.is_none() {
            ui.add_space(6.0);
            precise_row(app, ui, locked);
        }
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
                // 滑鼠提示：螢幕型號（有的話）與顯示卡
                let tip = if m.name.is_empty() { trf!("{}（{}）", "{} ({})", full(&m), m.adapter_name) } else { trf!("{}\n{}（{}）", "{}\n{} ({})", full(&m), m.name, m.adapter_name) };
                if Btn::new(label).small().selected(on).enabled(!locked).tooltip(tip).show(ui).clicked() && !on {
                    app.s.monitor_id = Some(m.id.clone());
                    app.s.monitor_key = m.key.clone();
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
                    if multi { tr!("（跨不同顯示卡，會比較吃電腦資源）", " (spans different graphics cards, so it uses more of the computer's resources)") } else { "" }
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
            // 在螢幕上直接框選（和框選截圖一樣的畫面，不存檔）：比在小預覽圖上拖曳準確
            // 框選要先截下整個畫面（用 FFmpeg）：沒有錄影元件時停用並說明，和「框選截圖」一樣
            let pick = region_pick_btn().enabled(!locked && app.env.ffmpeg.found);
            let pick = if app.env.ffmpeg.found {
                pick
            } else {
                pick.tooltip(tr!("請先下載錄影元件（上方的「下載並開始使用」）", "Download the recording component first (“Download and get started” above)"))
            };
            if app.s.follow_window.is_none() && pick.show(ui).clicked() {
                super::snip::pick_region(app, |app: &mut UiApp, r: Option<DRect>| {
                    let Some(r) = r else { return };
                    if r.width < 16 || r.height < 16 {
                        return app.toast(trf!("範圍太小（{}×{}），寬高至少 16 像素", "The area is too small ({}×{}). It must be at least 16 pixels wide and high.", r.width, r.height), true);
                    }
                    set_region(app, r);
                    app.save_settings();
                    app.toast(trf!("錄影範圍：{}×{}", "Recording area: {}×{}", r.width, r.height), false);
                });
            }
            if app.s.follow_window.is_none() {
                let open = app.main.precise;
                if Btn::new(tr!("精確數值", "Exact values"))
                    .icon(if open { Icon::ChevD } else { Icon::ChevR })
                    .small()
                    .selected(open)
                    .tooltip(tr!("用數字輸入範圍的位置與大小（X、Y、寬、高）", "Enter the area's position and size as numbers (X, Y, width, height)"))
                    .show(ui)
                    .clicked()
                {
                    app.main.precise = !open;
                }
            }
            window_picker(app, ui, locked);
            // 範圍太小：精確數值收起來時也要看得到
            if region_too_small(app) && !app.main.precise {
                ui.label(RichText::new(tr!("寬高至少 16 像素", "Width and height must be at least 16 pixels")).font(theme::font(12.5)).color(p.rec));
                return;
            }
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

/// 自訂範圍那一列最前面的「在螢幕上框選…」
fn region_pick_btn() -> Btn {
    Btn::new(tr!("在螢幕上框選…", "Select on screen…"))
        .icon(Icon::Fullscreen)
        .small()
        .tooltip(tr!("用整個螢幕框選要錄的範圍（和框選截圖一樣，可以調整後按 Enter 確定）", "Select the area to record on the whole screen (like a screenshot selection; adjust it, then press Enter)"))
}

/// 範圍分頁旁的細節至少要多寬：放不下時換到第二行（文字不縮短成圖示）
fn detail_width(app: &UiApp, ui: &Ui) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    match app.s.source_type {
        SourceType::Monitor => {
            // 放不下完整的「螢幕 1  1920×1080・主」時會只寫「螢幕 1」
            let buttons: f32 = app.env.monitors.iter().map(|m| Btn::new(trf!("螢幕 {}", "Screen {}", m.display_number)).small().width(ui) + gap).sum();
            let whole = if app.s.region_in_monitor(&app.env).is_some() { Btn::new(tr!("整個螢幕", "Whole screen")).icon(Icon::Close).small().width(ui) + gap } else { 0.0 };
            buttons.max(if app.env.monitors.is_empty() { 220.0 } else { 0.0 }) + whole
        }
        SourceType::All | SourceType::Audio => 220.0,
        SourceType::Region => match &app.s.follow_window {
            Some(_) => 200.0 + Btn::new(tr!("只錄視窗內容", "Window only")).small().width(ui) + 28.0 + gap * 2.0,
            None => {
                region_pick_btn().width(ui)
                    + gap
                    + Btn::new(tr!("精確數值", "Exact values")).icon(Icon::ChevR).small().width(ui)
                    + Btn::new(tr!("選擇視窗", "Choose window")).icon(Icon::Window).small().width(ui)
                    + gap * 2.0
            }
        },
    }
}

/// 自訂範圍太小（寬或高不到 16 像素，不能錄）
fn region_too_small(app: &UiApp) -> bool {
    app.s.source_type == SourceType::Region && (app.s.region.width < 16 || app.s.region.height < 16)
}

/// 自訂範圍的精確數值：X、Y、寬、高（按 − ＋ 或直接輸入）；寬高太小時直接在旁邊用紅字提示
fn precise_row(app: &mut UiApp, ui: &mut Ui, locked: bool) {
    let p = theme::pal(ui);
    let d = app.env.desktop;
    let (x0, y0, dw, dh) = if d.width > 0 && d.height > 0 { (d.x, d.y, d.width, d.height) } else { (0, 0, 7680, 4320) };
    ui.add_enabled_ui(!locked, |ui| {
        ui.horizontal_wrapped(|ui| {
            let r = app.s.region;
            let fields = [
                (tr!("X", "X"), r.x, x0 as f64..=(x0 + dw - 1) as f64),
                (tr!("Y", "Y"), r.y, y0 as f64..=(y0 + dh - 1) as f64),
                (tr!("寬", "W"), r.width, 1.0..=dw as f64),
                (tr!("高", "H"), r.height, 1.0..=dh as f64),
            ];
            let mut next = r;
            for (i, (label, v, range)) in fields.into_iter().enumerate() {
                ui.label(theme::muted(ui, label));
                let mut val = v as f64;
                if theme::stepper(ui, Id::new(("region-value", i)), &mut val, 10.0, range, 0, "") {
                    let v = val.round() as i32;
                    match i {
                        0 => next.x = v,
                        1 => next.y = v,
                        2 => next.width = v,
                        _ => next.height = v,
                    }
                }
                ui.add_space(6.0);
            }
            if next != r {
                app.s.region = next;
                app.s.follow_window = None;
                app.save_settings();
            }
            if next.width < 16 || next.height < 16 {
                // 整句不拆開（放不下時換到下一行）
                ui.add(egui::Label::new(RichText::new(tr!("寬高至少 16 像素", "Width and height must be at least 16 pixels")).font(theme::font(12.5)).color(p.rec)).extend());
            }
        });
    });
}

/// 「視窗」：選一個視窗，範圍設成它的位置，錄影時跟著它移動；已選時顯示視窗名稱，✕ 取消
fn window_picker(app: &mut UiApp, ui: &mut Ui, locked: bool) {
    let p = theme::pal(ui);
    if let Some(w) = app.s.follow_window.clone() {
        let text = trf!("只錄「{}」", "Only “{}”", w.title);
        // 先留位置給後面的「只錄視窗內容」與 ✕，視窗名稱用剩下的寬度（太長時截斷，不會蓋到右邊的即時預覽）
        let gap = ui.spacing().item_spacing.x;
        let after = if app.env.ffmpeg.has_gfxcapture { Btn::new(tr!("只錄視窗內容", "Window only")).small().width(ui) + gap } else { 0.0 } + if locked { 0.0 } else { 28.0 + gap };
        let name_w = (ui.available_width() - after).max(40.0);
        ui.scope(|ui| {
            ui.set_max_width(name_w);
            ui.add(egui::Label::new(RichText::new(&text).color(p.accent).font(theme::font(12.5))).truncate()).on_hover_text(trf!(
                "{}\n錄影時範圍跟著這個視窗移動（視窗大小改變時維持原本的大小）",
                "{}\nWhile recording, the area follows this window (if the window is resized, the area keeps its original size)",
                w.title
            ));
        });
        // 只錄視窗內容（Windows Graphics Capture；FFmpeg 支援時才顯示）
        if app.env.ffmpeg.has_gfxcapture
            && Btn::new(tr!("只錄視窗內容", "Window only"))
                .small()
                .selected(app.s.window_capture)
                .enabled(!locked)
                .tooltip(tr!(
                    "直接擷取這個視窗：被其他視窗擋住、移到哪裡都錄得到，畫面上的其他東西不會錄進去（攝影機小視窗、點擊與按鍵效果、螢幕畫筆也不會）。不能用時自動改回跟著視窗的範圍",
                    "Capture this window directly: it's recorded even when covered by other windows or moved, and nothing else on screen is recorded (including the camera bubble, click and key effects, and the screen pen). Falls back to the area that follows the window if it can't be used."
                ))
                .show(ui)
                .clicked()
        {
            app.s.window_capture = !app.s.window_capture;
            app.save_settings();
        }
        if !locked && Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("不跟著視窗，改回一般的自訂範圍", "Stop following the window and go back to a normal custom area")).show(ui).clicked()
        {
            app.s.follow_window = None;
        }
        return;
    }
    let tip = tr!("只錄某個視窗：範圍設成那個視窗的位置，錄影時跟著它移動", "Record one window only: the area is set to that window and follows it while recording");
    // 一定寫出「選擇視窗」（放不下時整列換到第二行）
    let b = Btn::new(tr!("選擇視窗", "Choose window")).icon(Icon::Window).small().enabled(!locked).tooltip(tip).show(ui);
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
    // 主畫面內播放：在預覽的位置播放（暫停即時預覽）
    if app.inline.is_some() {
        if app.preview.running() {
            app.preview.stop(&app.core);
        }
        return super::inline_play::show(app, ui, area);
    }
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
    // 螢幕名稱標籤的位置（範圍大小的標籤要避開）
    let mut tags = vec![];
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
        tags.push(tag);
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
                app.s.monitor_key = m.key.clone();
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
    let size = g.size() + vec2(12.0, 6.0);
    // 範圍大小的標籤：放在紅框上方；會蓋到「螢幕 1」標籤時改放紅框裡面的左上角，再不行就放在紅框下方
    let clear = |t: &Rect| !tags.iter().any(|m: &Rect| m.expand(2.0).intersects(*t));
    let above = Rect::from_min_size(rr.min + vec2(0.0, -24.0).max(rect.min - rr.min), size);
    let inside = Rect::from_min_size(rr.min + vec2(4.0, 4.0), size);
    let below = Rect::from_min_size(pos2(rr.min.x, (rr.max.y + 4.0).min(rect.max.y - size.y)), size);
    let tag = [above, inside, below].into_iter().find(|t| clear(t)).unwrap_or(below);
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
    let system = if app.s.audio_app.is_empty() {
        tr!("系統聲音（電腦播放的聲音）", "System audio (sound played by the computer)").to_string()
    } else {
        trf!("{} 的聲音", "Sound from {}", app.s.audio_app)
    };
    let parts: Vec<String> = [app.s.audio_system.then_some(system), app.s.audio_mic.then(|| tr!("麥克風", "Microphone").to_string())].into_iter().flatten().collect();
    let (line, warn) = if parts.is_empty() {
        (tr!("還沒選要錄的聲音：請在下方「聲音」打開系統聲音或麥克風", "No audio selected: turn on system audio or the microphone under “Audio” below").to_string(), true)
    } else {
        (trf!("會錄：{}", "Will record: {}", parts.join(tr!("、", ", "))), false)
    };
    ui.painter().text(c + vec2(0.0, 20.0 * s), egui::Align2::CENTER_CENTER, line, theme::font(13.5), if warn { p.warn } else { p.text });
    // 存成：M4A（只有聲音，建議）／ MP4
    let label = tr!("存成：", "Save as:");
    let items = [(true, tr!("M4A（只有聲音，建議）", "M4A (audio only, recommended)")), (false, "MP4")];
    let text_w = |t: &str, size: f32| ui.painter().layout_no_wrap(t.to_string(), theme::font(size), Color32::WHITE).size().x;
    let row_w = text_w(label, 13.0) + 8.0 + items.iter().map(|(_, t)| text_w(t, 13.5) + 24.0).sum::<f32>() + 8.0;
    let top = c.y + 40.0 * s;
    let row = Rect::from_min_size(pos2(c.x - row_w / 2.0, top), vec2(row_w + 4.0, 36.0));
    let mut m4a = app.s.audio_m4a;
    let locked = app.locked();
    ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.label(RichText::new(label).font(theme::font(13.0)).color(p.muted));
        if segmented(ui, &mut m4a, &items, !locked) {
            app.s.audio_m4a = m4a;
            app.save_settings();
        }
    });
    let hint = if m4a {
        tr!(
            "M4A 是一般的聲音檔，手機、電腦都能直接播放。程式也會留一份 MP4，可以在「最近錄影」播放、剪輯、降噪。",
            "M4A is a regular audio file that plays on any phone or computer. An MP4 copy is also kept so you can play, edit and reduce noise in “Recent recordings”."
        )
    } else {
        tr!(
            "存成 MP4：畫面是一張「只錄聲音」的卡片，檔案幾乎只有聲音的大小；一樣可以暫停、加標記、剪輯、降噪。",
            "Saved as MP4 with an “Audio only” card, so the file stays small. You can still pause, add markers, edit and reduce noise."
        )
    };
    let hint_rect = Rect::from_min_max(pos2((c.x - 270.0).max(area.min.x + 12.0), top + 42.0), pos2((c.x + 270.0).min(area.max.x - 12.0), area.max.y - 4.0));
    ui.scope_builder(UiBuilder::new().max_rect(hint_rect).layout(Layout::top_down(Align::Center)), |ui| {
        ui.add(egui::Label::new(RichText::new(hint).font(theme::font(12.0)).color(p.muted)).wrap().halign(Align::Center));
    });
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
                DRect { x: (o.x + dx).min(dk.x + dk.width - o.width).max(dk.x), y: (o.y + dy).min(dk.y + dk.height - o.height).max(dk.y), ..o }
            }
            DragMode::Resize { n, s, e, w } => {
                let pt = to_desk(pos, false);
                let (dx, dy) = (pt.0 - d.from.0, pt.1 - d.from.1);
                let (mut l, mut t, mut rt, mut b) = (o.x, o.y, o.x + o.width, o.y + o.height);
                if w {
                    l = (o.x + dx).min(rt - MIN).max(dk.x);
                }
                if e {
                    rt = (rt + dx).min(dk.x + dk.width).max(l + MIN);
                }
                if n {
                    t = (o.y + dy).min(b - MIN).max(dk.y);
                }
                if s {
                    b = (b + dy).min(dk.y + dk.height).max(t + MIN);
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
            // 範圍比桌面大時（螢幕排列改過）也不會出錯：先靠右下、再靠左上
            r.x = r.x.min(b.x + b.width - r.width).max(b.x);
            r.y = r.y.min(b.y + b.height - r.height).max(b.y);
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
    // 設定組合：有建立組合時才放在這裡（沒有時在「更多工具」）
    let has_presets = !app.presets.is_empty();
    let set_w = set_btn.width(ui) + if has_presets { super::presets::width(app, ui) + 4.0 } else { 0.0 };
    let out = app.s.source_rect(&app.env).map(|r| output_size(r.width, r.height, app.s.scale as f64));
    let big = out.is_some_and(|(w, h)| (w as f64) * (h as f64) > 3840.0 * 2160.0 * 1.05);
    let out_text = out.map(|(w, h)| trf!("輸出 {w}×{h}{}", "Output {w}×{h}{}", if big { tr!("（很大，建議 50%）", " (very large; 50% recommended)") } else { "" }));
    let out_w = out_text.as_ref().map(|t| ui.painter().layout_no_wrap(t.clone(), theme::font(12.5), p.muted).size().x + 16.0).unwrap_or(0.0);
    // 左邊：錄影、聲音、儲存位置的摘要，依剩下的寬度取捨
    let room = ui.available_width() - set_w - 8.0;
    let show_out = room - out_w > 360.0;
    let mut room = if show_out { room - out_w } else { room };
    let mut rec = settings_dialog::record_summary(app);
    // 攝影機有自己的按鈕
    rec.retain(|t| t != tr!("攝影機", "Camera"));
    let (audio, audio_icon) = settings_dialog::audio_summary(app);
    let item = |text: &str, icon: Icon| Btn::new(text).icon(icon).ghost().small();
    let measure = |ui: &Ui, text: &str, icon: Icon| item(text, icon).width(ui) + 2.0;
    let mut open: Option<Page> = None;
    // 錄影：太窄時只寫每秒張數
    let rec_full = rec.join(tr!("・", " · "));
    let rec_text = if measure(ui, &rec_full, Icon::Video) + 200.0 <= room { rec_full.clone() } else { rec[0].clone() };
    room -= measure(ui, &rec_text, Icon::Video);
    if item(&rec_text, Icon::Video).enabled(!locked).tooltip(trf!("錄影：{rec_full}（點一下修改）", "Recording: {rec_full} (click to change)")).show(ui).clicked() {
        open = Some(Page::Record);
    }
    // 聲音：太窄時只剩圖示
    // 聲音、攝影機太窄時寫短一點，但不會只剩圖示
    let audio_text = if measure(ui, &audio, audio_icon) + 120.0 <= room { audio.clone() } else { tr!("聲音", "Audio").to_string() };
    room -= measure(ui, &audio_text, audio_icon);
    if item(&audio_text, audio_icon).enabled(!locked).tooltip(trf!("錄製聲音：{audio}（點一下修改）", "Audio: {audio} (click to change)")).show(ui).clicked() {
        open = Some(Page::Audio);
    }
    // 攝影機：直接選要用哪一台（不用進設定）；太窄時只剩圖示
    let cam = camera_label(&app.s.camera);
    let cam_text = if measure(ui, &cam, Icon::Webcam) + 80.0 <= room { cam.clone() } else { tr!("攝影機", "Camera").to_string() };
    room -= measure(ui, &cam_text, Icon::Webcam);
    let cam_resp =
        item(&cam_text, Icon::Webcam).selected(!app.s.camera.is_empty()).enabled(!locked).tooltip(trf!("攝影機子母畫面：{cam}（點一下修改）", "Camera overlay: {cam} (click to change)")).show(ui);
    if cam_resp.clicked() {
        app.load_cameras(false);
    }
    egui::Popup::menu(&cam_resp).show(|ui| {
        if camera_menu(app, ui) {
            open = Some(Page::Record);
        }
    });
    // 儲存位置：縮短路徑（保留結尾）
    let dir = app.s.out_dir(&app.env);
    let fits = |t: &str| measure(ui, t, Icon::Folder) <= room;
    let short: String = if fits(&dir) {
        dir.clone()
    } else {
        let chars: Vec<char> = dir.chars().collect();
        (1..chars.len()).map(|i| format!("…{}", chars[i..].iter().collect::<String>())).find(|t| fits(t)).unwrap_or_default()
    };
    // 點一下：開啟資料夾或換一個資料夾（不用進設定打字）
    let dir_resp = item(&short, Icon::Folder).enabled(!locked).tooltip(trf!("錄影的儲存位置：{dir}（點一下修改）", "Recordings are saved to: {dir} (click to change)")).show(ui);
    egui::Popup::menu(&dir_resp).show(|ui| {
        ui.set_min_width(200.0);
        if ui.button(tr!("在檔案總管開啟", "Open in File Explorer")).clicked() {
            if let Err(e) = actions::open(actions::OpenAction::Folder, &dir) {
                app.toast(e.message().to_string(), true);
            }
        }
        if ui.button(tr!("選擇其他資料夾…", "Choose another folder…")).clicked() {
            settings_dialog::choose_output_dir(app);
        }
        // 截圖和錄影分開存
        let shots = app.s.shots_dir(&app.env);
        if ui.button(tr!("開啟截圖資料夾", "Open screenshot folder")).on_hover_text(&shots).clicked() {
            let _ = std::fs::create_dir_all(&shots);
            if let Err(e) = actions::open(actions::OpenAction::Folder, &shots) {
                app.toast(e.message().to_string(), true);
            }
        }
        if ui.button(tr!("儲存位置設定…", "Save location settings…")).clicked() {
            open = Some(Page::Save);
        }
    });
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let set = set_btn.show(ui);
        tour::mark(ui, Spot::Settings, set.rect);
        if set.clicked() {
            open = Some(Page::Record);
        }
        if has_presets {
            super::presets::button(app, ui);
        }
        if let Some(t) = out_text.filter(|_| show_out) {
            ui.add_space(8.0);
            ui.label(RichText::new(t).font(theme::font(12.5)).color(if big { p.warn } else { p.muted }));
        }
    });
    if let Some(page) = open {
        settings_dialog::open(app, page);
    }
}

/// 主畫面攝影機按鈕的文字
fn camera_label(name: &str) -> String {
    if name.is_empty() {
        return tr!("攝影機：關", "Camera: off").into();
    }
    // 名稱太長時只留前面
    let short: String = name.chars().take(18).collect();
    if short.chars().count() < name.chars().count() {
        format!("{short}…")
    } else {
        short
    }
}

/// 攝影機選單：不使用、找到的攝影機、重新尋找、更多設定；回傳是否要開啟設定視窗
fn camera_menu(app: &mut UiApp, ui: &mut Ui) -> bool {
    ui.set_min_width(220.0);
    let mut pick: Option<String> = None;
    if ui.radio(app.s.camera.is_empty(), tr!("不使用", "None")).clicked() {
        pick = Some(String::new());
    }
    match app.cameras.clone() {
        None => {
            ui.add_enabled(false, egui::Label::new(tr!("正在尋找攝影機…", "Looking for cameras…")));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        Some(list) => {
            for name in &list {
                if ui.radio(app.s.camera == *name, name.as_str()).clicked() {
                    pick = Some(name.clone());
                }
            }
            // 選過、但現在找不到的攝影機（拔掉了）
            if !app.s.camera.is_empty() && !list.contains(&app.s.camera) {
                let _ = ui.radio(true, trf!("{}（找不到）", "{} (not found)", app.s.camera));
            }
            if list.is_empty() {
                ui.add_enabled(false, egui::Label::new(if cfg!(windows) { tr!("沒有找到攝影機", "No camera found") } else { tr!("攝影機只支援 Windows", "Camera is supported on Windows only") }));
            }
        }
    }
    ui.separator();
    if ui.add_enabled(app.cameras.is_some(), egui::Button::new(tr!("重新尋找攝影機", "Search for cameras again"))).clicked() {
        app.load_cameras(true);
    }
    let more = ui.button(tr!("位置、大小與形狀…", "Position, size and shape…")).clicked();
    if let Some(name) = pick {
        app.s.camera = name;
        app.save_settings();
        ui.close();
    }
    if more {
        ui.close();
    }
    more
}

/// 錄完自動處理中：一張卡片寫出第幾步、做什麼、進度，只有一個「取消」（轉檔的部分不另外顯示轉檔卡）
fn after_record_strip(app: &mut UiApp, ui: &mut Ui) {
    let Some(s) = app.core.after_status() else { return };
    let p = theme::pal(ui);
    // 轉檔的步驟用轉檔工作的進度
    let export = app.status.export.clone().filter(|e| s.exporting && e.state == ExportState::Running);
    let frac = match &export {
        Some(e) => e.progress as f32,
        None => s.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0,
    };
    let recording = app.locked();
    ui.add_space(6.0);
    egui::Frame::new().fill(p.accent.gamma_multiply(0.1)).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let title = if s.step_no == 0 || s.step.is_empty() {
            tr!("錄完自動處理：等待中", "Post-processing: waiting").to_string()
        } else {
            let pct = if frac > 0.0 { format!(" {}%", (frac * 100.0).floor()) } else { String::new() };
            let step = if s.steps > 1 { trf!("（步驟 {}/{}）", " (step {}/{})", s.step_no, s.steps) } else { String::new() };
            match s.batch {
                Some((i, n)) => trf!("一次處理（第 {i}/{n} 支）：{}{pct}", "Processing {i} of {n}: {}{pct}", s.step),
                None => trf!("錄完自動處理{step}：{}{pct}", "Post-processing{step}: {}{pct}", s.step),
            }
        };
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(tr!("取消", "Cancel"))
                    .icon(Icon::Close)
                    .ghost()
                    .small()
                    .tooltip(if s.batch.is_some() {
                        tr!("取消（這一支與還沒開始的都不做了；已經做好的會留著）", "Cancel this one and the rest (finished results are kept)")
                    } else {
                        tr!("取消這支錄影的自動處理（已經做好的會留著）", "Cancel post-processing for this recording (finished steps are kept)")
                    })
                    .show(ui)
                    .clicked()
                {
                    app.core.cancel_after();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&title).color(p.accent).font(theme::font_bold(13.0))).wrap());
                });
            });
        });
        ui.add(egui::Label::new(theme::muted(ui, &s.name)).truncate());
        if s.waiting && recording {
            ui.label(theme::muted(ui, tr!("錄影中先暫停，錄完後繼續處理", "Paused while recording; it continues when you stop")));
        } else if s.waiting {
            ui.label(theme::muted(ui, tr!("等其他轉檔工作完成", "Waiting for another conversion to finish")));
        } else if frac > 0.0 {
            theme::progress(ui, frac, p.accent, 4.0);
        }
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
}

/// 錄完自動處理做完了：「已自動處理：…」，可以播放處理好的版本、在資料夾中顯示，按 ✕ 關掉
fn after_done_card(app: &mut UiApp, ui: &mut Ui) {
    let Some(d) = app.core.after_done() else { return };
    if app.main.dismissed_after == Some(d.seq) || app.core.after_status().is_some() {
        return;
    }
    let p = theme::pal(ui);
    ui.add_space(8.0);
    let (fill, stroke) = if d.ok { (p.ok_soft, p.ok) } else { (p.rec_soft, p.rec) };
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke.gamma_multiply(0.4))).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let title = match (d.batch, d.ok) {
            (true, true) => tr!("一次處理完成", "Batch processing done"),
            (true, false) => tr!("一次處理沒有完成", "Batch processing didn't finish"),
            (false, true) => tr!("已自動處理", "Post-processing done"),
            (false, false) => tr!("自動處理沒有完成", "Post-processing didn't finish"),
        };
        if card_title(ui, title, if d.ok { p.ok } else { p.rec }) {
            app.main.dismissed_after = Some(d.seq);
        }
        ui.add(egui::Label::new(RichText::new(&d.text).font(theme::font(12.5))).wrap());
        let target = d.output.clone().unwrap_or_else(|| d.source.clone());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            if let Some(out) = &d.output {
                if Btn::new(tr!("播放處理好的版本", "Play the processed version")).icon(Icon::Play).small().show(ui).clicked() {
                    app.act_path(EntryAction::PlayHere, out.clone());
                }
            }
            if Btn::new(tr!("在資料夾中顯示", "Show in folder")).icon(Icon::Folder).small().show(ui).clicked() {
                app.act_path(EntryAction::Reveal, target.clone());
            }
        });
    });
}

// ───────────── 錄影狀態 ─────────────

pub(super) fn live_recorded_ms(app: &UiApp) -> f64 {
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
        // 視窗矮（小螢幕、介面放大）時計時器的字小一點，開始錄影、截圖的按鈕才放得下
        let clock_size = match ui.available_height() {
            h if h < 340.0 => 30.0,
            h if h < 420.0 => 38.0,
            _ => 46.0,
        };
        let r = app.status.recorder.clone();
        let active = r.state != RecorderState::Idle;
        // 回溯錄影中（不是一般錄影）
        let replay = r.replay.clone().filter(|_| active);
        // 狀態
        ui.horizontal(|ui| {
            let (text, tone) = match r.state {
                RecorderState::Recording if replay.is_some() => (tr!("● 回溯錄影中", "● Replay buffer"), Tone::Bad),
                RecorderState::Idle => (tr!("待命", "Ready"), Tone::Plain),
                RecorderState::Countdown => (tr!("倒數中", "Countdown"), Tone::Warn),
                RecorderState::Recording => (tr!("● 錄影中", "● Recording"), Tone::Bad),
                RecorderState::Paused if r.auto_paused => (tr!("沒動靜，已自動暫停", "Idle, paused"), Tone::Warn),
                RecorderState::Paused => (tr!("已暫停", "Paused"), Tone::Warn),
                RecorderState::Stopping => (tr!("處理中", "Processing"), Tone::Accent),
            };
            chip(ui, text, tone, false);
            let mut busy = r.busy.clone().unwrap_or_default();
            if busy.is_empty() && r.auto_paused {
                busy = tr!("動一下滑鼠或按鍵就繼續", "Move the mouse or press a key to continue").into();
            }
            if busy.is_empty() && r.max_ms > 0 && active {
                busy = trf!("剩餘 {}", "{} left", clock((r.max_ms as f64 - live_recorded_ms(app)).max(0.0)));
            }
            if let Some(rp) = replay.as_ref().filter(|_| busy.is_empty()) {
                busy = if rp.saving { tr!("正在存…", "Saving…").into() } else { trf!("保留最近 {}", "Keeping the last {}", screenrecorder_core::replay::length_label(rp.sec)) };
            }
            // 右上角：「更多工具」（回溯錄影、排程、講稿、設定組合：不常用的收在這裡，主畫面保持簡單）
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                more_tools(app, ui, active);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(theme::muted(ui, busy)).truncate());
                });
            });
        });
        // 有新版本：標題下方一個明顯的「更新到 vX.Y.Z」
        if let Some(u) = app.env.update.clone().filter(|_| !active) {
            ui.add_space(4.0);
            let busy = update_progress(app);
            let label = busy.clone().unwrap_or_else(|| if can_self_update(&u) { trf!("更新到 v{}", "Update to v{}", u.version) } else { trf!("下載新版 v{}", "Download v{}", u.version) });
            let tip = if can_self_update(&u) {
                tr!("自動下載、核對後重新啟動；設定與錄影都會保留", "Downloads and verifies the update, then restarts. Your settings and recordings are kept")
            } else {
                tr!("開啟下載頁面", "Open the download page")
            };
            if Btn::new(label).icon(Icon::Refresh).primary().small().enabled(busy.is_none()).min_width(ui.available_width()).tooltip(tip).show(ui).clicked() {
                ask_update(app, &u);
            }
        }
        // 計時器：錄影時才出現（待命時是 00:00:00，沒有意義）
        if active {
            ui.add_space(4.0);
            let ms = live_recorded_ms(app);
            ui.label(RichText::new(clock(ms)).font(theme::mono(clock_size)).color(if r.state == RecorderState::Recording { p.rec } else { p.text }));
            if r.max_ms > 0 && r.state != RecorderState::Countdown {
                theme::progress(ui, (ms / r.max_ms as f64) as f32, p.rec, 4.0);
            }
        }
        ui.add_space(6.0);
        // 中間（統計、控制按鈕、提示、結果、快速鍵 / 事件紀錄）放不下時可以捲動：系統狀態一定在最下面，不會蓋住按鈕
        let full = ui.max_rect();
        let body = Rect::from_min_max(ui.cursor().min, pos2(full.max.x, full.max.y - BOTTOM_BAR_H - BOTTOM_LINE_GAP - 6.0));
        // 高度不夠（視窗矮、介面放大）：省略比較不重要的列
        // 0 = 放得下；1 = 有點擠（省略不重要的列、列高縮小）；2 = 很擠（只留聲音一列，開始錄影的按鈕才看得到）
        let tight = if body.height() < 180.0 {
            2
        } else if body.height() < 330.0 {
            1
        } else {
            0
        };
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            // 捲軸佔自己的寬度，不蓋在按鈕上
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::vertical().id_salt("rec-body").auto_shrink([false, false]).max_height(body.height()).show(ui, |ui| {
                rec_body(app, ui, &r, replay.as_ref(), tight);
            });
        });
        let bottom = Rect::from_min_max(pos2(full.min.x, full.max.y - BOTTOM_BAR_H), full.max);
        ui.painter().hline(bottom.x_range(), bottom.min.y - BOTTOM_LINE_GAP, Stroke::new(1.0, p.border));
        ui.scope_builder(UiBuilder::new().max_rect(bottom).layout(Layout::left_to_right(Align::Center)), |ui| sys_status(app, ui));
    });
}

/// 「更多工具 ▾」：回溯錄影、排程錄影、講稿小視窗、設定組合（有組合時設定組合也在下方設定列）
fn more_tools(app: &mut UiApp, ui: &mut Ui, active: bool) {
    let notes_open = app.notes.is_some();
    let resp = Btn::new(tr!("更多工具", "More tools"))
        .icon(Icon::More)
        .ghost()
        .small()
        .menu()
        .tooltip(tr!("回溯錄影、排程錄影、講稿小視窗、設定組合", "Replay buffer, scheduled recording, script window, presets"))
        .show(ui);
    let mut schedule = false;
    let mut notes = false;
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(220.0);
        if !active {
            let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && app.manual_export().is_none();
            // 不能用時寫出原因（還沒下載錄影元件、轉檔中）
            let why = if !(app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some()) {
                tr!("請先下載錄影元件", "Download the recording component first")
            } else {
                tr!("轉檔中，完成後再開始", "Wait for the conversion to finish")
            };
            ui.add_enabled_ui(can, |ui| {
                ui.menu_button(tr!("回溯錄影（錄最近幾分鐘）", "Replay buffer (last few minutes)"), |ui| super::replay::menu_body(app, ui));
            })
            .response
            .on_disabled_hover_text(why);
            if ui
                .button(tr!("排程錄影…", "Schedule recording…"))
                .on_hover_text(tr!("指定時間自動開始錄影，也可以設定錄幾分鐘後自動停止", "Start recording automatically at a set time, optionally stopping after a set length"))
                .clicked()
            {
                schedule = true;
                ui.close();
            }
        }
        if ui
            .button(if notes_open { tr!("關閉講稿小視窗", "Close script window") } else { tr!("講稿小視窗", "Script window") })
            .on_hover_text(tr!(
                "浮在最上層、不會被錄進影片；可以用大字自動往上捲（提詞），開始錄影時自動開始",
                "Stays on top and isn't recorded. It can show your script in large text that scrolls up automatically (teleprompter), starting when you record."
            ))
            .clicked()
        {
            notes = true;
            ui.close();
        }
        if !active && app.presets.is_empty() {
            ui.add_enabled_ui(!app.locked(), |ui| {
                ui.menu_button(tr!("設定組合", "Presets"), |ui| super::presets::menu_body(app, ui));
            });
        }
    });
    if schedule {
        super::schedule::open(app);
    }
    if notes {
        super::notes::toggle(app);
    }
}

/// 錄影面板中間可以捲動的部分
fn rec_body(app: &mut UiApp, ui: &mut Ui, r: &screenrecorder_core::types::RecorderStatus, replay: Option<&screenrecorder_core::types::ReplayStatus>, tight: u8) {
    let p = theme::pal(ui);
    let active = r.state != RecorderState::Idle;
    // 統計
    let (out, method, audio) = if active {
        (
            trf!("{}×{}・每秒 {} 張", "{}×{} · {} fps", r.out_width, r.out_height, r.fps),
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
        let system = if app.s.audio_app.is_empty() { tr!("系統聲音", "System audio").to_string() } else { app.s.audio_app.clone() };
        let parts: Vec<String> = [app.s.audio_system.then_some(system), app.s.audio_mic.then(|| tr!("麥克風", "Microphone").to_string())].into_iter().flatten().collect();
        (
            if app.s.source_type == SourceType::Audio {
                tr!("只錄聲音", "Audio only").into()
            } else {
                o.map(|(w, h)| trf!("{w}×{h}・每秒 {} 張", "{w}×{h} · {} fps", app.s.fps)).unwrap_or_else(|| "—".into())
            },
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
    let mut keys = tr!(["影片長度", "檔案大小", "輸出", "錄影方式", "聲音"], ["Duration", "File size", "Output", "Method", "Audio"]);
    let mut values = [video_clock(r.video_sec), if active || r.bytes > 0 { format_bytes(r.bytes) } else { "—".into() }, out, method, audio];
    if let Some(rp) = replay {
        // 回溯錄影：緩衝裡有幾秒（最多保留的長度）、在硬碟上佔多少
        keys[0] = tr!("已保留", "Buffer");
        keys[1] = tr!("暫存大小", "Temp size");
        values[0] = video_clock((live_recorded_ms(app) / 1000.0).min(rp.sec as f64));
        values[1] = format_bytes(rp.bytes);
    }
    // 高度不夠時省略比較不重要的列：待命時的影片長度、檔案大小（都還是 0）；錄影中的影片長度（和上面的計時差不多）、錄影方式；
    // 很擠的時候輸出、錄影方式也省略（下方摘要與滑鼠提示有）。只錄聲音時沒有「錄影方式」
    let audio_only = app.s.source_type == SourceType::Audio;
    // 待命時只留「聲音」一列（還沒錄影，長度、大小都是 0）；錄影中才列出數據
    let show = |i: usize| match i {
        _ if !active => i == 4,
        0 => tight == 0 || replay.is_some(),
        1 => tight == 0 || active,
        2 => tight < 2,
        3 => !(tight > 0 && active) && tight < 2 && !audio_only,
        _ => true,
    };
    // 擠的時候每一列矮一點
    let row_h = ui.spacing().interact_size.y;
    if tight > 0 {
        ui.spacing_mut().interact_size.y = 22.0;
    }
    let key_w = keys.iter().enumerate().filter(|(i, _)| show(*i)).map(|(_, k)| ui.painter().layout_no_wrap(k.to_string(), theme::font(13.0), p.muted).size().x).fold(0.0, f32::max);
    for (i, (k, v)) in keys.into_iter().zip(values).enumerate() {
        if !show(i) {
            continue;
        }
        // 最後一列是「聲音」
        let audio_row = i == 4;
        let row = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let (cell, _) = ui.allocate_exact_size(vec2(key_w, 18.0), Sense::hover());
            ui.painter().text(cell.left_center(), egui::Align2::LEFT_CENTER, k, theme::font(13.0), p.muted);
            // 錄影中聲音來源打不開或中斷（改錄靜音）：紅色麥克風與說明
            if audio_row && active && r.silent_audio != 0 {
                let (icon, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                theme::paint_icon(ui.painter(), icon, if r.silent_audio & SILENT_MIC != 0 { Icon::Mic } else { Icon::Speaker }, p.rec);
                let text = silent_text(r.silent_audio);
                ui.add(egui::Label::new(RichText::new(text).font(theme::font(13.0)).color(p.rec)).truncate()).on_hover_text(trf!("{text}（錄到的是靜音）\n{v}", "{text} (recording silence)\n{v}"));
                return;
            }
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
        if audio_row {
            tour::mark(ui, Spot::Audio, row.response.rect.expand2(vec2(4.0, 2.0)));
        }
    }
    ui.spacing_mut().item_spacing.y = gap_y;
    ui.spacing_mut().interact_size.y = row_h;
    ui.add_space(if tight > 0 { 6.0 } else { 10.0 });
    // 控制按鈕
    ui.horizontal(|ui| {
        if !active {
            // 錄完自動處理不擋錄影（錄影時先暫停，錄完再繼續）；手動開始的轉檔要先停止（下方的按鈕）
            let manual = app.manual_export();
            let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && manual.is_none() && !region_too_small(app);
            let tip = if !app.env.ffmpeg.found {
                tr!("請先下載錄影元件（上方的「下載並開始使用」）", "Download the recording component first (“Download and get started” above)").to_string()
            } else if region_too_small(app) {
                tr!("範圍太小：寬高至少 16 像素", "The area is too small: it must be at least 16 pixels wide and tall").to_string()
            } else if let Some(e) = &manual {
                trf!(
                    "正在{}，完成後才能錄影；也可以按下方的「停止處理並開始錄影」",
                    "{} is in progress. You can record when it finishes, or click “Stop it and start recording” below",
                    actions::job_label(e)
                )
            } else if app.core.after_status().is_some() {
                tr!("錄完自動處理會先暫停，錄完再繼續", "Post-processing pauses while you record and continues afterwards").to_string()
            } else {
                String::new()
            };
            // 只錄聲音：「開始錄音」
            let label = if app.s.source_type == SourceType::Audio { tr!("開始錄音", "Start audio recording") } else { tr!("開始錄影", "Start recording") };
            // 快速鍵直接寫在按鈕上（可以用時）
            let k = app.keys.label(0);
            let label = if k.is_empty() || !app.env.hotkeys.is_some_and(|h| h.all().first().copied().unwrap_or(false)) { label.to_string() } else { format!("{label}　{k}") };
            let start = Btn::new(label).kind(theme::Kind::Record).enabled(can).tooltip(tip).min_width(ui.available_width()).show(ui);
            tour::mark(ui, Spot::Record, start.rect);
            if start.clicked() {
                let config = app.s.record_config(&app.env);
                let core = app.core.clone();
                app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
            }
        } else if let Some(rp) = replay {
            // 回溯錄影：存下最近 N 分鐘、停止（不能暫停、不能加標記）
            let w = ui.available_width();
            let key = app.keys.label(6);
            let tip = if key.is_empty() { String::new() } else { trf!("快速鍵 {key}", "Shortcut {key}") };
            let label = trf!("存下最近 {}", "Save the last {}", screenrecorder_core::replay::length_label(rp.sec));
            if Btn::new(label).icon(Icon::Export).primary().enabled(!rp.saving && r.state == RecorderState::Recording && r.busy.is_none()).min_width(w * 0.62).tooltip(tip).show(ui).clicked() {
                super::replay::save(app);
            }
            if Btn::new(tr!("停止", "Stop"))
                .icon(Icon::Stop)
                .danger()
                .enabled(r.state != RecorderState::Stopping && !rp.saving)
                .min_width(ui.available_width())
                .tooltip(tr!("停止回溯錄影（沒存下的部分會刪掉）", "Stop the replay buffer (anything not saved is discarded)"))
                .show(ui)
                .clicked()
            {
                super::replay::ask_stop(app, rp, live_recorded_ms(app));
            }
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
            // 麥克風靜音 / 取消靜音（這次有錄麥克風時）
            if let Some(muted) = r.mic_muted {
                let key = app.keys.label(7);
                let mk = if key.is_empty() { String::new() } else { trf!("（{key}）", " ({key})") };
                let tip = if muted {
                    trf!("麥克風靜音中，點一下取消靜音{mk}", "The microphone is muted. Click to unmute{mk}")
                } else {
                    trf!(
                        "暫時把麥克風靜音（咳嗽、接電話時），剪輯時會在時間軸上標出來{mk}",
                        "Mute the microphone for a moment (to cough or take a call). Muted parts are marked on the editing timeline{mk}"
                    )
                };
                if Btn::icon_only(Icon::Mic).selected(muted).tooltip(tip).show(ui).clicked() {
                    match app.core.recorder.toggle_mic_mute() {
                        Ok(m) => app.core.mic_mute_changed(m),
                        Err(e) => app.toast(e.message().to_string(), true),
                    }
                }
            }
            let stop_label = if r.state == RecorderState::Countdown {
                if r.countdown_covers_ui == Some(false) {
                    trf!("{} 秒後開始・取消", "Starts in {} s · Cancel", countdown_left(app))
                } else {
                    tr!("取消倒數（Esc）", "Cancel countdown (Esc)").into()
                }
            } else {
                tr!("停止", "Stop").into()
            };
            let stop = Btn::new(stop_label).icon(Icon::Stop).danger().enabled(r.state != RecorderState::Stopping).min_width(ui.available_width()).show(ui);
            tour::mark(ui, Spot::Record, stop.rect);
            if stop.clicked() {
                let core = app.core.clone();
                app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
            }
        }
    });
    // 麥克風靜音中：按鈕下面寫清楚（錄到的是靜音）
    if active && r.mic_muted == Some(true) {
        ui.add_space(4.0);
        ui.label(RichText::new(tr!("🔇 麥克風靜音中，這段不會錄到你的聲音", "🔇 Microphone muted. Your voice isn't being recorded")).color(p.rec).font(theme::font(12.5)));
    }
    // 截圖（只錄聲音時不顯示）：框選截圖、截取錄影範圍、更多
    if !active && app.s.source_type != SourceType::Audio {
        ui.add_space(6.0);
        shot_row(app, ui);
    }
    // 手動開始的轉檔還在進行：說明為什麼不能錄，可以停止它直接開始錄影
    if let Some(e) = app.manual_export().filter(|_| !active) {
        ui.add_space(6.0);
        let what = actions::job_label(&e);
        ui.add(egui::Label::new(theme::muted(ui, trf!("正在{what}（{}%），完成後才能錄影", "{what} ({}%). You can record when it finishes", (e.progress * 100.0).floor()))).wrap());
        if Btn::new(tr!("停止處理並開始錄影", "Stop it and start recording")).icon(Icon::Stop).small().enabled(app.env.ffmpeg.encoder.is_some()).show(ui).clicked() {
            let config = app.s.record_config(&app.env);
            let core = app.core.clone();
            app.toast(trf!("已停止{what}，錄完後可以再做一次", "{what} was stopped. You can do it again after recording"), false);
            app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
        }
    }
    // 排程錄影：什麼時候開始、還有多久、取消
    if !active {
        super::schedule::strip(app, ui);
    }
    after_record_strip(app, ui);
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
                        .tooltip(tr!("停止截圖，整理步驟後做成教學文件", "Stop capturing, organize the steps, then make the step-by-step guide"))
                        .show(ui)
                        .clicked()
                    {
                        let core = app.core.clone();
                        app.spawn(async move { core.steps_finish().await }, |app, r| match r {
                            // 有步驟時會開啟整理步驟的視窗
                            Ok(true) => {}
                            Ok(false) => app.toast(tr!("沒有截到任何步驟", "No steps were captured"), false),
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
            "電腦跟不上：實際每秒約 {} 張（設定 {} 張），建議調低解析度或每秒張數",
            "The computer can't keep up: about {} fps (set to {}). Try a lower resolution or frame rate",
            r.actual_fps.map(|f| format!("{f:.1}")).unwrap_or_else(|| "?".into()),
            r.fps
        ));
    }
    if !msgs.is_empty() {
        ui.add_space(6.0);
        egui::Frame::new().fill(p.warn_soft).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(msgs.join("　")).color(p.warn)).wrap());
            // 跟不上：一鍵改成每秒 15 張（這次錄影不變，之後的錄影用）
            if r.slow
                && app.s.fps > 15.0
                && Btn::new(tr!("改成每秒 15 張", "Use 15 fps"))
                    .small()
                    .tooltip(tr!("之後的錄影改成每秒 15 張；這次錄影不變", "Future recordings use 15 fps; this recording isn't changed"))
                    .show(ui)
                    .clicked()
            {
                app.s.fps = 15.0;
                app.save_settings();
                app.toast(tr!("之後的錄影改成每秒 15 張（這次錄影不變）", "Future recordings will use 15 fps (this recording isn't changed)"), false);
            }
        });
    }
    job_card(app, ui);
    // 錄影結果（可以按 ✕ 關掉）
    if let Some(res) = r.result.clone().filter(|_| !active) {
        let key = res.path.clone().unwrap_or_else(|| res.message.clone());
        if app.main.dismissed_result.as_deref() != Some(key.as_str()) {
            ui.add_space(8.0);
            result_card(app, ui, &res, key);
        }
    }
    after_done_card(app, ui);
    // 快速鍵小抄或事件紀錄（放不下時整個中間部分可以捲動）
    if r.log.is_empty() && !active {
        // 還沒有事件：一行寫截圖的快速鍵（開始錄影的寫在按鈕上），其他的在設定裡
        ui.add_space(8.0);
        shortcut_line(app, ui);
    } else {
        // 事件紀錄：預設收起來，按標題展開；收起時標出還沒看過的警告與錯誤（只要一行，空間不夠時也顯示）
        ui.add_space(10.0);
        let open = app.s.show_log;
        if open {
            app.main.log_seen = r.log.last().map_or(0, |l| l.t);
        }
        let unseen = |level: LogLevel| r.log.iter().filter(|l| l.t > app.main.log_seen && l.level == level).count();
        let (errors, warns) = (unseen(LogLevel::Error), unseen(LogLevel::Warn));
        let row = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (icon, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                theme::paint_icon(ui.painter(), icon, if open { Icon::ChevD } else { Icon::ChevR }, p.muted);
                ui.label(RichText::new(tr!("事件紀錄", "Event log")).font(theme::font_bold(12.5)).color(p.muted));
                if !open && errors + warns > 0 {
                    let mut parts = Vec::new();
                    if errors > 0 {
                        parts.push(if is_en() { format!("{errors} error{}", if errors == 1 { "" } else { "s" }) } else { format!("{errors} 則錯誤") });
                    }
                    if warns > 0 {
                        parts.push(if is_en() { format!("{warns} warning{}", if warns == 1 { "" } else { "s" }) } else { format!("{warns} 則警告") });
                    }
                    let color = if errors > 0 { p.rec } else { p.warn };
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(RichText::new(format!("・{}", parts.join(tr!("、", ", ")))).font(theme::font(12.5)).color(color));
                }
                ui.add_space(ui.available_width());
            })
            .response;
        let tip = if open {
            tr!("收起事件紀錄", "Hide the event log")
        } else {
            tr!("展開事件紀錄：錄影過程的訊息、警告與錯誤", "Show the event log: messages, warnings and errors while recording")
        };
        if ui.interact(row.rect, ui.id().with("log_toggle"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
            app.s.show_log = !open;
            app.save_settings();
        }
        if open {
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
        }
    }
}

/// 快速鍵一行：框選截圖、截取錄影範圍的按鍵與「全部快速鍵…」；有不能用的時候標出來
fn shortcut_line(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let works = app.env.hotkeys.map(|h| h.all());
    let ok = |i: usize| works.is_some_and(|w| w.get(i).copied().unwrap_or(false));
    // 常用的：開始 / 停止錄影、截圖（範圍）、框選截圖、暫停 / 繼續、回溯錄影
    let broken = [0usize, 1, 2, 3, 6].iter().filter(|&&i| !app.keys.label(i).is_empty() && !ok(i)).count();
    let parts: Vec<String> = [(3usize, tr!("框選", "Select")), (2, tr!("截取範圍", "Capture area"))]
        .into_iter()
        .filter(|(i, _)| ok(*i) && !app.keys.label(*i).is_empty())
        .map(|(i, n)| format!("{} {n}", app.keys.label(i)))
        .collect();
    let mut open_keys = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // 右邊：「全部快速鍵…」；有不能用的時候直接寫幾個不能用（點一下去換）
        let tip = match works {
            _ if broken == 0 => tr!("在「設定 → 快速鍵」查看與更換", "View and change them in Settings → Shortcuts"),
            None => tr!("系統匣沒有啟動，全域快速鍵無法使用", "The tray icon isn't running, so global shortcuts don't work"),
            Some(_) => tr!("被其他程式占用，點一下到「設定 → 快速鍵」換一組", "Used by another program. Click to choose a different one in Settings → Shortcuts"),
        };
        let label = if broken > 0 { trf!("{broken} 個快速鍵無法使用…", "{broken} shortcut(s) unavailable…") } else { tr!("全部快速鍵…", "All shortcuts…").to_string() };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new(label).ghost().small().tooltip(tip).show(ui).clicked() {
                open_keys = true;
            }
            // 左邊：常用的快速鍵，先放按鈕再用剩下的寬度，放不下就截斷，不會和按鈕疊在一起
            if !parts.is_empty() {
                let text = parts.join(tr!("・", " · "));
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&text).font(theme::mono(12.0)).color(p.muted)).truncate()).on_hover_text(&text);
                });
            }
        });
    });
    if open_keys {
        settings_dialog::open(app, Page::Keys);
    }
}

/// 截圖的方式（截圖那一列的按鈕與選單、系統匣的「截圖」選單相同的項目與順序）
#[derive(Clone, PartialEq)]
pub(super) enum ShotAct {
    /// 在螢幕上框選範圍或點選視窗；幾秒後再框選
    Select(u64),
    /// 目前的錄影範圍
    Area,
    /// 整個螢幕（None = 所有螢幕）
    Screen(Option<String>),
    /// 重複上次框選的範圍
    Last,
    Scroll,
    Steps,
    EditLast,
    /// 只抓文字（框選後辨識，複製到剪貼簿）
    Text,
    Pen,
}

/// 截圖那一列：「框選截圖」「截取錄影範圍」與更多的小箭頭（按右鍵也有同一個選單）；下面一行寫快速鍵
fn shot_row(app: &mut UiApp, ui: &mut Ui) {
    let found = app.env.ffmpeg.found;
    let can = found && !app.main.shooting;
    let need = tr!("請先下載錄影元件（上方的「下載並開始使用」）", "Download the recording component first (“Download and get started” above)").to_string();
    let key = |i: usize| {
        let k = app.keys.label(i);
        if k.is_empty() || !app.env.hotkeys.is_some_and(|h| h.all().get(i).copied().unwrap_or(false)) {
            String::new()
        } else {
            trf!("（{k}）", " ({k})")
        }
    };
    let select_tip = if found {
        trf!("在螢幕上框選範圍或點一下視窗，截下來存成 PNG 並複製到剪貼簿{}", "Select an area or click a window on the screen to save it as PNG and copy it to the clipboard{}", key(3))
    } else {
        need.clone()
    };
    let area_tip = if found {
        trf!("截下目前的錄影範圍（{}），存成 PNG 並複製到剪貼簿{}", "Capture the current recording area ({}) as PNG and copy it to the clipboard{}", app.s.source_label(&app.env), key(2))
    } else {
        need.clone()
    };
    let mut act: Option<ShotAct> = None;
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let arrow_w = 34.0;
        let w = ui.available_width() - arrow_w - 8.0;
        let select = Btn::new(tr!("框選截圖", "Select area")).icon(Icon::Camera).enabled(can).tooltip(select_tip).min_width((w * 0.55).floor()).show(ui);
        let area = Btn::new(tr!("截取錄影範圍", "Capture area")).enabled(can).tooltip(area_tip).min_width(ui.available_width() - arrow_w - 4.0).show(ui);
        let arrow = Btn::icon_only(Icon::ChevD)
            .enabled(can)
            .tooltip(if found {
                tr!(
                    "更多截圖方式與小工具：延遲框選、全螢幕、長截圖、步驟截圖、只抓文字、螢幕畫筆",
                    "More screenshot options and tools: delayed selection, full screen, scrolling screenshot, step capture, grab text, screen pen"
                )
                .to_string()
            } else {
                need.clone()
            })
            .min_width(arrow_w)
            .show(ui);
        if select.clicked() {
            act = Some(ShotAct::Select(0));
        }
        if area.clicked() {
            act = Some(ShotAct::Area);
        }
        for r in [&select, &area] {
            egui::Popup::context_menu(r).show(|ui| shot_menu(app, ui, &mut act));
        }
        egui::Popup::menu(&arrow).show(|ui| shot_menu(app, ui, &mut act));
    });
    tour::mark(ui, Spot::Shot, row.response.rect);
    if let Some(a) = act.filter(|_| can) {
        run_shot(app, a);
    }
}

/// 截圖選單：「截圖方式」「小工具」兩組（與系統匣的「截圖」選單相同的項目、順序與文字）
fn shot_menu(app: &UiApp, ui: &mut Ui, act: &mut Option<ShotAct>) {
    let p = theme::pal(ui);
    ui.set_min_width(250.0);
    let heading = |ui: &mut Ui, t: &str| {
        ui.label(RichText::new(t).font(theme::font_bold(12.0)).color(p.muted));
    };
    let item = |ui: &mut Ui, label: String, key: usize, enabled: bool| -> bool {
        let k = app.keys.label(key);
        let b = egui::Button::new(label).shortcut_text(RichText::new(k).font(theme::mono(11.5)));
        ui.add_enabled(enabled, b).clicked()
    };
    let steps_on = app.status.steps.is_some();
    heading(ui, tr!("截圖方式", "Screenshot"));
    if item(ui, tr!("框選範圍或視窗…", "Select area or window…").into(), 3, true) {
        *act = Some(ShotAct::Select(0));
    }
    ui.menu_button(tr!("延遲框選", "Delayed selection"), |ui| {
        for sec in [3u64, 5, 10] {
            if ui
                .button(trf!("{sec} 秒後", "In {sec} seconds"))
                .on_hover_text(tr!("先打開要截的選單或提示，時間到畫面會凍結讓你框選", "Open the menu or tooltip you want to capture first; the screen freezes for selection when time is up"))
                .clicked()
            {
                *act = Some(ShotAct::Select(sec));
            }
        }
    });
    if item(ui, trf!("整個錄影範圍：{}", "Whole recording area: {}", app.s.source_label(&app.env)), 2, true) {
        *act = Some(ShotAct::Area);
    }
    ui.add_enabled_ui(!app.env.monitors.is_empty(), |ui| {
        ui.menu_button(tr!("全螢幕", "Full screen"), |ui| {
            for m in &app.env.monitors {
                let label = trf!("螢幕 {}{}　{}×{}", "Screen {}{}   {}×{}", m.display_number, if m.primary { tr!("（主螢幕）", " (primary)") } else { "" }, m.width, m.height);
                if ui.button(label).clicked() {
                    *act = Some(ShotAct::Screen(Some(m.id.clone())));
                }
            }
            if app.env.monitors.len() > 1 {
                ui.separator();
                if ui.button(tr!("所有螢幕（整個延伸桌面）", "All screens (entire extended desktop)")).clicked() {
                    *act = Some(ShotAct::Screen(None));
                }
            }
        });
    });
    if ui.add_enabled(app.core.last_snip().is_some(), egui::Button::new(tr!("重複上次框選", "Repeat last selection"))).clicked() {
        *act = Some(ShotAct::Last);
    }
    if ui
        .button(tr!("長截圖（捲動）…", "Scrolling screenshot…"))
        .on_hover_text(tr!(
            "框選要捲動的內容（例如網頁），自動往下捲並接成一張長圖；按 Esc 停止",
            "Select the content to scroll (e.g. a web page); it scrolls down automatically and stitches everything into one long image. Press Esc to stop."
        ))
        .clicked()
    {
        *act = Some(ShotAct::Scroll);
    }
    if ui
        .add_enabled(!steps_on, egui::Button::new(tr!("步驟截圖（做成教學文件）", "Step capture (step-by-step guide)")))
        .on_hover_text(tr!(
            "開始後每點一下滑鼠就截一張，標出點的位置；完成時做成一份圖文並茂的教學文件",
            "Once started, every mouse click takes a screenshot with the click marked. When you finish, it becomes an illustrated step-by-step guide."
        ))
        .clicked()
    {
        *act = Some(ShotAct::Steps);
    }
    if ui.add_enabled(app.core.last_shot().is_some(), egui::Button::new(tr!("編輯上次截圖…", "Edit last screenshot…"))).clicked() {
        *act = Some(ShotAct::EditLast);
    }
    ui.separator();
    heading(ui, tr!("小工具", "Tools"));
    if ui
        .add(egui::Button::new(tr!("只抓文字…", "Grab text…")).shortcut_text(RichText::new(app.keys.label(9)).font(theme::mono(11.5))))
        .on_hover_text(tr!(
            "框選畫面上的字（圖片、影片、不能選取的視窗也可以），直接複製到剪貼簿，不存圖；表格可以貼到 Excel",
            "Select text anywhere on the screen (even in images, videos or windows that don't allow selection) to copy it straight to the clipboard without saving an image; tables paste into Excel"
        ))
        .clicked()
    {
        *act = Some(ShotAct::Text);
    }
    if item(ui, tr!("螢幕畫筆", "Screen pen").into(), 5, true) {
        *act = Some(ShotAct::Pen);
    }
}

/// 執行截圖選單選的動作（完成後由狀態更新顯示提示、更新清單）
pub(super) fn run_shot(app: &mut UiApp, act: ShotAct) {
    use screenrecorder_core::types::SourceConfig;
    let mut config = app.s.record_config(&app.env);
    let core = app.core.clone();
    let done = |app: &mut UiApp, r: screenrecorder_core::Result<()>| {
        app.main.shooting = false;
        if let Err(e) = r {
            app.toast(e.message().to_string(), true);
        }
    };
    match act {
        ShotAct::Select(sec) => {
            app.main.shooting = true;
            if sec > 0 {
                app.toast(trf!("{sec} 秒後框選：現在先打開要截的選單或提示", "Selecting in {sec} s: open the menu or tooltip you want to capture now"), false);
            }
            // 延遲截圖：core 在畫面上顯示倒數（可以按 Esc 或點一下取消）
            app.spawn(async move { core.snip_delayed(&config, sec).await.map(|_| ()) }, done);
        }
        ShotAct::Area | ShotAct::Screen(_) | ShotAct::Last => {
            match act {
                ShotAct::Screen(Some(id)) => config.source = SourceConfig::Monitor { monitor_id: id },
                ShotAct::Screen(None) => config.source = SourceConfig::All,
                ShotAct::Last => match app.core.last_snip() {
                    Some(r) => config.source = SourceConfig::Region { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 },
                    None => return app.toast(tr!("還沒有框選過範圍", "You haven't selected an area yet"), true),
                },
                _ => {}
            }
            app.main.shooting = true;
            app.spawn(async move { core.screenshot(&config).await.map(|_| ()) }, done);
        }
        ShotAct::Scroll => {
            app.main.shooting = true;
            app.spawn(async move { core.long_shot(&config).await }, |app, r| {
                app.main.shooting = false;
                if let Err(e) = r {
                    // 捲動時被捲的視窗在前面，操作視窗常在它後面：沒在前面時也用系統通知說明
                    if app.ctx.input(|i| i.viewport().focused) != Some(true) {
                        app.core.notify(tr!("長截圖", "Scrolling screenshot"), e.message(), true);
                    }
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        ShotAct::Steps => match app.core.steps_start(&config.shots_dir()) {
            Ok(()) => app.toast(
                tr!(
                    "步驟截圖開始：之後每點一下滑鼠就截一張，做完後按畫面下方的「完成」",
                    "Step capture started: every mouse click now takes a screenshot. Click “Finish” at the bottom of the screen when you're done."
                ),
                false,
            ),
            Err(e) => app.toast(e.message().to_string(), true),
        },
        ShotAct::EditLast => match app.core.last_shot() {
            Some(_) if app.editor.is_some() => app.toast(tr!("編輯視窗已經開著，請先關閉", "The editor is already open. Close it first"), true),
            Some(s) => super::editor::shot::open(app, s.path),
            None => app.toast(tr!("還沒有截圖", "No screenshots yet"), true),
        },
        ShotAct::Text => {
            app.main.shooting = true;
            app.spawn(async move { core.text_snip(&config).await }, |app, r| {
                app.main.shooting = false;
                match r {
                    Ok(Some(l)) => app.show_grabbed_text(l),
                    Ok(None) => {}
                    Err(e) => super::ocr::error(app, e.message()),
                }
            });
        }
        ShotAct::Pen => {
            #[cfg(windows)]
            screenrecorder_core::screen_pen_win::toggle();
            #[cfg(not(windows))]
            app.toast(tr!("螢幕畫筆只支援 Windows", "The screen pen is only available on Windows"), true);
        }
    }
}

/// 卡片標題列：左邊標題，右邊 ✕（關閉）；回傳是否按了關閉
fn card_title(ui: &mut Ui, title: &str, color: Color32) -> bool {
    let mut close = false;
    // 先放右邊的 ✕，標題用剩下的寬度（太長時結尾「…」）
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = Btn::icon_only(Icon::Close).ghost().small().height(22.0).tooltip(tr!("關閉", "Close")).show(ui).clicked();
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(title).font(theme::font_bold(13.5)).color(color)).truncate()).on_hover_text(title);
            });
        });
    });
    close
}

/// 改錄靜音的聲音來源：「麥克風無法使用」「系統聲音無法使用」
fn silent_text(bits: u8) -> &'static str {
    match (bits & SILENT_MIC != 0, bits & SILENT_SYSTEM != 0) {
        (true, true) => tr!("麥克風與系統聲音無法使用", "Mic and system audio unavailable"),
        (true, false) => tr!("麥克風無法使用", "Microphone unavailable"),
        _ => tr!("系統聲音無法使用", "System audio unavailable"),
    }
}

/// 錄影結果卡：沒錄到的聲音（整支或一段）
fn silent_result_text(bits: u8) -> Option<String> {
    let full = |b: u8| bits & b != 0;
    let part = |b: u8| bits & (b << 2) != 0;
    let mut v = vec![];
    if full(SILENT_MIC) {
        v.push(tr!("這支錄影沒有錄到麥克風聲音", "This recording has no microphone audio"));
    } else if part(SILENT_MIC) {
        v.push(tr!("這支錄影有一段沒有錄到麥克風聲音", "Part of this recording has no microphone audio"));
    }
    if full(SILENT_SYSTEM) {
        v.push(tr!("這支錄影沒有錄到系統聲音", "This recording has no system audio"));
    } else if part(SILENT_SYSTEM) {
        v.push(tr!("這支錄影有一段沒有錄到系統聲音", "Part of this recording has no system audio"));
    }
    (!v.is_empty()).then(|| v.join(tr!("；", "; ")))
}

/// 錄影失敗的白話說明：發生什麼事、可以試試什麼、能不能改用相容模式再錄一次
struct Failure {
    summary: &'static str,
    suggest: &'static str,
    /// 可以改用相容模式（gdigrab）再錄一次
    compat: bool,
}

fn explain_failure(app: &UiApp, res: &screenrecorder_core::types::RecordingResult) -> Failure {
    let m = &res.message;
    let no_frames = res.frames == 0 && res.parts_dir.is_none() && (m.starts_with("沒有擷取到任何畫面") || m.starts_with("No frames were captured"));
    if no_frames && app.s.method != MethodPreference::Gdigrab && app.s.source_type != SourceType::Audio {
        return Failure {
            summary: tr!("無法擷取畫面，沒有產生影片。", "The screen couldn't be captured, so no video was created."),
            suggest: tr!("可以試試：改用相容模式再錄一次。", "Try this: record again in compatibility mode."),
            compat: true,
        };
    }
    if no_frames {
        return Failure {
            summary: tr!("無法擷取畫面，沒有產生影片。", "The screen couldn't be captured, so no video was created."),
            suggest: tr!(
                "可以試試：重新開機後再錄一次；一直失敗時，請把記錄檔附在問題回報裡。",
                "Try this: restart the computer and record again. If it keeps failing, attach the log file to a problem report."
            ),
            compat: false,
        };
    }
    if m.contains("空間") || m.to_lowercase().contains("space") {
        return Failure {
            summary: tr!("儲存位置的空間不夠，錄影沒有存好。", "There isn't enough space at the save location, so the recording wasn't saved."),
            suggest: tr!(
                "可以試試：清出空間，或在下方換一個儲存位置。已錄的部分可以按「救回已錄的部分」。",
                "Try this: free up space or choose another save location below. Use “Recover what was recorded” to save the part already recorded."
            ),
            compat: false,
        };
    }
    if res.parts_dir.is_some() {
        return Failure {
            summary: tr!("錄好的畫面沒能合併成影片，已錄的部分還保留著。", "The recorded parts couldn't be merged into a video, but they were kept."),
            suggest: tr!("可以試試：按「救回已錄的部分」再合併一次。", "Try this: click “Recover what was recorded” to merge them again."),
            compat: false,
        };
    }
    Failure {
        summary: tr!("錄影沒有完成。", "The recording didn't finish."),
        suggest: tr!("可以試試：再錄一次；一直失敗時，請把記錄檔附在問題回報裡。", "Try this: record again. If it keeps failing, attach the log file to a problem report."),
        compat: false,
    }
}

/// 錄影結果卡：成功時可以播放、剪輯；失敗時用白話說明原因與建議，原始訊息收在「詳細資訊」
fn result_card(app: &mut UiApp, ui: &mut Ui, res: &screenrecorder_core::types::RecordingResult, key: String) {
    let p = theme::pal(ui);
    let (fill, stroke) = if res.ok { (p.ok_soft, p.ok) } else { (p.rec_soft, p.rec) };
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke.gamma_multiply(0.4))).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match (&res.path, res.ok) {
            (Some(path), true) => {
                let audio = screenrecorder_core::after_record::is_audio_recording(path);
                let saved = if audio {
                    trf!("錄音已儲存・{}・{}", "Audio recording saved · {} · {}", short_clock(res.video_sec), format_bytes(res.bytes.unwrap_or(0)))
                } else {
                    trf!("錄影已儲存・{}・{}", "Recording saved · {} · {}", short_clock(res.video_sec), format_bytes(res.bytes.unwrap_or(0)))
                };
                if card_title(ui, &saved, p.ok) {
                    app.main.dismissed_result = Some(key);
                }
                ui.label(RichText::new(file_name(path)).font(theme::mono(12.0))).on_hover_text(path);
                if let Some(t) = silent_result_text(res.silent_audio) {
                    ui.add(egui::Label::new(RichText::new(t).font(theme::font(12.5)).color(p.rec)).wrap()).on_hover_text(tr!(
                        "錄影時麥克風或播放裝置打不開、或中途斷掉，那段錄到的是靜音",
                        "The microphone or playback device couldn't be opened or disconnected while recording, so that part is silent"
                    ));
                }
                action_buttons(app, ui, path, true, !audio);
                // 只錄聲音：另存的 M4A
                let m4a = screenrecorder_core::after_record::m4a_path(path).display().to_string();
                if audio && std::path::Path::new(&m4a).is_file() {
                    ui.add_space(2.0);
                    ui.add(egui::Label::new(RichText::new(trf!("也存成 M4A：{}", "Also saved as M4A: {}", file_name(&m4a))).font(theme::font(12.5))).truncate()).on_hover_text(&m4a);
                    ui.horizontal_wrapped(|ui| {
                        if Btn::new(tr!("播放 M4A", "Play M4A")).icon(Icon::Play).small().tooltip(tr!("用 Windows 預設的程式播放", "Play with the Windows default app")).show(ui).clicked() {
                            screenrecorder_core::desktop::open_with_explorer(&m4a, false);
                        }
                        if Btn::new(tr!("在資料夾中顯示", "Show in folder")).icon(Icon::Folder).small().show(ui).clicked() {
                            screenrecorder_core::desktop::open_with_explorer(&m4a, true);
                        }
                        if Btn::new(tr!("複製檔案（貼到 LINE）", "Copy file (paste into LINE)")).icon(Icon::Copy).small().show(ui).clicked() {
                            match screenrecorder_core::clipboard::copy_files(std::slice::from_ref(&m4a)) {
                                Ok(()) => app.toast(tr!("已複製檔案，可以直接貼到 LINE、Teams、信件或資料夾", "File copied. Paste it into LINE, Teams, an email or a folder"), false),
                                Err(e) => app.toast(e, true),
                            }
                        }
                    });
                }
            }
            _ => {
                if card_title(ui, tr!("錄影沒有完成", "The recording didn't finish"), p.rec) {
                    app.main.dismissed_result = Some(key.clone());
                }
                let f = explain_failure(app, res);
                ui.add(egui::Label::new(RichText::new(f.summary).font(theme::font(13.0)).color(p.text)).wrap());
                ui.add(egui::Label::new(RichText::new(f.suggest).font(theme::font(12.5)).color(p.muted)).wrap());
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                    let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && app.manual_export().is_none();
                    if f.compat
                        && Btn::new(tr!("改用相容模式重試", "Retry in compatibility mode"))
                            .primary()
                            .small()
                            .enabled(can)
                            .tooltip(tr!(
                                "相容模式（gdigrab）哪台電腦都能用；之後也會用這個方式（可在「設定 → 進階 → 擷取方式」改回）",
                                "Compatibility mode (gdigrab) works on any PC and will be used from now on (change it back in Settings → Advanced → Capture method)"
                            ))
                            .show(ui)
                            .clicked()
                    {
                        app.s.method = MethodPreference::Gdigrab;
                        app.save_settings();
                        app.main.dismissed_result = Some(key.clone());
                        let config = app.s.record_config(&app.env);
                        let core = app.core.clone();
                        app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
                    }
                    if res.parts_dir.is_some() && Btn::new(tr!("救回已錄的部分", "Recover what was recorded")).primary().small().show(ui).clicked() {
                        app.main.dismissed_result = Some(key.clone());
                        app.recover_now();
                    }
                    if Btn::new(tr!("開啟記錄檔資料夾", "Open log folder")).icon(Icon::Folder).small().show(ui).clicked() {
                        screenrecorder_core::desktop::open_with_explorer(&screenrecorder_core::log::log_file().display().to_string(), true);
                    }
                    if Btn::new(tr!("複製錯誤資訊", "Copy error details"))
                        .icon(Icon::Copy)
                        .small()
                        .tooltip(tr!("複製錯誤訊息與最近的事件紀錄，可以貼到問題回報裡", "Copies the error and the recent event log so you can paste it into a problem report"))
                        .show(ui)
                        .clicked()
                    {
                        let log: Vec<String> = app.status.recorder.log.iter().rev().take(15).rev().map(|l| l.text.clone()).collect();
                        let text = format!("{} v{}\n{}\n\n{}", screenrecorder_core::winui::app_title(), app.env.app_version, res.message, log.join("\n"));
                        app.ctx.copy_text(text);
                        app.toast(tr!("已複製錯誤資訊，可以貼到問題回報裡", "Error details copied. Paste them into your problem report"), false);
                    }
                });
                // 原始訊息（FFmpeg 的英文）收起來
                ui.add_space(2.0);
                let open = app.main.result_detail;
                let row = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let (icon, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        theme::paint_icon(ui.painter(), icon, if open { Icon::ChevD } else { Icon::ChevR }, p.muted);
                        ui.label(RichText::new(tr!("詳細資訊", "Details")).font(theme::font(12.5)).color(p.muted));
                    })
                    .response;
                if ui.interact(row.rect, ui.id().with("result-detail"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    app.main.result_detail = !open;
                }
                if open {
                    ui.add(egui::Label::new(RichText::new(&res.message).font(theme::font(12.0)).color(p.muted)).wrap().selectable(true));
                }
            }
        }
    });
}

/// 播放 / 顯示 / 剪輯 / 製作加速版的按鈕
fn action_buttons(app: &mut UiApp, ui: &mut Ui, path: &str, edit: bool, export: bool) {
    ui.horizontal_wrapped(|ui| {
        if Btn::new(tr!("播放", "Play")).icon(Icon::Play).small().show(ui).clicked() {
            app.act_path(EntryAction::PlayHere, path.to_string());
        }
        if Btn::new(tr!("在資料夾中顯示", "Show in folder")).icon(Icon::Folder).small().show(ui).clicked() {
            app.act_path(EntryAction::Reveal, path.to_string());
        }
        if Btn::new(tr!("複製檔案（貼到 LINE）", "Copy file (paste into LINE)"))
            .icon(Icon::Copy)
            .small()
            .tooltip(tr!("複製檔案，可以直接貼到 LINE、Teams、信件或資料夾", "Copy the file so you can paste it into LINE, Teams, an email or a folder"))
            .show(ui)
            .clicked()
        {
            app.act_path(EntryAction::CopyFile, path.to_string());
        }
        if edit && Btn::new(tr!("剪輯", "Edit")).icon(Icon::Cut).small().show(ui).clicked() {
            app.act_path(EntryAction::Edit, path.to_string());
        }
        if export && Btn::new(tr!("製作加速版 / GIF", "Speed up / GIF")).icon(Icon::Fast).small().show(ui).clicked() {
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
    // 錄完自動處理開始的轉檔：在錄完自動處理的卡片裡顯示
    if (app.dismissed_job == Some(e.id) && e.state != ExportState::Running) || app.core.after_owns_export(e.id) {
        return;
    }
    ui.add_space(8.0);
    let gif_speed = if e.speed > 1.0 { format!(" {}×", speed_label(e.speed)) } else { String::new() };
    let label = match e.kind {
        ExportKind::Cut => tr!("剪輯", "edited video").to_string(),
        ExportKind::Merge => tr!("合併錄影", "merged video").to_string(),
        ExportKind::Convert => tr!("轉成 MP4", "MP4 copy").to_string(),
        ExportKind::Upright => tr!("轉好畫面另存", "upright copy").to_string(),
        ExportKind::Gif => trf!("製作 GIF{gif_speed}", "GIF{gif_speed}"),
        ExportKind::Webp => trf!("製作 WebP 動圖{gif_speed}", "WebP{gif_speed}"),
        // 原速：只壓縮
        ExportKind::Speed if e.speed <= 1.0 => tr!("製作壓縮版", "compressed copy").to_string(),
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
                ExportState::Done => trf!("{}・{}", "{} · {}", short_clock(e.expected_sec), format_bytes(e.bytes.unwrap_or(0))),
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
                ui.horizontal(|ui| {
                    // 剪輯失敗或取消：剪輯、標註、字幕都還在（草稿），可以回去改了再存
                    if e.kind == ExportKind::Cut && Btn::new(tr!("回到剪輯", "Back to editing")).icon(Icon::Cut).small().show(ui).clicked() {
                        super::editor::resume(app, &e.source);
                    }
                    if Btn::new(tr!("關閉", "Close")).ghost().small().show(ui).clicked() {
                        app.dismissed_job = Some(e.id);
                    }
                });
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
        "gfxcapture" => tr!("視窗擷取", "Window capture"),
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
        let ver = ff.version.clone().unwrap_or_default().split('-').next().unwrap_or("").to_string();
        // 有問題的才另外標出來；正常時只有一個淡淡的「已就緒」，細節在滑鼠提示
        let (text, tone, tip): (String, Tone, String) = if !app.env_ready {
            (tr!("偵測中…", "Detecting…").into(), Tone::Plain, String::new())
        } else if !ff.found {
            (
                tr!("找不到 FFmpeg", "FFmpeg not found").into(),
                Tone::Bad,
                tr!("請先下載錄影元件（上方的「下載並開始使用」）", "Download the recording component first (“Download and get started” above)").into(),
            )
        } else if ff.encoder.is_none() {
            (
                tr!("無 H.264 編碼器", "No H.264 encoder").into(),
                Tone::Bad,
                trf!("這個 FFmpeg {ver} 沒有可以用的 H.264 編碼器，請重新下載錄影元件", "This FFmpeg {ver} has no usable H.264 encoder. Download the recording component again"),
            )
        } else {
            let mut lines = vec![trf!("FFmpeg {ver}（{}）", "FFmpeg {ver} ({})", ff.path.clone().unwrap_or_default())];
            let compat = !ff.has_ddagrab || ff.ddagrab_works == Some(false);
            let testing = ff.has_ddagrab && ff.ddagrab_works.is_none();
            lines.push(if !ff.has_ddagrab {
                tr!(
                    "擷取：相容模式（gdigrab）。這個 FFmpeg 不支援顯示卡擷取，大畫面時比較吃 CPU",
                    "Capture: compatibility mode (gdigrab). This FFmpeg doesn't support GPU capture, so it uses more CPU on large screens"
                )
                .into()
            } else {
                match ff.ddagrab_works {
                    None => tr!("擷取：正在測試顯示卡擷取…", "Capture: testing GPU capture…").into(),
                    Some(true) => tr!("擷取：顯示卡擷取（ddagrab）", "Capture: GPU capture (ddagrab)").into(),
                    Some(false) => trf!(
                        "擷取：相容模式（gdigrab）。顯示卡擷取在這台電腦不能用\n{}",
                        "Capture: compatibility mode (gdigrab). GPU capture doesn't work on this PC\n{}",
                        ff.ddagrab_error.clone().unwrap_or_default()
                    ),
                }
            });
            if let Some(g) = &ff.gpu_convert {
                lines.push(trf!("畫面處理：{g}", "Video processing: {g}"));
            }
            if let Some(e) = &ff.encoder {
                lines.push(trf!("編碼：{}（{e}）", "Encoding: {} ({e})", encoder_name(e)));
            }
            let label = if testing {
                tr!("已就緒（檢查中…）", "Ready (checking…)")
            } else if compat {
                tr!("已就緒（相容模式）", "Ready (compatibility mode)")
            } else {
                tr!("已就緒", "Ready")
            };
            (label.into(), Tone::Plain, lines.join("\n"))
        };
        let r = chip(ui, &text, tone, false);
        if !tip.is_empty() {
            r.on_hover_text(tip);
        }
    });
}

/// 可以在程式內更新（Release 有附 exe 與 SHA-256）
pub fn can_self_update(u: &screenrecorder_core::types::UpdateInfo) -> bool {
    u.download_url.is_some() && u.sha256.is_some()
}

/// 正在更新時的進度（下載中、驗證中、重新啟動中）；沒在更新時 None
pub fn update_progress(app: &UiApp) -> Option<String> {
    use screenrecorder_core::selfupdate::InstallPhase;
    let i = app.status.install.as_ref()?;
    match i.phase {
        InstallPhase::Downloading => Some(match i.total.filter(|t| *t > 0) {
            Some(total) => trf!("下載新版 {}%", "Downloading update {}%", (i.received as f64 / total as f64 * 100.0).floor()),
            None => trf!("下載新版 {}", "Downloading update {}", format_bytes(i.received)),
        }),
        InstallPhase::Verifying => Some(tr!("驗證新版…", "Verifying update…").into()),
        InstallPhase::Restarting => Some(tr!("重新啟動中…", "Restarting…").into()),
        _ => None,
    }
}

/// 更新到新版：可以在程式內更新時先確認（下載、核對後自動重新啟動），不行時開啟下載頁面
pub fn ask_update(app: &mut UiApp, u: &screenrecorder_core::types::UpdateInfo) {
    if !can_self_update(u) {
        let _ = actions::open_url(&u.url);
        return;
    }
    let size = u.size.map(|s| trf!("（約 {}）", " (about {})", format_bytes(s))).unwrap_or_default();
    // 剪輯視窗有還沒儲存的修改：更新會結束程式，先說清楚
    let unsaved = app.editor.as_ref().is_some_and(|e| e.has_unsaved());
    let mut msg = trf!(
        "會下載新版{size}、核對檔案後自動重新啟動程式；設定與錄影都會保留。",
        "The new version{size} will be downloaded and verified, then the app restarts automatically. Your settings and recordings are kept."
    );
    if unsaved {
        msg += tr!("\n\n剪輯視窗有未儲存的修改，更新時會關閉，修改會不見。", "\n\nThe editor has unsaved changes. They will be lost when the app restarts.");
    }
    let info = u.clone();
    let mut ask = Ask::confirm(trf!("更新到 v{}", "Update to v{}", u.version), msg, if unsaved { tr!("仍要更新", "Update anyway") } else { tr!("立即更新", "Update now") }, |app, _| {
        app.ask = None;
        if let Err(e) = app.core.start_self_update() {
            app.toast(e.message().to_string(), true);
        }
    })
    // 看這一版改了什麼（在瀏覽器開啟），對話框留著
    .with_alt(tr!("看更新內容", "What's new"), move |app, _| {
        let _ = actions::open_url(&info.url);
        ask_update(app, &info);
    });
    ask.danger = unsaved;
    app.ask = Some(ask);
}

// ───────────── 最近錄影 ─────────────

/// 「檔案庫」那一塊的最小寬度
const LIB_BTN_W: f32 = 92.0;

fn recent_strip(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let strip = theme::card(ui).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
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
            // 右邊「檔案庫」：錄影、截圖兩個按鈕放在一起（點哪個直接開那一頁，一步就到）
            let rec_btn = Btn::new(tr!("錄影", "Videos")).icon(Icon::Video).small().tooltip(tr!("全部錄影：搜尋、整理、合併、一次處理", "All recordings: search, organize, merge, batch process"));
            let shot_btn =
                Btn::new(tr!("截圖", "Screenshots")).icon(Icon::Camera).small().tooltip(tr!("全部截圖：用圖裡的字搜尋、整理、拼圖", "All screenshots: search by text in images, organize, combine"));
            // 兩個按鈕上下疊（一樣寬），比並排省一半寬度，多放得下一張卡片
            let btn_w = rec_btn.width(ui).max(shot_btn.width(ui));
            let lib_w = btn_w.max(LIB_BTN_W) + 8.0;
            let cards_w = ui.available_width() - lib_w - 44.0;
            // 卡片最窄：縮圖＋下面一排 6 個小圖示（播放、剪輯、製作、傳到媒體庫、資料夾、⋯）放得下就好
            let card_w = card_min_w(ui);
            let per = ((cards_w + 8.0) / (card_w + 8.0)).floor().clamp(1.0, 10.0) as usize;
            // 卡片平均分配寬度（填滿這一列），但不會寬到裡面空一大塊
            let card_w = ((cards_w - 8.0 * (per as f32 - 1.0)) / per as f32).clamp(card_w, card_w + 60.0);
            if per != app.recent_per_page {
                app.recent_per_page = per;
                app.load_recent();
            }
            let items = app.recent.as_ref().map(|r| r.items.clone()).unwrap_or_default();
            ui.allocate_ui(vec2(cards_w, ui.available_height()), |ui| {
                ui.horizontal_centered(|ui| {
                    if items.is_empty() {
                        let dir = app.s.out_dir(&app.env);
                        // 只寫資料夾名稱（完整路徑在滑鼠提示）；太長時截斷，不蓋到右邊的按鈕
                        let name = std::path::Path::new(&dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.clone());
                        let text = trf!(
                            "還沒有錄影。按「開始錄影」錄第一支，錄好會出現在這裡（存在「{name}」資料夾）。",
                            "No recordings yet. Click “Start recording” to make your first one; it will show up here (saved in the “{name}” folder)."
                        );
                        ui.add(egui::Label::new(theme::muted(ui, &text)).truncate()).on_hover_text(&dir);
                    }
                    let dates = date_labels(items.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
                    for e in &items {
                        recent_card(app, ui, e, dates.get(&e.media.name).cloned().unwrap_or_default(), card_w);
                    }
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 檔案庫：上面一行標題，下面錄影、截圖上下疊（一樣寬）
                ui.allocate_ui_with_layout(vec2(btn_w.max(LIB_BTN_W), 84.0), Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(RichText::new(tr!("檔案庫", "Library")).font(theme::font_bold(12.5)).color(p.muted));
                    let w = btn_w.max(LIB_BTN_W);
                    if rec_btn.min_width(w).show(ui).clicked() {
                        app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video));
                    }
                    if shot_btn.min_width(w).show(ui).clicked() {
                        app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot));
                    }
                });
                if Btn::icon_only(Icon::ChevR).ghost().small().enabled(app.recent_page < pages).tooltip(tr!("較舊的錄影", "Older recordings")).show(ui).clicked() {
                    app.recent_page += 1;
                    app.load_recent();
                }
            });
        });
    });
    tour::mark(ui, Spot::Recent, strip.response.rect);
}

/// 最近錄影卡片的縮圖：4:3（從 16:9 的畫面裁掉左右一點），卡片窄一點，一列放得下更多張
const CARD_H: f32 = 80.0;
fn card_thumb_w() -> f32 {
    (CARD_H - 12.0) * 4.0 / 3.0
}

/// 卡片最窄的寬度：縮圖＋右邊一排 6 個小圖示
fn card_min_w(ui: &Ui) -> f32 {
    let icon = Btn::icon_only(Icon::More).ghost().small().width(ui);
    6.0 + card_thumb_w() + 10.0 + icon * 6.0 + 6.0
}

fn recent_card(app: &mut UiApp, ui: &mut Ui, e: &LibraryEntry, date: String, w: f32) {
    let p = theme::pal(ui);
    let h = CARD_H;
    // 整張卡片可以按右鍵（和「⋯」同一個選單）
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let painter = ui.painter_at(rect);
    if app.recent.as_ref().and_then(|r| r.items.first()).is_some_and(|f| f.media.path == e.media.path) {
        tour::mark(ui, Spot::RecentCard, rect);
    }
    let playing = app.inline.as_ref().is_some_and(|i| i.is(&e.media.path));
    let edge = if playing { Stroke::new(2.0, p.accent) } else { Stroke::new(1.0, p.border) };
    painter.rect(rect, CornerRadius::same(theme::RADIUS_SM), if resp.hovered() { p.surface2 } else { p.surface }, edge, StrokeKind::Inside);
    let thumb = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(card_thumb_w(), h - 12.0));
    painter.rect_filled(thumb, CornerRadius::same(5), p.surface2);
    if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
        theme::paint_thumb_cover(&painter, thumb, &t);
    }
    // 縮圖：點一下就播放（中間有播放鍵，滑鼠移上去變明顯）
    let thumb_resp = ui.interact(thumb, Id::new(("recent-thumb", &e.media.path)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let hot_thumb = thumb_resp.hovered();
    // 滑過縮圖：旁邊浮出預覽，左右移動看不同時間的畫面（聲音檔仍顯示「播放」提示）
    let thumb_resp = if hot_thumb && e.media.path.to_ascii_lowercase().ends_with(".mp4") {
        super::thumbs::scrub_preview(app, ui, thumb, thumb, &e.media.path, e.media.mtime, e.media.duration_sec.unwrap_or(0.0));
        thumb_resp
    } else {
        thumb_resp.on_hover_text(tr!("播放", "Play"))
    };
    painter.circle_filled(thumb.center(), if hot_thumb { 15.0 } else { 13.0 }, Color32::from_black_alpha(if hot_thumb { 170 } else { 110 }));
    let c = thumb.center();
    painter.add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -6.5), c + vec2(7.0, 0.0), c + vec2(-4.0, 6.5)], Color32::WHITE, Stroke::NONE));
    if thumb_resp.clicked() {
        app.known.insert(e.media.path.clone(), e.clone());
        app.act(EntryAction::PlayHere, e.clone());
    }
    // 點兩下卡片：在播放器開啟（完整的功能）
    if resp.double_clicked() || thumb_resp.double_clicked() {
        app.inline = None;
        app.known.insert(e.media.path.clone(), e.clone());
        app.act(EntryAction::Play, e.clone());
    }
    let x = thumb.max.x + 10.0;
    let meta = format!("{}・{}", e.media.duration_sec.map(short_clock).unwrap_or_else(|| "—".into()), format_bytes(e.media.bytes));
    let meta_g = painter.layout_no_wrap(meta, theme::font(12.0), p.muted);
    let meta_w = meta_g.size().x;
    painter.galley(pos2(x, rect.min.y + 28.0), meta_g, p.muted);
    // 另外做過的版本（加速版、GIF…）：在長度、大小後面寫「另有 2 個版本」（放不下時「+2 版本」），滑鼠提示列出來
    if !e.exports.is_empty() {
        let n = e.exports.len();
        let list: Vec<String> = e.exports.iter().map(super::library_dialog::export_tag).collect();
        let room = rect.max.x - 8.0 - (x + meta_w + 8.0);
        let long = if is_en() { format!("{n} more version{}", if n == 1 { "" } else { "s" }) } else { format!("另有 {n} 個版本") };
        let short = trf!("+{n} 版本", "+{n}");
        let fits = |t: &str| ui.painter().layout_no_wrap(t.to_string(), theme::font_bold(11.5), p.accent).size().x + 14.0 <= room;
        let text = if fits(&long) { long } else { short };
        let chip_rect = Rect::from_min_max(pos2(x + meta_w + 8.0, rect.min.y + 24.0), pos2(rect.max.x - 6.0, rect.min.y + 46.0));
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(chip_rect).layout(Layout::left_to_right(Align::Center)));
        chip(ui, &text, Tone::Accent, false).on_hover_text(trf!("已製作 {}", "Made: {}", list.join(tr!("、", ", "))));
    }
    // 第一列：日期（或名稱）與標籤
    let top = Rect::from_min_max(pos2(x, rect.min.y + 5.0), pos2(rect.max.x - 6.0, rect.min.y + 27.0));
    {
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(top).layout(Layout::right_to_left(Align::Center)));
        ui.spacing_mut().item_spacing.x = 3.0;
        if is_cut_name(&e.media.name) {
            chip(ui, tr!("剪輯版", "Edited"), Tone::Warn, false);
        }
        // 已經傳到媒體庫（Jellyfin）：綠色標籤
        if let Some(at) = app.library_where(&e.media.path, e.media.bytes) {
            let watched = app.watched_by(&e.media.name);
            super::library_dialog::library_chip(ui, app, &at, &watched);
        } else {
            super::library_dialog::pending_chip(ui, &e.media.path);
        }
        super::library_dialog::folder_chip(ui, &e.media.folder);
        // 有聲音：灰色小喇叭（不用彩色標籤）
        if e.media.has_audio == Some(true) {
            super::library_dialog::audio_mark(ui);
        }
        let title = if is_default_name(&e.media.name) { date } else { base_name(&e.media.name) };
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(title).font(theme::font_bold(13.0))).truncate());
        });
    }
    // 下方：操作按鈕（播放一直是深色，其他平常淡灰色，滑鼠移到卡片上才變深）
    let row = Rect::from_min_max(pos2(x - 6.0, rect.max.y - 32.0), pos2(rect.max.x - 4.0, rect.max.y - 4.0));
    {
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        // 圖示之間不留空隙（按鈕本身有內距），卡片窄一點，一列放得下更多張
        ui.spacing_mut().item_spacing.x = 0.0;
        // 播放、剪輯、製作、傳到媒體庫直接按得到，其他在「⋯」（和全部錄影、檢視器同一份清單）
        let groups = super::entry_menu::groups(app, e, Place::Home);
        let hot = ui.rect_contains_pointer(rect);
        if let Some(a) = super::entry_menu::quick_icons(ui, &groups, hot, tr!("更多（也可以在卡片上按右鍵）", "More (or right-click the card)")) {
            app.known.insert(e.media.path.clone(), e.clone());
            app.act(a, e.clone());
        }
    }
    let groups = super::entry_menu::groups(app, e, Place::Home);
    if let Some(a) = egui::Popup::context_menu(&resp).show(|ui| super::entry_menu::menu(ui, &groups)).and_then(|r| r.inner) {
        app.known.insert(e.media.path.clone(), e.clone());
        app.act(a, e.clone());
    }
    resp.on_hover_text(&e.media.name);
}

// ───────────── 倒數 ─────────────

/// 操作視窗在錄影範圍內：全畫面倒數（接著縮小）；在別的螢幕：不擋畫面，只在取消按鈕上倒數
pub fn countdown_overlay(app: &mut UiApp, ctx: &egui::Context) {
    let r = &app.status.recorder;
    if r.state != RecorderState::Countdown {
        return;
    }
    // 倒數中按 Esc 取消（沒有開著其他對話框時）
    if app.ask.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        let core = app.core.clone();
        app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
        return;
    }
    if r.countdown_covers_ui == Some(false) {
        return;
    }
    let n = countdown_left(app);
    let hint = trf!(
        "即將開始錄影{}。按 Esc{} 取消",
        "Recording is about to start{}. Press Esc{} to cancel",
        if app.s.hide_ui { tr!("，這個視窗若在錄影範圍內會自動縮小", ". This window will minimize if it's inside the recording area") } else { "" },
        if app.env.hotkeys.is_some_and(|h| h.record) && !app.keys.label(0).is_empty() { trf!(" 或 {}", " or {}", app.keys.label(0)) } else { String::new() }
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
            if Btn::new(tr!("取消（Esc）", "Cancel (Esc)")).show(ui).clicked() {
                let core = app.core.clone();
                app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
            }
        });
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
}
