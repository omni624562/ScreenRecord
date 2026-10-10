//! 設定視窗：左邊分頁（錄影 / 聲音 / 儲存位置 / 快速鍵 / 進階），右邊表單（標籤固定寬度，控制項對齊）。
//! 主畫面下方只留摘要，點一下開到對應的分頁。

use super::settings::{FPS_CHOICES, MAX_PRESETS, UI_SCALES};
use super::theme::{self, segmented, switch, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::format::{human_duration, output_size};
use screenrecorder_core::types::{EncoderPreference, Hotkey, Hotkeys, MethodPreference, CAMERA_SIZES, HOTKEY_NAMES};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Record,
    Audio,
    Save,
    Keys,
    Advanced,
}

const PAGES: [(Page, &str, Icon); 5] =
    [(Page::Record, "錄影", Icon::Camera), (Page::Audio, "聲音", Icon::Speaker), (Page::Save, "儲存位置", Icon::Folder), (Page::Keys, "快速鍵", Icon::List), (Page::Advanced, "進階", Icon::Settings)];

/// 設定視窗的狀態（輸入中的文字、設定中的快速鍵）
pub struct SettingsDialog {
    pub page: Page,
    max_custom_open: bool,
    max_text: String,
    dir_text: Option<String>,
    /// 正在設定第幾個快速鍵（等使用者按下新的組合）
    key_capture: Option<usize>,
    key_msg: Option<String>,
    /// 找到的攝影機（None = 還沒找；找的時候先放空的）
    cameras: Option<Vec<String>>,
}

pub fn open(app: &mut UiApp, page: Page) {
    match &mut app.settings_dlg {
        Some(d) => d.page = page,
        None => app.settings_dlg = Some(SettingsDialog { page, max_custom_open: false, max_text: String::new(), dir_text: None, key_capture: None, key_msg: None, cameras: None }),
    }
}

/// 主畫面摘要：錄影（第一項一定是 FPS）
pub fn record_summary(app: &UiApp) -> Vec<String> {
    let s = &app.s;
    let mut v = vec![format!("{} FPS", s.fps)];
    if s.scale != 100 {
        v.push(format!("解析度 {}%", s.scale));
    }
    v.push(if s.draw_mouse { "含游標".into() } else { "不含游標".into() });
    match (s.show_clicks, s.show_keys) {
        (true, true) => v.push("顯示點擊與按鍵".into()),
        (true, false) => v.push("顯示點擊".into()),
        (false, true) => v.push("顯示按鍵".into()),
        _ => {}
    }
    if !s.camera.is_empty() {
        v.push("攝影機".into());
    }
    if s.countdown_sec != 3 {
        v.push(if s.countdown_sec > 0 { format!("倒數 {} 秒", s.countdown_sec) } else { "不倒數".into() });
    }
    if s.max_minutes > 0.0 {
        v.push(format!("最長 {}", human_duration(s.max_minutes * 60.0)));
    }
    v
}

/// 主畫面摘要：聲音
pub fn audio_summary(app: &UiApp) -> (String, Icon) {
    let label = match (app.s.audio_system, app.s.audio_mic) {
        (true, true) => "系統＋麥克風",
        (true, false) => "系統聲音",
        (false, true) => "麥克風",
        _ => "不錄聲音",
    };
    (label.to_string(), if app.s.audio_mic && !app.s.audio_system { Icon::Mic } else { Icon::Speaker })
}

const NAV_W: f32 = 148.0;
const BODY_W: f32 = 500.0;
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
            ui.label(RichText::new("設定").font(theme::font_bold(18.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip("關閉（Esc）").show(ui).clicked() {
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
            for (page, name, icon) in PAGES {
                if nav_item(ui, name, icon, d.page == page).clicked() && d.page != page {
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
                let locked = app.locked() && d.page != Page::Keys;
                if locked {
                    form_hint_full(ui, "錄影中無法變更這些設定，停止錄影後再調整。", p.warn);
                    ui.add_space(6.0);
                }
                ui.add_enabled_ui(!locked, |ui| match d.page {
                    Page::Record => record_page(app, &mut d, ui),
                    Page::Audio => audio_page(app, ui),
                    Page::Save => save_page(app, &mut d, ui),
                    Page::Keys => keys_page(app, &mut d, ui),
                    Page::Advanced => advanced_page(app, ui),
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
fn nav_item(ui: &mut Ui, text: &str, icon: Icon, selected: bool) -> egui::Response {
    let p = theme::pal(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
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
        ui.painter().rect_filled(Rect::from_min_size(pos2(r.left(), r.top() + 9.0), vec2(3.0, 16.0)), CornerRadius::same(2), p.accent);
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
        let (r, _) = ui.allocate_exact_size(vec2(FORM_LABEL_W, FORM_ROW_H), Sense::hover());
        ui.painter().text(pos2(r.left(), r.center().y), egui::Align2::LEFT_CENTER, label, theme::font(13.0), p.muted);
        ui.set_min_height(FORM_ROW_H);
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
fn camera_rows(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    if d.cameras.is_none() {
        d.cameras = Some(vec![]);
        let core = app.core.clone();
        app.spawn(async move { actions::list_cameras(&core).await }, |app, list| {
            if let Some(d) = &mut app.settings_dlg {
                d.cameras = Some(list);
            }
        });
    }
    let found = d.cameras.clone().unwrap_or_default();
    form_row(ui, "攝影機", |ui| {
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
                        "不使用".to_string()
                    } else if found.contains(n) {
                        n.clone()
                    } else {
                        format!("{n}（找不到）")
                    },
                    true,
                )
            })
            .collect();
        if form_combo(ui, "camera", FORM_CTRL_W + 60.0, &mut sel, &items) {
            app.s.camera = names[sel].clone();
            app.save_settings();
        }
        if Btn::icon_only(Icon::Refresh).ghost().small().tooltip("重新尋找攝影機").show(ui).clicked() {
            d.cameras = None;
        }
    });
    if app.s.camera.is_empty() {
        if found.is_empty() {
            form_hint(ui, if cfg!(windows) { "沒有找到攝影機。" } else { "攝影機只支援 Windows。" }, p.muted);
        }
        return;
    }
    form_row(ui, "攝影機位置", |ui| {
        // 錄影時拖曳過：多一個「上次拖曳的位置」；改選角落就不再用記住的位置
        const DRAGGED: u8 = u8::MAX;
        let mut c = if app.s.camera_pos.is_some() { DRAGGED } else { app.s.camera_corner };
        let mut items = vec![(0u8, "右下".to_string(), true), (1, "左下".into(), true), (2, "右上".into(), true), (3, "左上".into(), true)];
        if app.s.camera_pos.is_some() {
            items.push((DRAGGED, "上次拖曳的位置".into(), true));
        }
        if form_combo(ui, "cameraCorner", FORM_CTRL_W, &mut c, &items) && c != DRAGGED {
            app.s.camera_corner = c;
            app.s.camera_pos = None;
            app.save_settings();
        }
    });
    form_row(ui, "大小與形狀", |ui| {
        let mut sz = app.s.camera_size;
        let items: Vec<(u32, &str)> = CAMERA_SIZES.iter().copied().zip(["小", "中", "大"]).collect();
        if segmented(ui, &mut sz, &items, true) {
            app.s.camera_size = sz;
            app.save_settings();
        }
        if !CAMERA_SIZES.contains(&app.s.camera_size) {
            // 在小視窗上調過大小
            ui.label(RichText::new("自訂").font(theme::font(12.0)).color(p.muted));
        }
        let mut circle = app.s.camera_circle;
        if segmented(ui, &mut circle, &[(true, "圓形"), (false, "方形")], true) {
            app.s.camera_circle = circle;
            app.save_settings();
        }
    });
    form_hint(
        ui,
        "主畫面開著與錄影時，螢幕上會出現攝影機小視窗（左右翻轉，像照鏡子），看到的就是錄到的。拖曳中間移動、拖曳外圈或轉動滑鼠滾輪調整大小，都會記住。攝影機被其他程式使用時，錄影不含攝影機。",
        p.muted,
    );
}

fn record_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, "畫面", |_| {});
    form_row(ui, "每秒張數", |ui| {
        let mut fps = app.s.fps;
        let mut items: Vec<(f64, String, bool)> = FPS_CHOICES.iter().map(|f| (*f, format!("{f} FPS"), true)).collect();
        if !FPS_CHOICES.contains(&fps) {
            items.push((fps, format!("{fps} FPS"), true));
        }
        if form_combo(ui, "fps", FORM_CTRL_W, &mut fps, &items) {
            app.s.fps = fps;
            app.save_settings();
        }
    });
    form_hint(ui, "一般操作畫面 30 就很順；要錄影片或遊戲再用 60。", p.muted);
    form_row(ui, "解析度", |ui| {
        let mut sc = app.s.scale;
        let items: Vec<(u32, String, bool)> = [(100, "100%（原尺寸）"), (75, "75%"), (50, "50%"), (25, "25%")].iter().map(|(v, t)| (*v, t.to_string(), true)).collect();
        if form_combo(ui, "scale", FORM_CTRL_W, &mut sc, &items) {
            app.s.scale = sc;
            app.save_settings();
        }
    });
    if let Some(r) = app.s.source_rect(&app.env) {
        let (w, h) = output_size(r.width, r.height, app.s.scale as f64);
        let big = (w as f64) * (h as f64) > 3840.0 * 2160.0 * 1.05;
        form_hint(ui, format!("目前範圍 {}×{}，輸出 {w}×{h}{}", r.width, r.height, if big { "（很大，建議 50%）" } else { "" }), if big { p.warn } else { p.muted });
    }
    form_row(ui, "滑鼠游標", |ui| {
        let mut dm = app.s.draw_mouse;
        if switch(ui, &mut dm, "錄進影片", true).changed() {
            app.s.draw_mouse = dm;
            app.save_settings();
        }
    });
    form_row(ui, "畫面效果", |ui| {
        ui.vertical(|ui| {
            let mut c = app.s.show_clicks;
            if switch(ui, &mut c, "顯示滑鼠點擊", true).on_hover_text("按下滑鼠時在游標位置出現一圈波紋（左鍵黃色、右鍵藍色），錄進影片，看的人知道點了哪裡").changed()
            {
                app.s.show_clicks = c;
                app.save_settings();
            }
            let mut k = app.s.show_keys;
            if switch(ui, &mut k, "顯示按下的快速鍵", true)
                .on_hover_text("按下 Ctrl / Alt / Win 組合鍵或 Enter、Esc 等功能鍵時，在畫面下方顯示按了什麼（例如「Ctrl + C」）；一般打字不會顯示")
                .changed()
            {
                app.s.show_keys = k;
                app.save_settings();
            }
            let mut h = app.s.cursor_halo;
            if switch(ui, &mut h, "游標周圍加光暈", true).on_hover_text("游標周圍有一圈淡黃色的光暈，看影片的人比較容易跟上游標在哪裡").changed() {
                app.s.cursor_halo = h;
                app.save_settings();
            }
        });
    });
    if app.s.show_clicks || app.s.show_keys || app.s.cursor_halo {
        form_hint(ui, "波紋、按鍵與光暈會錄進影片；只支援 Windows。", p.muted);
    }
    form_row(ui, "桌面", |ui| {
        let mut v = app.s.hide_icons;
        if switch(ui, &mut v, "錄影時隱藏桌面圖示", true).on_hover_text("開始錄影時把桌面上的圖示藏起來，畫面比較乾淨；停止後自動還原").changed() {
            app.s.hide_icons = v;
            app.save_settings();
        }
    });
    camera_rows(app, d, ui);
    form_divider(ui);
    form_section(ui, "開始與結束", |_| {});
    form_row(ui, "開始前倒數", |ui| {
        let mut cd = app.s.countdown_sec;
        let items = [(0, "不倒數".to_string(), true), (3, "3 秒".into(), true), (5, "5 秒".into(), true), (10, "10 秒".into(), true)];
        if form_combo(ui, "countdown", FORM_CTRL_W, &mut cd, &items) {
            app.s.countdown_sec = cd;
            app.save_settings();
        }
    });
    form_row(ui, "最長錄影時間", |ui| {
        let custom = d.max_custom_open || !MAX_PRESETS.contains(&app.s.max_minutes);
        let mut sel: f64 = if custom { -1.0 } else { app.s.max_minutes };
        let items = [(0.0, "不限".to_string(), true), (30.0, "30 分鐘".into(), true), (60.0, "1 小時".into(), true), (120.0, "2 小時".into(), true), (-1.0, "自訂…".into(), true)];
        if form_combo(ui, "maxPreset", if custom { 120.0 } else { FORM_CTRL_W }, &mut sel, &items) {
            d.max_custom_open = sel < 0.0;
            if sel >= 0.0 {
                app.s.max_minutes = sel;
                app.save_settings();
            } else {
                d.max_text = if app.s.max_minutes > 0.0 { app.s.max_minutes.to_string() } else { String::new() };
            }
        }
        if custom {
            if !d.max_custom_open && d.max_text.is_empty() {
                d.max_text = app.s.max_minutes.to_string();
            }
            let r = ui.add(egui::TextEdit::singleline(&mut d.max_text).desired_width(64.0).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center).hint_text("分鐘"));
            ui.label(theme::muted(ui, "分鐘"));
            if r.changed() {
                let v: f64 = d.max_text.trim().parse().unwrap_or(0.0);
                app.s.max_minutes = if v.is_finite() && v > 0.0 { v.round().min(screenrecorder_core::format::MAX_MINUTES_MAX) } else { 0.0 };
                app.save_settings();
            }
        }
    });
    form_hint(ui, "時間到自動停止並儲存。", p.muted);
    form_row(ui, "操作視窗", |ui| {
        let mut hide = app.s.hide_ui;
        if ui.checkbox(&mut hide, "開始錄影時縮小").on_hover_text("視窗不在錄影範圍內時不縮小").changed() {
            app.s.hide_ui = hide;
            app.save_settings();
        }
    });
    form_divider(ui);
    form_section(ui, "截圖", |_| {});
    form_row(ui, "截圖後", |ui| {
        ui.vertical(|ui| {
            let mut pv = app.s.shot_preview;
            if switch(ui, &mut pv, "右下角顯示小縮圖", true).on_hover_text("截圖後在螢幕右下角出現小縮圖，可以直接編輯、複製、釘選或刪除；7 秒後自動消失").changed()
            {
                app.s.shot_preview = pv;
                app.save_settings();
            }
            let mut on = app.s.edit_after_shot;
            if switch(ui, &mut on, "直接開啟編輯", true).changed() {
                app.s.edit_after_shot = on;
                app.save_settings();
            }
        });
    });
    form_hint(ui, "也可以從系統匣「截圖 → 編輯上次截圖」開啟編輯。", theme::pal(ui).muted);
}

// ───────────── 聲音 ─────────────

fn audio_page(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let a = app.env.audio.clone();
    form_section(ui, "錄製聲音", |_| {});
    form_row(ui, "系統聲音", |ui| {
        let mut sys = app.s.audio_system;
        if switch(ui, &mut sys, "", true).changed() {
            app.s.audio_system = sys;
            app.save_settings();
        }
    });
    // 裝置名稱可能很長：一行顯示，放不下時結尾「…」（滑鼠移上去看完整名稱）
    ui.horizontal(|ui| {
        ui.add_space(FORM_LABEL_W + 6.0);
        let text = a.render.clone().map(|r| format!("播放裝置：{r}")).unwrap_or_else(|| "找不到播放裝置".into());
        ui.add(egui::Label::new(RichText::new(text).font(theme::font(12.0)).color(p.muted)).truncate());
    });
    form_row(ui, "麥克風", |ui| {
        let mut mic = app.s.audio_mic;
        if switch(ui, &mut mic, "", true).changed() {
            app.s.audio_mic = mic;
            app.save_settings();
        }
    });
    if !app.s.mic_id.is_empty() && !a.captures.iter().any(|d| d.id == app.s.mic_id) {
        app.s.mic_id.clear();
    }
    form_row(ui, "麥克風裝置", |ui| {
        let def = a.captures.iter().find(|d| d.is_default).map(|d| format!("預設（{}）", d.name)).unwrap_or_else(|| "預設麥克風".into());
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
    let hint = a.error.clone().unwrap_or_else(|| {
        if a.captures.is_empty() {
            "找不到麥克風。".into()
        } else {
            "聲音與畫面以同一個時鐘對齊。系統聲音會受 Windows 音量影響。".into()
        }
    });
    ui.add_space(6.0);
    form_hint_full(ui, hint, if a.error.is_some() { p.warn } else { p.muted });
}

// ───────────── 儲存位置 ─────────────

fn save_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, "儲存位置", |_| {});
    let text = d.dir_text.get_or_insert_with(|| app.s.output_dir.clone());
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center).hint_text(app.s.out_dir(&app.env)));
    if r.changed() {
        app.s.output_dir = text.clone();
        app.save_settings();
        app.main.dir_changed_at = Some(Instant::now());
        app.ctx.request_repaint_after(std::time::Duration::from_millis(620));
    }
    if !r.has_focus() {
        d.dir_text = None;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("留空時使用預設位置").font(theme::font(12.0)).color(p.muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new("在檔案總管開啟").icon(Icon::Folder).small().show(ui).clicked() {
                if let Err(e) = actions::open(actions::OpenAction::Folder, &app.s.out_dir(&app.env)) {
                    app.toast(e.message().to_string(), true);
                }
            }
        });
    });
    form_divider(ui);
    form_section(ui, "檔名", |_| {});
    form_row(ui, "錄影", |ui| {
        ui.label(RichText::new("Rec_年-月-日_時-分-秒.mp4").font(theme::mono(12.5)));
    });
    form_row(ui, "截圖", |ui| {
        ui.label(RichText::new("Shot_年-月-日_時-分-秒.png").font(theme::mono(12.5)));
    });
    form_hint(ui, "加速版、GIF、剪輯版存在同一個資料夾，檔名接在原檔後面。", p.muted);
}

// ───────────── 進階 ─────────────

fn advanced_page(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    form_section(ui, "介面", |_| {});
    form_row(ui, "介面大小", |ui| {
        let mut z = app.s.ui_scale;
        let items: Vec<(u32, String)> = UI_SCALES.iter().map(|v| (*v, format!("{v}%"))).collect();
        let refs: Vec<(u32, &str)> = items.iter().map(|(v, t)| (*v, t.as_str())).collect();
        if segmented(ui, &mut z, &refs, true) {
            app.s.ui_scale = z;
            app.save_settings();
        }
    });
    form_hint(ui, "字和按鈕放大一點比較好看清楚；螢幕小（例如 1366×768）時建議 100%。", p.muted);
    form_divider(ui);
    form_section(ui, "擷取與編碼", |_| {});
    form_row(ui, "擷取方式", |ui| {
        let mut method = app.s.method;
        let items = [
            (MethodPreference::Auto, "自動（建議）".to_string(), true),
            (MethodPreference::Ddagrab, "顯示卡擷取（ddagrab，較省 CPU）".into(), true),
            (MethodPreference::Gdigrab, "相容模式（gdigrab，哪台電腦都能用）".into(), true),
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
        None => "顯示卡編碼（偵測中…）".to_string(),
        Some(h) if !h.is_empty() => format!("顯示卡編碼（{}）", h.join("、")),
        _ => "顯示卡編碼（這台電腦沒有可用的）".into(),
    };
    form_row(ui, "編碼器", |ui| {
        let mut enc = app.s.encoder;
        let items = [
            (EncoderPreference::Auto, "自動（建議）".to_string(), true),
            (EncoderPreference::Cpu, "CPU 編碼（libx264，畫質最穩）".into(), true),
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
            Some(h) if h.iter().any(|n| n == screenrecorder_core::args::AUTO_PREFERRED_GPU) => "優先用 Intel 顯示卡（QSV），CPU 負擔最小；不能用時改用 CPU。".into(),
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
        form_hint(ui, hint, p.muted);
    }
    if app.s.encoder == EncoderPreference::Auto && ff.prefer_gpu == Some(true) && hw.as_ref().is_some_and(|h| !h.is_empty()) {
        let mut reset = false;
        form_row(ui, "", |ui| reset = Btn::new("重設：恢復平常用 CPU 編碼").small().show(ui).clicked());
        if reset {
            app.core.reset_learned_gpu();
            app.env.ffmpeg.prefer_gpu = Some(false);
            app.toast("已重設，之後的錄影平常會用 CPU 編碼", false);
        }
    }
    form_divider(ui);
    form_section(ui, "新版本", |_| {});
    form_row(ui, "自動檢查", |ui| {
        let mut on = app.update.enabled;
        if ui.checkbox(&mut on, "啟動後與每 12 小時檢查").changed() {
            app.core.set_check_updates(on);
            app.update = actions::update_state(&app.core);
        }
    });
    form_row(ui, "", |ui| {
        if Btn::new("立即檢查").small().show(ui).clicked() {
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
        ui.label(theme::muted(ui, format!("目前版本 v{}", app.env.app_version)));
    });
    if let Some(e) = &app.update.error {
        form_hint(ui, format!("無法檢查新版本：{}", e.trim_start_matches("無法檢查新版本：")), p.warn);
    }
}

// ───────────── 快速鍵（點按鍵後按下新的組合，Esc 取消；可停用或還原預設） ─────────────

fn keys_page(app: &mut UiApp, d: &mut SettingsDialog, ui: &mut Ui) {
    let p = theme::pal(ui);
    let Some(hk) = app.env.hotkeys else {
        form_section(ui, "全域快速鍵", |_| {});
        form_hint_full(ui, "全域快速鍵需要系統匣常駐時才能使用。", p.muted);
        return;
    };
    let ok = hk.all();
    form_section(ui, "全域快速鍵", |ui| {
        if app.keys != Hotkeys::default() && Btn::new("還原預設").ghost().small().show(ui).clicked() {
            d.key_capture = None;
            save_keys(app, d, Hotkeys::default());
        }
    });
    form_hint_full(ui, "程式在背景時也能用。點一下按鍵，再按下新的組合（要包含 Ctrl 或 Alt）。", p.muted);
    ui.add_space(6.0);
    for i in 0..ok.len() {
        form_row(ui, HOTKEY_NAMES[i], |ui| {
            let capturing = d.key_capture == Some(i);
            let label = app.keys.label(i);
            let text = if capturing {
                "請按下組合鍵…".to_string()
            } else if label.is_empty() {
                "停用".to_string()
            } else if !ok[i] {
                format!("{label}・被占用")
            } else {
                label.clone()
            };
            let tip = if capturing {
                "按 Esc 取消"
            } else if !ok[i] && !label.is_empty() {
                "已被其他程式使用，請點一下換一組"
            } else {
                "點一下後按下新的組合鍵（要包含 Ctrl 或 Alt）"
            };
            if Btn::new(text).small().height(30.0).selected(capturing).min_width(FORM_CTRL_W).tooltip(tip).show(ui).clicked() {
                if capturing {
                    stop_capture(app, d);
                } else {
                    // 先暫停全部快速鍵，才按得到原本已登記的組合
                    d.key_capture = Some(i);
                    d.key_msg = None;
                    app.core.apply_hotkeys(&Hotkeys::none());
                }
            }
            if !label.is_empty() && !capturing && Btn::icon_only(Icon::Close).ghost().small().tooltip("停用這個快速鍵").show(ui).clicked() {
                let mut k = app.keys;
                k.set(i, None);
                save_keys(app, d, k);
            }
        });
    }
    if let Some(m) = d.key_msg.clone() {
        form_hint(ui, m, p.warn);
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
        d.key_msg = Some(format!("不支援「{}」，請用 A–Z、0–9、F1–F12 等按鍵", key.name()));
        return;
    };
    let k = Hotkey { ctrl: m.ctrl, alt: m.alt, shift: m.shift, win: false, key: vk };
    if !k.valid() {
        d.key_msg = Some(format!("{} 會擋到一般打字，請加上 Ctrl 或 Alt", k.label()));
        return;
    }
    if let Some(j) = app.keys.conflict(i, &k) {
        d.key_msg = Some(format!("{} 已經用在「{}」", k.label(), HOTKEY_NAMES[j]));
        return;
    }
    let mut next = app.keys;
    next.set(i, Some(k));
    d.key_capture = None;
    save_keys(app, d, next);
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
        if let Some(i) = (0..ok.len()).find(|i| !ok[*i]) {
            d.key_msg = Some(format!("{} 已被其他程式使用，請換一組", k.label(i)));
        }
    }
}

/// egui 的按鍵 → Windows 虛擬鍵碼（只限支援的按鍵）
fn vk_of(key: egui::Key) -> Option<u32> {
    let name = key.name();
    (0x20..=0x7B).find(|vk| screenrecorder_core::types::key_name(*vk).as_deref() == Some(name))
}
