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
use screenrecorder_core::format::{clock, format_bytes, human_duration, short_clock};
use screenrecorder_core::i18n::is_en;
use screenrecorder_core::types::{DownloadPhase, ExportKind, ExportState, LibraryEntry, LogLevel, MethodPreference, RecorderState, Rect as DRect};
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
    /// 控制列的訊息有好幾則時，目前顯示第幾則（按「還有 N 則」換下一則）
    notice_idx: usize,
    /// 每一則訊息第一次出現的時間（新的排前面）
    notice_seen: std::collections::HashMap<String, Instant>,
    /// 點了訊息裡的檔名：最近錄影捲回最上面
    recent_to_top: bool,
    /// 最近錄影目前第一張（換了，例如剛錄好一支，就捲回最上面）
    recent_first: Option<String>,
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
    // 外圍、區塊之間只留 8 點：視窗小的時候預覽和最近錄影多一點位置，三塊還是分得出來
    let inner = full.shrink(8.0);
    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
        banners(app, ui);
        let rest = ui.available_rect_before_wrap();
        let gap = 8.0;
        // 左欄：上面範圍與預覽，下面一條控制列（開始錄影、截圖；錄影中的控制；結果與提醒）；右欄整欄是最近錄影與檔案庫
        let rec_w = (inner.width() * 0.3).clamp(300.0, 360.0);
        let left = Rect::from_min_max(rest.min, pos2(rest.max.x - rec_w - gap, rest.max.y));
        let right = Rect::from_min_max(pos2(rest.max.x - rec_w, rest.min.y), rest.max);
        let cap = Rect::from_min_max(left.min, pos2(left.max.x, left.max.y - CONTROL_H - gap));
        let bar = Rect::from_min_max(pos2(left.min.x, left.max.y - CONTROL_H), left.max);
        ui.scope_builder(UiBuilder::new().max_rect(cap), |ui| capture_panel(app, ui));
        ui.scope_builder(UiBuilder::new().max_rect(bar), |ui| control_bar(app, ui));
        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| recent_panel(app, ui));
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
        ui.add_space(8.0);
    }
    let warns: Vec<String> = [app.env.ffmpeg.found.then(|| app.env.ffmpeg.error.clone()).flatten(), app.env.monitor_error.clone()].into_iter().flatten().collect();
    if !warns.is_empty() {
        egui::Frame::new().fill(p.warn_soft).corner_radius(CornerRadius::same(theme::RADIUS)).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(warns.join("　")).color(p.warn));
        });
        ui.add_space(8.0);
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

/// 預覽下方控制列的高度
const CONTROL_H: f32 = 72.0;

fn capture_panel(app: &mut UiApp, ui: &mut Ui) {
    theme::card(ui).inner_margin(10.0).show(ui, |ui| {
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
        // 右邊：重新整理、即時預覽，再來是攝影機、設定組合（有建立組合時）
        let extras_w = if audio { 0.0 } else { camera_width(app, ui) + gap } + if app.presets.is_empty() { 0.0 } else { super::presets::width(app, ui) + gap };
        let right_w = 28.0 + gap + if audio { 0.0 } else { 34.0 + 8.0 + text_w(ui, live_label, 14.0) + gap } + extras_w;
        let avail = ui.available_width();
        let detail_w = detail_width(app, ui);
        let one_row = tabs_w + gap + detail_w + gap + right_w <= avail;
        // 視窗窄：分頁和右邊的按鈕放不進同一行時，按鈕移到第二行（細節放得下就和按鈕同一行，不然再下一行）
        let tools_below = tabs_w + gap + right_w > avail;
        let detail_with_tools = tools_below && detail_w + gap + right_w <= avail;
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
            if !tools_below {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| capture_tools(app, ui, audio, live_label));
            }
        });
        if tools_below {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if detail_with_tools {
                    let room = ui.available_width() - right_w - gap;
                    ui.scope(|ui| {
                        ui.set_max_width(room.max(0.0));
                        source_detail(app, ui, room);
                    });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| capture_tools(app, ui, audio, live_label));
            });
        }
        if !one_row && !detail_with_tools {
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
        ui.add_space(8.0);
        let desk_area = Rect::from_min_max(ui.cursor().min, ui.max_rect().max);
        desk(app, ui, desk_area);
    });
}

/// 擷取範圍右上角（由右往左）：重新整理、即時預覽、攝影機、設定組合
fn capture_tools(app: &mut UiApp, ui: &mut Ui, audio: bool, live_label: &str) {
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
    if !audio {
        ui.add_space(8.0);
        camera_button(app, ui);
    }
    if !app.presets.is_empty() {
        super::presets::button(app, ui);
    }
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
                tr!("請先開啟「系統聲音」或「麥克風」", "Turn on “System audio” or “Microphone” first")
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
                ui.label(RichText::new(tr!("寬高至少 16 像素", "Width and height must be at least 16 pixels")).font(theme::font(12.5)).color(p.err));
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
                ui.add(egui::Label::new(RichText::new(tr!("寬高至少 16 像素", "Width and height must be at least 16 pixels")).font(theme::font(12.5)).color(p.err)).extend());
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
        (tr!("還沒選要錄的聲音：請在下方「聲音」開啟系統聲音或麥克風", "No audio selected: turn on system audio or the microphone under “Audio” below").to_string(), true)
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

// ───────────── 錄影設定摘要（下方一列；按一下開啟「設定」視窗的那一頁） ─────────────

/// 錄影前的提醒：會讓錄影少了東西、或錄不成的設定（正常時沒有）。每一項：(說明, 滑鼠移上去的提示, 點了開啟的設定分頁)
fn rec_warnings(app: &UiApp, ctx: &egui::Context) -> Vec<(String, String, Page)> {
    let mut out = vec![];
    if !app.env_ready || app.status.recorder.state != RecorderState::Idle {
        return out;
    }
    let a = &app.env.audio;
    if app.s.source_type == SourceType::Audio && !app.s.audio_system && !app.s.audio_mic {
        out.push((
            tr!("只錄聲音，但系統聲音和麥克風都關著", "Audio only, but system audio and the mic are both off").into(),
            tr!("按一下到「設定 → 聲音」開啟要錄的聲音", "Click to turn on a source in Settings → Audio").into(),
            Page::Audio,
        ));
    }
    if app.s.audio_mic && a.error.is_none() && a.captures.is_empty() {
        out.push((
            tr!("找不到麥克風：錄影不會有你的聲音", "No microphone found: your voice won't be recorded").into(),
            tr!("接上麥克風後按預覽右上的重新整理；不錄麥克風的話，到「設定 → 聲音」關掉", "Plug in a microphone and click Refresh above the preview, or turn the mic off in Settings → Audio").into(),
            Page::Audio,
        ));
    }
    if let Some(e) = &a.error {
        out.push((trf!("聲音裝置有問題：{e}", "Audio device problem: {e}"), tr!("按一下到「設定 → 聲音」檢查", "Click to check Settings → Audio").into(), Page::Audio));
    }
    let dir = app.s.out_dir(&app.env);
    match dir_issue(ctx, &dir) {
        Some(DirIssue::NotWritable) => out.push((
            tr!("儲存位置不能寫入，錄影會存不起來", "Can't write to the save folder, so recordings can't be saved").into(),
            trf!("{dir}\n按一下到「設定 → 存檔與分享」換一個資料夾", "{dir}\nClick to choose another folder in Settings → Save & share"),
            Page::Save,
        )),
        Some(DirIssue::LowSpace(free)) => out.push((
            trf!("儲存位置的磁碟只剩 {}", "Only {} left on the save drive", format_bytes(free)),
            trf!(
                "{dir}\n錄影一分鐘約需數十 MB；空間用完會自動停止錄影。按一下到「設定 → 存檔與分享」換一個資料夾",
                "{dir}\nA minute of video takes tens of MB, and recording stops when the disk is full. Click to choose another folder in Settings → Save & share"
            ),
            Page::Save,
        )),
        None => {}
    }
    out
}

/// 儲存位置的問題
#[derive(Clone, Copy, PartialEq)]
enum DirIssue {
    NotWritable,
    /// 剩下的空間（位元組）
    LowSpace(u64),
}

/// 剩下不到這麼多空間時提醒
const LOW_SPACE: u64 = 2 * 1024 * 1024 * 1024;

/// 儲存位置的檢查結果：在背景檢查（網路磁碟可能很慢，不能卡住畫面），每 15 秒重新檢查一次，換了資料夾時馬上檢查。
/// (資料夾, 檢查的時間, 結果, 檢查中)
#[allow(clippy::type_complexity)]
static DIR_CHECK: std::sync::Mutex<Option<(String, Instant, Option<DirIssue>, bool)>> = std::sync::Mutex::new(None);

fn dir_issue(ctx: &egui::Context, dir: &str) -> Option<DirIssue> {
    let mut g = DIR_CHECK.lock().unwrap_or_else(|e| e.into_inner());
    let same = g.as_ref().is_some_and(|(d, ..)| d == dir);
    let stale = !same || g.as_ref().is_some_and(|(_, at, _, busy)| !busy && at.elapsed() >= std::time::Duration::from_secs(15));
    if stale {
        let prev = g.as_ref().filter(|_| same).and_then(|(_, _, r, _)| *r);
        *g = Some((dir.to_string(), Instant::now(), prev, true));
        let (dir, ctx) = (dir.to_string(), ctx.clone());
        let _ = std::thread::Builder::new().name("dir-check".into()).spawn(move || {
            let r = check_dir(&dir);
            let mut g = DIR_CHECK.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = g.as_mut().filter(|(d, ..)| *d == dir) {
                let changed = c.2 != r;
                *c = (dir, Instant::now(), r, false);
                if changed {
                    ctx.request_repaint();
                }
            }
        });
    }
    g.as_ref().and_then(|(_, _, r, _)| *r)
}

/// 檔案的修改時間（秒；沒有這個檔案時 None）：畫面每格都要問，3 秒內不重查（錄影存在 NAS 時每次查都要等網路）
pub fn file_mtime(path: &str) -> Option<f64> {
    type Seen = std::collections::HashMap<String, (Instant, Option<f64>)>;
    static SEEN: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(Default::default);
    if let Some((at, r)) = map.get(path) {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return *r;
        }
    }
    let r = std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64()));
    if map.len() > 200 {
        map.clear();
    }
    map.insert(path.to_string(), (Instant::now(), r));
    r
}

/// 資料夾在不在：在背景檢查（NAS 斷線時檢查可能要等幾十秒，不能卡住畫面），同一個資料夾 15 秒內不重查；
/// 還沒查完時 None
pub fn dir_exists(ctx: &egui::Context, dir: &str) -> Option<bool> {
    type Seen = std::collections::HashMap<String, (Instant, Option<bool>)>;
    static SEEN: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(Default::default);
    let known = map.get(dir).copied();
    if known.is_none_or(|(at, r)| r.is_some() && at.elapsed() >= std::time::Duration::from_secs(15)) {
        map.insert(dir.to_string(), (Instant::now(), known.and_then(|k| k.1)));
        let (dir, ctx) = (dir.to_string(), ctx.clone());
        let _ = std::thread::Builder::new().name("dir-exists".into()).spawn(move || {
            let r = std::path::Path::new(&dir).is_dir();
            let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = g.as_mut() {
                let changed = map.get(&dir).and_then(|k| k.1) != Some(r);
                map.insert(dir, (Instant::now(), Some(r)));
                if changed {
                    ctx.request_repaint();
                }
            }
        });
    }
    known.and_then(|k| k.1)
}

/// 試著在資料夾裡寫一個暫存檔（資料夾還沒建立時試最近一層已經存在的上層），再看剩下的空間
fn check_dir(dir: &str) -> Option<DirIssue> {
    let mut at = std::path::Path::new(dir);
    while !at.exists() {
        at = at.parent()?;
    }
    let probe = at.join(format!(".screenrecorder-write-test-{}", std::process::id()));
    if std::fs::write(&probe, b"").is_err() {
        return Some(DirIssue::NotWritable);
    }
    let _ = std::fs::remove_file(&probe);
    screenrecorder_core::recorder::disk_free_sync(&at.to_string_lossy()).filter(|f| *f < LOW_SPACE).map(DirIssue::LowSpace)
}

/// 攝影機按鈕的寬度（預覽上方那一列先扣掉）
fn camera_width(app: &UiApp, ui: &Ui) -> f32 {
    Btn::new(camera_label(&app.s.camera)).icon(Icon::Webcam).ghost().small().width(ui)
}

/// 攝影機子母畫面：直接選要用哪一台（不用進設定）
fn camera_button(app: &mut UiApp, ui: &mut Ui) {
    let cam = camera_label(&app.s.camera);
    let resp = Btn::new(&cam)
        .icon(Icon::Webcam)
        .ghost()
        .small()
        .selected(!app.s.camera.is_empty())
        .enabled(!app.locked())
        .tooltip(trf!("攝影機子母畫面：{cam}（按一下修改）", "Camera overlay: {cam} (click to change)"))
        .show(ui);
    if resp.clicked() {
        app.load_cameras(false);
    }
    let mut open = false;
    egui::Popup::menu(&resp).show(|ui| open = camera_menu(app, ui));
    if open {
        settings_dialog::open(app, Page::Record);
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

// ───────────── 錄影狀態 ─────────────

/// 可以開始錄影：有錄影元件與編碼器、沒有手動開始的轉檔、範圍夠大（主畫面、小視窗的「開始錄影」）
pub(super) fn can_start(app: &UiApp) -> bool {
    start_blocker(app).is_none()
}

/// 不能開始錄影的原因（停用的錄影鈕滑鼠移上去時提示）
pub(super) fn start_blocker(app: &UiApp) -> Option<String> {
    if !app.env.ffmpeg.found {
        Some(tr!("請先下載錄影元件（上方的「下載並開始使用」）", "Download the recording component first (“Download and get started” above)").to_string())
    } else if app.env.ffmpeg.encoder.is_none() {
        Some(tr!("沒有可以用的 H.264 編碼器：請重新下載錄影元件", "No usable H.264 encoder. Download the recording component again").to_string())
    } else if region_too_small(app) {
        Some(tr!("範圍太小：寬高至少 16 像素", "The area is too small: it must be at least 16 pixels wide and tall").to_string())
    } else {
        app.manual_export().map(|e| trf!("正在{}，完成後才能錄影", "{} is in progress. You can record when it finishes", actions::job_label(&e)))
    }
}

/// 用目前的設定開始錄影
pub(super) fn start_recording(app: &UiApp) {
    let config = app.s.record_config(&app.env);
    let core = app.core.clone();
    app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
}

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

/// 截圖的方式（「⋯」→「截圖工具」、系統匣的「截圖」選單相同的項目與順序）
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

/// 現在可以截圖（有錄影元件、沒有正在截）
pub(super) fn can_shoot(app: &UiApp) -> bool {
    app.env.ffmpeg.found && !app.main.shooting
}

/// 「⋯」→「截圖工具」子選單：點了就截
pub(super) fn shot_menu_body(app: &mut UiApp, ui: &mut Ui) {
    let mut act = None;
    shot_menu(app, ui, &mut act);
    if let Some(a) = act.filter(|_| can_shoot(app)) {
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
        let b = egui::Button::new(label).shortcut_text(RichText::new(k).font(theme::mono(12.0)));
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
                .on_hover_text(tr!("先開啟要截的選單或提示，時間到畫面會凍結讓你框選", "Open the menu or tooltip you want to capture first; the screen freezes for selection when time is up"))
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
            "開始後每按一下滑鼠就截一張，標出點的位置；完成時做成一份圖文並茂的教學文件",
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
        .add(egui::Button::new(tr!("只抓文字…", "Grab text…")).shortcut_text(RichText::new(app.keys.label(9)).font(theme::mono(12.0))))
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
                app.toast(trf!("{sec} 秒後框選：現在先開啟要截的選單或提示", "Selecting in {sec} s: open the menu or tooltip you want to capture now"), false);
            }
            // 延遲截圖：core 在畫面上顯示倒數（可以按 Esc 或按一下取消）
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
                    "步驟截圖開始：之後每按一下滑鼠就截一張，做完後按畫面下方的「完成」",
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
                    ui.add(egui::Label::new(RichText::new(t).font(theme::font(12.5)).color(p.err)).wrap()).on_hover_text(tr!(
                        "錄影時麥克風或播放裝置打不開、或中途斷掉，那段錄到的是靜音",
                        "The microphone or playback device couldn't be opened or disconnected while recording, so that part is silent"
                    ));
                }
                action_buttons(app, ui, path, true, !audio);
                // 只錄聲音：另存的 M4A
                let m4a = screenrecorder_core::after_record::m4a_path(path).display().to_string();
                if audio && file_mtime(&m4a).is_some() {
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

/// 「設定 → 聲音」的「測試音量」：量 20 秒，測試中顯示每個來源的音量條與「停止（剩幾秒）」；回傳用掉的範圍。
/// 錄影中、不錄聲音時不顯示
pub(super) fn level_test(app: &mut UiApp, ui: &mut Ui, locked: bool) -> Rect {
    let p = theme::pal(ui);
    let start = ui.cursor().min;
    if locked || !(app.s.audio_system || app.s.audio_mic) {
        return Rect::from_min_size(start, egui::Vec2::ZERO);
    }
    let r = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let Some(l) = app.meter.as_ref().map(|m| m.levels()) else {
            if Btn::new(tr!("開始（20 秒）", "Start (20 s)"))
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
            return;
        };
        // 音量表：每個來源一條，前面寫名稱
        let tip = match &l.error {
            Some(e) => trf!("打不開：{e}", "Couldn't open: {e}"),
            None => tr!("錄影前先講幾句話：麥克風的條會跳動，就表示收得到聲音", "Say a few words before recording: if the microphone bar moves, your voice is being picked up").into(),
        };
        let bars = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (n, v) in [(tr!("系統", "System"), l.system), (tr!("麥克風", "Mic"), l.mic)].into_iter().filter_map(|(n, v)| v.map(|v| (n, v))) {
                ui.label(RichText::new(n).font(theme::font(12.5)));
                level_bar(ui, v);
            }
            if l.error.is_some() {
                ui.label(RichText::new("⚠").color(p.warn));
            }
        });
        bars.response.on_hover_text(tip);
        // 「停止（12）」：不用等 20 秒，按了就關掉音量表、放開麥克風；括號裡是還剩幾秒
        let left = app.meter_until.map_or(0, |t| t.saturating_duration_since(Instant::now()).as_secs() + 1);
        if Btn::new(trf!("停止（{left}）", "Stop ({left})")).ghost().small().tooltip(tr!("不用等時間到，現在就停止測試音量", "Stop the level test now instead of waiting")).show(ui).clicked()
        {
            app.meter_until = None;
        }
    });
    r.response.rect
}

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
    level_bar_w(ui, v, 46.0, true);
}

/// 指定寬度的音量表；on = false 時整條變淡（例如麥克風靜音中）
fn level_bar_w(ui: &mut Ui, v: f32, w: f32, on: bool) {
    let p = theme::pal(ui);
    let (r, _) = ui.allocate_exact_size(vec2(w, 8.0), Sense::hover());
    if !on {
        ui.painter().rect_filled(r, CornerRadius::same(4), p.surface2.gamma_multiply(0.6));
        return;
    }
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

/// 錄影環境的細節（擷取方式、畫面處理、編碼器），「設定 → 外部元件」的 FFmpeg 下面列出來
pub(super) fn ready_details(app: &UiApp) -> Vec<String> {
    let ff = &app.env.ffmpeg;
    let mut lines: Vec<String> = vec![];
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
    lines
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

// ───────────── 控制列（預覽下方） ─────────────

/// 預覽下方的控制列：待命時「開始錄影」，錄影中換成計時與暫停、標記、麥克風、停止；
/// 右邊的空間顯示一則訊息（錄好了、製作中 / 完成、提醒），有好幾則時「還有 N 則」切換。高度固定，預覽不會跳
fn control_bar(app: &mut UiApp, ui: &mut Ui) {
    let r = app.status.recorder.clone();
    let active = r.state != RecorderState::Idle;
    theme::card(ui).inner_margin(egui::Margin::symmetric(10, 0)).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        // 視窗很窄時也不畫到旁邊的最近錄影上
        ui.set_clip_rect(ui.max_rect().expand2(vec2(10.0, 0.0)).intersect(ui.clip_rect()));
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if active {
                recording_controls(app, ui, &r);
            } else {
                idle_controls(app, ui);
            }
        });
    });
}

/// 待命：錄影膠囊（灰色計時、紅色錄影鈕；快速鍵在滑鼠提示）；右邊是訊息，沒有訊息時空著
fn idle_controls(app: &mut UiApp, ui: &mut Ui) {
    let tip = if let Some(why) = start_blocker(app) {
        why
    } else if app.core.after_status().is_some() {
        tr!("錄完自動處理會先暫停，錄完再繼續", "Post-processing pauses while you record and continues afterwards").to_string()
    } else {
        String::new()
    };
    let label = if app.s.source_type == SourceType::Audio { tr!("錄音", "Record audio") } else { tr!("錄影", "Record") };
    // 快速鍵不寫在按鈕上，滑鼠移上去才提示
    let k = app.keys.label(0);
    let tip = if tip.is_empty() && !k.is_empty() && app.env.hotkeys.is_some_and(|h| h.all().first().copied().unwrap_or(false)) { trf!("快速鍵 {k}", "Shortcut {k}") } else { tip };
    let start = idle_capsule(app, ui, label, &tip);
    tour::mark(ui, Spot::Record, start.rect);
    if start.clicked() {
        start_recording(app);
    }
    notice_area(app, ui, false);
}

/// 錄影膠囊的底色（深色；暫停時變灰；滑鼠移上去稍微亮一點）
fn capsule_bg(ui: &Ui, paused: bool, hover: bool) -> Color32 {
    let c = match (paused, ui.visuals().dark_mode) {
        (false, false) => Color32::from_rgb(0x1f, 0x23, 0x28),
        (true, false) => Color32::from_rgb(0x55, 0x5a, 0x62),
        (false, true) => Color32::from_rgb(0x34, 0x39, 0x40),
        (true, true) => Color32::from_rgb(0x4c, 0x51, 0x59),
    };
    if hover {
        c.gamma_multiply(1.3)
    } else {
        c
    }
}

/// 待命時的錄影膠囊：和錄影中同一條、一樣寬（灰色的 00:00:00、變淡的暫停 / 標記 / 麥克風、右邊紅色圓鈕），
/// 按下去原地變成錄影中、紅鈕變停止鈕。
/// 整條都可以按；不能錄時變淡、滑鼠提示寫原因
fn idle_capsule(app: &mut UiApp, ui: &mut Ui, label: &str, tip: &str) -> egui::Response {
    let p = theme::pal(ui);
    let enabled = can_start(app);
    let timer = clock(0.0);
    let timer_w = ui.painter().layout_no_wrap(timer.clone(), theme::mono(20.0), Color32::WHITE).size().x;
    // 錄影中才能按的暫停、標記、麥克風（有錄麥克風時）也先畫出來、變淡：膠囊和錄影中一樣寬，按下去不會變長
    let mut icons = vec![Icon::Pause, Icon::Flag];
    if app.s.audio_mic {
        icons.push(Icon::Mic);
    }
    let icons_w = 7.0 + icons.len() as f32 * 38.0 + 6.0;
    let w = 16.0 + 14.0 + 8.0 + timer_w + 10.0 + icons_w + 1.0 + 7.0 + 36.0 + 6.0;
    let (pill, resp) = ui.allocate_exact_size(vec2(w, 44.0), if enabled { Sense::click() } else { Sense::hover() });
    let hover = enabled && resp.hovered();
    let painter = ui.painter().clone();
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    painter.rect_filled(pill, 22.0, dim(capsule_bg(ui, false, hover)));
    let cy = pill.center().y;
    let mut x = pill.min.x + 16.0;
    painter.circle_filled(pos2(x + 7.0, cy), 6.0, Color32::from_gray(110));
    x += 14.0 + 8.0;
    painter.text(pos2(x, cy), egui::Align2::LEFT_CENTER, timer, theme::mono(20.0), Color32::from_gray(if enabled { 150 } else { 120 }));
    x += timer_w + 10.0;
    let sep = |x: f32| painter.vline(x, (cy - 11.0)..=(cy + 11.0), Stroke::new(1.0, Color32::from_white_alpha(60)));
    sep(x);
    x += 7.0;
    for icon in icons {
        theme::paint_icon(&painter, Rect::from_center_size(pos2(x + 19.0, cy), vec2(16.0, 16.0)), icon, Color32::from_gray(if enabled { 95 } else { 80 }));
        x += 38.0;
    }
    x += 6.0;
    sep(x);
    x += 7.0;
    let c = pos2(x + 18.0, cy);
    painter.circle_filled(
        c,
        17.0,
        if !enabled {
            p.rec.gamma_multiply(0.45)
        } else if hover {
            p.rec.gamma_multiply(0.85)
        } else {
            p.rec
        },
    );
    painter.circle_filled(c, 6.0, Color32::WHITE);
    if hover {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // 滑鼠提示：「開始錄影」接快速鍵；不能錄時寫原因
    let what = trf!("開始{label}", "Start: {label}");
    let text = match (enabled, tip.is_empty()) {
        (_, true) => what,
        (true, false) => format!("{what}・{tip}"),
        (false, false) => tip.to_string(),
    };
    resp.on_hover_text(text)
}

/// 錄影中：左邊一條深色膠囊（紅點、計時、暫停 / 繼續、標記、麥克風、停止；回溯錄影只有停止，旁邊是「存下最近 N」），
/// 膠囊右邊一行數據（大小、剩多久；聲音沒錄到時紅字），再右邊是訊息；有警告或錯誤時最右邊多一顆「⚠ N」（事件紀錄）
fn recording_controls(app: &mut UiApp, ui: &mut Ui, r: &screenrecorder_core::types::RecorderStatus) {
    let p = theme::pal(ui);
    let replay = r.replay.clone();
    let ms = live_recorded_ms(app);
    let paused = r.state == RecorderState::Paused;
    let stopping = r.state == RecorderState::Stopping;
    let muted = r.mic_muted == Some(true);
    // 膠囊右邊的一行：狀態（不是一般錄影中時）、處理中的說明或數據（大小、剩多久）；解析度放在滑鼠提示
    let mut line: Vec<String> = vec![];
    match r.state {
        RecorderState::Countdown => line.push(trf!("{} 秒後開始", "Starts in {} s", countdown_left(app))),
        RecorderState::Paused if r.auto_paused => line.push(tr!("沒動靜，已自動暫停（動一下滑鼠或按鍵就繼續）", "Idle, paused (move the mouse or press a key to continue)").into()),
        RecorderState::Paused => line.push(tr!("已暫停", "Paused").into()),
        RecorderState::Stopping => line.push(tr!("處理中…", "Processing…").into()),
        _ => {}
    }
    if let Some(b) = &r.busy {
        line.push(b.clone());
    } else if let Some(rp) = replay.as_ref() {
        line.push(if rp.saving {
            tr!("正在存…", "Saving…").into()
        } else {
            trf!("保留最近 {}・暫存 {}", "Keeping the last {} · {} temp", screenrecorder_core::replay::length_label(rp.sec), format_bytes(rp.bytes))
        });
    } else if r.state != RecorderState::Countdown {
        line.push(format_bytes(r.bytes));
        if r.max_ms > 0 {
            line.push(trf!("剩 {}", "{} left", clock((r.max_ms as f64 - ms).max(0.0))));
        }
    }
    // 聲音沒錄到：紅字寫出來
    if r.silent_audio != 0 {
        line.push(silent_text(r.silent_audio).to_string());
    }
    let stats = line.join(tr!("・", " · "));
    let detail = trf!("{}×{}・每秒 {} 張", "{}×{} · {} fps", r.out_width, r.out_height, r.fps);
    let shortcut = |app: &UiApp, i: usize| {
        let key = app.keys.label(i);
        if key.is_empty() {
            String::new()
        } else {
            trf!("（{key}）", " ({key})")
        }
    };
    // 膠囊裡的圖示按鈕：（圖示, 滑鼠提示, 動作, 能不能按, 紅色）
    // 回溯錄影以外，暫停、標記、麥克風的位置一直留著（倒數、處理中變淡不能按），膠囊和待命時一樣寬，開始、結束都不會縮放
    let mut icons: Vec<(Icon, String, RecAct, bool, bool)> = vec![];
    if replay.is_none() {
        let live = matches!(r.state, RecorderState::Recording | RecorderState::Paused);
        icons.push(if paused {
            (Icon::Play, tr!("繼續", "Resume").into(), RecAct::Resume, live && r.busy.is_none(), false)
        } else {
            (Icon::Pause, tr!("暫停", "Pause").into(), RecAct::Pause, live && r.busy.is_none(), false)
        });
        let mk = shortcut(app, 4);
        let tip = if r.markers > 0 {
            trf!("標記（已標 {} 個）：記下現在的位置，剪輯時可以直接跳過去{mk}", "Mark ({} so far): note this position so you can jump straight to it when editing{mk}", r.markers)
        } else {
            trf!("標記：記下現在的位置，剪輯時可以直接跳過去{mk}", "Mark: note this position so you can jump straight to it when editing{mk}")
        };
        icons.push((Icon::Flag, tip, RecAct::Mark, live, false));
        // 麥克風：這次有錄（或設定要錄，和待命時一樣）就留位置
        if r.mic_muted.is_some() || app.s.audio_mic {
            let mk = shortcut(app, 7);
            let tip = if muted {
                trf!("麥克風靜音中，按一下取消靜音{mk}", "The microphone is muted. Click to unmute{mk}")
            } else {
                trf!(
                    "暫時把麥克風靜音（咳嗽、接電話時），剪輯時會在時間軸上標出來{mk}",
                    "Mute the microphone for a moment (to cough or take a call). Muted parts are marked on the editing timeline{mk}"
                )
            };
            icons.push((if muted { Icon::Muted } else { Icon::Mic }, tip, RecAct::Mute, r.mic_muted.is_some() && !stopping, muted));
        }
    }
    let stop_tip = match (r.state, replay.as_ref()) {
        (RecorderState::Countdown, _) => tr!("取消倒數（Esc）", "Cancel countdown (Esc)").to_string(),
        (_, Some(_)) => tr!("停止回溯錄影（沒存下的部分會刪掉）", "Stop the replay buffer (anything not saved is discarded)").to_string(),
        _ => trf!("停止{}", "Stop{}", shortcut(app, 1)),
    };
    let stop_enabled = !stopping && !replay.as_ref().is_some_and(|rp| rp.saving);
    // 膠囊：深色（暫停時變灰），標記過的數字畫在旗子右上角
    let mut act = RecAct::None;
    let timer = clock(ms);
    let timer_w = ui.painter().layout_no_wrap(timer.clone(), theme::mono(20.0), Color32::WHITE).size().x;
    let icons_w = if icons.is_empty() { 0.0 } else { 7.0 + icons.len() as f32 * 38.0 + 6.0 };
    let w = 16.0 + 14.0 + 8.0 + timer_w + 10.0 + icons_w + 1.0 + 7.0 + 36.0 + 6.0;
    let (pill, _) = ui.allocate_exact_size(vec2(w, 44.0), Sense::hover());
    let bg = capsule_bg(ui, paused, false);
    let painter = ui.painter().clone();
    painter.rect_filled(pill, 22.0, bg);
    let cy = pill.center().y;
    let mut x = pill.min.x + 16.0;
    // 錄影中是會閃的紅點，暫停是兩條線，倒數、處理中是不閃的紅點
    if paused {
        theme::paint_icon(&painter, Rect::from_center_size(pos2(x + 7.0, cy), vec2(14.0, 14.0)), Icon::Pause, Color32::WHITE);
    } else {
        let a = if r.state == RecorderState::Recording {
            let t = ui.input(|i| i.time) as f32;
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
            0.55 + 0.45 * (t * std::f32::consts::TAU / 1.4).sin().abs()
        } else {
            0.5
        };
        painter.circle_filled(pos2(x + 7.0, cy), 6.0, p.rec.gamma_multiply(a));
    }
    x += 14.0 + 8.0;
    let timer_rect = Rect::from_min_size(pos2(x, pill.min.y), vec2(timer_w, pill.height()));
    painter.text(pos2(x, cy), egui::Align2::LEFT_CENTER, timer, theme::mono(20.0), Color32::WHITE);
    if replay.is_none() {
        ui.interact(timer_rect, ui.id().with("recbar-timer"), Sense::hover()).on_hover_text(&detail);
    }
    x += timer_w + 10.0;
    let sep = |x: f32| painter.vline(x, (cy - 11.0)..=(cy + 11.0), Stroke::new(1.0, Color32::from_white_alpha(60)));
    if !icons.is_empty() {
        sep(x);
        x += 7.0;
        for (i, (icon, tip, a, enabled, warn)) in icons.into_iter().enumerate() {
            let c = Rect::from_center_size(pos2(x + 19.0, cy), vec2(34.0, 34.0));
            let resp = ui.interact(c, ui.id().with(("recbar", i)), if enabled { Sense::click() } else { Sense::hover() }).on_hover_text(tip);
            if enabled && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.circle_filled(c.center(), 17.0, Color32::from_white_alpha(30));
            }
            let fg = if !enabled {
                Color32::from_gray(95)
            } else if warn {
                Color32::from_rgb(0xff, 0x8a, 0x8a)
            } else {
                Color32::from_gray(230)
            };
            theme::paint_icon(&painter, Rect::from_center_size(c.center(), vec2(16.0, 16.0)), icon, fg);
            if matches!(a, RecAct::Mark) && r.markers > 0 {
                painter.text(c.center() + vec2(9.0, -8.0), egui::Align2::CENTER_CENTER, r.markers.to_string(), theme::font_bold(11.0), Color32::WHITE);
            }
            if resp.clicked() {
                act = a;
            }
            x += 38.0;
        }
        x += 6.0;
    }
    sep(x);
    x += 7.0;
    let c = Rect::from_center_size(pos2(x + 18.0, cy), vec2(34.0, 34.0));
    let stop = ui.interact(c, ui.id().with("recbar-stop"), if stop_enabled { Sense::click() } else { Sense::hover() }).on_hover_text(stop_tip);
    if stop_enabled && stop.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let fill = if !stop_enabled {
        p.rec.gamma_multiply(0.45)
    } else if stop.hovered() {
        p.rec.gamma_multiply(0.85)
    } else {
        p.rec
    };
    painter.circle_filled(c.center(), 17.0, fill);
    painter.rect_filled(Rect::from_center_size(c.center(), vec2(11.0, 11.0)), 2.0, Color32::WHITE);
    tour::mark(ui, Spot::Record, stop.rect);
    if stop.clicked() {
        act = RecAct::Stop;
    }
    // 回溯錄影：膠囊旁邊「存下最近 N」
    if let Some(rp) = replay.as_ref() {
        ui.add_space(4.0);
        let key = app.keys.label(6);
        let tip = if key.is_empty() { String::new() } else { trf!("快速鍵 {key}", "Shortcut {key}") };
        let label = trf!("存下最近 {}", "Save the last {}", screenrecorder_core::replay::length_label(rp.sec));
        if Btn::new(label).icon(Icon::Export).primary().enabled(!rp.saving && r.state == RecorderState::Recording && r.busy.is_none()).tooltip(tip).show(ui).clicked() {
            super::replay::save(app);
        }
    }
    ui.add_space(6.0);
    let color = if r.silent_audio != 0 { p.rec } else { p.muted };
    let resp = ui.add(egui::Label::new(RichText::new(&stats).font(theme::font(12.5)).color(color)).truncate());
    if replay.is_none() {
        resp.on_hover_text(format!("{stats}\n{detail}"));
    } else {
        resp.on_hover_text(&stats);
    }
    // 最右邊：事件紀錄；中間剩下的空間：訊息（跟不上、製作中…）
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        event_log_button(app, ui, r);
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| notice_area(app, ui, true));
    });
    let core = app.core.clone();
    match act {
        RecAct::None => {}
        RecAct::Pause => app.guarded(async move { core.recorder.pause().await }, |_, _| {}),
        RecAct::Resume => app.guarded(async move { core.recorder.resume().await }, |_, _| {}),
        RecAct::Stop => match replay.as_ref() {
            Some(rp) => super::replay::ask_stop(app, rp, ms),
            None => app.guarded(async move { core.recorder.stop(None).await }, |_, _| {}),
        },
        RecAct::Mark => {
            if let Err(e) = app.core.recorder.add_marker() {
                app.toast(e.message().to_string(), true);
            }
        }
        RecAct::Mute => match app.core.recorder.toggle_mic_mute() {
            Ok(m) => app.core.mic_mute_changed(m),
            Err(e) => app.toast(e.message().to_string(), true),
        },
    }
}

/// 錄影列的動作（按了膠囊裡的哪一顆）
enum RecAct {
    None,
    Pause,
    Resume,
    Mark,
    Mute,
    Stop,
}

/// 事件紀錄：有警告或錯誤時才出現「⚠ N」（還沒看過的時候是橘色），按了在選單裡看最近 20 則
fn event_log_button(app: &mut UiApp, ui: &mut Ui, r: &screenrecorder_core::types::RecorderStatus) {
    let bad = r.log.iter().filter(|l| l.level != LogLevel::Info).count();
    if bad == 0 {
        return;
    }
    let p = theme::pal(ui);
    let unseen = r.log.iter().filter(|l| l.t > app.main.log_seen && l.level != LogLevel::Info).count();
    let tip = if unseen > 0 {
        trf!("錄影過程有 {unseen} 則新的警告或錯誤，按一下看事件紀錄", "{unseen} new warning(s) or error(s) while recording. Click to see the event log")
    } else {
        tr!("事件紀錄：錄影過程的訊息、警告與錯誤", "Event log: messages, warnings and errors while recording").into()
    };
    let btn = Btn::new(format!("⚠ {bad}")).tooltip(tip);
    let resp = if unseen > 0 { btn.danger().show(ui) } else { btn.ghost().show(ui) };
    if resp.clicked() {
        app.main.log_seen = r.log.last().map_or(0, |l| l.t);
    }
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(420.0);
        for l in r.log.iter().rev().take(20) {
            let time = chrono::DateTime::from_timestamp_millis(l.t as i64).map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string()).unwrap_or_default();
            let color = match l.level {
                LogLevel::Info => p.text,
                LogLevel::Warn => p.warn,
                LogLevel::Error => p.rec,
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(time).font(theme::mono(12.0)).color(p.muted));
                ui.add(egui::Label::new(RichText::new(&l.text).font(theme::font(12.5)).color(color)).truncate()).on_hover_text(&l.text);
            });
        }
    });
}

/// 控制列右邊的一則訊息
#[derive(Clone)]
enum Notice {
    /// 找不到可以用的編碼器
    Sys,
    /// 步驟截圖進行中（已截幾步）
    Steps(u32),
    /// 錄影中的提醒：重試中、電腦跟不上
    Alert,
    /// 錄好了 / 沒有完成
    Result(screenrecorder_core::types::RecordingResult, String),
    /// 製作加速版、GIF、剪輯…（進行中、完成、失敗）
    Job(screenrecorder_core::types::ExportStatus),
    /// 錄完自動處理進行中 / 做完了
    After,
    AfterDone,
    /// 手動開始的轉檔還在進行（不能錄影）
    Manual(screenrecorder_core::types::ExportStatus),
    /// 排程錄影
    Schedule,
    /// 錄影前的提醒（說明、提示、點了開啟的設定）
    Warn(String, String, Page),
}

impl Notice {
    /// 認得同一則訊息用的（記第一次出現的時間，新的排前面）
    fn key(&self, app: &UiApp) -> String {
        match self {
            Notice::Sys => "sys".into(),
            Notice::Steps(_) => "steps".into(),
            Notice::Alert => "alert".into(),
            Notice::Result(_, k) => format!("result:{k}"),
            Notice::Job(e) => format!("job:{}:{}", e.id, e.state != ExportState::Running),
            Notice::After => "after".into(),
            Notice::AfterDone => format!("after-done:{}", app.core.after_done().map_or(0, |d| d.seq)),
            Notice::Manual(e) => format!("manual:{}", e.id),
            Notice::Schedule => "schedule".into(),
            Notice::Warn(t, ..) => format!("warn:{t}"),
        }
    }

    /// 一定要先看到的（排在新的前面）：沒有編碼器、步驟截圖中、錄影跟不上
    fn urgent(&self) -> bool {
        matches!(self, Notice::Sys | Notice::Steps(_) | Notice::Alert)
    }

    /// 可以按 ✕ 關掉的（做完了的結果）
    fn closable(&self) -> bool {
        matches!(self, Notice::Result(..) | Notice::AfterDone) || matches!(self, Notice::Job(e) if e.state != ExportState::Running)
    }
}

/// 現在要顯示的訊息：緊急的在前，其他的新的在前
fn notices(app: &mut UiApp, ctx: &egui::Context, active: bool) -> Vec<Notice> {
    let r = &app.status.recorder;
    let mut out = vec![];
    if app.env_ready && app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_none() {
        out.push(Notice::Sys);
    }
    if let Some(n) = app.status.steps {
        out.push(Notice::Steps(n));
    }
    if active && (r.retrying.is_some() || r.slow) {
        out.push(Notice::Alert);
    }
    if let Some(res) = r.result.clone().filter(|_| !active) {
        let key = res.path.clone().unwrap_or_else(|| res.message.clone());
        if app.main.dismissed_result.as_deref() != Some(key.as_str()) {
            out.push(Notice::Result(res, key));
        }
    }
    if let Some(e) = app.status.export.clone() {
        let hidden = (app.dismissed_job == Some(e.id) && e.state != ExportState::Running) || app.core.after_owns_export(e.id);
        // 手動開始、還在跑的轉檔不能錄影：待命時用「停止處理並開始錄影」那一則
        if !hidden && !(e.state == ExportState::Running && !active && app.manual_export().is_some()) {
            out.push(Notice::Job(e));
        }
    }
    if app.core.after_status().is_some() {
        out.push(Notice::After);
    } else if app.core.after_done().is_some_and(|d| app.main.dismissed_after != Some(d.seq)) {
        out.push(Notice::AfterDone);
    }
    if !active {
        if let Some(e) = app.manual_export() {
            out.push(Notice::Manual(e));
        }
        if app.core.scheduled().is_some() {
            out.push(Notice::Schedule);
        }
        for (text, tip, page) in rec_warnings(app, ctx) {
            out.push(Notice::Warn(text, tip, page));
        }
        // 常用的快速鍵（開始 / 停止、截圖、框選、暫停、回溯錄影）被占用或系統匣沒啟動
        let works = app.env.hotkeys.map(|h| h.all());
        let broken = [0usize, 1, 2, 3, 6].iter().filter(|&&i| !app.keys.label(i).is_empty() && !works.is_some_and(|w| w.get(i).copied().unwrap_or(false))).count();
        if broken > 0 && app.env_ready {
            let tip = if works.is_none() {
                tr!("系統匣沒有啟動，全域快速鍵無法使用", "The tray icon isn't running, so global shortcuts don't work")
            } else {
                tr!("被其他程式占用，到「設定 → 快速鍵」換一組", "Used by another program. Choose a different one in Settings → Shortcuts")
            };
            out.push(Notice::Warn(trf!("{broken} 個快速鍵無法使用", "{broken} shortcut(s) unavailable"), tip.into(), Page::Keys));
        }
    }
    // 記下每一則第一次出現的時間：新的排前面；有新的一則出現時翻回第一則
    let now = Instant::now();
    let keys: Vec<String> = out.iter().map(|n| n.key(app)).collect();
    let mut fresh = false;
    for k in &keys {
        if !app.main.notice_seen.contains_key(k) {
            app.main.notice_seen.insert(k.clone(), now);
            fresh = true;
        }
    }
    app.main.notice_seen.retain(|k, _| keys.contains(k));
    let seen = &app.main.notice_seen;
    let mut order: Vec<(Notice, String)> = out.into_iter().zip(keys).collect();
    order.sort_by_key(|(n, k)| (!n.urgent(), std::cmp::Reverse(seen.get(k).copied().unwrap_or(now))));
    if fresh {
        app.main.notice_idx = 0;
    }
    order.into_iter().map(|(n, _)| n).collect()
}

/// 一則訊息怎麼顯示：左邊色條的顏色、標題、補充、第二行（檔名，點了捲到最近錄影）、進度
struct NoticeView {
    color: Color32,
    title: String,
    meta: String,
    file: Option<String>,
    frac: Option<f32>,
}

/// 做好的檔案的說法：「1.15× 加速版做好了」「GIF 做好了」…
fn done_label(e: &screenrecorder_core::types::ExportStatus) -> String {
    let speed = if e.speed > 1.0 { format!("（{}×）", screenrecorder_core::format::speed_label(e.speed)) } else { String::new() };
    match e.kind {
        ExportKind::Cut => tr!("剪輯版存好了", "Edited copy saved").into(),
        ExportKind::Merge => tr!("合併好了", "Merged").into(),
        ExportKind::Convert => tr!("轉成 MP4 了", "Converted to MP4").into(),
        ExportKind::Upright => tr!("轉好的畫面另存好了", "Upright copy saved").into(),
        ExportKind::Gif => trf!("GIF{speed} 做好了", "GIF{speed} ready"),
        ExportKind::Webp => trf!("WebP 動圖{speed} 做好了", "WebP{speed} ready"),
        ExportKind::Speed if e.speed <= 1.0 => tr!("壓縮版做好了", "Compressed copy ready").into(),
        ExportKind::Speed => trf!("{}× 加速版做好了", "{}× sped-up video ready", screenrecorder_core::format::speed_label(e.speed)),
    }
}

fn notice_view(app: &UiApp, n: &Notice, p: &theme::Pal) -> NoticeView {
    let file_name = |path: &str| std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned());
    let v = |color, title: String, meta: String| NoticeView { color, title, meta, file: None, frac: None };
    match n {
        Notice::Sys => v(p.rec, tr!("沒有可以用的 H.264 編碼器", "No usable H.264 encoder").into(), tr!("請重新下載錄影元件", "Download the recording component again").into()),
        Notice::Steps(n) => v(p.accent, if is_en() { format!("Step capture · {n} step{}", if *n == 1 { "" } else { "s" }) } else { format!("步驟截圖中・已截 {n} 步") }, String::new()),
        Notice::Alert => {
            let r = &app.status.recorder;
            let text = match &r.retrying {
                Some(t) => t.clone(),
                None => trf!(
                    "電腦跟不上：實際每秒約 {} 張（設定 {} 張）",
                    "The computer can't keep up: about {} fps (set to {})",
                    r.actual_fps.map(|f| format!("{f:.1}")).unwrap_or_else(|| "?".into()),
                    r.fps
                ),
            };
            v(p.warn, text, String::new())
        }
        Notice::Result(res, _) => match (&res.path, res.ok) {
            (Some(path), true) => {
                let audio = screenrecorder_core::after_record::is_audio_recording(path);
                let mut meta = trf!("{}・{}", "{} · {}", short_clock(res.video_sec), format_bytes(res.bytes.unwrap_or(0)));
                if let Some(t) = silent_result_text(res.silent_audio) {
                    meta = format!("{meta}・{t}");
                }
                let title = if audio { tr!("✓ 錄音已儲存", "✓ Audio recording saved") } else { tr!("✓ 錄影已儲存", "✓ Recording saved") };
                NoticeView { file: file_name(path), ..v(p.ok, title.into(), meta) }
            }
            _ => v(p.rec, tr!("錄影沒有完成", "The recording didn't finish").into(), explain_failure(app, res).summary.to_string()),
        },
        Notice::Job(e) => match e.state {
            ExportState::Running => {
                let meta = format!("{}%{}", (e.progress * 100.0).floor(), e.eta_sec.map(|s| trf!("・剩 {}", " · {} left", human_duration(s))).unwrap_or_default());
                NoticeView { frac: Some(e.progress as f32), ..v(p.accent, actions::job_label(e), meta) }
            }
            ExportState::Done => {
                let meta = trf!("{}・{}", "{} · {}", short_clock(e.expected_sec), format_bytes(e.bytes.unwrap_or(0)));
                NoticeView { file: file_name(&e.output), ..v(p.ok, format!("✓ {}", done_label(e)), meta) }
            }
            state => {
                let label = actions::job_label(e);
                let title = if state == ExportState::Error { trf!("{label}失敗", "Failed: {label}") } else { trf!("{label}已取消", "Canceled: {label}") };
                v(if state == ExportState::Error { p.rec } else { p.muted }, title, e.message.clone().unwrap_or_default())
            }
        },
        Notice::After => {
            let Some(s) = app.core.after_status() else { return v(p.accent, String::new(), String::new()) };
            let export = app.status.export.clone().filter(|e| s.exporting && e.state == ExportState::Running);
            let frac = match &export {
                Some(e) => e.progress as f32,
                None => s.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0,
            };
            let title = if s.step_no == 0 || s.step.is_empty() {
                tr!("錄完自動處理：等待中", "Post-processing: waiting").to_string()
            } else {
                match s.batch {
                    Some((i, n)) => trf!("一次處理（第 {i}/{n} 支）：{}", "Processing {i} of {n}: {}", s.step),
                    None => trf!("錄完自動處理：{}", "Post-processing: {}", s.step),
                }
            };
            let meta = if s.waiting && app.locked() {
                tr!("錄影中先暫停，錄完後繼續", "Paused while recording").to_string()
            } else if s.waiting {
                tr!("等其他轉檔工作完成", "Waiting for another conversion").to_string()
            } else {
                s.name.clone()
            };
            NoticeView { frac: (frac > 0.0 && !s.waiting).then_some(frac), ..v(p.accent, title, meta) }
        }
        Notice::AfterDone => {
            let Some(d) = app.core.after_done() else { return v(p.ok, String::new(), String::new()) };
            let title = match (d.batch, d.ok) {
                (true, true) => tr!("✓ 一次處理完成", "✓ Batch processing done"),
                (true, false) => tr!("一次處理沒有完成", "Batch processing didn't finish"),
                (false, true) => tr!("✓ 已自動處理", "✓ Post-processing done"),
                (false, false) => tr!("自動處理沒有完成", "Post-processing didn't finish"),
            };
            NoticeView { file: d.output.as_deref().and_then(file_name), ..v(if d.ok { p.ok } else { p.rec }, title.into(), d.text.clone()) }
        }
        Notice::Manual(e) => NoticeView {
            frac: Some(e.progress as f32),
            ..v(p.accent, trf!("正在{}", "{} in progress", actions::job_label(e)), trf!("{}%・完成後才能錄影", "{}% · you can record when it finishes", (e.progress * 100.0).floor()))
        },
        Notice::Schedule => match app.core.scheduled() {
            Some(s) => {
                let now = chrono::Local::now();
                v(
                    p.accent,
                    trf!("排程錄影：{}", "Scheduled: {}", screenrecorder_core::schedule::describe(&s, now)),
                    trf!("還有 {}", "Starts in {}", screenrecorder_core::schedule::left_text(s.at, now)),
                )
            }
            None => v(p.accent, String::new(), String::new()),
        },
        Notice::Warn(text, ..) => v(p.warn, format!("⚠ {text}"), String::new()),
    }
}

/// 控制列的訊息區：左邊色條、文字（標題＋補充，第二行檔名或進度條），緊接著動作；最右邊「‹ 1/2 ›」與 ✕
fn notice_area(app: &mut UiApp, ui: &mut Ui, active: bool) {
    let list = notices(app, ui.ctx(), active);
    if list.is_empty() {
        return;
    }
    let p = theme::pal(ui);
    let (sep, _) = ui.allocate_exact_size(vec2(1.0, 36.0), Sense::hover());
    ui.painter().vline(sep.center().x, sep.y_range(), Stroke::new(1.0, p.border));
    let count = list.len();
    let idx = app.main.notice_idx.min(count - 1);
    app.main.notice_idx = idx;
    let n = list[idx].clone();
    // 錄影已儲存：縮圖卡片（其他訊息照舊：色條、兩行文字、動作）
    let saved = match &n {
        Notice::Result(res, _) if res.ok => res.path.clone().map(|path| (path, res.video_sec, res.bytes.unwrap_or(0))),
        _ => None,
    };
    let view = notice_view(app, &n, p);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // ✕ 的位置固定留著（不能關的訊息是空的），「‹ 1/2 ›」才不會左右跳
        if n.closable() {
            if mini(ui, Icon::Close, tr!("關閉", "Close")) {
                close_notice(app, &n);
            }
        } else {
            ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
        }
        if count > 1 {
            // 「‹ 1/2 ›」：往前、往後看
            if Btn::icon_only(Icon::ChevR).ghost().small().enabled(idx + 1 < count).tooltip(tr!("下一則", "Next")).show(ui).clicked() {
                app.main.notice_idx = idx + 1;
            }
            ui.label(RichText::new(format!("{}/{count}", idx + 1)).font(theme::font(12.0)).color(p.muted));
            if Btn::icon_only(Icon::ChevL).ghost().small().enabled(idx > 0).tooltip(tr!("上一則", "Previous")).show(ui).clicked() {
                app.main.notice_idx = idx - 1;
            }
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some((path, sec, bytes)) = &saved {
                return saved_card(app, ui, path, *sec, *bytes);
            }
            // 色條：綠＝完成、藍＝進行中、橘＝提醒、紅＝失敗
            let (bar, _) = ui.allocate_exact_size(vec2(4.0, 40.0), Sense::hover());
            ui.painter().rect_filled(bar, CornerRadius::same(2), view.color);
            // 文字的寬度：需要多寬就多寬（動作緊接在後面），最多留 230 點給動作
            let measure = |t: &str, f: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), f, p.text).size().x;
            let line1 = measure(&view.title, theme::font_bold(13.5)) + if view.meta.is_empty() { 0.0 } else { 10.0 + measure(&view.meta, theme::font(12.0)) };
            let line2 = match (&view.frac, &view.file) {
                (Some(_), _) => 160.0,
                (None, Some(f)) => measure(f, theme::font(12.0)),
                _ => 0.0,
            };
            let text_w = line1.max(line2).ceil().min((ui.available_width() - 230.0).max(120.0));
            let two_lines = view.frac.is_some() || view.file.is_some();
            ui.allocate_ui_with_layout(vec2(text_w, 44.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(text_w);
                ui.spacing_mut().item_spacing.y = 3.0;
                // 只有一行時放在中間
                if !two_lines {
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let title_color = if view.color == p.accent || view.color == p.muted { p.text } else { view.color };
                    ui.add(egui::Label::new(RichText::new(&view.title).font(theme::font_bold(13.5)).color(title_color)).truncate()).on_hover_text(&view.title);
                    if !view.meta.is_empty() && ui.available_width() > 48.0 {
                        ui.add(egui::Label::new(RichText::new(&view.meta).font(theme::font(12.0)).color(p.muted)).truncate()).on_hover_text(&view.meta);
                    }
                });
                if let Some(f) = view.frac {
                    ui.add_space(3.0);
                    let (bar, _) = ui.allocate_exact_size(vec2(text_w.clamp(160.0, 260.0).min(text_w), 5.0), Sense::hover());
                    ui.painter().rect_filled(bar, CornerRadius::same(3), p.surface2);
                    let mut fill = bar;
                    fill.set_width(bar.width() * f.clamp(0.0, 1.0));
                    ui.painter().rect_filled(fill, CornerRadius::same(3), p.accent);
                } else if let Some(name) = &view.file {
                    let r = ui.add(egui::Label::new(RichText::new(name).font(theme::font(12.0)).color(p.muted)).truncate().sense(Sense::click()));
                    if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("在右邊的最近錄影找到它", "Find it in Recent recordings on the right")).clicked() {
                        app.main.recent_to_top = true;
                    }
                }
            });
            notice_actions(app, ui, &n);
        });
    });
    if list.iter().any(|n| matches!(n, Notice::Job(_) | Notice::After | Notice::Schedule | Notice::Manual(_))) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// 錄影已儲存：左邊縮圖（點了播放），檔名（點了在最近錄影找到它）和「✓ 已儲存・長度・大小」，後面是剪輯、資料夾、複製、更多
fn saved_card(app: &mut UiApp, ui: &mut Ui, path: &str, sec: f64, bytes: u64) {
    let p = theme::pal(ui);
    ui.spacing_mut().item_spacing.x = 8.0;
    let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let audio = screenrecorder_core::after_record::is_audio_recording(path);
    // 位置不夠時先拿掉縮圖，再不夠就把剪輯、資料夾、複製收進「⋯」
    let acts_w = if audio { 2.0 } else { 4.0 } * 36.0;
    let avail = ui.available_width();
    let menu_only = avail < 110.0 + acts_w;
    let show_thumb = avail >= 76.0 + 110.0 + acts_w;
    if show_thumb {
        saved_thumb(app, ui, path, audio);
    }
    // 檔名、已儲存・長度・大小
    let meta = format!("{}・{}", short_clock(sec), format_bytes(bytes));
    let measure = |t: &str, f: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), f, p.text).size().x;
    let reserve = if menu_only { 36.0 } else { acts_w };
    let text_w = measure(&name, theme::font_bold(13.5)).max(measure(&meta, theme::font(12.0)) + 70.0).ceil().min((ui.available_width() - reserve).max(60.0));
    ui.allocate_ui_with_layout(vec2(text_w, 44.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(text_w);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(3.0);
        let r = ui.add(egui::Label::new(RichText::new(&name).font(theme::font_bold(13.5)).color(p.text)).truncate().sense(Sense::click()));
        if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("在右邊的最近錄影找到它", "Find it in Recent recordings on the right")).clicked() {
            app.main.recent_to_top = true;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("✓").font(theme::font_bold(12.0)).color(p.ok));
            ui.add(egui::Label::new(RichText::new(trf!("已儲存・{meta}", "Saved · {meta}")).font(theme::font(12.0)).color(p.muted)).truncate());
        });
    });
    if menu_only {
        let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("播放、剪輯、在資料夾中顯示、複製…", "Play, edit, show in folder, copy…")).show(ui);
        egui::Popup::menu(&more).show(|ui| {
            let mut pick = |label: &str, act: EntryAction| {
                if ui.button(label).clicked() {
                    app.act_path(act, path.to_string());
                }
            };
            pick(tr!("播放", "Play"), EntryAction::PlayHere);
            if !audio {
                pick(tr!("剪輯", "Edit"), EntryAction::Edit);
            }
            pick(tr!("在資料夾中顯示", "Show in folder"), EntryAction::Reveal);
            pick(tr!("複製檔案（貼到 LINE）", "Copy file (paste into LINE)"), EntryAction::CopyFile);
            if !audio {
                pick(tr!("製作加速版 / GIF / 壓縮…", "Speed up / GIF / compress…"), EntryAction::Export);
            }
        });
        return;
    }
    saved_actions(app, ui, path, audio);
}

/// 錄好的縮圖（點了在主畫面播放）
fn saved_thumb(app: &mut UiApp, ui: &mut Ui, path: &str, audio: bool) {
    let p = theme::pal(ui);
    // 縮圖（只錄聲音的沒有畫面：灰底加音符圖示）
    let (r, thumb) = ui.allocate_exact_size(vec2(68.0, 40.0), Sense::click());
    let mtime = file_mtime(path).unwrap_or(0.0);
    let rounding = CornerRadius::same(6);
    match (!audio).then(|| app.thumb(path, mtime)).flatten() {
        Some(t) => {
            ui.painter().add(egui::epaint::RectShape::filled(r, rounding, Color32::WHITE).with_texture(t.id(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))));
        }
        None => {
            ui.painter().rect_filled(r, rounding, p.surface2);
        }
    }
    ui.painter().rect_stroke(r, rounding, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    ui.painter().circle_filled(r.center(), 11.0, Color32::from_black_alpha(if thumb.hovered() { 200 } else { 140 }));
    theme::paint_icon(ui.painter(), Rect::from_center_size(r.center() + vec2(1.0, 0.0), vec2(10.0, 10.0)), Icon::Play, Color32::WHITE);
    if thumb.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("播放", "Play")).clicked() {
        app.act_path(EntryAction::PlayHere, path.to_string());
    }
}

/// 錄好的動作：剪輯（有畫面才有）、資料夾、複製、更多
fn saved_actions(app: &mut UiApp, ui: &mut Ui, path: &str, audio: bool) {
    if !audio && mini(ui, Icon::Cut, tr!("剪輯", "Edit")) {
        app.act_path(EntryAction::Edit, path.to_string());
    }
    if mini(ui, Icon::Folder, tr!("在資料夾中顯示", "Show in folder")) {
        app.act_path(EntryAction::Reveal, path.to_string());
    }
    if mini(ui, Icon::Copy, tr!("複製檔案（貼到 LINE）", "Copy file (paste into LINE)")) {
        app.act_path(EntryAction::CopyFile, path.to_string());
    }
    if !audio {
        let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("更多：製作加速版、GIF、壓縮…", "More: speed up, GIF, compress…")).show(ui);
        egui::Popup::menu(&more).show(|ui| {
            if ui.button(tr!("製作加速版 / GIF / 壓縮…", "Speed up / GIF / compress…")).clicked() {
                app.act_path(EntryAction::Export, path.to_string());
            }
        });
    }
}

/// 按了 ✕：記下來，這一則不再出現
fn close_notice(app: &mut UiApp, n: &Notice) {
    match n {
        Notice::Result(_, key) => app.main.dismissed_result = Some(key.clone()),
        Notice::Job(e) => app.dismissed_job = Some(e.id),
        Notice::AfterDone => app.main.dismissed_after = app.core.after_done().map(|d| d.seq),
        _ => {}
    }
}

/// 小的圖示按鈕（訊息的動作）
fn mini(ui: &mut Ui, icon: Icon, tip: &str) -> bool {
    Btn::icon_only(icon).ghost().small().tooltip(tip).show(ui).clicked()
}

/// 「▶ 播放」（有字，最常用）
fn play_btn(app: &mut UiApp, ui: &mut Ui, path: &str) {
    if Btn::new(tr!("播放", "Play")).icon(Icon::Play).small().show(ui).clicked() {
        app.act_path(EntryAction::PlayHere, path.to_string());
    }
}

/// 檔案的動作：▶ 播放、在資料夾中顯示、複製檔案（、剪輯、⋯ 製作加速版 / GIF / 壓縮）
fn file_actions(app: &mut UiApp, ui: &mut Ui, path: &str, edit: bool, export: bool) {
    play_btn(app, ui, path);
    if mini(ui, Icon::Folder, tr!("在資料夾中顯示", "Show in folder")) {
        app.act_path(EntryAction::Reveal, path.to_string());
    }
    if mini(ui, Icon::Copy, tr!("複製檔案（貼到 LINE）", "Copy file (paste into LINE)")) {
        app.act_path(EntryAction::CopyFile, path.to_string());
    }
    if edit && mini(ui, Icon::Cut, tr!("剪輯", "Edit")) {
        app.act_path(EntryAction::Edit, path.to_string());
    }
    if export {
        let more = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("更多：製作加速版、GIF、壓縮…", "More: speed up, GIF, compress…")).show(ui);
        egui::Popup::menu(&more).show(|ui| {
            if ui.button(tr!("製作加速版 / GIF / 壓縮…", "Speed up / GIF / compress…")).clicked() {
                app.act_path(EntryAction::Export, path.to_string());
            }
        });
    }
}

/// 一則訊息的動作（緊接在文字後面）
fn notice_actions(app: &mut UiApp, ui: &mut Ui, n: &Notice) {
    match n {
        Notice::Sys => {
            if Btn::new(tr!("重新下載…", "Download again…")).small().show(ui).clicked() {
                settings_dialog::open(app, Page::Components);
            }
        }
        Notice::Steps(_) => {
            if Btn::new(tr!("完成", "Finish"))
                .primary()
                .small()
                .tooltip(tr!("停止截圖，整理步驟後做成教學文件", "Stop capturing, organize the steps, then make the step-by-step guide"))
                .show(ui)
                .clicked()
            {
                let core = app.core.clone();
                app.spawn(async move { core.steps_finish().await }, |app, r| match r {
                    Ok(true) => {}
                    Ok(false) => app.toast(tr!("沒有截到任何步驟", "No steps were captured"), false),
                    Err(e) => app.toast(e.message().to_string(), true),
                });
            }
        }
        Notice::Alert => {
            if app.status.recorder.slow
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
        }
        Notice::Result(res, key) => match (&res.path, res.ok) {
            (Some(path), true) => {
                let audio = screenrecorder_core::after_record::is_audio_recording(path);
                file_actions(app, ui, path, true, !audio);
            }
            _ => {
                if res.parts_dir.is_some() && Btn::new(tr!("救回已錄的部分", "Recover what was recorded")).primary().small().show(ui).clicked() {
                    app.main.dismissed_result = Some(key.clone());
                    app.recover_now();
                }
                // 「詳細」開啟原本的完整卡片（原因與建議、救回、記錄檔…）
                let more = Btn::new(tr!("詳細", "Details")).small().show(ui);
                egui::Popup::menu(&more).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.set_width(400.0);
                    result_card(app, ui, res, key.clone());
                });
            }
        },
        Notice::Job(e) => match e.state {
            ExportState::Running => {
                if Btn::new(tr!("取消", "Cancel")).small().show(ui).clicked() {
                    let _ = app.core.exporter.cancel();
                }
            }
            ExportState::Done => {
                let cut = e.kind == ExportKind::Cut;
                file_actions(app, ui, &e.output, cut, cut);
            }
            _ => {
                if e.kind == ExportKind::Cut && Btn::new(tr!("回到剪輯", "Back to editing")).icon(Icon::Cut).small().show(ui).clicked() {
                    super::editor::resume(app, &e.source);
                }
            }
        },
        Notice::After => {
            if Btn::new(tr!("取消", "Cancel")).small().tooltip(tr!("取消自動處理（已經做好的會留著）", "Cancel post-processing (finished steps are kept)")).show(ui).clicked() {
                app.core.cancel_after();
            }
        }
        Notice::AfterDone => {
            let Some(d) = app.core.after_done() else { return };
            if let Some(out) = &d.output {
                play_btn(app, ui, out);
            }
            if mini(ui, Icon::Folder, tr!("在資料夾中顯示", "Show in folder")) {
                app.act_path(EntryAction::Reveal, d.output.clone().unwrap_or_else(|| d.source.clone()));
            }
        }
        Notice::Manual(e) => {
            let what = actions::job_label(e);
            if Btn::new(tr!("停止處理並開始錄影", "Stop it and start recording"))
                .icon(Icon::Stop)
                .small()
                .enabled(app.env.ffmpeg.encoder.is_some())
                .show(ui)
                .on_disabled_hover_text(tr!("沒有可以用的 H.264 編碼器：請重新下載錄影元件", "No usable H.264 encoder. Download the recording component again"))
                .clicked()
            {
                let config = app.s.record_config(&app.env);
                let core = app.core.clone();
                app.toast(trf!("已停止{what}，錄完後可以再做一次", "{what} was stopped. You can do it again after recording"), false);
                app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
            }
        }
        Notice::Schedule => {
            if Btn::new(tr!("取消排程", "Cancel")).small().show(ui).clicked() {
                app.core.cancel_schedule();
                app.toast(tr!("已取消排程錄影", "Scheduled recording canceled"), false);
            }
        }
        Notice::Warn(_, tip, page) => {
            if Btn::new(tr!("設定…", "Settings…")).small().tooltip(tip).show(ui).clicked() {
                settings_dialog::open(app, *page);
            }
        }
    }
}

// ───────────── 最近錄影 ─────────────

/// 右欄：最近錄影（卡片直排，捲動看全部，捲到快底時再多讀一批）＋檔案庫（錄影、截圖、Jellyfin）
fn recent_panel(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let panel = theme::card(ui).inner_margin(10.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let full = ui.max_rect();
        // 標題列：最近錄影、共幾支
        ui.horizontal(|ui| {
            let title = ui.label(RichText::new(tr!("最近錄影", "Recent recordings")).font(theme::font_bold(14.0)));
            // 窄的時候先拿掉「縮圖」兩個字，再不夠就拿掉「共幾支」（滑鼠移到標題上看得到）
            let size_label = tr!("縮圖", "Size");
            let w = |t: &str, f: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), f, Color32::WHITE).size().x;
            let seg_w = [tr!("小", "S"), tr!("中", "M"), tr!("大", "L")].iter().map(|t| w(t, theme::font(13.0)) + 22.0).sum::<f32>() + 8.0;
            let count = app.recent.as_ref().filter(|r| r.total > 0).map(|r| trf!("共 {} 支", "{} in total", r.total));
            let gap = ui.spacing().item_spacing.x;
            let room = ui.available_width() - seg_w - gap;
            let count_w = count.as_deref().map_or(0.0, |c| w(c, theme::font(13.0)) + gap);
            let show_label = count_w + w(size_label, theme::font(12.0)) + gap * 2.0 <= room;
            let show_count = count_w + gap <= room;
            match &count {
                Some(c) if show_count => drop(ui.label(theme::muted(ui, c))),
                Some(c) => drop(title.on_hover_text(c)),
                None => {}
            }
            // 縮圖大小：小、中、大（大：縮圖在上面、滿寬）
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut size = app.s.recent_size;
                // 由右往左排：反過來給，畫出來才是「小 中 大」
                let items = [(2u8, tr!("大", "L")), (1, tr!("中", "M")), (0, tr!("小", "S"))];
                if theme::segmented_h(ui, &mut size, &items, true, 22.0) {
                    app.s.recent_size = size;
                    app.save_settings();
                }
                if show_label {
                    ui.label(theme::muted(ui, size_label).font(theme::font(12.0)));
                }
            });
        });
        ui.add_space(4.0);
        // 下面的檔案庫：標題＋一排按鈕
        let jf = app.jellyfin().is_some();
        let lib_h = 62.0;
        let list = Rect::from_min_max(ui.cursor().min, pos2(full.max.x, full.max.y - lib_h - 10.0));
        let recent = app.recent.clone();
        let items: &[screenrecorder_core::types::LibraryEntry] = recent.as_ref().map_or(&[], |r| r.items.as_slice());
        let total = app.recent.as_ref().map_or(0, |r| r.total);
        // 第一張換了（剛錄好、存下回溯錄影、換了儲存位置）：捲回最上面
        let first = items.first().map(|e| e.media.path.clone());
        let to_top = first != app.main.recent_first || std::mem::take(&mut app.main.recent_to_top);
        app.main.recent_first = first;
        ui.scope_builder(UiBuilder::new().max_rect(list).layout(Layout::top_down(Align::Min)), |ui| {
            if items.is_empty() {
                let dir = app.s.out_dir(&app.env);
                let name = std::path::Path::new(&dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.clone());
                let text = trf!(
                    "還沒有錄影。按紅色的「錄影」錄第一支，錄好會出現在這裡（存在「{name}」資料夾）。",
                    "No recordings yet. Click the red “Record” button to make your first one; it will show up here (saved in the “{name}” folder)."
                );
                ui.add(egui::Label::new(theme::muted(ui, &text)).wrap()).on_hover_text(&dir);
                return;
            }
            let dates = date_labels(items.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
            ui.spacing_mut().item_spacing.y = 8.0;
            let mut area = egui::ScrollArea::vertical().id_salt("recent-list").auto_shrink([false, false]);
            if to_top {
                area = area.vertical_scroll_offset(0.0);
            }
            // 只畫看得到的卡片，幾百支也不會卡；右邊留一點給捲軸，不壓在卡片上
            let size = app.s.recent_size;
            let w = ui.available_width() - 10.0;
            let card_h = card_h(size, w);
            let out = area.show_rows(ui, card_h, items.len(), |ui, rows| {
                for e in &items[rows] {
                    recent_card(app, ui, e, dates.get(&e.media.name).cloned().unwrap_or_default(), w, size);
                }
            });
            // 捲到剩不到 3 張時再多讀 30 支（上一批還沒回來時不重複要）
            let left = out.content_size.y - out.state.offset.y - out.inner_rect.height();
            if left < (card_h + 8.0) * 3.0 && items.len() == app.recent_per_page && total > items.len() {
                app.recent_per_page += RECENT_BATCH;
                app.load_recent();
            }
        });
        let lib = Rect::from_min_max(pos2(full.min.x, full.max.y - lib_h), full.max);
        ui.painter().hline(lib.x_range(), lib.min.y - 6.0, Stroke::new(1.0, p.border));
        // 提示放在右欄「檔案庫」那一排的上方
        super::dialogs::toast_spot(ui.ctx(), pos2(lib.max.x, lib.min.y - 6.0));
        ui.scope_builder(UiBuilder::new().max_rect(lib).layout(Layout::top_down(Align::Min)), |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(tr!("檔案庫", "Library")).font(theme::font_bold(12.5)).color(p.muted));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let n = if jf { 3.0 } else { 2.0 };
                let w = ((ui.available_width() - 6.0 * (n - 1.0)) / n).floor();
                let rec_btn = Btn::new(tr!("錄影", "Videos"))
                    .icon(Icon::Video)
                    .small()
                    .min_width(w)
                    .tooltip(tr!("全部錄影：搜尋、整理、合併、一次處理", "All recordings: search, organize, merge, batch process"));
                if rec_btn.show(ui).clicked() {
                    app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video));
                }
                let shot_btn = Btn::new(tr!("截圖", "Screenshots"))
                    .icon(Icon::Camera)
                    .small()
                    .min_width(w)
                    .tooltip(tr!("全部截圖：用圖裡的字搜尋、整理、拼圖", "All screenshots: search by text in images, organize, combine"));
                if shot_btn.show(ui).clicked() {
                    app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot));
                }
                let jf_btn = Btn::new("Jellyfin")
                    .icon(Icon::Play)
                    .small()
                    .min_width(w)
                    .tooltip(tr!("Jellyfin 上的影片：搜尋、播放、下載、改影片資訊、找重複的", "Videos on Jellyfin: search, play, download, edit details, find duplicates"));
                if jf && jf_btn.show(ui).clicked() {
                    super::library_dialog::open_jellyfin(app);
                }
            });
        });
    });
    tour::mark(ui, Spot::Recent, panel.response.rect);
}

/// 最近錄影一次多讀幾支
pub(super) const RECENT_BATCH: usize = 30;

/// 最近錄影卡片的高度：小、中是縮圖在左（4:3，從 16:9 的畫面裁掉左右一點），大是縮圖在上面、滿寬 16:9
fn card_h(size: u8, w: f32) -> f32 {
    match size {
        0 => 80.0,
        1 => 112.0,
        _ => big_thumb_h(w) + 12.0 + RECENT_INFO_H,
    }
}

/// 大縮圖下面那一塊（名稱、長度大小、按鈕）的高度
const RECENT_INFO_H: f32 = 82.0;

fn big_thumb_h(w: f32) -> f32 {
    ((w - 12.0) * 9.0 / 16.0).round()
}

fn recent_card(app: &mut UiApp, ui: &mut Ui, e: &LibraryEntry, date: String, w: f32, size: u8) {
    let p = theme::pal(ui);
    let h = card_h(size, w);
    // 整張卡片可以按右鍵（和「⋯」同一個選單）
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let painter = ui.painter_at(rect);
    if app.recent.as_ref().and_then(|r| r.items.first()).is_some_and(|f| f.media.path == e.media.path) {
        tour::mark(ui, Spot::RecentCard, rect);
    }
    let playing = app.inline.as_ref().is_some_and(|i| i.is(&e.media.path));
    // 剛錄好的那支（控制列的「錄影已儲存」還沒關掉）：藍框、淡藍底
    let fresh = app.status.recorder.result.as_ref().filter(|r| r.ok).and_then(|r| r.path.as_deref()).is_some_and(|f| f == e.media.path && app.main.dismissed_result.as_deref() != Some(f));
    let edge = if playing || fresh { Stroke::new(2.0, p.accent) } else { Stroke::new(1.0, p.border) };
    let fill = if resp.hovered() {
        p.surface2
    } else if fresh {
        p.surface.lerp_to_gamma(p.accent, 0.08)
    } else {
        p.surface
    };
    painter.rect(rect, CornerRadius::same(theme::RADIUS_SM), fill, edge, StrokeKind::Inside);
    let big = size >= 2;
    let thumb =
        if big { Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(w - 12.0, big_thumb_h(w))) } else { Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2((h - 12.0) * 4.0 / 3.0, h - 12.0)) };
    // 名稱、長度大小、按鈕那一塊：小、中在縮圖右邊，大在縮圖下面
    let info = if big { Rect::from_min_max(pos2(rect.min.x + 10.0, thumb.max.y + 2.0), rect.max) } else { Rect::from_min_max(pos2(thumb.max.x + 10.0, rect.min.y), rect.max) };
    painter.rect_filled(thumb, CornerRadius::same(5), p.surface2);
    if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
        theme::paint_thumb_cover(&painter, thumb, &t);
    }
    // 縮圖：按一下就播放（中間有播放鍵，滑鼠移上去變明顯）
    let thumb_resp = ui.interact(thumb, Id::new(("recent-thumb", &e.media.path)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let hot_thumb = thumb_resp.hovered();
    // 滑過縮圖：左右移動，縮圖直接換成那個時間點的畫面（聲音檔仍顯示「播放」提示）
    // 停住不動約 1 秒：在縮圖裡靜音播放（從停住的時間點開始）
    let mut scrubbing = false;
    let thumb_resp = if hot_thumb && e.media.path.to_ascii_lowercase().ends_with(".mp4") {
        let dur = e.media.duration_sec.unwrap_or(0.0);
        let vsize = (e.media.width.unwrap_or(0), e.media.height.unwrap_or(0));
        let fps = e.media.fps.unwrap_or(0.0);
        if !super::hover_play::hover(ui.ctx(), &e.media.path, thumb, dur, |_| super::thumbs::hover_player(app, &e.media.path, dur, fps, vsize)) {
            scrubbing = super::thumbs::scrub_here(app, ui, thumb, &e.media.path, e.media.mtime, dur);
        }
        thumb_resp
    } else {
        thumb_resp.on_hover_text(tr!("播放", "Play"))
    };
    if !super::hover_play::paint(&painter, &e.media.path, thumb, 5) && !(scrubbing && super::thumbs::paint_scrub(&painter, &e.media.path, thumb, 5)) {
        painter.circle_filled(thumb.center(), if hot_thumb { 15.0 } else { 13.0 }, Color32::from_black_alpha(if hot_thumb { 170 } else { 110 }));
        let c = thumb.center();
        painter.add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -6.5), c + vec2(7.0, 0.0), c + vec2(-4.0, 6.5)], Color32::WHITE, Stroke::NONE));
    }
    if thumb_resp.clicked() {
        app.known.insert(e.media.path.clone(), e.clone());
        app.act(EntryAction::PlayHere, e.clone());
    }
    // 按兩下卡片：在播放器開啟（完整的功能）
    if resp.double_clicked() || thumb_resp.double_clicked() {
        app.inline = None;
        app.known.insert(e.media.path.clone(), e.clone());
        app.act(EntryAction::Play, e.clone());
    }
    let x = info.min.x;
    let y0 = info.min.y;
    let meta = format!("{}・{}", e.media.duration_sec.map(short_clock).unwrap_or_else(|| "—".into()), format_bytes(e.media.bytes));
    let meta_g = painter.layout_no_wrap(meta, theme::font(12.0), p.muted);
    let meta_w = meta_g.size().x;
    painter.galley(pos2(x, y0 + 28.0), meta_g, p.muted);
    // 另外做過的版本（加速版、GIF…）：在長度、大小後面寫「另有 2 個版本」（放不下時「+2 版本」），滑鼠提示列出來
    if !e.exports.is_empty() {
        let n = e.exports.len();
        let list: Vec<String> = e.exports.iter().map(super::library_dialog::export_tag).collect();
        let room = rect.max.x - 8.0 - (x + meta_w + 8.0);
        let long = if is_en() { format!("{n} more version{}", if n == 1 { "" } else { "s" }) } else { format!("另有 {n} 個版本") };
        let short = trf!("+{n} 版本", "+{n}");
        let fits = |t: &str| ui.painter().layout_no_wrap(t.to_string(), theme::font_bold(12.0), p.accent).size().x + 14.0 <= room;
        let text = if fits(&long) { long } else { short };
        let chip_rect = Rect::from_min_max(pos2(x + meta_w + 8.0, y0 + 24.0), pos2(rect.max.x - 6.0, y0 + 46.0));
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(chip_rect).layout(Layout::left_to_right(Align::Center)));
        chip(ui, &text, Tone::Accent, false).on_hover_text(trf!("已製作 {}", "Made: {}", list.join(tr!("、", ", "))));
    }
    // 第一列：日期（或名稱）與標籤
    let top = Rect::from_min_max(pos2(x, y0 + 5.0), pos2(rect.max.x - 6.0, y0 + 27.0));
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
    // 操作清單建一次：下面的按鈕和按右鍵的選單共用
    let groups = super::entry_menu::groups(app, e, Place::Home);
    {
        let ui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        // 圖示之間不留空隙（按鈕本身有內距），卡片窄一點，一列放得下更多張
        ui.spacing_mut().item_spacing.x = 0.0;
        // 播放、剪輯（、傳到媒體庫）直接按得到，其他在「⋯」（和全部錄影、檢視器同一份清單）
        let slots = if app.s.after_copy_to.trim().is_empty() { 2 } else { 3 };
        let hot = ui.rect_contains_pointer(rect);
        let more_tip = tr!("更多（也可以在卡片上按右鍵）", "More (or right-click the card)");
        // 「大」的卡片比較寬：資料夾、製作、複製檔案、改名、刪除也直接按得到（放不下的在「⋯」）
        let picked = if big {
            use EntryAction as A;
            let wanted = [A::PlayHere, A::Edit, A::SendToLibrary, A::Reveal, A::Export, A::CopyFile, A::Rename, A::Trash];
            super::entry_menu::wide_icons(ui, &groups, hot, &wanted, more_tip)
        } else {
            super::entry_menu::quick_icons(ui, &groups, hot, slots, slots, more_tip)
        };
        if let Some(a) = picked {
            app.known.insert(e.media.path.clone(), e.clone());
            app.act(a, e.clone());
        }
    }
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
