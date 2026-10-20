//! 設定視窗：左邊分頁（一般 / 錄影 / 聲音 / 截圖 / 存檔與分享 / 錄完自動處理 / 快速鍵 / 進階 / 外部元件 / 關於），右邊表單（標籤固定寬度，控制項對齊）。
//! 主畫面下方只留摘要，點一下開到對應的分頁。

use super::settings::{FPS_CHOICES, MAX_MB, MAX_PRESETS, UI_MIN_SIZE, UI_SCALES};
use super::theme::{self, segmented, switch, Btn, Icon};
use super::{dialogs, main_view, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::components::Kind;
use screenrecorder_core::format::{human_duration, output_size};
use screenrecorder_core::types::{EncoderPreference, Hotkey, Hotkeys, MethodPreference, RecBarMode, CAMERA_SIZES};
use screenrecorder_core::version::{release_notes, APP_VERSION, ISSUES_URL, PROJECT_URL};
use screenrecorder_core::{tr, trf};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 語言、外觀、介面大小、開機、結束程式
    General,
    Record,
    Audio,
    /// 截圖後、框選截圖
    Shots,
    /// 錄影、截圖資料夾、清單、檔名，與媒體庫（NAS、Jellyfin）
    Save,
    /// 錄完自動處理
    After,
    Keys,
    Advanced,
    /// 外部元件：FFmpeg、語音辨識用的 FFmpeg、語音模型的下載與 Proxy
    Components,
    /// 版本、更新說明、新版本、連結
    About,
}

/// 分頁：中文名稱、英文名稱、圖示
const PAGES: [(Page, &str, &str, Icon); 10] = [
    (Page::General, "一般", "General", Icon::Window),
    (Page::Record, "錄影", "Recording", Icon::Video),
    (Page::Audio, "聲音", "Audio", Icon::Speaker),
    (Page::Shots, "截圖", "Screenshots", Icon::Camera),
    (Page::Save, "存檔與分享", "Saving & sharing", Icon::Folder),
    (Page::After, "錄完自動處理", "After recording", Icon::Cut),
    (Page::Keys, "快速鍵", "Shortcuts", Icon::Keyboard),
    (Page::Advanced, "進階", "Advanced", Icon::Settings),
    (Page::Components, "外部元件", "Components", Icon::Download),
    (Page::About, "關於", "About", Icon::Info),
];

/// 設定視窗的狀態（輸入中的文字、設定中的快速鍵）
pub struct SettingsDialog {
    pub page: Page,
    max_custom_open: bool,
    /// 正在設定第幾個快速鍵（等使用者按下新的組合）
    key_capture: Option<usize>,
    /// 快速鍵的提示（第幾列、內容）：顯示在那一列下面
    key_msg: Option<(usize, String)>,
    /// 按下的組合已經用在別的功能：(這一列, 用到的那一列, 組合)，可以改用這組、原本的停用
    key_conflict: Option<(usize, usize, Hotkey)>,
    /// 有視窗的程式（系統聲音只錄某個程式用；None = 還沒找）
    apps: Option<Vec<screenrecorder_core::winui::AppInfo>>,
    /// 開機時自動啟動：None = 還沒讀；Some(None) = 不能設定（開發版）
    autostart: Option<Option<bool>>,
    autostart_busy: bool,
    /// 外部元件：正在安裝選擇的檔案
    installing: Option<Kind>,
    /// 外部元件：測試連線中 / 結果
    net_testing: bool,
    net_result: Option<Result<(), String>>,
    /// Jellyfin：測試連線中 / 結果（成功時是伺服器名稱）
    jf_testing: bool,
    jf_result: Option<Result<String, String>>,
}

pub fn open(app: &mut UiApp, page: Page) {
    match &mut app.settings_dlg {
        Some(d) => d.page = page,
        None => {
            app.settings_dlg = Some(SettingsDialog {
                page,
                max_custom_open: false,
                key_capture: None,
                key_msg: None,
                key_conflict: None,
                apps: None,
                autostart: None,
                autostart_busy: false,
                installing: None,
                net_testing: false,
                net_result: None,
                jf_testing: false,
                jf_result: None,
            })
        }
    }
}

/// 主畫面摘要：錄影（第一項一定是每秒張數）
pub fn record_summary(app: &UiApp) -> Vec<String> {
    let s = &app.s;
    let mut v = vec![trf!("每秒 {} 張", "{} fps", s.fps)];
    if s.scale != 100 {
        v.push(trf!("解析度 {}%", "Resolution {}%", s.scale));
    }
    v.push(if s.draw_mouse { tr!("含游標", "With cursor").into() } else { tr!("不含游標", "No cursor").into() });
    match (s.show_clicks, s.show_keys) {
        (true, true) => v.push(tr!("顯示點擊與按鍵", "Show clicks and keys").into()),
        (true, false) => v.push(tr!("顯示點擊", "Show clicks").into()),
        (false, true) => v.push(tr!("顯示按鍵", "Show keys").into()),
        _ => {}
    }
    if !s.camera.is_empty() {
        v.push(tr!("攝影機", "Camera").into());
    }
    if s.countdown_sec != 3 {
        v.push(if s.countdown_sec > 0 { trf!("倒數 {} 秒", "{} s countdown", s.countdown_sec) } else { tr!("不倒數", "No countdown").into() });
    }
    if s.max_minutes > 0.0 {
        v.push(trf!("最長 {}", "Max {}", human_duration(s.max_minutes * 60.0)));
    }
    if s.auto_pause_sec > 0 {
        v.push(trf!("沒動靜 {} 自動暫停", "Auto-pause after {} idle", human_duration(s.auto_pause_sec as f64)));
    }
    v
}

/// 主畫面摘要：聲音
pub fn audio_summary(app: &UiApp) -> (String, Icon) {
    let label = match (app.s.audio_system, app.s.audio_mic) {
        (true, true) => tr!("系統＋麥克風", "System + mic"),
        (true, false) => tr!("系統聲音", "System audio"),
        (false, true) => tr!("麥克風", "Microphone"),
        _ => tr!("不錄聲音", "No audio"),
    };
    // 只錄某個程式：「chrome.exe＋麥克風」
    let label = match (app.s.audio_system && !app.s.audio_app.is_empty(), app.s.audio_mic) {
        (true, true) => trf!("{}＋麥克風", "{} + mic", app.s.audio_app),
        (true, false) => app.s.audio_app.clone(),
        _ => label.to_string(),
    };
    (label, if app.s.audio_mic && !app.s.audio_system { Icon::Mic } else { Icon::Speaker })
}

const NAV_W: f32 = 180.0;
const BODY_W: f32 = 600.0;
/// 內容區的高度（視窗太矮時縮短，內容可捲動）
const BODY_H: f32 = 470.0;

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.settings_dlg.take() else {
        return;
    };
    // 設定快速鍵時，Esc 只取消設定；下拉選單開著時，Esc 與點外面只關下拉選單
    let busy = d.key_capture.is_some() || egui::Popup::is_any_open(ctx);
    let mut close = false;
    let modal = egui::Modal::new(Id::new("settings")).frame(theme::modal_frame(ctx).inner_margin(0)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let body_h = BODY_H.min(ctx.content_rect().height() - 140.0).max(240.0);
        let total = vec2(NAV_W + BODY_W, 56.0 + body_h);
        let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
        // 標題列
        let head = Rect::from_min_size(rect.min, vec2(total.x, 56.0));
        ui.scope_builder(UiBuilder::new().max_rect(head.shrink2(vec2(20.0, 0.0))).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.label(RichText::new(tr!("設定", "Settings")).font(theme::font_bold(18.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.painter().hline(rect.x_range(), head.bottom(), Stroke::new(1.0, p.border));
        // 左邊分頁
        let nav = Rect::from_min_max(pos2(rect.left(), head.bottom()), pos2(rect.left() + NAV_W, rect.bottom()));
        ui.painter().rect_filled(nav.shrink(1.0), CornerRadius { sw: theme::RADIUS - 1, ..Default::default() }, p.surface2);
        ui.painter().vline(nav.right(), nav.y_range(), Stroke::new(1.0, p.border));
        ui.scope_builder(UiBuilder::new().max_rect(nav.shrink2(vec2(10.0, 12.0))).layout(Layout::top_down(Align::Min)), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            // 視窗矮的時候分頁矮一點，全部放得下
            let h = (ui.available_height() / PAGES.len() as f32 - 2.0).clamp(26.0, 34.0);
            for (page, zh, en, icon) in PAGES {
                if nav_item(ui, tr!(zh, en), icon, d.page == page, h).clicked() && d.page != page {
                    d.page = page;
                }
            }
        });
        // 右邊內容：用獨立的子區域（new_child），內容再寬也不會撐大視窗；每個分頁的視窗大小都一樣，超出的部分裁掉
        let body = Rect::from_min_max(pos2(nav.right() + 1.0, head.bottom()), rect.max);
        let mut page_ui = ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(24.0, 16.0))).layout(Layout::top_down(Align::Min)));
        page_ui.shrink_clip_rect(body);
        {
            let ui = &mut page_ui;
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            egui::ScrollArea::vertical().id_salt(("settings-page", d.page as u8)).auto_shrink([false, false]).show(ui, |ui| {
                // 右邊留給捲軸，按鈕不會貼著捲軸
                ui.set_max_width(ui.available_width() - 14.0);
                let locked = app.locked() && !matches!(d.page, Page::Keys | Page::About);
                if locked {
                    form_hint_full(ui, tr!("錄影中無法變更這些設定，停止錄影後再調整。", "These settings can't be changed while recording. Stop recording first."), p.warn);
                    ui.add_space(6.0);
                }
                ui.add_enabled_ui(!locked, |ui| match d.page {
                    Page::General => general_page(app, &mut d, ui),
                    Page::Record => record_page(app, &mut d, ui),
                    Page::Audio => audio_page(app, &mut d, ui),
                    Page::Shots => shots_page(app, ui),
                    Page::Save => save_page(app, &mut d, ui),
                    Page::After => after_record_section(app, &mut d, ui),
                    Page::Keys => keys_page(app, &mut d, ui),
                    Page::Advanced => advanced_page(app, ui),
                    Page::Components => components_page(app, &mut d, ui),
                    Page::About => about_page(app, ui),
                });
            });
        }
    });
    if !busy && modal.should_close() {
        close = true;
    }
    if close {
        if d.key_capture.is_some() {
            stop_capture(app, &mut d);
        }
    } else {
        app.settings_dlg = Some(d);
    }
}

/// 左邊的分頁項目
fn nav_item(ui: &mut Ui, text: &str, icon: Icon, selected: bool, h: f32) -> egui::Response {
    let p = theme::pal(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let bg = if selected {
        p.surface
    } else if resp.hovered() {
        p.surface.gamma_multiply(0.6)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, CornerRadius::same(8), bg);
    if selected {
        ui.painter().rect_filled(Rect::from_center_size(pos2(r.left() + 1.5, r.center().y), vec2(3.0, 16.0)), CornerRadius::same(2), p.accent);
    }
    let color = if selected { p.text } else { p.muted };
    theme::paint_icon(ui.painter(), Rect::from_center_size(pos2(r.left() + 22.0, r.center().y), vec2(16.0, 16.0)), icon, color);
    let f = if selected { theme::font_bold(13.5) } else { theme::font(13.5) };
    ui.painter().text(pos2(r.left() + 40.0, r.center().y), egui::Align2::LEFT_CENTER, text, f, color);
    resp
}

// ───────────── 表單（左邊標籤固定寬度，控制項對齊） ─────────────

const FORM_LABEL_W: f32 = 112.0;
const FORM_CTRL_W: f32 = 240.0;
const FORM_ROW_H: f32 = 34.0;

/// 分區標題（右邊可放按鈕）
fn form_section(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.set_height(26.0);
        ui.label(RichText::new(title).font(theme::font_bold(14.0)).color(p.text));
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.add_space(2.0);
}

/// 表單的一列：左邊標籤（灰色、固定寬度），右邊控制項
fn form_row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // 標籤太長（例如英文）就在這一欄裡換行，這一列跟著變高，不會壓到右邊的控制項或下一列
        let g = ui.painter().layout(label.to_string(), theme::font(13.0), p.muted, FORM_LABEL_W - 4.0);
        let h = FORM_ROW_H.max(g.size().y + 8.0);
        let (r, _) = ui.allocate_exact_size(vec2(FORM_LABEL_W, h), Sense::hover());
        ui.painter().galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g, p.muted);
        ui.set_min_height(h);
        add(ui);
    });
}

/// 控制項下方的說明（對齊控制項那一欄）
fn form_hint(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    ui.horizontal(|ui| {
        ui.add_space(FORM_LABEL_W + 6.0);
        ui.add(egui::Label::new(RichText::new(text.into()).font(theme::font(12.0)).color(color)).wrap());
    });
}

/// 整列寬的說明
fn form_hint_full(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text.into()).font(theme::font(12.5)).color(color)).wrap());
}

fn form_divider(ui: &mut Ui) {
    let p = theme::pal(ui);
    ui.add_space(8.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, p.border));
    ui.add_space(8.0);
}

/// 表單的下拉選單（與按鈕相同外觀）；items 的第三個值 = 能不能選。
/// 寬度固定：選項太長（例如裝置名稱）時結尾顯示「…」，滑鼠移上去看完整名稱
fn form_combo<T: PartialEq + Copy>(ui: &mut Ui, id: &str, width: f32, value: &mut T, items: &[(T, String, bool)]) -> bool {
    let mut changed = false;
    let text = items.iter().find(|(v, _, _)| v == value).map(|(_, t, _)| t.clone()).unwrap_or_default();
    ui.scope(|ui| {
        theme::field_style(ui);
        let r = egui::ComboBox::from_id_salt(id).selected_text(RichText::new(&text).font(theme::font(13.0))).width(width).truncate().show_ui(ui, |ui| {
            for (v, t, en) in items {
                if ui.add_enabled(*en, egui::Button::selectable(value == v, RichText::new(t).font(theme::font(13.0)))).clicked() && value != v {
                    *value = *v;
                    changed = true;
                }
            }
        });
        let full = ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), theme::font(13.0), Color32::WHITE).size().x);
        if full > width - 36.0 {
            r.response.on_hover_text(text);
        }
    });
    changed
}

// ───────────── 錄影 ─────────────

/// 攝影機子母畫面：選攝影機、位置、大小、形狀
fn camera_rows(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    app.load_cameras(false);
    let searching = app.cameras.is_none();
    let found = app.cameras.clone().unwrap_or_default();
    form_row(ui, tr!("攝影機", "Camera"), |ui| {
        let mut names: Vec<String> = vec![String::new()];
        names.extend(found.iter().cloned());
        if !app.s.camera.is_empty() && !found.contains(&app.s.camera) {
            names.push(app.s.camera.clone());
        }
        let mut sel = names.iter().position(|n| *n == app.s.camera).unwrap_or(0);
        let items: Vec<(usize, String, bool)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                (
                    i,
                    if n.is_empty() {
                        tr!("不使用", "None").to_string()
                    } else if found.contains(n) || searching {
                        n.clone()
                    } else {
                        trf!("{n}（找不到）", "{n} (not found)")
                    },
                    true,
                )
            })
            .collect();
        if form_combo(ui, "camera", FORM_CTRL_W + 60.0, &mut sel, &items) {
            app.s.camera = names[sel].clone();
            app.save_settings();
        }
        if Btn::icon_only(Icon::Refresh).ghost().small().enabled(!searching).tooltip(tr!("重新尋找攝影機", "Search for cameras again")).show(ui).clicked() {
            app.load_cameras(true);
        }
    });
    if app.s.camera.is_empty() {
        if searching {
            form_hint(ui, tr!("正在尋找攝影機…", "Looking for cameras…"), p.muted);
        } else if found.is_empty() {
            form_hint(ui, if cfg!(windows) { tr!("沒有找到攝影機。", "No camera found.") } else { tr!("攝影機只支援 Windows。", "Camera is supported on Windows only.") }, p.muted);
        }
        return;
    }
    form_row(ui, tr!("攝影機位置", "Camera position"), |ui| {
        // 錄影時拖曳過：多一個「上次拖曳的位置」；改選角落就不再用記住的位置
        const DRAGGED: u8 = u8::MAX;
        let mut c = if app.s.camera_pos.is_some() { DRAGGED } else { app.s.camera_corner };
        let mut items = vec![
            (0u8, tr!("右下", "Bottom right").to_string(), true),
            (1, tr!("左下", "Bottom left").into(), true),
            (2, tr!("右上", "Top right").into(), true),
            (3, tr!("左上", "Top left").into(), true),
        ];
        if app.s.camera_pos.is_some() {
            items.push((DRAGGED, tr!("上次拖曳的位置", "Last dragged position").into(), true));
        }
        if form_combo(ui, "cameraCorner", FORM_CTRL_W, &mut c, &items) && c != DRAGGED {
            app.s.camera_corner = c;
            app.s.camera_pos = None;
            app.save_settings();
        }
    });
    form_row(ui, tr!("大小與形狀", "Size and shape"), |ui| {
        let mut sz = app.s.camera_size;
        let items: Vec<(u32, &str)> = CAMERA_SIZES.iter().copied().zip(tr!(["小", "中", "大"], ["S", "M", "L"])).collect();
        if segmented(ui, &mut sz, &items, true) {
            app.s.camera_size = sz;
            app.save_settings();
        }
        if !CAMERA_SIZES.contains(&app.s.camera_size) {
            // 在小視窗上調過大小
            ui.label(RichText::new(tr!("自訂", "Custom")).font(theme::font(12.0)).color(p.muted));
        }
        let mut circle = app.s.camera_circle;
        if segmented(ui, &mut circle, &[(true, tr!("圓形", "Circle")), (false, tr!("方形", "Square"))], true) {
            app.s.camera_circle = circle;
            app.save_settings();
        }
    });
    form_hint(
        ui,
        tr!(
            "主畫面開著與錄影時，螢幕上會出現攝影機小視窗（左右翻轉，像照鏡子），看到的就是錄到的。拖曳中間移動、拖曳外圈或轉動滑鼠滾輪調整大小，都會記住。攝影機被其他程式使用時，錄影不含攝影機。",
            "While the main window is open and while recording, a camera bubble appears on screen (flipped like a mirror); what you see is what gets recorded. Drag the middle to move it; drag the edge or scroll the mouse wheel to resize. Both are remembered. If another app is using the camera, the recording won't include it."
        ),
        p.muted,
    );
}

fn record_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, tr!("畫面", "Video"), |_| {});
    form_row(ui, tr!("每秒張數", "Frame rate"), |ui| {
        let mut fps = app.s.fps;
        let mut items: Vec<(f64, String, bool)> = FPS_CHOICES.iter().map(|f| (*f, trf!("每秒 {f} 張", "{f} fps"), true)).collect();
        if !FPS_CHOICES.contains(&fps) {
            items.push((fps, trf!("每秒 {fps} 張", "{fps} fps"), true));
        }
        if form_combo(ui, "fps", FORM_CTRL_W, &mut fps, &items) {
            app.s.fps = fps;
            app.save_settings();
        }
    });
    form_hint(ui, tr!("一般操作畫面 30 就很順；要錄影片或遊戲再用 60。", "30 is smooth for everyday screen work; use 60 for videos or games."), p.muted);
    form_row(ui, tr!("解析度", "Resolution"), |ui| {
        let mut sc = app.s.scale;
        let items: Vec<(u32, String, bool)> = [(100, tr!("100%（原尺寸）", "100% (original size)")), (75, "75%"), (50, "50%"), (25, "25%")].iter().map(|(v, t)| (*v, t.to_string(), true)).collect();
        if form_combo(ui, "scale", FORM_CTRL_W, &mut sc, &items) {
            app.s.scale = sc;
            app.save_settings();
        }
    });
    if let Some(r) = app.s.source_rect(&app.env) {
        let (w, h) = output_size(r.width, r.height, app.s.scale as f64);
        let big = (w as f64) * (h as f64) > 3840.0 * 2160.0 * 1.05;
        let note = if big { tr!("（很大，建議 50%）", " (very large; 50% recommended)") } else { "" };
        form_hint(ui, trf!("目前範圍 {}×{}，輸出 {w}×{h}{note}", "Current area {}×{}, output {w}×{h}{note}", r.width, r.height), if big { p.warn } else { p.muted });
    }
    form_divider(ui);
    form_section(ui, tr!("游標與強調", "Cursor and highlights"), |_| {});
    form_row(ui, tr!("滑鼠游標", "Mouse cursor"), |ui| {
        let mut dm = app.s.draw_mouse;
        if switch(ui, &mut dm, tr!("錄進影片", "Include in recording"), true)
            .on_hover_text(tr!(
                "關掉時影片裡沒有游標，剪輯時可以換成平滑移動、放大的游標（錄影時一樣會記下游標的位置）",
                "When off, the video has no cursor and you can add a smooth, larger cursor when editing (the cursor position is still recorded)"
            ))
            .changed()
        {
            app.s.draw_mouse = dm;
            app.save_settings();
        }
    });
    form_row(ui, tr!("點擊與按鍵", "Clicks and keys"), |ui| {
        ui.vertical(|ui| {
            let mut c = app.s.show_clicks;
            if switch(ui, &mut c, tr!("顯示滑鼠點擊", "Show mouse clicks"), true)
                .on_hover_text(tr!(
                    "按下滑鼠時在游標位置出現一圈波紋（左鍵黃色、右鍵藍色），錄進影片，看的人知道點了哪裡",
                    "Shows a ripple at the cursor when you click (yellow for left, blue for right), recorded into the video so viewers can see where you clicked"
                ))
                .changed()
            {
                app.s.show_clicks = c;
                app.save_settings();
            }
            let mut k = app.s.show_keys;
            if switch(ui, &mut k, tr!("顯示按下的快速鍵", "Show pressed shortcuts"), true)
                .on_hover_text(tr!(
                    "按下 Ctrl / Alt / Win 組合鍵或 Enter、Esc 等功能鍵時，在畫面下方顯示按了什麼（例如「Ctrl + C」）；一般打字不會顯示",
                    "When you press Ctrl / Alt / Win combinations or keys like Enter and Esc, shows what you pressed at the bottom of the screen (e.g. “Ctrl + C”). Normal typing isn't shown"
                ))
                .changed()
            {
                app.s.show_keys = k;
                app.save_settings();
            }
        });
    });
    form_row(ui, tr!("光暈", "Highlight"), |ui| {
        let mut h = app.s.cursor_halo;
        if switch(ui, &mut h, tr!("游標周圍加光暈", "Cursor highlight"), true)
            .on_hover_text(tr!("游標周圍有一圈淡黃色的光暈，看影片的人比較容易跟上游標在哪裡", "Adds a soft yellow glow around the cursor so viewers can follow it more easily"))
            .changed()
        {
            app.s.cursor_halo = h;
            app.save_settings();
        }
    });
    if app.s.show_clicks || app.s.show_keys || app.s.cursor_halo {
        form_hint(ui, tr!("波紋、按鍵與光暈會錄進影片；只支援 Windows。", "Ripples, keys and the highlight are recorded into the video. Windows only."), p.muted);
    }
    form_hint(
        ui,
        tr!(
            "平滑放大的游標、跟著點擊放大、點擊音效不用在錄影前決定：錄完在剪輯視窗的「畫面」「聲音」分頁加上。",
            "A smooth, larger cursor, zoom on clicks and click sounds don't need to be decided before recording: add them later in the editor's Frame and Audio tabs."
        ),
        p.muted,
    );
    form_divider(ui);
    form_section(ui, tr!("錄影時", "While recording"), |_| {});
    form_row(ui, tr!("桌面", "Desktop"), |ui| {
        let mut v = app.s.hide_icons;
        if switch(ui, &mut v, tr!("錄影時隱藏桌面圖示", "Hide desktop icons while recording"), true)
            .on_hover_text(tr!("開始錄影時把桌面上的圖示藏起來，畫面比較乾淨；停止後自動還原", "Hides desktop icons when recording starts for a cleaner look; they come back when you stop"))
            .changed()
        {
            app.s.hide_icons = v;
            app.save_settings();
        }
    });
    form_row(ui, tr!("通知", "Notifications"), |ui| {
        let mut v = app.s.rec_quiet;
        if switch(ui, &mut v, tr!("錄影時打開勿擾", "Turn on Do not disturb while recording"), true)
            .on_hover_text(tr!(
                "開始錄影時打開 Windows 的勿擾（只允許鬧鐘），LINE、Outlook 等的通知不會跳出來被錄進去；停止後恢復原本的設定。自己畫通知視窗的程式擋不住",
                "Turns on Windows Do not disturb (alarms only) when recording starts so notifications from apps like Outlook don't pop up in the video; your previous setting comes back when you stop. Apps that draw their own pop-ups can't be blocked"
            ))
            .changed()
        {
            app.s.rec_quiet = v;
            app.save_settings();
        }
    });
    masks_row(app, ui);
    camera_rows(app, ui);
    form_divider(ui);
    form_section(ui, tr!("開始與結束", "Start and stop"), |_| {});
    form_row(ui, tr!("開始前倒數", "Countdown"), |ui| {
        let mut cd = app.s.countdown_sec;
        let items = [(0, tr!("不倒數", "No countdown").to_string(), true), (3, tr!("3 秒", "3 s").into(), true), (5, tr!("5 秒", "5 s").into(), true), (10, tr!("10 秒", "10 s").into(), true)];
        if form_combo(ui, "countdown", FORM_CTRL_W, &mut cd, &items) {
            app.s.countdown_sec = cd;
            app.save_settings();
        }
    });
    form_row(ui, tr!("最長錄影時間", "Time limit"), |ui| {
        let custom = d.max_custom_open || !MAX_PRESETS.contains(&app.s.max_minutes);
        let mut sel: f64 = if custom { -1.0 } else { app.s.max_minutes };
        let items = [
            (0.0, tr!("不限", "No limit").to_string(), true),
            (30.0, tr!("30 分鐘", "30 min").into(), true),
            (60.0, tr!("1 小時", "1 h").into(), true),
            (120.0, tr!("2 小時", "2 h").into(), true),
            (-1.0, tr!("自訂…", "Custom…").into(), true),
        ];
        if form_combo(ui, "maxPreset", if custom { 120.0 } else { FORM_CTRL_W }, &mut sel, &items) {
            d.max_custom_open = sel < 0.0;
            if sel >= 0.0 {
                app.s.max_minutes = sel;
                app.save_settings();
            } else if app.s.max_minutes <= 0.0 {
                // 從「不限」改成自訂：先給一個常用的長度，再用 − ＋ 調整
                app.s.max_minutes = 45.0;
                app.save_settings();
            }
        }
        if custom {
            // 用 − ＋ 調整（也可以直接打數字；看不懂的輸入不會變成「不限」）
            let mut v = app.s.max_minutes.max(1.0);
            if theme::stepper(ui, Id::new("max-minutes"), &mut v, 5.0, 1.0..=screenrecorder_core::format::MAX_MINUTES_MAX, 0, tr!("分鐘", "min")) {
                app.s.max_minutes = v.round();
                app.save_settings();
            }
        }
    });
    form_hint(ui, tr!("時間到自動停止並儲存。", "Recording stops and saves automatically when time is up."), p.muted);
    form_row(ui, tr!("沒動靜時", "When idle"), |ui| {
        let mut sec = app.s.auto_pause_sec;
        let label = |s: u32| if s == 0 { tr!("繼續錄", "Keep recording").to_string() } else { trf!("{} 後自動暫停", "Pause after {}", human_duration(s as f64)) };
        let items: Vec<(u32, String, bool)> = super::settings::AUTO_PAUSE.iter().map(|s| (*s, label(*s), true)).collect();
        if form_combo(ui, "autoPause", FORM_CTRL_W, &mut sec, &items) {
            app.s.auto_pause_sec = sec;
            app.save_settings();
        }
    });
    form_hint(
        ui,
        tr!(
            "滑鼠、鍵盤都沒動、也沒有聲音時自動暫停，動一下滑鼠或按鍵就接著錄，影片裡不會有一大段空白。只錄聲音、回溯錄影不適用。",
            "Pauses when the mouse and keyboard are idle and there's no sound; move the mouse or press a key to continue, so the video has no long blank stretches. Not used for audio-only or the replay buffer."
        ),
        p.muted,
    );
    form_row(ui, tr!("操作視窗", "Main window"), |ui| {
        let mut hide = app.s.hide_ui;
        if switch(ui, &mut hide, tr!("開始錄影時縮小", "Minimize when recording starts"), true)
            .on_hover_text(tr!("視窗不在錄影範圍內時不縮小", "Not minimized when the window is outside the capture area"))
            .changed()
        {
            app.s.hide_ui = hide;
            app.save_settings();
        }
    });
    form_row(ui, tr!("錄影控制列", "Recording bar"), |ui| {
        let mut m = app.s.rec_bar;
        let items = [
            (RecBarMode::Full, tr!("完整：時間與按鈕", "Full: time and buttons").to_string(), true),
            (RecBarMode::Compact, tr!("精簡：只有狀態點與停止", "Compact: status dot and stop").into(), true),
            (RecBarMode::Hidden, tr!("不顯示", "Hidden").into(), true),
        ];
        if form_combo(ui, "recBar", FORM_CTRL_W, &mut m, &items) {
            app.s.rec_bar = m;
            app.save_settings();
        }
    });
    let hint = match app.s.rec_bar {
        RecBarMode::Hidden => match app.keys.label(0) {
            k if k.is_empty() => tr!("倒數完就隱藏。要停止錄影時，從系統匣或這個視窗停止。", "Hidden once the countdown ends. Stop recording from the tray icon or this window.").to_string(),
            k => trf!("倒數完就隱藏。要停止錄影時按 {k}，或從系統匣停止。", "Hidden once the countdown ends. Press {k} or use the tray icon to stop recording."),
        },
        _ => tr!(
            "錄影時螢幕上的小控制列，不會被錄進影片。按控制列右邊的「‹」縮小、「›」展開。錄整個螢幕時可以把控制列拖到別的地方（例如另一個螢幕），會記住位置；雙擊控制列放回上方中間。",
            "The small bar shown on screen while recording. It isn't captured in the video. Click ‹ / › at its right end to shrink or expand it. When recording a whole screen you can drag the bar elsewhere (e.g. to another monitor) and it stays there; double-click it to put it back at the top center."
        )
        .to_string(),
    };
    form_hint(ui, hint, p.muted);
}

// ───────────── 截圖 ─────────────

fn shots_page(app: &mut UiApp, ui: &mut Ui) {
    form_section(ui, tr!("截圖", "Screenshots"), |_| {});
    form_row(ui, tr!("截圖後", "After capture"), |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add_space(6.0);
            let mut pv = app.s.shot_preview;
            if switch(ui, &mut pv, tr!("右下角顯示小縮圖", "Show thumbnail in corner"), true)
                .on_hover_text(tr!(
                    "截圖後在螢幕右下角出現小縮圖，可以直接編輯、複製、釘選或刪除；7 秒後自動消失",
                    "After a screenshot, a thumbnail appears in the bottom-right corner of the screen so you can edit, copy, pin or delete it. It disappears after 7 seconds"
                ))
                .changed()
            {
                app.s.shot_preview = pv;
                app.save_settings();
            }
            let mut on = app.s.edit_after_shot;
            if switch(ui, &mut on, tr!("直接開啟編輯", "Open in editor"), true).changed() {
                app.s.edit_after_shot = on;
                app.save_settings();
            }
        });
    });
    form_row(ui, tr!("框選截圖", "Area screenshot"), |ui| {
        let mut on = app.s.snip_instant;
        if switch(ui, &mut on, tr!("放開滑鼠就直接存檔", "Save as soon as you release the mouse"), true)
            .on_hover_text(tr!(
                "關閉時（預設）：放開滑鼠後可以再調整範圍，並選擇完成、編輯、複製或釘在桌面",
                "Off (default): after you release the mouse you can still adjust the area and choose Done, Edit, Copy or Pin"
            ))
            .changed()
        {
            app.s.snip_instant = on;
            app.save_settings();
        }
    });
    form_hint(ui, tr!("也可以從系統匣「截圖 → 編輯上次截圖」開啟編輯。", "You can also open the editor from the system tray: “Screenshot → Edit last screenshot”."), theme::pal(ui).muted);
}

// ───────────── 聲音 ─────────────

fn audio_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    let a = app.env.audio.clone();
    form_section(ui, tr!("錄製聲音", "Record audio"), |_| {});
    form_row(ui, tr!("系統聲音", "System audio"), |ui| {
        let mut sys = app.s.audio_system;
        if switch(ui, &mut sys, "", true).changed() {
            app.s.audio_system = sys;
            app.save_settings();
        }
    });
    // 裝置名稱可能很長：一行顯示，放不下時結尾「…」（滑鼠移上去看完整名稱）
    ui.horizontal(|ui| {
        ui.add_space(FORM_LABEL_W + 6.0);
        let text = a.render.clone().map(|r| trf!("播放裝置：{r}", "Playback device: {r}")).unwrap_or_else(|| tr!("找不到播放裝置", "No playback device found").into());
        ui.add(egui::Label::new(RichText::new(text).font(theme::font(12.0)).color(p.muted)).truncate());
    });
    app_audio_row(app, d, ui);
    form_row(ui, tr!("麥克風", "Microphone"), |ui| {
        let mut mic = app.s.audio_mic;
        if switch(ui, &mut mic, "", true).changed() {
            app.s.audio_mic = mic;
            app.save_settings();
        }
    });
    if !app.s.mic_id.is_empty() && !a.captures.iter().any(|d| d.id == app.s.mic_id) {
        app.s.mic_id.clear();
    }
    form_row(ui, tr!("麥克風裝置", "Mic device"), |ui| {
        let def = a.captures.iter().find(|d| d.is_default).map(|d| trf!("預設（{}）", "Default ({})", d.name)).unwrap_or_else(|| tr!("預設麥克風", "Default microphone").into());
        let mut items: Vec<(usize, String, bool)> = vec![(0, def, true)];
        items.extend(a.captures.iter().enumerate().map(|(i, d)| (i + 1, d.name.clone(), true)));
        let mut sel = a.captures.iter().position(|d| d.id == app.s.mic_id).map(|i| i + 1).unwrap_or(0);
        ui.add_enabled_ui(app.s.audio_mic, |ui| {
            // 裝置名稱通常很長：用到整列寬度
            if form_combo(ui, "mic", ui.available_width(), &mut sel, &items) {
                app.s.mic_id = if sel == 0 { String::new() } else { a.captures[sel - 1].id.clone() };
                app.save_settings();
            }
        });
    });
    form_row(ui, tr!("分軌", "Separate tracks"), |ui| {
        let both = app.s.audio_system && app.s.audio_mic;
        let mut on = app.s.separate_audio;
        if switch(ui, &mut on, tr!("系統聲音與麥克風分開存", "Save system audio and mic separately"), both)
            .on_hover_text(tr!(
                "影片多兩條音軌（系統聲音、麥克風）：剪輯時可以分別調整音量、只對麥克風降噪，自動字幕也只聽麥克風。第一條仍是混在一起的，一般播放器照常播放",
                "Adds two audio tracks (system audio and mic) to the video, so you can set their volumes separately and apply noise reduction to the mic only when editing; automatic subtitles listen to the mic only. The first track is still the mix, so players work as usual."
            ))
            .changed()
        {
            app.s.separate_audio = on;
            app.save_settings();
        }
    });
    form_row(ui, tr!("降噪", "Noise reduction"), |ui| {
        let mut on = app.s.mic_denoise;
        if switch(ui, &mut on, tr!("錄影時降低麥克風雜音", "Reduce mic noise while recording"), app.s.audio_mic)
            .on_hover_text(tr!(
                "錄影的同時去掉麥克風收到的風扇、冷氣、鍵盤等持續的雜音，不用事後再處理；只處理麥克風，系統聲音（影片、音樂）不受影響",
                "Removes steady background noise from the mic (fans, air conditioning, keyboards) while recording, so there's nothing to fix afterwards. Only the mic is processed; system audio (videos, music) isn't affected"
            ))
            .changed()
        {
            app.s.mic_denoise = on;
            app.save_settings();
        }
    });
    let hint = a.error.clone().unwrap_or_else(|| {
        if a.captures.is_empty() {
            tr!("找不到麥克風。", "No microphone found.").into()
        } else {
            tr!("聲音與畫面以同一個時鐘對齊。系統聲音會受 Windows 音量影響。", "Audio and video are synced to the same clock. System audio is affected by the Windows volume.").into()
        }
    });
    ui.add_space(6.0);
    form_hint_full(ui, hint, if a.error.is_some() { p.warn } else { p.muted });
}

/// 系統聲音來源：全部，或只錄某個程式（例如只錄瀏覽器的聲音，通知音、音樂不會錄進去）
fn app_audio_row(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let apps = d.apps.get_or_insert_with(screenrecorder_core::winui::audio_apps).clone();
    form_row(ui, tr!("系統聲音來源", "System audio from"), |ui| {
        // 選過、但現在沒有開啟的程式也列出來（錄影時才開也可以）
        let mut exes: Vec<String> = vec![String::new()];
        exes.extend(apps.iter().map(|a| a.exe.clone()));
        if !app.s.audio_app.is_empty() && !exes.contains(&app.s.audio_app) {
            exes.push(app.s.audio_app.clone());
        }
        let mut sel = exes.iter().position(|e| *e == app.s.audio_app).unwrap_or(0);
        let items: Vec<(usize, String, bool)> = exes
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let text = if e.is_empty() {
                    tr!("全部系統聲音", "All system audio").to_string()
                } else if let Some(a) = apps.iter().find(|a| a.exe == *e) {
                    format!("{e} — {}", a.title)
                } else {
                    trf!("{e}（沒有開啟）", "{e} (not running)")
                };
                (i, text, true)
            })
            .collect();
        ui.add_enabled_ui(app.s.audio_system, |ui| {
            if form_combo(ui, "audio_app", FORM_CTRL_W + 60.0, &mut sel, &items) {
                app.s.audio_app = exes[sel].clone();
                app.save_settings();
            }
        });
        if Btn::icon_only(Icon::Refresh).ghost().small().tooltip(tr!("重新列出開啟中的程式", "List running apps again")).show(ui).clicked() {
            d.apps = None;
        }
    });
    if !app.s.audio_app.is_empty() && app.s.audio_system {
        form_hint(
            ui,
            tr!(
                "只錄這個程式發出的聲音（含它的子程序），其他程式的通知音、音樂不會錄進去。Windows 版本太舊或程式沒有開啟時改錄全部系統聲音。",
                "Records only the sound from this app (and its child processes); notifications and music from other apps are left out. On older Windows versions, or if the app isn't running, all system audio is recorded instead."
            ),
            theme::pal(ui).muted,
        );
    }
}

// ───────────── 存檔與分享 ─────────────

fn save_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    // 錄影：標題列右邊「在檔案總管開啟」，下面資料夾，再下面一行說明（和「改回預設位置」）
    let dir = app.s.out_dir(&app.env);
    let custom = !app.s.output_dir.trim().is_empty() && !app.s.output_dir.trim().eq_ignore_ascii_case(&app.env.default_output_dir);
    let mut open: Option<String> = None;
    form_section(ui, tr!("錄影", "Recordings"), |ui| {
        if Btn::new(tr!("在檔案總管開啟", "Open in File Explorer")).icon(Icon::Folder).small().ghost().show(ui).clicked() {
            open = Some(dir.clone());
        }
    });
    if folder_row(ui, &dir, "save-video") {
        choose_output_dir(app);
    }
    let missing = !std::path::Path::new(&dir).is_dir();
    let mut reset = false;
    folder_status(
        ui,
        if missing {
            Some((tr!("找不到這個資料夾，錄影時會自動建立", "This folder doesn't exist yet. It will be created when you record"), p.warn))
        } else if !custom {
            Some((tr!("預設位置（「影片」資料夾裡的 Timelapse）", "Default location (Timelapse in your Videos folder)"), p.muted))
        } else {
            None
        },
        custom.then(|| trf!("改存到 {}", "Save to {} instead", app.env.default_output_dir)),
        &mut reset,
    );
    if reset {
        set_output_dir(app, String::new());
        let dir = app.s.out_dir(&app.env);
        app.toast(trf!("之後的錄影會存到 {dir}", "New recordings will be saved to {dir}"), false);
    }
    save_gap(ui);
    // 截圖（和錄影分開放）
    let shot_dir = app.s.shots_dir(&app.env);
    let shot_custom = !app.s.shot_dir.trim().is_empty();
    form_section(ui, tr!("截圖", "Screenshots"), |ui| {
        if Btn::new(tr!("在檔案總管開啟", "Open in File Explorer")).icon(Icon::Folder).small().ghost().show(ui).clicked() {
            open = Some(shot_dir.clone());
        }
    });
    if folder_row(ui, &shot_dir, "save-shot") {
        choose_shot_dir(app);
    }
    let missing = !std::path::Path::new(&shot_dir).is_dir();
    let mut reset = false;
    folder_status(
        ui,
        if missing {
            Some((tr!("找不到這個資料夾，截圖時會自動建立", "This folder doesn't exist yet. It will be created when you take a screenshot"), p.warn))
        } else if !shot_custom {
            Some((tr!("預設位置（Windows 的「螢幕擷取畫面」資料夾）", "Default location (the Windows Screenshots folder)"), p.muted))
        } else {
            None
        },
        shot_custom.then(|| trf!("改存到 {}", "Save to {} instead", app.env.default_shot_dir)),
        &mut reset,
    );
    if reset {
        set_shot_dir(app, String::new());
    }
    let extra = app.s.shot_extra(&app.env);
    if !extra.is_empty() {
        ui.add_space(6.0);
        form_hint_full(ui, trf!("「全部截圖」也會列出留在 {} 的舊截圖", "“All screenshots” also lists the older screenshots left in {}", extra.join("、")), p.muted);
        ui.add_space(2.0);
        if Btn::new(tr!("把舊截圖搬到截圖資料夾…", "Move older screenshots to the screenshot folder…")).small().show(ui).clicked() {
            if let Some(from) = extra.first() {
                offer_shot_move(app, from.clone(), false);
            }
        }
    }
    save_gap(ui);
    library_section(app, d, ui, &mut open);
    save_gap(ui);
    // 清單：子資料夾
    form_section(ui, tr!("清單", "Lists"), |_| {});
    let mut subdirs = app.s.list_subdirs;
    if theme::check(ui, &mut subdirs, tr!("也列出子資料夾裡的錄影和截圖", "Also list recordings and screenshots in subfolders")).changed() {
        app.s.list_subdirs = subdirs;
        // core 讀設定決定要不要列子資料夾：先存好再重新讀清單
        app.save_settings_now();
        app.load_recent();
        if let Some(d) = app.library.as_mut() {
            d.reload();
        }
    }
    indented_hint(
        ui,
        trf!(
            "例如「教學」「會議」裡的檔案也會列出，縮圖上標出所在的資料夾。最多往下 {} 層。",
            "Files in folders such as “Tutorials” or “Meetings” are listed too, labelled with their folder. Up to {} levels deep.",
            screenrecorder_core::library::SUBDIR_DEPTH
        ),
    );
    save_gap(ui);
    form_section(ui, tr!("檔名", "File names"), |_| {});
    name_rule_row(app, ui, false);
    name_rule_row(app, ui, true);
    indented_hint(
        ui,
        tr!(
            "可以用 {日期} {時間} {序號} {程式} {視窗} {內容}。{序號} 依同一個資料夾的檔案往下編；{內容} 依說的話和畫面上的字取名（只有錄影）。空白就用預設名稱；只錄聲音的錄音不套用",
            "Use {date} {time} {n} {app} {title} {content}. {n} counts up from the files in the same folder; {content} names it from what was said and shown (recordings only). Leave empty for the default names; audio-only recordings keep theirs."
        ),
    );
    ui.add_space(4.0);
    form_hint_full(
        ui,
        tr!(
            "加速版、GIF、剪輯版存在原片旁邊；擷取的畫面、拼圖、步驟截圖存在截圖資料夾。",
            "Sped-up versions, GIFs and edits are saved next to the original; grabbed frames, combined images and step captures go to the screenshot folder."
        ),
        p.muted,
    );
    if let Some(d) = open {
        open_folder(app, &d);
    }
}

/// 媒體庫：NAS、Jellyfin 的媒體庫或雲端硬碟的同步資料夾。「傳到媒體庫」、錄完自動處理、剪輯另存好都傳到這裡；
/// 填了 Jellyfin 的網址與 API 金鑰時，傳好就通知它掃描，影片資訊、改名、刪除也會同步過去
fn library_section(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui, open: &mut Option<String>) {
    let p = theme::pal(ui);
    let dir = app.s.after_copy_to.clone();
    // 標題直接寫出 Jellyfin，找 Jellyfin 設定的人看得到
    form_section(ui, tr!("媒體庫（NAS、Jellyfin）", "Media library (NAS, Jellyfin)"), |ui| {
        if !dir.is_empty() && Btn::new(tr!("在檔案總管開啟", "Open in File Explorer")).icon(Icon::Folder).small().ghost().show(ui).clicked() {
            *open = Some(dir.clone());
        }
    });
    if folder_row(ui, &dir, "save-library") {
        pick_copy_dir(app, false);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let text = if dir.is_empty() {
            tr!("選了之後，錄影卡片的「傳到媒體庫」一按就複製過去（連同字幕）", "Once chosen, “Send to library” on a recording copies it there (with subtitles)")
        } else {
            tr!(
                "「傳到媒體庫」時選要放進這裡的哪個子資料夾（同一個資料夾裡同名的覆蓋，連同字幕）",
                "“Send to library” lets you pick a subfolder here (replacing one with the same name in that folder, with subtitles)"
            )
        };
        ui.add(egui::Label::new(RichText::new(text).font(theme::font(12.0)).color(p.muted)).wrap());
        if !dir.is_empty() && Btn::new(tr!("不用媒體庫", "Stop using")).small().ghost().show(ui).clicked() {
            app.s.after_copy_to.clear();
            app.save_settings();
        }
    });
    if dir.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new(tr!(
                    "例如 \\\\伺服器\\videos（Jellyfin 的媒體庫）、NAS，或 OneDrive、Google 雲端硬碟的同步資料夾（由同步軟體上傳，不用另外登入）。",
                    "For example \\\\server\\videos (a Jellyfin library), a NAS, or a OneDrive / Google Drive sync folder (the sync app uploads them; no extra sign-in)."
                ))
                .font(theme::font(12.0))
                .color(p.muted),
            )
            .wrap(),
        );
    }
    // Jellyfin 的網址與金鑰一直顯示（還沒選資料夾也可以先填），兩個勾選要有資料夾才有意義
    if !dir.is_empty() {
        library_checks(app, ui);
    }
    ui.add_space(6.0);
    jellyfin_rows(app, d, ui, dir.is_empty());
}

/// 媒體庫：錄完、另存好之後也傳過去，放在哪個子資料夾
fn library_checks(app: &mut UiApp, ui: &mut Ui) {
    ui.add_space(4.0);
    auto_sub_row(app, ui);
    if theme::check(ui, &mut app.s.after_copy, tr!("錄完自動處理好之後也傳過去", "Also send new recordings after processing"))
        .on_hover_text(tr!("和「錄完自動處理 → 媒體庫」是同一個設定", "Same setting as After recording → Library"))
        .changed()
    {
        app.save_settings();
    }
    if theme::check(ui, &mut app.s.copy_exports, tr!("剪輯、加速版另存好也傳過去", "Also send edited and sped-up copies"))
        .on_hover_text(tr!(
            "剪輯、加速版、壓縮、合併另存好之後，也複製到這個資料夾（同名的覆蓋），有設定 Jellyfin 時通知它",
            "After an edited, sped-up, compressed or merged copy is saved, copy it to this folder too (replacing one with the same name) and notify Jellyfin if set up"
        ))
        .changed()
    {
        app.save_settings();
    }
}

/// 錄完、剪輯好自動傳的時候放進媒體庫的哪個子資料夾（手動傳的時候每次選）
fn auto_sub_row(app: &mut UiApp, ui: &mut Ui) {
    let folder = app.library_folder().unwrap_or_default();
    let mut subs = app.library_subs().unwrap_or_default();
    let cur = app.s.after_copy_sub.clone();
    // 選的資料夾還沒建立（傳的時候會建立）也列出來
    if !cur.is_empty() && !subs.iter().any(|s| s.eq_ignore_ascii_case(&cur)) {
        subs.push(cur.clone());
    }
    let top = trf!("{folder}（最上層）", "{folder} (top level)");
    form_row(ui, tr!("自動傳到", "Auto-send to"), |ui| {
        let shown = if cur.is_empty() { top.clone() } else { cur.clone() };
        let mut pick: Option<String> = None;
        egui::ComboBox::from_id_salt("auto-sub")
            .width(FORM_CTRL_W)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui.selectable_label(cur.is_empty(), &top).clicked() {
                    pick = Some(String::new());
                }
                for s in &subs {
                    if ui.selectable_label(cur.eq_ignore_ascii_case(s), s).clicked() {
                        pick = Some(s.clone());
                    }
                }
            })
            .response
            .on_hover_text(tr!(
                "錄完自動處理、剪輯和加速版另存好自動傳的時候，放進媒體庫的這個資料夾。手動按「傳到媒體庫」時每次都可以選",
                "Where automatic sends go (after recording, and after an edited or sped-up copy is saved). When you click “Send to library” you choose each time"
            ));
        if let Some(sub) = pick.filter(|s| *s != cur) {
            app.s.after_copy_sub = sub;
            app.save_settings();
        }
    });
}

/// Jellyfin：傳到它的媒體庫資料夾後，通知它馬上掃描；影片資訊、改名、刪除也同步過去
fn jellyfin_rows(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui, no_folder: bool) {
    let p = theme::pal(ui);
    form_row(ui, "Jellyfin", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.s.jellyfin_url).desired_width(FORM_CTRL_W).hint_text(tr!("網址，例如 192.168.1.10:8096", "Address, e.g. 192.168.1.10:8096")));
        if r.changed() {
            d.jf_result = None;
            app.save_settings();
        }
    });
    form_row(ui, tr!("API 金鑰", "API key"), |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.s.jellyfin_key).password(true).desired_width(FORM_CTRL_W).hint_text(tr!("Jellyfin 控制台 → API 金鑰", "Jellyfin Dashboard → API Keys")));
        if r.changed() {
            d.jf_result = None;
            app.save_settings();
        }
    });
    let jf = screenrecorder_core::jellyfin::Jellyfin::new(&app.s.jellyfin_url, &app.s.jellyfin_key);
    form_row(ui, "", |ui| {
        let label = if d.jf_testing { tr!("測試中…", "Testing…") } else { tr!("測試連線", "Test connection") };
        if Btn::new(label).small().enabled(jf.is_some() && !d.jf_testing).show(ui).clicked() {
            if let Some(jf) = jf.clone() {
                d.jf_testing = true;
                d.jf_result = None;
                app.spawn(async move { tokio::task::spawn_blocking(move || jf.test()).await.unwrap_or_else(|e| Err(e.to_string())) }, |app, r| {
                    if let Some(d) = &mut app.settings_dlg {
                        d.jf_testing = false;
                        d.jf_result = Some(r);
                    }
                });
            }
        }
        match &d.jf_result {
            Some(Ok(name)) => {
                ui.label(RichText::new(trf!("已連上 {}", "Connected to {}", name)).font(theme::font(12.5)).color(p.ok));
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(e).font(theme::font(12.5)).color(p.rec)).wrap());
            }
            None => {}
        }
    });
    form_hint(
        ui,
        if no_folder {
            tr!(
                "要先在上面選 Jellyfin 媒體庫的資料夾（例如 \\\\伺服器\\videos），傳過去之後才會通知 Jellyfin 掃描。",
                "First choose the Jellyfin library folder above (e.g. \\\\server\\videos); Jellyfin is told to scan after files are sent there."
            )
        } else if jf.is_some() {
            tr!(
                "傳好之後會通知 Jellyfin 掃描，新影片馬上出現；影片資訊、改名、刪除也會同步過去。",
                "Jellyfin is told to scan right after sending, so new videos show up at once; video details, renames and deletions are synced too."
            )
        } else {
            tr!(
                "這個資料夾是 Jellyfin 的媒體庫時，填這兩個就會在傳好之後通知它掃描（不填就等 Jellyfin 定時掃描）。",
                "If this folder is a Jellyfin library, fill these in to have it scan right after sending (otherwise Jellyfin finds them on its next scheduled scan)."
            )
        },
        p.muted,
    );
}

/// 影片格式：H.264（預設）、HEVC、AV1；HEVC / AV1 要這台電腦的顯示卡支援
fn codec_row(app: &mut UiApp, ui: &mut Ui) {
    use screenrecorder_core::types::VideoCodec;
    let p = theme::pal(ui);
    let list = app.env.ffmpeg.codec_encoders.clone();
    let has = |c: VideoCodec| list.as_ref().is_some_and(|l| l.iter().any(|n| screenrecorder_core::args::codec_of(n) == c));
    let label = |c: VideoCodec, text: &str| match &list {
        None => trf!("{text}（偵測中…）", "{text} (detecting…)"),
        Some(_) if !has(c) => trf!("{text}（這台電腦的顯示卡不支援）", "{text} (not supported by this PC's GPU)"),
        _ => text.to_string(),
    };
    form_row(ui, tr!("影片格式", "Video format"), |ui| {
        let mut c = app.s.codec;
        let items = [
            (VideoCodec::H264, tr!("H.264（最通用，建議）", "H.264 (works everywhere, recommended)").to_string(), true),
            (VideoCodec::Hevc, label(VideoCodec::Hevc, tr!("HEVC（檔案小約 4 成）", "HEVC (about 40% smaller)")), has(VideoCodec::Hevc) || app.s.codec == VideoCodec::Hevc),
            (VideoCodec::Av1, label(VideoCodec::Av1, tr!("AV1（檔案最小，新顯示卡）", "AV1 (smallest, newer GPUs)")), has(VideoCodec::Av1) || app.s.codec == VideoCodec::Av1),
        ];
        if form_combo(ui, "codec", FORM_CTRL_W, &mut c, &items) {
            app.s.codec = c;
            app.save_settings();
        }
    });
    if app.s.codec != VideoCodec::H264 {
        let warn = list.is_some() && !has(app.s.codec);
        let text = if warn {
            trf!("這台電腦的顯示卡不支援 {}，錄影時會用 H.264。", "This PC's GPU doesn't support {}, so recordings use H.264.", app.s.codec.label())
        } else {
            tr!(
                "用顯示卡壓縮；不能用時自動改用 H.264。剪輯、加速版、壓縮做出來的仍是 H.264。舊電腦、部分手機和 LINE 可能播不了，要分享時建議做成壓縮版。",
                "Encoded on the GPU; falls back to H.264 automatically. Edits, sped-up and compressed versions are still H.264. Older PCs, some phones and LINE may not play it, so share a compressed version."
            )
            .to_string()
        };
        form_hint(ui, text, if warn { p.warn } else { p.muted });
    }
}

/// 命名規則的一列：常用的範本、自己打，旁邊顯示套用後的樣子
fn name_rule_row(app: &mut UiApp, ui: &mut Ui, shot: bool) {
    let p = theme::pal(ui);
    let (label, ext, default) = if shot { (tr!("截圖", "Screenshots"), "png", "Shot_") } else { (tr!("錄影", "Recordings"), "mp4", "Rec_") };
    form_row(ui, label, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                let rule = if shot { &mut app.s.shot_name_rule } else { &mut app.s.name_rule };
                let hint = trf!("{default}年-月-日_時-分-秒（預設）", "{default}YYYY-MM-DD_HH-MM-SS (default)");
                let r = ui.add(egui::TextEdit::singleline(rule).desired_width(FORM_CTRL_W - 34.0).char_limit(120).hint_text(hint).font(theme::mono(12.5)));
                let mut changed = r.changed();
                let b = Btn::icon_only(Icon::More).ghost().small().tooltip(tr!("常用的範本", "Common patterns")).show(ui);
                egui::Popup::menu(&b).show(|ui| {
                    ui.set_min_width(240.0);
                    let presets: [(&str, &str); 5] = if screenrecorder_core::i18n::is_en() {
                        [
                            ("", "Default"),
                            ("{date}_{n}", "Date + number"),
                            ("Project_{date}_{n}", "Project name + date + number"),
                            ("{app}_{date}_{time}", "App + date + time"),
                            ("{content}_{date}", "Content + date (recordings)"),
                        ]
                    } else {
                        [
                            ("", "預設"),
                            ("{日期}_{序號}", "日期＋序號"),
                            ("專案名稱_{日期}_{序號}", "專案名稱＋日期＋序號"),
                            ("{程式}_{日期}_{時間}", "程式＋日期＋時間"),
                            ("{內容}_{日期}", "內容＋日期（錄影）"),
                        ]
                    };
                    for (v, name) in presets {
                        if shot && (v.contains("{內容}") || v.contains("{content}")) {
                            continue;
                        }
                        if ui.button(name).clicked() {
                            *rule = v.to_string();
                            changed = true;
                        }
                    }
                });
                if changed {
                    app.save_settings();
                }
            });
            let rule = if shot { app.s.shot_name_rule.clone() } else { app.s.name_rule.clone() };
            if !rule.trim().is_empty() {
                let (text, color) = match screenrecorder_core::naming::rule_preview(&rule, ext) {
                    Some(n) => (trf!("例如：{n}", "e.g. {n}"), p.muted),
                    None => (
                        tr!("這樣取不出檔名（可能只有不能用的字），會用預設的名稱", "This doesn't make a usable name (maybe only invalid characters); the default name will be used").to_string(),
                        p.warn,
                    ),
                };
                ui.label(RichText::new(text).font(theme::font(12.0)).color(color));
            }
        });
    });
}

/// 資料夾的最後一段（太長時只顯示名稱）
fn short_dir(dir: &str) -> String {
    std::path::Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.to_string())
}

/// 選媒體庫資料夾（不能是儲存位置本身或它裡面，否則複製的檔案會出現在清單裡）；
/// enable = 同時打開「錄完處理好之後傳過去」（從錄完自動處理打開開關時）
fn pick_copy_dir(app: &mut UiApp, enable: bool) {
    app.spawn(
        async move { tokio::task::spawn_blocking(|| screenrecorder_core::filepick::pick_folder(tr!("選擇媒體庫資料夾", "Choose the library folder"), None)).await.unwrap_or(Ok(None)) },
        move |app, r| match r {
            Ok(Some(dir)) => {
                let out = std::path::PathBuf::from(app.s.out_dir(&app.env));
                if screenrecorder_core::paths::inside(&dir, &out) {
                    return app.toast(tr!("不能選儲存位置本身或它裡面的資料夾", "You can't choose the save folder or a folder inside it"), true);
                }
                app.s.after_copy_to = dir.display().to_string();
                if enable {
                    app.s.after_copy = true;
                }
                app.save_settings();
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

/// 儲存位置分頁：段落之間的間距與分隔線（比其他分頁寬鬆一點）
fn save_gap(ui: &mut Ui) {
    ui.add_space(6.0);
    form_divider(ui);
    ui.add_space(6.0);
}

/// 資料夾下面一行：狀態說明（預設位置、找不到資料夾）與「改回預設位置」；兩個都沒有時不佔位置
fn folder_status(ui: &mut Ui, status: Option<(&str, Color32)>, reset_tip: Option<String>, reset: &mut bool) {
    if status.is_none() && reset_tip.is_none() {
        return;
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if let Some((text, color)) = status {
            ui.label(RichText::new(text).font(theme::font(12.0)).color(color));
        }
        if let Some(tip) = reset_tip {
            if Btn::new(tr!("改回預設位置", "Use default location")).small().ghost().tooltip(tip).show(ui).clicked() {
                *reset = true;
            }
        }
    });
}

/// 勾選框下面的說明：對齊勾選框的文字
fn indented_hint(ui: &mut Ui, text: impl Into<String>) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.add(egui::Label::new(RichText::new(text.into()).font(theme::font(12.0)).color(p.muted)).wrap());
    });
}

/// 目前的資料夾（不用打字）：點一下或按「選擇資料夾」用 Windows 的視窗選；回傳 true = 要選資料夾
fn folder_row(ui: &mut Ui, dir: &str, id: &str) -> bool {
    let p = theme::pal(ui);
    let mut choose = false;
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let btn = Btn::new(tr!("選擇資料夾…", "Choose folder…")).icon(Icon::Folder);
            let w = ui.available_width() - btn.width(ui) - ui.spacing().item_spacing.x;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
            let hover = resp.hovered();
            ui.painter().rect(rect, CornerRadius::same(6), p.surface, Stroke::new(1.0, if hover { p.accent } else { p.border_strong }), egui::StrokeKind::Inside);
            theme::paint_icon(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 18.0, rect.center().y), vec2(16.0, 16.0)), Icon::Folder, p.muted);
            // 太長時保留結尾（資料夾名稱比較重要）
            let room = rect.width() - 40.0;
            let fits = |t: &str| ui.painter().layout_no_wrap(t.to_string(), theme::font(13.0), p.text).size().x <= room;
            let (shown, color) = if dir.is_empty() {
                (tr!("還沒選資料夾", "No folder chosen").to_string(), p.muted)
            } else if fits(dir) {
                (dir.to_string(), p.text)
            } else {
                let chars: Vec<char> = dir.chars().collect();
                ((1..chars.len()).map(|i| format!("…{}", chars[i..].iter().collect::<String>())).find(|t| fits(t)).unwrap_or_default(), p.text)
            };
            ui.painter().text(pos2(rect.min.x + 32.0, rect.center().y), egui::Align2::LEFT_CENTER, shown, theme::font(13.0), color);
            let tip = if dir.is_empty() {
                tr!("點一下選擇資料夾", "Click to choose a folder").to_string()
            } else {
                trf!("{dir}\n點一下選擇其他資料夾", "{dir}\nClick to choose another folder")
            };
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                choose = true;
            }
            if btn.show(ui).clicked() {
                choose = true;
            }
        });
    });
    choose
}

fn open_folder(app: &mut UiApp, dir: &str) {
    let _ = std::fs::create_dir_all(dir);
    if let Err(e) = actions::open(actions::OpenAction::Folder, dir) {
        app.toast(e.message().to_string(), true);
    }
}

/// 用 Windows 的視窗選儲存位置（主畫面下方的資料夾按鈕也用）
pub fn choose_output_dir(app: &mut UiApp) {
    let current = app.s.out_dir(&app.env);
    app.spawn(async move { tokio::task::spawn_blocking(move || actions::pick_output_dir(&current)).await.unwrap_or(Ok(None)) }, |app, r| match r {
        Ok(Some(dir)) => {
            // 選的就是預設位置：存成空白（之後預設位置變了也跟著）
            let value = if dir.eq_ignore_ascii_case(&app.env.default_output_dir) { String::new() } else { dir };
            set_output_dir(app, value);
            let dir = app.s.out_dir(&app.env);
            app.toast(trf!("之後的錄影會存到 {dir}", "New recordings will be saved to {dir}"), false);
        }
        Ok(None) => {}
        Err(e) => app.toast(e.message().to_string(), true),
    });
}

fn set_output_dir(app: &mut UiApp, value: String) {
    if app.s.output_dir == value {
        return;
    }
    // 原本的資料夾記下來（全部錄影可以切回之前用過的資料夾）
    let old = app.s.out_dir(&app.env);
    super::library_dialog::remember_dir(app, &old);
    app.s.output_dir = value;
    app.save_settings();
    // 重新讀取最近錄影
    app.main.dir_changed_at = Some(Instant::now());
    app.ctx.request_repaint_after(std::time::Duration::from_millis(620));
}

/// 選截圖資料夾（設定、全部截圖的資料夾選單）
pub fn choose_shot_dir(app: &mut UiApp) {
    let current = app.s.shots_dir(&app.env);
    app.spawn(async move { tokio::task::spawn_blocking(move || actions::pick_output_dir(&current)).await.unwrap_or(Ok(None)) }, |app, r| match r {
        Ok(Some(dir)) => {
            let value = if dir.eq_ignore_ascii_case(&app.env.default_shot_dir) { String::new() } else { dir };
            set_shot_dir(app, value);
        }
        Ok(None) => {}
        Err(e) => app.toast(e.message().to_string(), true),
    });
}

/// 換截圖資料夾：舊資料夾裡有截圖時問要不要一起搬過去
fn set_shot_dir(app: &mut UiApp, value: String) {
    let old = app.s.shots_dir(&app.env);
    if app.s.shot_dir.trim() == value.trim() {
        return;
    }
    app.s.shot_dir = value;
    app.save_settings();
    let new = app.s.shots_dir(&app.env);
    app.toast(trf!("之後的截圖會存到 {new}", "New screenshots will be saved to {new}"), false);
    if !old.eq_ignore_ascii_case(&new) {
        offer_shot_move(app, old, false);
    }
    if let Some(d) = &mut app.library {
        d.reload();
    }
}

/// 問要不要把 from 資料夾裡的截圖搬到截圖資料夾。先把 from 記成「留在原處的資料夾」（全部截圖照樣列得出來），
/// 搬完、資料夾裡沒有截圖了再拿掉。first_run：截圖改成分開存之後第一次啟動（不想現在決定時下次再問）
pub fn offer_shot_move(app: &mut UiApp, from: String, first_run: bool) {
    let to = app.s.shots_dir(&app.env);
    if from.trim().is_empty() || from.eq_ignore_ascii_case(&to) {
        if first_run {
            app.s.shot_move_asked = true;
            app.save_settings();
        }
        return;
    }
    let f = from.clone();
    app.spawn(async move { actions::count_shots(&f).await }, move |app, n| {
        if n == 0 {
            app.s.shot_extra_dirs.retain(|d| !d.eq_ignore_ascii_case(&from));
            if first_run {
                app.s.shot_move_asked = true;
            }
            app.save_settings();
            return;
        }
        if !app.s.shot_extra_dirs.iter().any(|d| d.eq_ignore_ascii_case(&from)) {
            app.s.shot_extra_dirs.push(from.clone());
            app.save_settings();
        }
        let to = app.s.shots_dir(&app.env);
        let (from2, to2) = (from.clone(), to.clone());
        let mut ask = dialogs::Ask::confirm(
            tr!("要把截圖搬到截圖資料夾嗎？", "Move your screenshots to the screenshot folder?"),
            trf!(
                "截圖和錄影現在分開存放，新的截圖會存到：\n{to}\n\n舊的資料夾裡還有 {n} 張截圖：\n{from}\n\n要一起搬過去嗎？同名的檔案會自動改名，不會覆蓋；截圖編輯的記錄會跟著更新，之後一樣可以再次編輯。",
                "Screenshots and recordings are now kept apart. New screenshots are saved in:\n{to}\n\nThere are {n} screenshots in the old folder:\n{from}\n\nMove them there too? Files with the same name are renamed, never overwritten, and your screenshot edits stay editable.",
                to = to,
                from = from,
                n = n
            ),
            tr!("搬過去", "Move them"),
            move |app, _| {
                app.s.shot_move_asked = true;
                app.save_settings();
                move_shots(app, from2, to2);
            },
        );
        let keep = from.clone();
        ask.alt = Some((
            tr!("留在原處", "Leave them").into(),
            Box::new(move |app: &mut UiApp, _| {
                app.s.shot_move_asked = true;
                if !app.s.shot_extra_dirs.iter().any(|d| d.eq_ignore_ascii_case(&keep)) {
                    app.s.shot_extra_dirs.push(keep);
                }
                app.save_settings();
                app.toast(tr!("舊截圖留在原處，「全部截圖」也會列出它們", "The older screenshots stay where they are; “All screenshots” still lists them"), false);
            }),
        ));
        ask.cancel = Some(if first_run { tr!("之後再說", "Not now") } else { tr!("取消", "Cancel") }.into());
        app.ask = Some(ask);
    });
}

/// 搬截圖，完成後說明搬了幾張、哪些沒搬成
fn move_shots(app: &mut UiApp, from: String, to: String) {
    let core = app.core.clone();
    let (f, t) = (from.clone(), to.clone());
    app.toast(tr!("正在搬移截圖…", "Moving screenshots…"), false);
    app.spawn(
        async move {
            let r = actions::move_shots(&core, &f, &t).await;
            let left = actions::count_shots(&f).await;
            (r, left)
        },
        move |app, (r, left)| {
            if left == 0 {
                app.s.shot_extra_dirs.retain(|d| !d.eq_ignore_ascii_case(&from));
                app.save_settings();
            }
            if let Some(d) = &mut app.library {
                d.reload();
            }
            if r.failed.is_empty() {
                let dir = to.clone();
                app.toast_with(
                    trf!("已把 {} 張截圖搬到 {}", "Moved {} screenshots to {}", r.moved.len(), to),
                    false,
                    vec![super::ToastAction::new(tr!("開啟資料夾", "Open folder"), move |app: &mut UiApp| open_folder(app, &dir))],
                );
            } else {
                let names: Vec<String> = r.failed.iter().take(3).map(|(n, _)| n.clone()).collect();
                let more = if r.failed.len() > 3 { tr!("等", " and others") } else { "" };
                app.toast(
                    trf!(
                        "搬了 {} 張，{} 張沒搬成（{}{}），原檔還在原處，可能正被其他程式開著；稍後可在「設定 → 存檔與分享」再搬一次",
                        "Moved {}; {} couldn't be moved ({}{}). They're still in the old folder and may be open in another app. You can try again in Settings → Saving & sharing",
                        r.moved.len(),
                        r.failed.len(),
                        names.join("、"),
                        more
                    ),
                    true,
                );
            }
        },
    );
}

/// 錄完自動處理：剪掉沒動靜的片段、產生字幕、壓縮
fn after_record_section(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    use screenrecorder_core::subtitles::{language_name, MODELS};
    let p = theme::pal(ui);
    form_section(ui, tr!("錄完自動處理", "After recording"), |_| {});
    form_row(ui, tr!("剪輯", "Edit"), |ui| {
        let mut on = app.s.after_idle_cut;
        if switch(ui, &mut on, tr!("剪掉沒動靜的片段", "Cut idle parts"), true)
            .on_hover_text(tr!(
                "畫面不動而且沒有聲音超過 2 秒的地方（等待、發呆）剪掉，另存剪輯版；原片保留，剪輯版之後在剪輯視窗可以再改",
                "Removes parts where nothing moves and there's no sound for over 2 seconds (waiting, pauses) and saves an edited version. The original is kept, and you can adjust the edit later in the editor."
            ))
            .changed()
        {
            app.s.after_idle_cut = on;
            app.save_settings();
        }
    });
    form_row(ui, tr!("字幕", "Subtitles"), |ui| {
        let mut on = app.s.after_subs;
        if switch(ui, &mut on, tr!("產生字幕（存成 SRT）", "Generate subtitles (SRT)"), true)
            .on_hover_text(tr!(
                "辨識說話的內容，在影片旁邊存一份同名的 SRT 字幕檔（YouTube、播放器都能用）",
                "Recognizes speech and saves an SRT subtitle file with the same name next to the video (works with YouTube and players)"
            ))
            .changed()
        {
            app.s.after_subs = on;
            app.save_settings();
        }
    });
    if app.s.after_subs {
        form_row(ui, tr!("語言", "Language"), |ui| {
            let mut lang = app.s.after_subs_lang as usize;
            let items: Vec<(usize, String, bool)> = (0..screenrecorder_core::subtitles::LANGUAGES.len()).map(|i| (i, language_name(i).to_string(), true)).collect();
            if form_combo(ui, "after-subs-lang", FORM_CTRL_W, &mut lang, &items) {
                app.s.after_subs_lang = lang as u32;
                app.save_settings();
            }
        });
        form_row(ui, tr!("辨識模型", "Model"), |ui| {
            let mut m = app.s.after_subs_model as usize;
            let items: Vec<(usize, String, bool)> = MODELS.iter().enumerate().map(|(i, m)| (i, m.label().to_string(), true)).collect();
            if form_combo(ui, "after-subs-model", FORM_CTRL_W, &mut m, &items) {
                app.s.after_subs_model = m as u32;
                app.save_settings();
            }
        });
        form_row(ui, tr!("英文字幕", "English"), |ui| {
            let mut on = app.s.after_subs_en;
            if switch(ui, &mut on, tr!("也翻成英文（存成 .en.srt）", "Also translate to English (.en.srt)"), true)
                .on_hover_text(tr!(
                    "再辨識一次並翻成英文，另存「同名.en.srt」，可以給外國客戶或上傳 YouTube；時間大約多一倍。剪輯視窗也可以做中英對照的字幕",
                    "Recognizes once more and translates to English, saved as “name.en.srt” for international viewers or YouTube; takes about twice as long. The editor can also make bilingual subtitles"
                ))
                .changed()
            {
                app.s.after_subs_en = on;
                app.save_settings();
            }
        });
        let mut need = screenrecorder_core::components::missing_for_subtitles(app.env.ffmpeg.has_whisper, app.s.after_subs_model as usize);
        if app.s.after_subs_en {
            need.extend(screenrecorder_core::components::missing_for_translation(app.s.after_subs_model as usize));
        }
        if !need.is_empty() {
            form_hint(
                ui,
                trf!(
                    "還沒下載{}，字幕這一步會跳過。請先到「外部元件」下載。",
                    "{} isn't downloaded yet, so this step will be skipped. Download it in Components first.",
                    screenrecorder_core::components::describe(&need)
                ),
                p.warn,
            );
            form_row(ui, "", |ui| {
                if Btn::new(tr!("前往外部元件", "Go to Components")).small().icon(Icon::Download).show(ui).clicked() {
                    d.page = Page::Components;
                }
            });
        }
    }
    form_row(ui, tr!("壓縮", "Compress"), |ui| {
        let mut mb = app.s.after_compress_mb;
        let items: Vec<(u32, String, bool)> = MAX_MB.iter().map(|v| (*v, if *v == 0 { tr!("不壓縮", "Don't compress").to_string() } else { trf!("{v} MB 以內", "Under {v} MB") }, true)).collect();
        if form_combo(ui, "after-compress", FORM_CTRL_W, &mut mb, &items) {
            app.s.after_compress_mb = mb;
            app.save_settings();
        }
    });
    form_row(ui, tr!("加速版", "Sped-up"), |ui| {
        let mut v = app.s.after_speed;
        let mut items: Vec<(u32, String, bool)> = vec![(0, tr!("不做", "Don't make").to_string(), true)];
        items.extend(screenrecorder_core::edit::FAST_SPEEDS.iter().map(|s| (*s, trf!("另存 {s}× 加速版", "Save a {s}× version"), true)));
        if form_combo(ui, "after-speed", FORM_CTRL_W, &mut v, &items) {
            app.s.after_speed = v;
            app.save_settings();
        }
    });
    form_row(ui, tr!("命名", "Name"), |ui| {
        let mut on = app.s.after_auto_name;
        if switch(ui, &mut on, tr!("依內容自動命名", "Name by content"), true)
            .on_hover_text(tr!(
                "依說的話（有字幕時）、畫面上的字、錄影時在用的程式取名，例如「報價單討論_10-15」。只改預設的 Rec_時間 名稱，自己取過的名稱不會動",
                "Names the recording from what was said (with subtitles), text on screen and the app you used, e.g. “Quote review_10-15”. Only default Rec_time names are changed; names you chose are kept."
            ))
            .changed()
        {
            app.s.after_auto_name = on;
            app.save_settings();
        }
    });
    // 媒體庫：資料夾和 Jellyfin 在「存檔與分享」，這裡只決定錄完要不要傳過去；還沒選資料夾時打開就選
    form_row(ui, tr!("媒體庫", "Library"), |ui| {
        let has = !app.s.after_copy_to.is_empty();
        let mut on = has && app.s.after_copy;
        let label = if has {
            trf!("處理完傳到「{}」", "Send to “{}” when done", short_dir(&screenrecorder_core::jellyfin::library_dest(&app.s.after_copy_to, &app.s.after_copy_sub)))
        } else {
            tr!("處理完傳到媒體庫", "Send to the library when done").to_string()
        };
        let r = switch(ui, &mut on, label, true).on_hover_text(tr!(
            "錄影、處理好的版本和字幕都複製一份到媒體庫資料夾，例如 NAS、Jellyfin 的媒體庫或 OneDrive、Google 雲端硬碟的同步資料夾（同名時加上 _2，不覆蓋）",
            "Copy the recording, processed versions and subtitles to the library folder, such as a NAS, a Jellyfin library or a OneDrive / Google Drive sync folder (a _2 is added instead of overwriting)"
        ));
        if r.changed() {
            if on && !has {
                pick_copy_dir(app, true);
            } else {
                app.s.after_copy = on;
                app.save_settings();
            }
        }
    });
    form_row(ui, "", |ui| {
        let text = if app.s.after_copy_to.is_empty() {
            tr!("媒體庫資料夾和 Jellyfin 在「存檔與分享」設定", "Set the library folder and Jellyfin in Saving & sharing")
        } else {
            tr!("換資料夾、設定 Jellyfin：「存檔與分享」", "Change the folder or set up Jellyfin in Saving & sharing")
        };
        if Btn::new(text).small().ghost().icon(Icon::Folder).show(ui).clicked() {
            d.page = Page::Save;
        }
    });
    form_hint(
        ui,
        tr!(
            "錄影存好後依序處理（有剪輯版時，字幕與壓縮用剪輯版），錄影面板顯示進度、可以取消。處理中開始錄影時會先暫停，錄完再繼續；手動的轉檔也優先。系統匣、快速鍵、排程的錄影也會處理。在「全部錄影」勾選好幾支、按「一次處理」時，也是用這裡的設定。",
            "Runs in order after each recording is saved (subtitles and compression use the edited version when there is one). The recording panel shows progress and lets you cancel. If you start recording meanwhile, it pauses and continues afterwards; manual conversions go first too. Recordings started from the tray, shortcuts or a schedule are processed too. “Process” for several recordings in All recordings uses these settings as well."
        ),
        p.muted,
    );
}

// ───────────── 一般 ─────────────

fn general_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, tr!("介面", "Interface"), |_| {});
    // 兩種語言都寫，選錯語言時也找得到
    form_row(ui, "語言 Language", |ui| {
        const CODES: [&str; 3] = ["auto", "zh-TW", "en"];
        let mut sel = CODES.iter().position(|c| *c == app.s.language).unwrap_or(0);
        let auto = tr!("自動（跟著 Windows）", "Auto (follow Windows)").to_string();
        let items = [(0usize, auto, true), (1, "繁體中文".into(), true), (2, "English".into(), true)];
        if form_combo(ui, "language", FORM_CTRL_W, &mut sel, &items) {
            app.s.language = CODES[sel].to_string();
            app.apply_language();
            app.save_settings();
        }
    });
    form_row(ui, tr!("外觀", "Appearance"), |ui| {
        let mut t = app.s.theme.clone();
        let items = [("system".to_string(), tr!("跟著 Windows", "Follow Windows")), ("light".into(), tr!("淺色", "Light")), ("dark".into(), tr!("深色", "Dark"))];
        let refs: Vec<(&str, &str)> = items.iter().map(|(v, l)| (v.as_str(), *l)).collect();
        let mut cur = refs.iter().find(|(v, _)| *v == t.as_str()).map(|(v, _)| *v).unwrap_or("system");
        if segmented(ui, &mut cur, &refs, true) {
            t = cur.to_string();
            app.s.theme = t;
            app.save_settings();
        }
    });
    // 螢幕太小時，放大後主畫面放不下（按鈕會被擋住）：那些大小不能選（目前用的除外）。
    // 用視窗所在的螢幕最大化時的大小（扣掉工作列、標題列）；不知道時用整個螢幕
    let base = screenrecorder_core::main_window::max_content_here()
        .map(|(w, h)| vec2(w, h))
        .or_else(|| ui.ctx().input(|i| i.viewport().monitor_size).filter(|m| m.x > 0.0 && m.y > 0.0).map(|m| m * ui.ctx().zoom_factor()));
    let fits = |v: u32| base.is_none_or(|b| b.x * 100.0 / v as f32 >= UI_MIN_SIZE.0 && b.y * 100.0 / v as f32 >= UI_MIN_SIZE.1);
    form_row(ui, tr!("介面大小", "Interface size"), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for v in UI_SCALES {
            let on = app.s.ui_scale == v;
            let ok = on || fits(v);
            let tip = if ok { "" } else { tr!("螢幕太小，放大後按鈕會被擋住", "The screen is too small; buttons would be cut off at this size") };
            if Btn::new(format!("{v}%")).small().selected(on).enabled(ok).min_width(58.0).tooltip(tip).show(ui).clicked() && !on {
                app.s.ui_scale = v;
                app.save_settings();
            }
        }
    });
    if !fits(app.s.ui_scale) {
        // 目前的大小就放不下：建議放得下的最大的
        let best = UI_SCALES.iter().rev().find(|v| fits(**v)).copied().unwrap_or(100);
        form_hint(ui, trf!("螢幕太小，放大後按鈕會被擋住，建議改成 {best}%。", "The screen is too small and buttons get cut off at this size. {best}% is recommended."), p.warn);
    } else if UI_SCALES.iter().any(|v| !fits(*v)) {
        form_hint(ui, tr!("螢幕太小，放大後按鈕會被擋住：灰色的大小不能選。", "The screen is too small: buttons would be cut off at the greyed-out sizes."), p.muted);
    } else {
        form_hint(ui, tr!("字和按鈕放大一點比較好看清楚。", "Larger text and buttons are easier to read."), p.muted);
    }
    form_divider(ui);
    form_section(ui, tr!("程式", "App"), |_| {});
    // 開機時自動啟動（與系統匣選單的同一個設定）：第一次看到時讀取
    if d.autostart.is_none() && !d.autostart_busy {
        d.autostart_busy = true;
        app.spawn(screenrecorder_core::desktop::get_autostart(), |app, v| {
            if let Some(d) = &mut app.settings_dlg {
                d.autostart = Some(v);
                d.autostart_busy = false;
            }
        });
    }
    form_row(ui, tr!("開機", "Startup"), |ui| {
        let state = d.autostart.flatten();
        let mut on = state == Some(true);
        let can = state.is_some() && !d.autostart_busy;
        if switch(ui, &mut on, tr!("開機時自動啟動（常駐在系統匣）", "Start with Windows (in the system tray)"), can).changed() {
            d.autostart_busy = true;
            app.spawn(
                async move {
                    let r = screenrecorder_core::desktop::set_autostart(on).await;
                    (r, screenrecorder_core::desktop::get_autostart().await)
                },
                |app, (r, now)| {
                    if let Some(d) = &mut app.settings_dlg {
                        d.autostart = Some(now);
                        d.autostart_busy = false;
                    }
                    screenrecorder_core::tray::autostart_changed();
                    if let Err(e) = r {
                        app.toast(e.message().to_string(), true);
                    }
                },
            );
        }
    });
    if d.autostart == Some(None) {
        form_hint(ui, tr!("開發版不能設定開機自動啟動。", "Start with Windows can't be set in development builds."), p.muted);
    }
    form_row(ui, tr!("結束", "Quit"), |ui| {
        if Btn::new(tr!("結束程式", "Quit"))
            .small()
            .tooltip(tr!("完全結束螢幕錄影（關掉視窗只是收到系統匣）", "Quit Screen Recorder completely (closing the window only hides it in the tray)"))
            .show(ui)
            .clicked()
        {
            app.request_quit();
        }
    });
}

// ───────────── 進階 ─────────────

fn advanced_page(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, tr!("擷取與編碼", "Capture and encoding"), |_| {});
    form_row(ui, tr!("擷取方式", "Capture method"), |ui| {
        let mut method = app.s.method;
        let items = [
            (MethodPreference::Auto, tr!("自動（建議）", "Auto (recommended)").to_string(), true),
            (MethodPreference::Ddagrab, tr!("顯示卡擷取（ddagrab，較省 CPU）", "GPU capture (ddagrab, lower CPU use)").into(), true),
            (MethodPreference::Gdigrab, tr!("相容模式（gdigrab，哪台電腦都能用）", "Compatibility mode (gdigrab, works on any PC)").into(), true),
        ];
        if form_combo(ui, "method", FORM_CTRL_W, &mut method, &items) {
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
        None => tr!("顯示卡編碼（偵測中…）", "GPU encoding (detecting…)").to_string(),
        Some(h) if !h.is_empty() => {
            let names = h.join(tr!("、", ", "));
            trf!("顯示卡編碼（{names}）", "GPU encoding ({names})")
        }
        _ => tr!("顯示卡編碼（這台電腦沒有可用的）", "GPU encoding (not available on this PC)").into(),
    };
    form_row(ui, tr!("編碼器", "Encoder"), |ui| {
        let mut enc = app.s.encoder;
        let items = [
            (EncoderPreference::Auto, tr!("自動（建議）", "Auto (recommended)").to_string(), true),
            (EncoderPreference::Cpu, tr!("CPU 編碼（libx264，畫質最穩）", "CPU encoding (libx264, most consistent quality)").into(), true),
            (EncoderPreference::Gpu, gpu_label.clone(), hw.as_ref().is_some_and(|h| !h.is_empty())),
        ];
        if form_combo(ui, "encoder", FORM_CTRL_W, &mut enc, &items) {
            app.s.encoder = enc;
            app.save_settings();
        }
    });
    let hint = if app.s.encoder != EncoderPreference::Auto {
        String::new()
    } else {
        match &hw {
            Some(h) if h.iter().any(|n| n == screenrecorder_core::args::AUTO_PREFERRED_GPU) => {
                tr!("優先用 Intel 顯示卡（QSV），CPU 負擔最小；不能用時改用 CPU。", "Uses the Intel GPU (QSV) first for the lowest CPU load; falls back to the CPU when it can't.").into()
            }
            Some(h) if !h.is_empty() => {
                if ff.prefer_gpu == Some(true) {
                    trf!("先前偵測到 CPU 編碼跟不上，目前會使用 GPU（{}）。", "CPU encoding couldn't keep up earlier, so the GPU ({}) is used for now.", h[0])
                } else {
                    trf!("平常用 CPU；畫面大或電腦跟不上時改用 GPU（{}）。", "Uses the CPU normally; switches to the GPU ({}) for large areas or when the PC can't keep up.", h[0])
                }
            }
            _ => tr!("這台電腦只能用 CPU 編碼。", "This PC can only use CPU encoding.").into(),
        }
    };
    if !hint.is_empty() {
        form_hint(ui, hint, p.muted);
    }
    if app.s.encoder == EncoderPreference::Auto && ff.prefer_gpu == Some(true) && hw.as_ref().is_some_and(|h| !h.is_empty()) {
        let mut reset = false;
        form_row(ui, "", |ui| reset = Btn::new(tr!("重設：恢復平常用 CPU 編碼", "Reset to normal CPU encoding")).small().show(ui).clicked());
        if reset {
            app.core.reset_learned_gpu();
            app.env.ffmpeg.prefer_gpu = Some(false);
            app.toast(tr!("已重設，之後的錄影平常會用 CPU 編碼", "Reset. Future recordings will normally use CPU encoding"), false);
        }
    }
    codec_row(app, ui);
    form_divider(ui);
    form_section(ui, tr!("疑難排解", "Troubleshooting"), |_| {});
    form_row(ui, tr!("記錄檔", "Log file"), |ui| {
        let log = screenrecorder_core::log::log_file().display().to_string();
        if Btn::new(tr!("開啟記錄檔", "Open log file")).small().tooltip(log.clone()).show(ui).clicked() {
            screenrecorder_core::desktop::open_with_explorer(&log, false);
        }
        if Btn::new(tr!("在資料夾中顯示", "Show in folder")).small().ghost().show(ui).clicked() {
            screenrecorder_core::desktop::open_with_explorer(&log, true);
        }
    });
    form_hint(
        ui,
        tr!(
            "程式做了什麼、哪裡出錯都會寫在這裡。回報問題時附上最後幾十行，比較容易找到原因。",
            "Everything the app does, and any errors, are written here. When reporting a problem, include the last few dozen lines so the cause is easier to find."
        ),
        p.muted,
    );
}

// ───────────── 關於：版本、這一版更新了什麼（可看所有版本的更新說明）、新版本、連結 ─────────────

/// 「這一版更新了什麼」最多列幾項（其餘在「所有版本的更新說明」看）
const ABOUT_NOTES_MAX: usize = 6;

fn about_page(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    // 程式圖示、名稱、版本
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let tex = ui.ctx().memory_mut(|m| m.data.get_temp::<egui::TextureHandle>(Id::new("about-icon"))).unwrap_or_else(|| {
            let rgba = screenrecorder_core::icon::render_icon(128, screenrecorder_core::icon::IconState::Idle);
            let img = egui::ColorImage::from_rgba_unmultiplied([128, 128], &rgba);
            let t = ui.ctx().load_texture("about-icon", img, egui::TextureOptions::LINEAR);
            ui.ctx().memory_mut(|m| m.data.insert_temp(Id::new("about-icon"), t.clone()));
            t
        });
        ui.add(egui::Image::new((tex.id(), vec2(56.0, 56.0))));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(tr!("螢幕錄影", "Screen Recorder")).font(theme::font_bold(20.0)));
            ui.horizontal(|ui| {
                ui.label(RichText::new(trf!("版本 {}", "Version {}", APP_VERSION)).font(theme::font(13.0)).color(p.muted));
                if let Some(u) = &app.env.update {
                    theme::chip(ui, &trf!("有新版本 v{}", "v{} available", u.version), theme::Tone::Accent, false);
                }
            });
            ui.label(RichText::new(tr!("錄影、截圖、剪輯都在這裡，錄完直接分享。", "Record, screenshot and edit in one place, then share right away.")).font(theme::font(12.5)).color(p.muted));
        });
    });
    form_divider(ui);
    // 這一版更新了什麼（CHANGELOG.md 裡這一版的段落）
    let notes = release_notes(APP_VERSION);
    // 標題「3.2.1（2026-10-14）」：版本後面的日期放右邊
    let date = notes.map(|(t, _)| t[APP_VERSION.len()..].trim().trim_start_matches(['（', '(']).trim_end_matches(['）', ')']).to_string()).unwrap_or_default();
    form_section(ui, &trf!("v{APP_VERSION} 更新了什麼", "What's new in v{APP_VERSION}"), |ui| {
        // 留一點空間給捲軸
        ui.add_space(10.0);
        ui.label(theme::muted(ui, date));
    });
    let items: Vec<&str> = notes.map(|(_, body)| body.lines().filter_map(|l| l.strip_prefix("- ")).collect()).unwrap_or_default();
    if items.is_empty() {
        form_hint_full(ui, tr!("這一版沒有更新說明。", "There are no release notes for this version."), p.muted);
    }
    for item in items.iter().take(ABOUT_NOTES_MAX) {
        dialogs::bullet(ui, item, 0.0);
    }
    if items.len() > ABOUT_NOTES_MAX {
        ui.label(theme::muted(ui, trf!("…還有 {} 項", "…and {} more", items.len() - ABOUT_NOTES_MAX)));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if Btn::new(tr!("所有版本的更新說明", "All release notes"))
            .icon(Icon::List)
            .small()
            .tooltip(tr!("每一版新增、變更與修正的內容", "What was added, changed and fixed in every version"))
            .show(ui)
            .clicked()
        {
            app.changelog_open = true;
        }
        if Btn::new(tr!("新功能介紹", "What's new tour")).small().ghost().tooltip(tr!("再看一次這一版的新功能介紹", "Show this version's feature tour again")).show(ui).clicked() {
            app.whats_new_open = true;
        }
        if Btn::new(tr!("重新看教學", "Replay tutorial"))
            .small()
            .ghost()
            .tooltip(tr!(
                "一步一步帶你錄第一部影片；之後第一次打開剪輯、截圖編輯、全部截圖時也會再顯示小提示",
                "Walks you through your first recording again; quick tips also reappear the next time you open the editor, screenshot editing or All screenshots"
            ))
            .show(ui)
            .clicked()
        {
            super::tour::restart(app);
        }
    });
    form_divider(ui);
    updates_section(app, ui);
    form_divider(ui);
    form_section(ui, tr!("連結", "Links"), |_| {});
    let link = |ui: &mut Ui, app: &mut UiApp, label: &str, text: &str, url: &str| {
        form_row(ui, label, |ui| {
            if Btn::new(text).small().ghost().tooltip(url).show(ui).clicked() {
                if let Err(e) = actions::open_url(url) {
                    app.toast(e.message().to_string(), true);
                }
            }
        });
    };
    link(ui, app, tr!("專案網頁", "Project page"), tr!("在瀏覽器開啟", "Open in browser"), PROJECT_URL);
    link(ui, app, tr!("回報問題", "Report a problem"), tr!("開啟問題回報頁", "Open the issue tracker"), ISSUES_URL);
    form_row(ui, "FFmpeg", |ui| {
        let ff = &app.env.ffmpeg;
        let text =
            if ff.found { ff.version.clone().map(|v| trf!("版本 {v}", "Version {v}")).unwrap_or_else(|| tr!("已找到", "Found").into()) } else { tr!("還沒有安裝", "Not installed").into() };
        let r = ui.label(theme::muted(ui, text));
        if let Some(path) = &ff.path {
            r.on_hover_text(path.clone());
        }
    });
}

/// 新版本：自動檢查、立即檢查、有新版本時更新
fn updates_section(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, tr!("新版本", "Updates"), |_| {});
    form_row(ui, tr!("自動檢查", "Auto check"), |ui| {
        let mut on = app.update.enabled;
        if switch(ui, &mut on, tr!("啟動後與每 12 小時檢查", "Check at startup and every 12 hours"), true).changed() {
            app.core.set_check_updates(on);
            app.update = actions::update_state(&app.core);
        }
    });
    // 有新版本：直接在這裡更新、看新版的更新內容
    if let Some(u) = app.env.update.clone() {
        form_row(ui, tr!("有新版本", "New version"), |ui| {
            let busy = main_view::update_progress(app);
            let label = busy.clone().unwrap_or_else(|| if main_view::can_self_update(&u) { trf!("更新到 v{}", "Update to v{}", u.version) } else { trf!("下載 v{}", "Download v{}", u.version) });
            if Btn::new(label).primary().small().enabled(busy.is_none()).show(ui).clicked() {
                main_view::ask_update(app, &u);
            }
            if Btn::new(tr!("新版的更新內容", "What's new")).small().ghost().tooltip(tr!("在瀏覽器開啟新版的說明", "Open the release notes in your browser")).show(ui).clicked() {
                let _ = actions::open_url(&u.url);
            }
        });
    }
    form_row(ui, if app.env.update.is_some() { "" } else { tr!("目前版本", "Current version") }, |ui| {
        if Btn::new(tr!("立即檢查", "Check now")).small().show(ui).clicked() {
            let core = app.core.clone();
            app.spawn(async move { actions::check_update(&core).await }, |app, r| match r {
                Ok(u) => {
                    app.update = actions::update_state(&app.core);
                    app.env.update = u.clone();
                    match u {
                        Some(u) => app.toast(trf!("有新版本 v{}", "Version v{} is available", u.version), false),
                        None => app.toast(trf!("目前已是最新版本（v{}）", "You're on the latest version (v{})", app.env.app_version), false),
                    }
                }
                Err(e) => {
                    app.update.error = Some(e.message().to_string());
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        if app.env.update.is_none() {
            ui.label(theme::muted(ui, trf!("v{}", "v{}", app.env.app_version)));
        }
    });
    if let Some(e) = &app.update.error {
        // 「立即檢查」的錯誤已經有這個開頭（中文或英文），不要重複
        let e = e.trim_start_matches("無法檢查新版本：").trim_start_matches("Couldn't check for updates: ");
        form_hint(ui, trf!("無法檢查新版本：{e}", "Couldn't check for updates: {e}"), p.warn);
    }
}

// ───────────── 快速鍵（點按鍵後按下新的組合，Esc 取消；可停用或還原預設） ─────────────

fn keys_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    let Some(hk) = app.env.hotkeys else {
        form_section(ui, tr!("全域快速鍵", "Global shortcuts"), |_| {});
        form_hint_full(ui, tr!("全域快速鍵需要系統匣常駐時才能使用。", "Global shortcuts only work while the app is running in the system tray."), p.muted);
        return;
    };
    let ok = hk.all();
    form_section(ui, tr!("全域快速鍵", "Global shortcuts"), |ui| {
        if app.keys != Hotkeys::default() && Btn::new(tr!("還原預設", "Restore defaults")).ghost().small().show(ui).clicked() {
            d.key_capture = None;
            save_keys(app, d, Hotkeys::default());
        }
    });
    form_hint_full(
        ui,
        tr!(
            "程式在背景時也能用。點一下按鍵，再按下新的組合（要包含 Ctrl 或 Alt）。",
            "These work even when the app is in the background. Click a key, then press a new combination (must include Ctrl or Alt)."
        ),
        p.muted,
    );
    ui.add_space(6.0);
    let names = screenrecorder_core::types::hotkey_names();
    let mut take_conflict = None;
    for (i, &name) in names.iter().enumerate().take(ok.len()) {
        key_row(ui, name, |ui| {
            let capturing = d.key_capture == Some(i);
            let label = app.keys.label(i);
            let busy = !ok[i] && !label.is_empty();
            let text = if capturing {
                tr!("請按下組合鍵…", "Press a key combination…").to_string()
            } else if label.is_empty() {
                tr!("停用", "Disabled").to_string()
            } else {
                label.clone()
            };
            let tip = if capturing {
                tr!("按 Esc 取消", "Press Esc to cancel")
            } else if busy {
                tr!("被其他程式占用，點一下換一組", "Used by another program. Click to choose a different one")
            } else {
                tr!("點一下後按下新的組合鍵（要包含 Ctrl 或 Alt）", "Click, then press a new key combination (must include Ctrl or Alt)")
            };
            if Btn::new(text).small().height(30.0).selected(capturing).min_width(KEY_BTN_W).tooltip(tip).show(ui).clicked() {
                if capturing {
                    stop_capture(app, d);
                } else {
                    // 先暫停全部快速鍵，才按得到原本已登記的組合
                    d.key_capture = Some(i);
                    d.key_msg = None;
                    d.key_conflict = None;
                    app.core.apply_hotkeys(&Hotkeys::none());
                }
            }
            if !label.is_empty() && !capturing && Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("停用這個快速鍵", "Disable this shortcut")).show(ui).clicked() {
                let mut k = app.keys;
                k.set(i, None);
                save_keys(app, d, k);
            }
        });
        // 被其他程式占用：橘色說明（點一下按鍵就能換）
        if !ok[i] && !app.keys.label(i).is_empty() && d.key_capture != Some(i) {
            key_hint(ui, tr!("⚠ 被其他程式占用，點一下換一組", "⚠ Used by another program. Click to choose a different one"), p.warn);
        }
        // 這一列的提示（不支援的按鍵、和別的功能重複）寫在這一列下面
        if let Some((_, m)) = d.key_msg.clone().filter(|(row, _)| *row == i) {
            key_hint(ui, m, p.warn);
        }
        if let Some((_, j, k)) = d.key_conflict.filter(|(row, _, _)| *row == i) {
            // 名稱比較長：靠右對齊在按鍵那一欄的右邊，放不下時往左延伸
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(34.0);
                    if Btn::new(trf!("改用這組（原本的「{}」改成停用）", "Use it here (turn off “{}”)", names[j])).small().show(ui).clicked() {
                        take_conflict = Some((i, j, k));
                    }
                });
            });
        }
    }
    if let Some((i, j, k)) = take_conflict {
        let mut next = app.keys;
        next.set(j, None);
        next.set(i, Some(k));
        d.key_capture = None;
        d.key_conflict = None;
        save_keys(app, d, next);
    }
    let Some(i) = d.key_capture else { return };
    // 等使用者按下組合鍵
    let pressed = ui.input(|inp| {
        inp.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
            _ => None,
        })
    });
    let Some((key, m)) = pressed else { return };
    if key == egui::Key::Escape {
        stop_capture(app, d);
        return;
    }
    let Some(vk) = vk_of(key) else {
        d.key_msg = Some((i, trf!("不支援「{}」，請用 A–Z、0–9、F1–F12 等按鍵", "“{}” isn't supported. Use keys like A–Z, 0–9 or F1–F12", key.name())));
        return;
    };
    let k = Hotkey { ctrl: m.ctrl, alt: m.alt, shift: m.shift, win: false, key: vk };
    if !k.valid() {
        d.key_msg = Some((i, trf!("{} 會擋到一般打字，請加上 Ctrl 或 Alt", "{} would interfere with normal typing. Add Ctrl or Alt", k.label())));
        return;
    }
    if let Some(j) = app.keys.conflict(i, &k) {
        // 先停止等待按鍵（恢復原本的快速鍵），讓使用者決定要不要改用這組
        stop_capture(app, d);
        d.key_msg = Some((i, trf!("{} 已經用在「{}」", "{} is already used for “{}”", k.label(), names[j])));
        d.key_conflict = Some((i, j, k));
        return;
    }
    let mut next = app.keys;
    next.set(i, Some(k));
    d.key_capture = None;
    save_keys(app, d, next);
}

/// 快速鍵頁的名稱欄寬（名稱比較長，例如「回溯錄影（開始 / 存下）」）與按鍵按鈕的寬度
const KEY_LABEL_W: f32 = 190.0;
const KEY_BTN_W: f32 = 180.0;

/// 快速鍵頁的一列：左邊名稱（比一般表單寬），右邊按鍵
fn key_row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // 名稱換成兩行時這一列跟著變高，不會和下一列擠在一起
        let g = ui.painter().layout(label.to_string(), theme::font(13.0), p.muted, KEY_LABEL_W - 4.0);
        let h = FORM_ROW_H.max(g.size().y + 8.0);
        let (r, _) = ui.allocate_exact_size(vec2(KEY_LABEL_W, h), Sense::hover());
        ui.painter().galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g, p.muted);
        ui.set_min_height(h);
        add(ui);
    });
}

/// 快速鍵那一列下面的說明
fn key_hint(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    ui.horizontal(|ui| {
        ui.add_space(KEY_LABEL_W + 6.0);
        ui.add(egui::Label::new(RichText::new(text.into()).font(theme::font(12.0)).color(color)).wrap());
    });
}

/// 換分頁或關掉設定視窗時：取消設定中的快速鍵（恢復原本的登記）
pub fn check_key_capture(app: &mut UiApp) {
    let stale = app.settings_dlg.as_ref().is_some_and(|d| d.key_capture.is_some() && d.page != Page::Keys);
    if stale {
        cancel_key_capture(app);
    }
}

/// 取消設定中的快速鍵（視窗關到系統匣時：不能讓快速鍵一直暫停）
pub fn cancel_key_capture(app: &mut UiApp) {
    if let Some(mut d) = app.settings_dlg.take() {
        if d.key_capture.is_some() {
            stop_capture(app, &mut d);
        }
        app.settings_dlg = Some(d);
    }
}

fn stop_capture(app: &mut UiApp, d: &mut SettingsDialog) {
    d.key_capture = None;
    d.key_msg = None;
    d.key_conflict = None;
    let k = app.keys;
    if let Some(st) = app.core.apply_hotkeys(&k) {
        app.env.hotkeys = Some(st);
    }
}

fn save_keys(app: &mut UiApp, d: &mut SettingsDialog, k: Hotkeys) {
    app.keys = k;
    d.key_msg = None;
    if let Some(st) = app.core.save_hotkeys(k) {
        app.env.hotkeys = Some(st);
        let ok = st.all();
        if let Some(i) = (0..ok.len()).find(|i| !ok[*i] && !k.label(*i).is_empty()) {
            d.key_msg = Some((i, trf!("{} 已被其他程式使用，請換一組", "{} is used by another program. Choose a different one", k.label(i))));
        }
    }
}

/// egui 的按鍵 → Windows 虛擬鍵碼（只限支援的按鍵）
fn vk_of(key: egui::Key) -> Option<u32> {
    let name = key.name();
    (0x20..=0x7B).find(|vk| screenrecorder_core::types::key_name(*vk).as_deref() == Some(name))
}

/// 錄影時遮住的固定範圍：列出已框好的範圍，可以新增（在螢幕上框選）或刪除
fn masks_row(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let mut remove = None;
    let mut add = false;
    form_row(ui, tr!("遮住範圍", "Hidden areas"), |ui| {
        ui.vertical(|ui| {
            for (i, r) in app.s.rec_masks.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(trf!("範圍 {}：{}×{}（{}, {}）", "Area {}: {}×{} at ({}, {})", i + 1, r.width, r.height, r.x, r.y)).font(theme::font(12.5)));
                    if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("刪除這個範圍", "Remove this area")).show(ui).clicked() {
                        remove = Some(i);
                    }
                });
            }
            let label = if app.s.rec_masks.is_empty() { tr!("在螢幕上框選要遮住的地方…", "Select an area to hide on screen…") } else { tr!("再加一個…", "Add another…") };
            let b = Btn::new(label).icon(Icon::Fullscreen).small().enabled(app.env.ffmpeg.found && app.s.rec_masks.len() < 20).tooltip(tr!(
                "框選螢幕上不想被錄到的地方（例如帳號、聊天視窗、通知區），錄影時直接打上馬賽克",
                "Select places on screen you don't want recorded (e.g. account names, chat windows, the notification area); they're pixelated while recording"
            ));
            if b.show(ui).clicked() {
                add = true;
            }
        });
    });
    if !app.s.rec_masks.is_empty() {
        form_hint(
            ui,
            tr!(
                "錄影時這些範圍會打上馬賽克（範圍是固定在螢幕上的位置，視窗移走就遮不到）。只錄視窗內容時不適用；顯示卡加速轉換會改用一般方式。",
                "These areas are pixelated while recording (they're fixed screen positions, so a window that moves away isn't covered). Not applied with Window only; GPU conversion is turned off while they're in use."
            ),
            p.muted,
        );
    }
    if let Some(i) = remove {
        app.s.rec_masks.remove(i);
        app.save_settings();
    }
    if add {
        super::snip::pick_region(app, |app: &mut UiApp, r| {
            let Some(r) = r else { return };
            if r.width < 4 || r.height < 4 {
                return app.toast(tr!("範圍太小", "The area is too small"), true);
            }
            app.s.rec_masks.push(r);
            app.save_settings();
            app.toast(tr!("已加上要遮住的範圍，錄影時會打上馬賽克", "Hidden area added; it'll be pixelated while recording"), false);
        });
    }
}

// ───────────── 外部元件 ─────────────

/// 外部元件：FFmpeg（必要）、語音辨識用的 FFmpeg 與語音模型（自動字幕）；下載不了時可以設定 Proxy，或自己下載後選擇檔案
fn components_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_hint_full(
        ui,
        trf!(
            "這些元件都是免費軟體，下載一次就好，下載後會檢查檔案是否完整。放在程式所在的資料夾（{}），整個資料夾帶著走也能用；那裡不能寫入時（例如放在 Program Files）才放到 %LOCALAPPDATA%\\ScreenRecorder。",
            "These are free components. They're downloaded once and checked after downloading. They're kept in the app's folder ({}), so you can move the whole folder and they come along. Only if that folder isn't writable (e.g. in Program Files) are they kept in %LOCALAPPDATA%\\ScreenRecorder.",
            screenrecorder_core::paths::app_dir().display()
        ),
        p.muted,
    );
    ui.add_space(8.0);
    form_section(ui, tr!("錄影", "Recording"), |_| {});
    component_card(app, d, ui, Kind::Ffmpeg);
    ui.add_space(12.0);
    form_section(ui, tr!("播放", "Playback"), |_| {});
    component_card(app, d, ui, Kind::Mpv);
    ui.add_space(4.0);
    let mut on = app.s.use_mpv;
    if switch(ui, &mut on, tr!("用 mpv 播放", "Play with mpv"), true)
        .on_hover_text(tr!("播放有問題時可以關掉，改用 FFmpeg 播放", "Turn off if playback has problems; FFmpeg is used instead"))
        .changed()
    {
        app.s.use_mpv = on;
        app.save_settings();
    }
    ui.add_space(12.0);
    form_section(ui, tr!("自動字幕（語音辨識）", "Auto subtitles (speech recognition)"), |_| {});
    form_hint_full(
        ui,
        tr!(
            "剪輯視窗的「自動產生字幕」、錄完自動處理的字幕要用到；不做字幕就不用下載。",
            "Needed for “Generate subtitles” in the editor and subtitles after recording. Skip these if you don't use subtitles."
        ),
        p.muted,
    );
    ui.add_space(4.0);
    for k in Kind::all().into_iter().filter(|k| !matches!(k, Kind::Ffmpeg | Kind::Mpv)) {
        component_card(app, d, ui, k);
        ui.add_space(6.0);
    }
    ui.add_space(8.0);
    form_divider(ui);
    network_section(app, d, ui);
    ui.add_space(8.0);
    form_section(ui, tr!("無法下載時", "If downloads don't work"), |_| {});
    for line in [
        tr!(
            "1. 確認已連上網路；公司網路請在上面設定 Proxy，或請 IT 放行 github.com、gyan.dev、sourceforge.net、huggingface.co。",
            "1. Check your internet connection. On a company network, set the proxy above, or ask IT to allow github.com, gyan.dev, sourceforge.net and huggingface.co."
        ),
        tr!("2. 按「用瀏覽器下載」，在瀏覽器（或另一台電腦）下載。", "2. Press “Download in browser” to download it in your browser (or on another computer)."),
        tr!(
            "3. 回到這裡按「選擇已下載的檔案…」：FFmpeg 可以選 zip 或 ffmpeg.exe，mpv 播放元件選 .7z 或 libmpv-2.dll，語音模型選 .bin。",
            "3. Come back and press “Choose downloaded file…”: for FFmpeg choose the zip or ffmpeg.exe; for the mpv component choose the .7z or libmpv-2.dll; for speech models choose the .bin file."
        ),
    ] {
        form_hint_full(ui, line, p.muted);
    }
    // 下載中：持續更新進度
    if Kind::all().iter().any(|k| app.core.component_status(*k).running) || d.installing.is_some() || d.net_testing {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// 一個元件：名稱、狀態、說明、進度或錯誤、按鈕
fn component_card(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui, k: Kind) {
    use theme::Tone;
    let p = theme::pal(ui);
    let st = app.core.component_status(k);
    // 目前的 FFmpeg 就能辨識語音：不用另外下載
    let builtin = k == Kind::SpeechFfmpeg && app.env.ffmpeg.has_whisper;
    let installed = app.core.component_installed(k);
    let installing = d.installing == Some(k);
    egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(k.name()).font(theme::font_bold(13.5)).color(p.text));
            let (text, tone) = if st.running {
                (trf!("下載中 {}%", "Downloading {}%", (st.progress * 100.0).floor() as u32), Tone::Accent)
            } else if installing {
                (tr!("安裝中…", "Installing…").to_string(), Tone::Accent)
            } else if installed {
                (tr!("已安裝", "Installed").to_string(), Tone::Ok)
            } else if st.error.is_some() {
                (tr!("下載失敗", "Download failed").to_string(), Tone::Bad)
            } else {
                (tr!("未安裝", "Not installed").to_string(), Tone::Plain)
            };
            theme::chip(ui, &text, tone, false);
        });
        let info = match k {
            Kind::Ffmpeg if installed => trf!("版本 {}：{}", "Version {}: {}", app.env.ffmpeg.version.clone().unwrap_or_default(), app.env.ffmpeg.path.clone().unwrap_or_default()),
            Kind::Ffmpeg => tr!("錄影、截圖、轉檔都要用到（必要，約 100 MB）。", "Required for recording, screenshots and conversion (about 100 MB).").into(),
            Kind::Mpv if installed => trf!(
                "播放影片時用 mpv（顯示卡解碼），4K、60 fps 的影片也很順。位置：{}",
                "Videos play with mpv (decoded on the graphics card), so even 4K 60 fps videos play smoothly. Location: {}",
                k.path().filter(|p| p.is_file()).map(|p| p.display().to_string()).unwrap_or_else(|| screenrecorder_core::mpv::LIB_NAME.into())
            ),
            Kind::Mpv => trf!(
                "讓檢視器、主畫面播放影片更順（手機的 4K、60 fps 影片也一樣），約 {} MB。不下載也可以播放，只是大影片可能卡卡的。",
                "Makes videos play more smoothly in the viewer and on the main screen (including 4K 60 fps phone videos), about {} MB. Videos still play without it, but large ones may stutter.",
                k.mb()
            ),
            _ if builtin => trf!(
                "就是上面「FFmpeg」那一個（版本 {}：{}），它本身就能辨識語音，不用另外下載。",
                "It's the same FFmpeg as above (version {}: {}); it can already recognize speech, so nothing else is needed.",
                app.env.ffmpeg.version.clone().unwrap_or_default(),
                app.env.ffmpeg.path.clone().unwrap_or_default()
            ),
            Kind::SpeechFfmpeg if installed => trf!(
                "另外下載的、有語音辨識功能的 FFmpeg，只用來產生字幕：{}",
                "A separate FFmpeg build with speech recognition, used only for subtitles: {}",
                k.path().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            Kind::SpeechFfmpeg => trf!("有語音辨識功能的 FFmpeg（約 {} MB），只用來產生字幕。", "An FFmpeg build with speech recognition (about {} MB), used only for subtitles.", k.mb()),
            Kind::Model(_) if installed => trf!(
                "模型越大辨識越準，但比較慢。只需要其中一個。位置：{}",
                "Bigger models are more accurate but slower. You only need one. Location: {}",
                k.path().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            Kind::Model(_) => tr!("模型越大辨識越準，但比較慢。只需要其中一個。", "Bigger models are more accurate but slower. You only need one.").into(),
        };
        // 程式資料夾不能寫入（例如放在 Program Files）才會放在 %LOCALAPPDATA%：說明原因
        let info = match k.path().filter(|p| installed && !builtin && p.is_file() && screenrecorder_core::paths::inside(p, &screenrecorder_core::paths::data_dir())) {
            Some(_) => info + tr!("（程式所在的資料夾不能寫入，所以放在這裡）", " (the app's folder isn't writable, so it's kept here)"),
            None => info,
        };
        let info = if k.model().is_some_and(|m| !m.translates) {
            info + tr!("這個模型不能翻成英文，要英文字幕時另外需要「精準」或「標準」。", " This model can't translate to English; English subtitles also need \"Accurate\" or \"Standard\".")
        } else {
            info
        };
        ui.add(egui::Label::new(RichText::new(info).font(theme::font(12.0)).color(p.muted)).wrap());
        if st.running {
            ui.add_space(4.0);
            theme::progress(ui, st.progress, p.accent, 6.0);
        }
        if let Some(e) = st.error.as_ref().filter(|_| !st.running) {
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(e).font(theme::font(12.0)).color(p.rec)).wrap());
        }
        if builtin {
            return;
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if st.running {
                if Btn::new(tr!("取消下載", "Cancel download")).small().show(ui).clicked() {
                    app.core.component_cancel(k);
                }
                return;
            }
            let busy = installing || app.locked();
            if !installed || k == Kind::Ffmpeg {
                let label = match (installed, st.error.is_some()) {
                    (true, _) => tr!("重新下載", "Download again"),
                    (false, true) => tr!("重試", "Retry"),
                    (false, false) => tr!("下載", "Download"),
                };
                let tip = if busy { tr!("錄影中或安裝中，稍後再下載", "Wait until recording or installing is done") } else { "" };
                let b = Btn::new(label).small().icon(Icon::Download).enabled(!busy).tooltip(tip);
                if (if installed { b } else { b.primary() }).show(ui).clicked() {
                    app.core.component_download(k);
                }
            }
            if Btn::new(tr!("用瀏覽器下載", "Download in browser"))
                .small()
                .ghost()
                .tooltip(tr!("自動下載失敗時：用瀏覽器下載，再按「選擇已下載的檔案…」", "If the automatic download fails: download it in your browser, then press “Choose downloaded file…”"))
                .show(ui)
                .clicked()
            {
                if let Err(e) = actions::open_url(&k.url()) {
                    app.toast(e.message().to_string(), true);
                }
            }
            if Btn::new(tr!("選擇已下載的檔案…", "Choose downloaded file…")).small().ghost().enabled(!busy).show(ui).clicked() {
                pick_component_file(app, d, k);
            }
            if k == Kind::Ffmpeg {
                if Btn::new(tr!("重新偵測", "Detect again")).small().ghost().show(ui).clicked() {
                    app.refresh_env(|app| {
                        let (msg, err) = if app.env.ffmpeg.found {
                            (trf!("已找到 FFmpeg {}", "Found FFmpeg {}", app.env.ffmpeg.version.clone().unwrap_or_default()), false)
                        } else {
                            (tr!("仍然找不到 ffmpeg.exe", "ffmpeg.exe still not found").into(), true)
                        };
                        app.toast(msg, err);
                    });
                }
            } else if installed
                && Btn::new(tr!("移除", "Remove"))
                    .small()
                    .ghost()
                    .enabled(!busy)
                    .tooltip(tr!("刪除下載的檔案，釋放空間；之後要用時再下載", "Delete the downloaded file to free up space; download it again when needed"))
                    .show(ui)
                    .clicked()
            {
                match app.core.component_remove(k) {
                    Ok(()) => app.toast(trf!("已移除{}", "Removed {}", k.name()), false),
                    Err(e) => app.toast(e, true),
                }
            }
            // FFmpeg：目前用的那一個所在的資料夾（可能是自己放的或 PATH 上的）
            let path = match (k, &app.env.ffmpeg.path) {
                (Kind::Ffmpeg, Some(found)) => Some(std::path::PathBuf::from(found)),
                _ => k.path(),
            };
            if let Some(dir) = path.and_then(|p| p.parent().map(|d| d.to_path_buf())).filter(|d| d.is_dir()) {
                if Btn::icon_only(Icon::Folder).small().ghost().tooltip(tr!("開啟所在的資料夾", "Open the folder")).show(ui).clicked() {
                    if let Err(e) = actions::open(actions::OpenAction::Folder, &dir.display().to_string()) {
                        app.toast(e.message().to_string(), true);
                    }
                }
            }
        });
    });
}

/// 選擇自己下載的檔案並安裝
fn pick_component_file(app: &mut UiApp, d: &mut SettingsDialog, k: Kind) {
    d.installing = Some(k);
    let core = app.core.clone();
    let title = trf!("選擇已下載的{}", "Choose the downloaded {}", k.name());
    let filter = (tr!("檔案", "Files"), k.file_filter());
    app.spawn(
        async move {
            let picked = tokio::task::spawn_blocking(move || screenrecorder_core::filepick::pick(&title, &[filter])).await.unwrap_or_else(|e| Err(e.to_string()))?;
            let Some(path) = picked else { return Ok(false) };
            core.component_install_file(k, path).await.map(|_| true)
        },
        move |app, r: Result<bool, String>| {
            if let Some(d) = &mut app.settings_dlg {
                d.installing = None;
            }
            match r {
                Ok(true) => {
                    app.toast(trf!("已安裝{}", "Installed {}", k.name()), false);
                    // FFmpeg 換了：重新讀環境（主畫面、剪輯都要用）
                    app.refresh_env(|_| {});
                }
                Ok(false) => {}
                Err(e) => app.toast(e, true),
            }
        },
    );
}

/// 網路：Proxy 與測試連線
fn network_section(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, tr!("網路", "Network"), |_| {});
    form_row(ui, "Proxy", |ui| {
        let items = [("auto", tr!("跟著系統", "System")), ("off", tr!("不使用", "None")), ("custom", tr!("自訂", "Custom"))];
        let mut cur = items.iter().find(|(v, _)| *v == app.s.proxy_mode).map(|(v, _)| *v).unwrap_or("auto");
        if segmented(ui, &mut cur, &items, true) {
            app.s.proxy_mode = cur.to_string();
            d.net_result = None;
            app.save_settings_now();
        }
    });
    match app.s.proxy_mode.as_str() {
        "custom" => {
            form_row(ui, tr!("位址", "Address"), |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut app.s.proxy).desired_width(FORM_CTRL_W).hint_text("proxy.company.com:8080"));
                if r.changed() {
                    d.net_result = None;
                    app.save_settings();
                }
            });
            form_hint(
                ui,
                tr!(
                    "例如 proxy.company.com:8080；需要帳號密碼時寫成 http://帳號:密碼@proxy.company.com:8080",
                    "e.g. proxy.company.com:8080; with a password: http://user:password@proxy.company.com:8080"
                ),
                p.muted,
            );
        }
        "auto" => {
            let text = match screenrecorder_core::http::system_proxy() {
                Some(px) => trf!("系統設定的 Proxy：{px}", "System proxy: {px}"),
                None => tr!(
                    "系統沒有設定 Proxy，直接連線（自動設定指令碼 PAC 不支援，請改用「自訂」）",
                    "No system proxy is set, so connections are direct (automatic configuration scripts aren't supported; use Custom instead)"
                )
                .into(),
            };
            form_hint(ui, text, p.muted);
        }
        _ => {}
    }
    form_row(ui, "", |ui| {
        if Btn::new(if d.net_testing { tr!("測試中…", "Testing…") } else { tr!("測試連線", "Test connection") }).small().enabled(!d.net_testing).show(ui).clicked() {
            d.net_testing = true;
            d.net_result = None;
            app.save_settings_now();
            app.spawn(async move { tokio::task::spawn_blocking(screenrecorder_core::http::test_connection).await.unwrap_or_else(|e| Err(e.to_string())) }, |app, r| {
                if let Some(d) = &mut app.settings_dlg {
                    d.net_testing = false;
                    d.net_result = Some(r);
                }
            });
        }
        match &d.net_result {
            Some(Ok(())) => {
                ui.label(RichText::new(tr!("可以連上下載來源", "The download sources can be reached")).font(theme::font(12.5)).color(p.ok));
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(e).font(theme::font(12.5)).color(p.rec)).wrap());
            }
            None => {}
        }
    });
}
