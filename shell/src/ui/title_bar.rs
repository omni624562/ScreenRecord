//! 自己畫的標題列（操作視窗不用 Windows 的標題列）：左邊「螢幕錄影 ⌄」選單（關於、檢查更新、更新說明），
//! 右邊更多工具（⋯）、檔案庫、設定、縮小成小視窗，再來是縮到最小、最大化、關閉。
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
        // 右邊：關閉、最大化、縮到最小，再來是檔案庫、設定、縮小成小視窗
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
            rui.spacing_mut().item_spacing.x = 2.0;
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
            if !compact {
                if Btn::icon_only(Icon::Settings).ghost().small().selected(app.settings_dlg.is_some()).enabled(app.ask.is_none()).tooltip(tr!("設定", "Settings")).show(rui).clicked() {
                    settings_dialog::open(app, Page::General);
                }
                let lib = Btn::icon_only(Icon::Library)
                    .ghost()
                    .small()
                    .selected(app.library.is_some())
                    .enabled(!covered)
                    .tooltip(tr!("檔案庫：錄影、截圖（、Jellyfin）", "Library: recordings, screenshots (and Jellyfin)"));
                if lib.show(rui).clicked() && app.library.is_none() {
                    app.library = Some(LibraryDialog::new(Kind::Video));
                }
                // 更多工具：回溯錄影、排程錄影、講稿小視窗、瀏覽 Jellyfin、設定組合
                let more = Btn::icon_only(Icon::More)
                    .ghost()
                    .small()
                    .enabled(!covered)
                    .tooltip(tr!("更多工具：回溯錄影、排程錄影、講稿小視窗、設定組合", "More tools: replay buffer, scheduled recording, script window, presets"))
                    .show(rui);
                main_view::more_tools(app, &more);
            }
            bar.max.x - rui.min_rect().min.x
        };
        // 左邊：圖示與「螢幕錄影 ⌄」選單（寬度不夠時省略文字）
        let left = Rect::from_min_max(bar.min + vec2(10.0, 0.0), egui::pos2(bar.max.x - right_w - 8.0, bar.max.y));
        let lui = &mut ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)));
        lui.spacing_mut().item_spacing.x = 4.0;
        let tex = app_icon(ctx);
        lui.add(egui::Image::new(&tex).fit_to_exact_size(vec2(18.0, 18.0)));
        let menu = Btn::new(tr!("螢幕錄影", "Screen Recorder")).ghost().small().menu().show(lui);
        egui::Popup::menu(&menu).show(|ui| {
            ui.set_min_width(170.0);
            if ui.button(tr!("關於", "About")).clicked() {
                settings_dialog::open(app, Page::About);
            }
            if ui.button(tr!("檢查更新", "Check for updates")).clicked() {
                settings_dialog::check_update_now(app);
            }
            if ui.button(tr!("更新說明", "Release notes")).clicked() {
                app.changelog_open = true;
            }
        });
    });
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
                let can = main_view::can_start(app);
                let label = if app.s.source_type == super::settings::SourceType::Audio { tr!("開始錄音", "Start audio recording") } else { tr!("開始錄影", "Start recording") };
                let w = (ui.available_width() * 0.56).floor();
                if Btn::new(label).kind(theme::Kind::Record).enabled(can).min_width(w).show(ui).clicked() {
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
