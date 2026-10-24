//! 自己畫的標題列（操作視窗不用 Windows 的標題列）：左邊程式圖示與名稱；
//! 右邊「⋯」總選單（檔案庫、工具、設定、說明、結束）、縮小成小視窗，再來是縮到最小、最大化、關閉。
//! 拖曳空白的地方移動視窗、按兩下最大化；視窗四邊與四個角可以拖曳調整大小。
//! 小視窗：只留開始 / 停止錄影、計時、暫停、截圖，不擋畫面；不會被錄進影片，錄影中浮在最上層

use super::library_dialog::{Kind, LibraryDialog};
use super::main_view::{self, ShotAct};
use super::settings::UI_MIN_SIZE;
use super::settings_dialog::{self, Page};
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, Color32, Id, Layout, Order, PointerButton, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, ViewportCommand};
use screenrecorder_core::format::clock;
use screenrecorder_core::types::RecorderState;
use screenrecorder_core::{tr, trf};

/// 標題列的高度（點）
pub const TITLE_H: f32 = 36.0;
/// 視窗邊緣多寬的範圍可以拖曳調整大小；角落沿著邊多長的範圍算角落
const EDGE: f32 = 5.0;
const CORNER: f32 = 14.0;
/// 縮到最小、最大化、關閉按鈕的寬度
const CTRL_W: f32 = 46.0;
/// 總選單的寬度
const MENU_W: f32 = 300.0;
/// 小視窗內容的大小（不含標題列）
const COMPACT: Vec2 = vec2(460.0, 64.0);

/// 小視窗：切換前的視窗大小（內容，點）與是不是最大化，回到完整視窗時還原
#[derive(Clone, Copy)]
pub struct Compact {
    size: Vec2,
    max: bool,
    /// 錄影中浮在最上層（目前設定的狀態，有變化才送給視窗）
    top: bool,
}

/// 全螢幕（檢視器）時不畫標題列
fn hidden(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen == Some(true))
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized == Some(true))
}

/// 內容（主畫面、對話框）要讓出來的上方空間：對話框、檔案庫都排在標題列下面
pub fn insets(ctx: &egui::Context) -> egui::SafeAreaInsets {
    let top = if hidden(ctx) { 0.0 } else { TITLE_H };
    egui::SafeAreaInsets(egui::epaint::MarginF32 { top, ..Default::default() })
}

/// 開著剪輯、檢視器、檔案庫、設定、對話框時不能切成小視窗（放不下）；已經是小視窗時自動回到完整視窗
fn needs_full(app: &UiApp) -> bool {
    app.editor.is_some()
        || app.viewer.is_some()
        || app.library.is_some()
        || app.settings_dlg.is_some()
        || app.export_dlg.is_some()
        || app.ask.is_some()
        || app.whats_new_open
        || app.changelog_open
        || app.about_open
        || app.jf_dupes.is_some()
        || app.video_tags.is_some()
        || app.jf_edit.is_some()
        || app.ocr.is_some()
        || app.steps_review.is_some()
        || app.send_dlg.is_some()
        || app.schedule_dlg.is_some()
        || app.tour.is_some()
}

/// 切成小視窗
pub fn enter_compact(app: &mut UiApp, ctx: &egui::Context) {
    let size = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size())).unwrap_or(vec2(super::settings::WINDOW_SIZE.0, super::settings::WINDOW_SIZE.1));
    let max = maximized(ctx);
    if max {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
    }
    let want = COMPACT + vec2(0.0, TITLE_H);
    ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(want));
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(want));
    // 小視窗不被錄到：開始錄影時不用縮小，錄影中浮在最上層
    screenrecorder_core::winui::hide_main_from_capture(true);
    app.compact = Some(Compact { size, max, top: false });
}

/// 回到完整視窗（原本的大小，原本最大化就再最大化）
pub fn leave_compact(app: &mut UiApp, ctx: &egui::Context) {
    let Some(c) = app.compact.take() else { return };
    screenrecorder_core::winui::hide_main_from_capture(false);
    if c.top {
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
    }
    ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(UI_MIN_SIZE.0, UI_MIN_SIZE.1)));
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(c.size));
    if c.max {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
    }
}

/// 每一畫格：小視窗時開了需要完整視窗的畫面（例如截圖後直接編輯、從系統匣開設定）就回到完整視窗
/// 小視窗錄影中（含倒數、暫停）浮在最上層，停止後恢復一般視窗
pub fn tick(app: &mut UiApp, ctx: &egui::Context) {
    // Ctrl+,：開啟設定（總選單裡「設定」旁邊寫的快速鍵）
    if app.settings_dlg.is_none() && app.ask.is_none() && app.snip.is_none() && ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Comma))) {
        settings_dialog::open(app, Page::General);
    }
    if app.compact.is_some() && needs_full(app) {
        leave_compact(app, ctx);
    }
    let recording = app.status.recorder.state != RecorderState::Idle;
    if let Some(c) = app.compact.as_mut().filter(|c| c.top != recording) {
        c.top = recording;
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if recording { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }));
    }
}

/// 標題列左邊的程式圖示（第一次用時做成材質）
fn app_icon(ctx: &egui::Context) -> egui::TextureHandle {
    let id = Id::new("title-bar-icon");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t;
    }
    let n = 40;
    let rgba = screenrecorder_core::icon::render_icon(n, screenrecorder_core::icon::IconState::Idle);
    let t = ctx.load_texture("title-bar-icon", egui::ColorImage::from_rgba_unmultiplied([n, n], &rgba), egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// 縮到最小、最大化、關閉：整個標題列高、沒有圓角（和 Windows 一樣）；關閉滑鼠移上去變紅色
fn ctrl_button(ui: &mut Ui, icon: Icon, tip: &str, close: bool) -> Response {
    let p = theme::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(CTRL_W, TITLE_H), Sense::click());
    let hover = resp.hovered();
    let down = resp.is_pointer_button_down_on();
    let (bg, fg) = match (close, hover) {
        (true, true) => (if down { Color32::from_rgb(0xf1, 0x70, 0x7a) } else { Color32::from_rgb(0xe8, 0x11, 0x23) }, Color32::WHITE),
        (false, true) => (if down { p.border } else { p.surface2 }, p.text),
        _ => (Color32::TRANSPARENT, p.text),
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    theme::paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(14.0, 14.0)), icon, fg);
    resp.on_hover_text(tip)
}

/// 畫標題列（在所有畫面最上面；對話框開著時也能移動、縮小、關閉視窗）
pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    if hidden(ctx) {
        return;
    }
    let full = ctx.viewport_rect();
    let bar = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
    let max = maximized(ctx);
    let compact = app.compact.is_some();
    // 開著剪輯、檢視器、確認對話框時不能開檔案庫、切小視窗（會疊在一起）
    let covered = app.editor.is_some() || app.viewer.is_some() || app.export_dlg.is_some() || app.snip.is_some() || app.ask.is_some();
    egui::Area::new(Id::new("title-bar")).order(Order::Debug).fixed_pos(bar.min).constrain(false).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_clip_rect(bar);
        ui.painter().rect_filled(bar, 0.0, p.surface);
        ui.painter().hline(bar.x_range(), bar.max.y - 0.5, Stroke::new(1.0, p.border));
        // 空白的地方：拖曳移動視窗、按兩下最大化 / 還原（先登記，上面的按鈕優先）
        let drag = ui.interact(bar, Id::new("title-bar-drag"), Sense::click_and_drag());
        if drag.double_clicked() && !compact {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
        } else if drag.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        // 右邊：關閉、最大化、縮到最小，再來是縮小成小視窗
        let right_w = {
            let rui = &mut ui.new_child(UiBuilder::new().max_rect(bar).layout(Layout::right_to_left(Align::Center)));
            rui.spacing_mut().item_spacing.x = 0.0;
            if ctrl_button(rui, Icon::Close, tr!("關閉", "Close"), true).clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            if !compact {
                let (icon, tip) = if max { (Icon::Restore, tr!("還原", "Restore")) } else { (Icon::Maximize, tr!("最大化", "Maximize")) };
                if ctrl_button(rui, icon, tip, false).clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
                }
            }
            if ctrl_button(rui, Icon::Minimize, tr!("縮到最小", "Minimize"), false).clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            rui.add_space(10.0);
            let tip = if compact {
                tr!("回到完整視窗", "Back to the full window")
            } else {
                tr!("縮小成小視窗：只留開始 / 停止錄影、計時、截圖，不擋畫面", "Shrink to a small window with just record / stop, the timer and screenshots")
            };
            if Btn::icon_only(Icon::Compact).ghost().small().selected(compact).enabled(compact || !covered && !needs_full(app)).tooltip(tip).show(rui).clicked() {
                if compact {
                    leave_compact(app, ctx);
                } else {
                    enter_compact(app, ctx);
                }
            }
            rui.add_space(2.0);
            let tip = match &app.env.update {
                Some(u) => trf!("選單：有新版本 v{}", "Menu: v{} is available", u.version),
                None => tr!("選單：檔案庫、工具、設定、說明", "Menu: library, tools, settings, help").to_string(),
            };
            let menu = Btn::icon_only(Icon::More).ghost().small().tooltip(tip).show(rui);
            // 有新版本：選單按鈕右上角一個紅點
            if app.env.update.is_some() {
                rui.painter().circle(menu.rect.right_top() + vec2(-6.0, 6.0), 4.0, p.rec, Stroke::new(1.5, p.surface));
            }
            super::tour::mark(rui, super::tour::Spot::Settings, menu.rect);
            egui::Popup::menu(&menu).align(egui::RectAlign::BOTTOM_END).show(|ui| main_menu(app, ui, covered));
            bar.max.x - rui.min_rect().min.x
        };
        // 左邊：圖示、程式名稱（名稱只是文字，拖曳它一樣可以移動視窗；寬度不夠時省略）
        let left = Rect::from_min_max(bar.min + vec2(12.0, 0.0), egui::pos2(bar.max.x - right_w - 8.0, bar.max.y));
        let lui = &mut ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)));
        lui.spacing_mut().item_spacing.x = 8.0;
        let tex = app_icon(ctx);
        lui.add(egui::Image::new(&tex).fit_to_exact_size(vec2(18.0, 18.0)));
        let name = tr!("螢幕錄影", "Screen Recorder");
        let room = lui.available_width();
        let galley = lui.painter().layout_no_wrap(name.to_string(), theme::font(13.0), p.text);
        if galley.size().x <= room {
            let at = egui::pos2(lui.cursor().min.x, bar.center().y - galley.size().y / 2.0);
            lui.painter().galley(at, galley, p.text);
        }
    });
}

/// 總選單（標題列右邊的「⋯」）：一層攤平、用分隔線分組；每一項左邊是圖示，右邊是快速鍵。
/// 回溯錄影、設定組合本來就有自己的選項，滑過去才展開。covered：開著剪輯、檢視器等視窗時不能開檔案庫和工具
fn main_menu(app: &mut UiApp, ui: &mut Ui, covered: bool) {
    ui.set_width(MENU_W);
    ui.spacing_mut().item_spacing.y = 2.0;
    let idle = app.status.recorder.state == RecorderState::Idle;
    let why = tr!("先關掉目前開著的視窗", "Close the open window first");
    // 檔案庫
    if menu_item(ui, Icon::Video, tr!("錄影", "Recordings"), None, false, !covered).on_disabled_hover_text(why).clicked() {
        app.library = Some(LibraryDialog::new(Kind::Video));
    }
    if menu_item(ui, Icon::Camera, tr!("截圖", "Screenshots"), None, false, !covered).on_disabled_hover_text(why).clicked() {
        app.library = Some(LibraryDialog::new(Kind::Shot));
    }
    if app.jellyfin().is_some() && menu_item(ui, Icon::Play, "Jellyfin", None, false, !covered).on_disabled_hover_text(why).clicked() {
        super::library_dialog::open_jellyfin(app);
    }
    ui.separator();
    // 工具（錄影中只有截圖工具、講稿小視窗）
    let shoot = super::main_view::can_shoot(app) && !covered;
    let r = menu_item(ui, Icon::Camera, tr!("截圖工具", "Screenshot tools"), None, true, shoot);
    if shoot {
        egui::containers::menu::SubMenu::new().show(ui, &r, |ui| super::main_view::shot_menu_body(app, ui));
    }
    if idle {
        let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && app.manual_export().is_none();
        let r = menu_item(ui, Icon::Undo, tr!("回溯錄影（錄最近幾分鐘）", "Replay buffer (last few minutes)"), None, true, can && !covered);
        if can && !covered {
            egui::containers::menu::SubMenu::new().show(ui, &r, |ui| super::replay::menu_body(app, ui));
        }
        let tip = tr!("指定時間自動開始錄影，也可以設定錄幾分鐘後自動停止", "Start recording automatically at a set time, optionally stopping after a set length");
        if menu_item(ui, Icon::Clock, tr!("排程錄影…", "Schedule recording…"), None, false, !covered).on_hover_text(tip).clicked() {
            super::schedule::open(app);
        }
    }
    let notes = if app.notes.is_some() { tr!("關閉講稿小視窗", "Close script window") } else { tr!("講稿小視窗", "Script window") };
    let tip = tr!(
        "浮在最上層、不會被錄進影片；可以用大字自動往上捲（提詞），開始錄影時自動開始",
        "Stays on top and isn't recorded. It can show your script in large text that scrolls up automatically (teleprompter), starting when you record."
    );
    if menu_item(ui, Icon::Edit, notes, None, false, true).on_hover_text(tip).clicked() {
        super::notes::toggle(app);
    }
    if idle {
        let r = menu_item(ui, Icon::Copy, tr!("設定組合", "Presets"), None, true, !app.locked());
        if !app.locked() {
            egui::containers::menu::SubMenu::new().show(ui, &r, |ui| super::presets::menu_body(app, ui));
        }
    }
    ui.separator();
    if menu_item(ui, Icon::Settings, tr!("設定", "Settings"), Some("Ctrl+,"), false, app.ask.is_none()).clicked() {
        settings_dialog::open(app, Page::General);
    }
    ui.separator();
    // 說明
    if menu_item(ui, Icon::Star, tr!("新功能介紹", "What's new tour"), None, false, true).clicked() {
        app.whats_new_open = true;
    }
    if menu_item(ui, Icon::List, tr!("更新說明", "Release notes"), None, false, true).clicked() {
        app.changelog_open = true;
    }
    // 有新版本時這一項換成「更新到 vX」（藍字、右邊一個紅點），更新中寫進度
    match app.env.update.clone() {
        Some(u) => {
            let busy = super::main_view::update_progress(app);
            let label =
                busy.clone().unwrap_or_else(|| if super::main_view::can_self_update(&u) { trf!("更新到 v{}", "Update to v{}", u.version) } else { trf!("下載 v{}", "Download v{}", u.version) });
            let r = if busy.is_none() { update_item(ui, &label) } else { menu_item(ui, Icon::Refresh, &label, None, false, false) };
            if r.clicked() {
                super::main_view::ask_update(app, &u);
            }
        }
        None => {
            if menu_item(ui, Icon::Refresh, tr!("檢查更新", "Check for updates"), None, false, true).clicked() {
                settings_dialog::check_update_now(app);
            }
        }
    }
    if menu_item(ui, Icon::Eye, tr!("重新看教學", "Replay tutorial"), None, false, true).clicked() {
        super::tour::restart(app);
    }
    if menu_item(ui, Icon::Info, tr!("關於", "About"), None, false, true).clicked() {
        app.about_open = true;
    }
    ui.separator();
    let quit = menu_item(ui, Icon::Close, tr!("結束程式", "Quit"), None, false, true);
    if quit.on_hover_text(tr!("完全結束螢幕錄影（關掉視窗只是收到系統匣）", "Quit Screen Recorder completely (closing the window only hides it in the tray)")).clicked() {
        app.request_quit();
    }
}

/// 總選單的「更新到 vX」：淡藍底、藍色粗字，右邊一個紅點（和選單按鈕上的紅點對應）
fn update_item(ui: &mut Ui, label: &str) -> Response {
    let p = theme::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(MENU_W, 38.0), Sense::click());
    let bg = if resp.hovered() { p.accent.gamma_multiply(0.18) } else { p.accent.gamma_multiply(0.1) };
    ui.painter().rect_filled(rect, egui::CornerRadius::same(6), bg);
    theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(rect.min.x + 20.0, rect.center().y), vec2(17.0, 17.0)), Icon::Refresh, p.accent);
    ui.painter().text(egui::pos2(rect.min.x + 44.0, rect.center().y), egui::Align2::LEFT_CENTER, label, theme::font_bold(14.0), p.accent);
    ui.painter().circle_filled(egui::pos2(rect.max.x - 16.0, rect.center().y), 4.0, p.rec);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 總選單的一項：左邊圖示、文字，右邊框起來的快速鍵或子選單的箭頭；整列都可以點（有子選單的只要滑過去）
fn menu_item(ui: &mut Ui, icon: Icon, label: &str, key: Option<&str>, sub: bool, enabled: bool) -> Response {
    let p = theme::pal(ui);
    let sense = if enabled && !sub { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(MENU_W, 38.0), sense);
    let resp = if enabled { resp } else { resp.with_new_rect(rect) };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(6), p.surface2);
    }
    let fg = if enabled { p.text } else { p.muted.gamma_multiply(0.7) };
    theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(rect.min.x + 20.0, rect.center().y), vec2(17.0, 17.0)), icon, fg);
    ui.painter().text(egui::pos2(rect.min.x + 44.0, rect.center().y), egui::Align2::LEFT_CENTER, label, theme::font(14.0), fg);
    if let Some(k) = key {
        let g = ui.painter().layout_no_wrap(k.to_string(), theme::font(12.0), p.muted);
        let kr = Rect::from_min_size(egui::pos2(rect.max.x - 8.0 - g.size().x - 14.0, rect.center().y - 11.0), vec2(g.size().x + 14.0, 22.0));
        ui.painter().rect_stroke(kr, egui::CornerRadius::same(4), Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        ui.painter().galley(kr.center() - g.size() / 2.0, g, p.muted);
    }
    if sub {
        theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(rect.max.x - 16.0, rect.center().y), vec2(12.0, 12.0)), Icon::ChevR, fg);
    }
    if enabled && !sub {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// 視窗四邊、四個角：游標變成調整大小的箭頭，按下去拖曳調整大小（最大化、全螢幕時不行）。
/// 在所有畫面畫完之後呼叫，游標形狀才不會被底下的按鈕蓋掉
pub fn resize_edges(ctx: &egui::Context) {
    if hidden(ctx) || maximized(ctx) {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let r = ctx.viewport_rect();
    let (dl, dr, dt, db) = (pos.x - r.min.x, r.max.x - pos.x, pos.y - r.min.y, r.max.y - pos.y);
    use egui::viewport::ResizeDirection as D;
    let dir = if dt < EDGE && dl < CORNER || dl < EDGE && dt < CORNER {
        D::NorthWest
    } else if dt < EDGE && dr < CORNER || dr < EDGE && dt < CORNER {
        D::NorthEast
    } else if db < EDGE && dl < CORNER || dl < EDGE && db < CORNER {
        D::SouthWest
    } else if db < EDGE && dr < CORNER || dr < EDGE && db < CORNER {
        D::SouthEast
    } else if dt < EDGE {
        D::North
    } else if db < EDGE {
        D::South
    } else if dl < EDGE {
        D::West
    } else if dr < EDGE {
        D::East
    } else {
        return;
    };
    ctx.set_cursor_icon(match dir {
        D::North | D::South => egui::CursorIcon::ResizeVertical,
        D::West | D::East => egui::CursorIcon::ResizeHorizontal,
        D::NorthWest | D::SouthEast => egui::CursorIcon::ResizeNwSe,
        D::NorthEast | D::SouthWest => egui::CursorIcon::ResizeNeSw,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

/// 小視窗的內容：沒在錄影時「開始錄影」「框選截圖」；錄影中計時、暫停 / 繼續、停止
pub fn compact_view(app: &mut UiApp, ui: &mut Ui) {
    let p = theme::pal(ui);
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, p.bg);
    let inner = full.shrink2(vec2(12.0, 0.0));
    let r = app.status.recorder.clone();
    ui.scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        match r.state {
            RecorderState::Idle => {
                let why = main_view::start_blocker(app);
                let label = if app.s.source_type == super::settings::SourceType::Audio { tr!("錄音", "Record audio") } else { tr!("錄影", "Record") };
                let w = (ui.available_width() * 0.56).floor();
                let rec = Btn::new(label).kind(theme::Kind::Record).enabled(why.is_none()).min_width(w).show(ui);
                if let Some(why) = why {
                    rec.on_disabled_hover_text(why);
                } else if rec.clicked() {
                    main_view::start_recording(app);
                }
                if app.s.source_type != super::settings::SourceType::Audio
                    && Btn::new(tr!("框選截圖", "Select area")).icon(Icon::Camera).height(40.0).enabled(app.env.ffmpeg.found && !app.main.shooting).min_width(ui.available_width()).show(ui).clicked()
                {
                    main_view::run_shot(app, ShotAct::Select(0));
                }
            }
            RecorderState::Countdown => {
                ui.label(RichText::new(tr!("準備開始…", "Getting ready…")).font(theme::font_bold(15.0)).color(p.rec));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::new(tr!("取消", "Cancel")).icon(Icon::Stop).danger().height(40.0).show(ui).clicked() {
                        stop(app);
                    }
                });
            }
            _ => {
                // 錄影中 / 暫停 / 停止中：紅點、計時，右邊暫停 / 繼續、停止（回溯錄影：存下最近幾分鐘、停止）
                let rec = r.state == RecorderState::Recording;
                let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 6.0, if rec { p.rec } else { p.muted });
                let ms = main_view::live_recorded_ms(app);
                ui.label(RichText::new(clock(ms)).font(theme::mono(20.0)).color(if rec { p.rec } else { p.text }));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(rp) = r.replay.clone() {
                        if Btn::new(tr!("停止", "Stop")).icon(Icon::Stop).danger().height(40.0).enabled(r.state != RecorderState::Stopping && !rp.saving).show(ui).clicked() {
                            super::replay::ask_stop(app, &rp, ms);
                        }
                        let label = trf!("存下最近 {}", "Save the last {}", screenrecorder_core::replay::length_label(rp.sec));
                        if Btn::new(label).icon(Icon::Export).primary().height(40.0).enabled(!rp.saving && rec && r.busy.is_none()).show(ui).clicked() {
                            super::replay::save(app);
                        }
                        return;
                    }
                    if Btn::new(tr!("停止", "Stop")).icon(Icon::Stop).danger().height(40.0).enabled(r.state != RecorderState::Stopping).show(ui).clicked() {
                        stop(app);
                    }
                    let core = app.core.clone();
                    if rec && Btn::new(tr!("暫停", "Pause")).icon(Icon::Pause).height(40.0).enabled(r.busy.is_none()).show(ui).clicked() {
                        app.guarded(async move { core.recorder.pause().await }, |_, _| {});
                    } else if r.state == RecorderState::Paused && Btn::new(tr!("繼續", "Resume")).icon(Icon::Play).height(40.0).enabled(r.busy.is_none()).show(ui).clicked() {
                        app.guarded(async move { core.recorder.resume().await }, |_, _| {});
                    }
                });
            }
        }
    });
}

fn stop(app: &mut UiApp) {
    let core = app.core.clone();
    app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
}
