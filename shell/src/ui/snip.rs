//! 在螢幕上框選（Windows 以外的平台；Windows 用 core 的原生框選畫面 snip_win.rs）：
//! 先截下整個桌面（凍結），每個螢幕開一個全螢幕視窗顯示變暗的畫面，拖曳框選範圍，或點一下選游標下的視窗；
//! Esc 或右鍵取消。從凍結的畫面裁切，所以截到的就是框選時看到的樣子。
//!
//! 放開滑鼠後範圍留著：拖曳邊、角調整或整個移動，下方的工具列選要做什麼（完成、編輯、複製、釘在桌面、取消）；
//! Enter 完成（還沒框選時是整個螢幕）、在範圍裡點兩下也是完成。設定「放開滑鼠就直接存檔」時放開就截。
//!
//! 也可以只選一個範圍、不存檔（例如選錄影範圍）：pick_region。

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, CursorIcon, Key, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, ViewportBuilder, ViewportCommand, ViewportId};
use screenrecorder_core::app::SnipSource;
use screenrecorder_core::snip_tools::{self, Grab, SnipAction, Tool};
use screenrecorder_core::types::{MonitorInfo, Rect as DRect};
use screenrecorder_core::{tr, trf};

/// 小於這個大小（實體像素）的拖曳當成點一下
const MIN_DRAG: i32 = 4;

/// 只選範圍時，選好（或取消 = None）要做的事
pub type OnRegion = Box<dyn FnOnce(&mut UiApp, Option<DRect>) + Send>;

struct Mon {
    info: MonitorInfo,
    /// winit 的螢幕編號（全螢幕開在這台）
    index: usize,
    tex: Option<TextureHandle>,
}

/// 拖曳中：從哪台螢幕開始、起點與目前的點（桌面座標）
type Drag = (usize, (i32, i32), (i32, i32));

pub struct Snip {
    src: SnipSource,
    mons: Vec<Mon>,
    drag: Option<Drag>,
    /// 框好、可以調整的範圍（桌面座標）與它在哪台螢幕
    sel: Option<(usize, DRect)>,
    /// 正在拖曳範圍的邊、角或整個範圍：(哪裡, 按下的點（桌面座標）, 原本的範圍)
    grab: Option<(Grab, (i32, i32), DRect)>,
    /// 放開滑鼠後範圍留著，可以調整並從工具列選要做什麼
    adjust: bool,
    /// 只選範圍（不存檔）：選好時呼叫
    region: Option<OnRegion>,
    /// 原本操作視窗是隱藏的（結束後再隱藏）
    was_hidden: bool,
}

enum Done {
    Cancel,
    Finish(DRect, SnipAction),
}

/// 收到框選截圖的要求（core 的 snip_offer）：取走凍結的畫面開始框選
pub fn start(app: &mut UiApp, ctx: &egui::Context) {
    let Some(src) = app.core.take_snip() else { return };
    open(app, ctx, src, None);
}

/// 在螢幕上框選一個範圍（不存檔，例如選錄影範圍）：選好時 done(app, Some(範圍))，取消時 done(app, None)。
/// 範圍是桌面座標（實體像素）。Windows 用原生的框選畫面（App::select_record_region），其他平台用這裡的框選視窗
pub fn pick_region(app: &mut UiApp, done: impl FnOnce(&mut UiApp, Option<DRect>) + Send + 'static) {
    let config = app.s.record_config(&app.env);
    let core = app.core.clone();
    if cfg!(windows) {
        return app.spawn(async move { core.select_record_region(&config).await }, move |app, r| match r {
            Ok(rect) => done(app, rect),
            Err(e) => {
                app.toast(e.message().to_string(), true);
                done(app, None);
            }
        });
    }
    app.spawn(async move { core.snip_source(&config).await }, move |app, r| match r {
        Ok(src) => {
            let ctx = app.ctx.clone();
            open(app, &ctx, src, Some(Box::new(done)));
        }
        Err(e) => {
            app.toast(e.message().to_string(), true);
            done(app, None);
        }
    });
}

/// 開始框選：在背景把凍結的畫面切成每個螢幕的材質。region：只選範圍（不存檔）
pub fn open(app: &mut UiApp, ctx: &egui::Context, src: SnipSource, region: Option<OnRegion>) {
    if app.snip.is_some() {
        app.core.snip_end(Some(&src));
        if let Some(cb) = region {
            cb(app, None);
        }
        return;
    }
    let was_hidden = !app.visible;
    if was_hidden {
        // 隱藏的視窗不跑介面（框選視窗也就出不來）：先縮到工作列
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(true));
    }
    let order = screenrecorder_core::winui::display_order();
    let mut monitors = src.monitors.clone();
    if monitors.is_empty() {
        // 沒有螢幕資訊（gdigrab 截整個桌面）：整個桌面當一台
        let d = src.desktop;
        monitors.push(MonitorInfo { x: d.x, y: d.y, width: d.width, height: d.height, primary: true, ..Default::default() });
    }
    let mons: Vec<Mon> = monitors.iter().enumerate().map(|(i, m)| Mon { info: m.clone(), index: order.iter().position(|d| *d == m.device_name).unwrap_or(i), tex: None }).collect();
    let (path, desk) = (src.path.clone(), src.desktop);
    let rects: Vec<DRect> = mons.iter().map(|m| DRect { x: m.info.x, y: m.info.y, width: m.info.width, height: m.info.height }).collect();
    // 選錄影範圍一定可以調整；截圖依設定「放開滑鼠就直接存檔」
    let adjust = region.is_some() || !app.s.snip_instant;
    app.snip = Some(Snip { src, mons, drag: None, sel: None, grab: None, adjust, region, was_hidden });
    app.spawn(async move { tokio::task::spawn_blocking(move || split(&path, desk, &rects)).await.ok().flatten() }, |app, imgs| {
        let ctx = app.ctx.clone();
        let Some(s) = app.snip.as_mut() else { return };
        match imgs {
            Some(imgs) => {
                for (i, (m, img)) in s.mons.iter_mut().zip(imgs).enumerate() {
                    m.tex = Some(ctx.load_texture(format!("snip-{i}"), img, TextureOptions::LINEAR));
                }
            }
            None => {
                finish(app, &ctx, Done::Cancel);
                report(app, Err(tr!("讀不到截下的畫面", "Couldn't read the captured screen").into()));
            }
        }
    });
}

/// 把整個桌面的畫面切成每個螢幕一張
fn split(path: &std::path::Path, desk: DRect, rects: &[DRect]) -> Option<Vec<egui::ColorImage>> {
    let bytes = std::fs::read(path).ok()?;
    let pm = tiny_skia::Pixmap::decode_png(&bytes).ok()?;
    let (sw, sh) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data();
    Some(
        rects
            .iter()
            .map(|r| {
                let x0 = (r.x - desk.x).clamp(0, sw);
                let y0 = (r.y - desk.y).clamp(0, sh);
                let w = (r.width).min(sw - x0).max(1) as usize;
                let h = (r.height).min(sh - y0).max(1) as usize;
                let mut out = vec![0u8; w * h * 4];
                for y in 0..h {
                    let s0 = ((y0 as usize + y) * sw as usize + x0 as usize) * 4;
                    let n = (w * 4).min(data.len().saturating_sub(s0));
                    out[y * w * 4..y * w * 4 + n].copy_from_slice(&data[s0..s0 + n]);
                }
                egui::ColorImage::from_rgba_premultiplied([w, h], &out)
            })
            .collect(),
    )
}

/// 每個螢幕一個全螢幕的框選視窗
pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut s) = app.snip.take() else { return };
    ctx.request_repaint();
    let mut done = None;
    if s.mons.iter().all(|m| m.tex.is_some()) {
        for i in 0..s.mons.len() {
            let m = &s.mons[i];
            let title = if s.region.is_some() { tr!("框選範圍", "Select area") } else { tr!("框選截圖", "Select area") };
            let builder = ViewportBuilder::default()
                .with_title(title)
                .with_decorations(false)
                .with_taskbar(false)
                .with_always_on_top()
                .with_active(true)
                // 在 Windows 上以 with_monitor 全螢幕開在那台螢幕（位置、大小只在找不到螢幕時用）
                .with_position(pos2(m.info.x as f32, m.info.y as f32))
                .with_inner_size(vec2(m.info.width as f32, m.info.height as f32))
                .with_monitor(m.index);
            if let Some(d) = ctx.show_viewport_immediate(ViewportId::from_hash_of(("snip", i)), builder, |ui, _| overlay(ui, &mut s, i)) {
                done = Some(d);
            }
        }
    }
    app.snip = Some(s);
    if let Some(d) = done {
        finish(app, ctx, d);
    }
}

fn tuple(r: DRect) -> (i32, i32, i32, i32) {
    (r.x, r.y, r.x + r.width, r.y + r.height)
}

fn drect((l, t, r, b): (i32, i32, i32, i32)) -> DRect {
    DRect { x: l, y: t, width: r - l, height: b - t }
}

fn inside(r: DRect, (x, y): (i32, i32)) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn overlay(ui: &mut egui::Ui, s: &mut Snip, i: usize) -> Option<Done> {
    let m = s.mons[i].info.clone();
    let tex = s.mons[i].tex.clone()?;
    let rect = ui.ctx().content_rect();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter().clone();
    painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    // 視窗座標（點）↔ 桌面座標（實體像素）：用比例換算，不受各螢幕縮放比例影響
    let to_desk = |p: Pos2| -> (i32, i32) {
        let fx = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        let fy = ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
        (m.x + (fx * m.width as f32).round() as i32, m.y + (fy * m.height as f32).round() as i32)
    };
    let to_screen = |x: i32, y: i32| pos2(rect.min.x + (x - m.x) as f32 / m.width as f32 * rect.width(), rect.min.y + (y - m.y) as f32 / m.height as f32 * rect.height());
    let screen_rect = |r: DRect| Rect::from_min_max(to_screen(r.x, r.y), to_screen(r.x + r.width, r.y + r.height));
    let monitor = DRect { x: m.x, y: m.y, width: m.width, height: m.height };
    // 邊、角可以按到的距離（7 點換成桌面像素）
    let tol = (7.0 * m.width as f32 / rect.width().max(1.0)).round() as i32;
    let shot = s.region.is_none();

    // 鍵盤：Esc 取消、Enter 完成（還沒框選時是這個螢幕）、Ctrl+C 只複製
    let (esc, enter, copy) =
        ui.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Enter), i.events.iter().any(|e| matches!(e, egui::Event::Copy)) || (i.modifiers.command && i.key_pressed(Key::C))));
    if esc || resp.secondary_clicked() {
        return Some(Done::Cancel);
    }
    if enter && s.drag.is_none() && s.grab.is_none() {
        return Some(Done::Finish(s.sel.map(|(_, r)| r).unwrap_or(monitor), SnipAction::Save));
    }
    if let Some((_, r)) = s.sel.filter(|_| copy && shot) {
        return Some(Done::Finish(r, SnipAction::Copy));
    }
    let pointer = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
    let my_sel = s.sel.filter(|(si, _)| *si == i).map(|(_, r)| r);
    // 在範圍裡點兩下：完成
    if resp.double_clicked() {
        if let (Some(r), Some(p)) = (my_sel, pointer) {
            if inside(r, to_desk(p)) {
                return Some(Done::Finish(r, SnipAction::Save));
            }
        }
    }
    // 按下：拉範圍的邊、角或移動，否則重新框選（限制在開始的那台螢幕內）
    if resp.drag_started() {
        // 從按下的位置算起（開始拖曳時游標已經移動了幾點）
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos()) {
            let d = to_desk(p);
            match my_sel.filter(|_| s.adjust).and_then(|r| snip_tools::grab_at(tuple(r), d, tol).map(|g| (g, r))) {
                Some((g, r)) => s.grab = Some((g, d, r)),
                None => {
                    s.sel = None;
                    s.drag = Some((i, d, d));
                }
            }
        }
    }
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
        let d = to_desk(p);
        if let Some((g, d0, orig)) = s.grab {
            s.sel = Some((i, drect(snip_tools::drag_grab(tuple(orig), g, (d.0 - d0.0, d.1 - d0.1), tuple(monitor)))));
        } else if let Some((si, a, _)) = s.drag {
            if si == i {
                s.drag = Some((si, a, d));
            }
        }
    }
    let selection = s.drag.filter(|d| d.0 == i).map(|(_, a, b)| DRect { x: a.0.min(b.0), y: a.1.min(b.1), width: (a.0 - b.0).abs(), height: (a.1 - b.1).abs() });
    let window_at = |p: (i32, i32)| s.src.windows.iter().find(|w| inside(**w, p)).copied();
    // 還沒框選、沒有在拖曳：游標下的視窗
    let window = if s.drag.is_none() && s.sel.is_none() { pointer.map(to_desk).and_then(window_at) } else { None };
    if resp.drag_stopped() && s.grab.take().is_none() {
        let sel = selection.filter(|r| r.width >= MIN_DRAG && r.height >= MIN_DRAG);
        s.drag = None;
        if let Some(r) = sel {
            if !s.adjust {
                return Some(Done::Finish(r, SnipAction::Save));
            }
            s.sel = Some((i, r));
        }
    }
    if resp.clicked() && s.drag.is_none() {
        // 點一下：選游標下的視窗（範圍裡點一下不變）
        let at = pointer.map(to_desk);
        let in_sel = my_sel.zip(at).is_some_and(|(r, p)| inside(r, p));
        if let Some(w) = at.filter(|_| !in_sel).and_then(window_at) {
            let r = clamp_to(w, s.src.desktop);
            if !s.adjust {
                return Some(Done::Finish(r, SnipAction::Save));
            }
            s.sel = Some((i, r));
        }
    }
    let my_sel = s.sel.filter(|(si, _)| *si == i).map(|(_, r)| r);

    // 選取範圍外變暗
    let hl = selection.or(my_sel).or(window).map(|r| screen_rect(r).intersect(rect));
    let dim = Color32::from_black_alpha(120);
    let accent = theme::pal_ctx(ui.ctx()).accent;
    match hl {
        Some(h) if h.is_positive() => {
            for part in [
                Rect::from_min_max(rect.min, pos2(rect.max.x, h.min.y)),
                Rect::from_min_max(pos2(rect.min.x, h.max.y), rect.max),
                Rect::from_min_max(pos2(rect.min.x, h.min.y), pos2(h.min.x, h.max.y)),
                Rect::from_min_max(pos2(h.max.x, h.min.y), pos2(rect.max.x, h.max.y)),
            ] {
                if part.is_positive() {
                    painter.rect_filled(part, CornerRadius::ZERO, dim);
                }
            }
            painter.rect_stroke(h, CornerRadius::ZERO, Stroke::new(2.0, accent), StrokeKind::Outside);
            let r = selection.or(my_sel).or(window).unwrap_or_default();
            let text = if selection.is_some() || my_sel.is_some() {
                format!("{} × {}", r.width, r.height)
            } else if s.adjust {
                trf!("視窗 {} × {}・點一下選這個視窗", "Window {} × {} · Click to select it", r.width, r.height)
            } else {
                trf!("視窗 {} × {}・點一下擷取", "Window {} × {} · Click to capture", r.width, r.height)
            };
            let g = painter.layout_no_wrap(text, theme::font_bold(13.0), Color32::WHITE);
            let mut at = h.min + vec2(0.0, -g.size().y - 10.0);
            if at.y < rect.min.y + 4.0 {
                at = h.min + vec2(6.0, 6.0);
            }
            let tag = Rect::from_min_size(at, g.size() + vec2(14.0, 8.0));
            painter.rect_filled(tag, CornerRadius::same(5), accent);
            painter.galley(tag.min + vec2(7.0, 4.0), g, Color32::WHITE);
        }
        _ => {
            painter.rect_filled(rect, CornerRadius::ZERO, dim);
        }
    }
    // 框好的範圍：八個把手、工具列
    let mut picked = None;
    if let Some(r) = my_sel.filter(|_| s.adjust && s.drag.is_none()) {
        for (x, y) in snip_tools::handles(tuple(r)) {
            let hr = Rect::from_center_size(to_screen(x, y), vec2(9.0, 9.0));
            painter.rect(hr, CornerRadius::ZERO, Color32::WHITE, Stroke::new(1.5, accent), StrokeKind::Middle);
        }
        if s.grab.is_none() {
            picked = toolbar(ui, screen_rect(r), rect, !shot);
        }
    }
    match picked {
        Some(Tool::Cancel) => return Some(Done::Cancel),
        Some(Tool::Full) => s.sel = Some((i, monitor)),
        Some(t) => {
            if let (Some(a), Some(r)) = (t.action(), my_sel) {
                return Some(Done::Finish(r, a));
            }
        }
        None => {}
    }
    // 游標：在邊、角上是調整大小，在範圍裡是移動
    let g = match (s.grab, my_sel) {
        (Some((g, ..)), _) => Some(g),
        (None, Some(r)) if s.adjust && s.drag.is_none() && resp.hovered() => pointer.and_then(|p| snip_tools::grab_at(tuple(r), to_desk(p), tol)),
        _ => None,
    };
    let cursor = match g {
        Some(Grab::Move) => CursorIcon::Move,
        Some(Grab::NW | Grab::SE) => CursorIcon::ResizeNwSe,
        Some(Grab::NE | Grab::SW) => CursorIcon::ResizeNeSw,
        Some(Grab::N | Grab::S) => CursorIcon::ResizeVertical,
        Some(Grab::E | Grab::W) => CursorIcon::ResizeHorizontal,
        None => CursorIcon::Crosshair,
    };
    if resp.hovered() || s.grab.is_some() {
        ui.ctx().set_cursor_icon(cursor);
    }
    // 上方的說明（游標移過去時改到下方，才不會擋住）
    let adjusting = my_sel.is_some() && s.adjust;
    let tip = match (shot, adjusting) {
        (true, true) => tr!("拖曳邊框調整範圍，在範圍裡點兩下或按 Enter 完成　　Esc 取消", "Drag the edges to adjust; double-click inside or press Enter to finish    Esc to cancel"),
        (true, false) => {
            tr!("拖曳框選範圍，或點一下選視窗；按 Enter 截整個螢幕　　Esc 或右鍵取消", "Drag to select an area or click a window; Enter for the whole screen    Esc or right-click to cancel")
        }
        (false, true) => tr!("拖曳邊框調整範圍，按 Enter 確定　　Esc 取消", "Drag the edges to adjust, then press Enter    Esc to cancel"),
        (false, false) => {
            tr!("拖曳框選範圍，或點一下選視窗；按 Enter 選整個螢幕　　Esc 或右鍵取消", "Drag to select an area or click a window; Enter for the whole screen    Esc or right-click to cancel")
        }
    };
    let g = painter.layout_no_wrap(tip.to_string(), theme::font(14.0), Color32::WHITE);
    let size = g.size() + vec2(28.0, 16.0);
    let mut tip_rect = Rect::from_center_size(pos2(rect.center().x, rect.min.y + 24.0 + size.y / 2.0), size);
    if pointer.is_some_and(|p| tip_rect.expand(40.0).contains(p)) {
        tip_rect = Rect::from_center_size(pos2(rect.center().x, rect.max.y - 24.0 - size.y / 2.0), size);
    }
    painter.rect_filled(tip_rect, CornerRadius::same(18), Color32::from_black_alpha(190));
    painter.galley(tip_rect.center() - g.size() / 2.0, g, Color32::WHITE);
    None
}

/// 框好範圍下方的工具列（範圍下方放不下時放上方或裡面）：整個螢幕｜編輯、複製、釘在桌面｜取消、完成。
/// record = 只選範圍：只有整個螢幕、取消、確定。回傳按下的按鈕
fn toolbar(ui: &mut egui::Ui, sel: Rect, screen: Rect, record: bool) -> Option<Tool> {
    let icon = |t: Tool| match t {
        Tool::Full => Icon::Window,
        Tool::Edit => Icon::Edit,
        Tool::Copy => Icon::Copy,
        Tool::Pin => Icon::Pin,
        Tool::Cancel => Icon::Close,
        Tool::Done => Icon::Check,
    };
    let btn = |t: Tool| {
        let b = Btn::new(t.label(record)).icon(icon(t)).small().tooltip(t.tip(record));
        if t == Tool::Done {
            b.primary()
        } else {
            b.ghost()
        }
    };
    let list = snip_tools::tools(record);
    let (gap, sep, pad) = (2.0, 10.0, 5.0);
    let group_end = |t: Tool| matches!(t, Tool::Full | Tool::Pin);
    let w: f32 = list.iter().map(|t| btn(*t).width(ui) + if group_end(*t) { sep } else { gap }).sum::<f32>() - gap + pad * 2.0;
    let h = 28.0 + pad * 2.0;
    let ir = |r: Rect| (r.min.x.round() as i32, r.min.y.round() as i32, r.max.x.round() as i32, r.max.y.round() as i32);
    let (x, y) = snip_tools::toolbar_pos(ir(sel), (w.ceil() as i32, h as i32), ir(screen), 8);
    let bar = Rect::from_min_size(pos2(x as f32, y as f32), vec2(w.ceil(), h));
    let p = theme::pal(ui);
    ui.painter().rect(bar, CornerRadius::same(10), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
    let mut picked = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink(pad)).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for &t in list {
            if btn(t).show(ui).clicked() {
                picked = Some(t);
            }
            if group_end(t) {
                ui.add_space(sep - gap);
            }
        }
    });
    picked
}

fn clamp_to(r: DRect, b: DRect) -> DRect {
    let x0 = r.x.max(b.x);
    let y0 = r.y.max(b.y);
    let x1 = (r.x + r.width).min(b.x + b.width);
    let y1 = (r.y + r.height).min(b.y + b.height);
    DRect { x: x0, y: y0, width: (x1 - x0).max(1), height: (y1 - y0).max(1) }
}

/// 結束框選：關掉框選視窗，依選的動作存檔（或複製、釘在桌面），或把選的範圍交出去；操作視窗回到原本的樣子
fn finish(app: &mut UiApp, ctx: &egui::Context, done: Done) {
    let Some(mut s) = app.snip.take() else { return };
    if s.was_hidden {
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(false));
    }
    let core = app.core.clone();
    // 只選範圍：不存檔，把範圍交給呼叫的地方
    if let Some(cb) = s.region.take() {
        core.snip_end(Some(&s.src));
        return cb(
            app,
            match done {
                Done::Finish(r, _) => Some(r),
                Done::Cancel => None,
            },
        );
    }
    match done {
        Done::Cancel => core.snip_end(Some(&s.src)),
        Done::Finish(r, action) => app.spawn(
            async move {
                let res = core.snip_finish(&s.src, r, action).await;
                core.snip_end(Some(&s.src));
                res.map_err(|e| e.message().to_string())
            },
            move |app, res| match res {
                Ok(_) if action == SnipAction::Copy => app.toast(tr!("已複製到剪貼簿（沒有存檔）", "Copied to the clipboard (not saved)"), false),
                Ok(Some(shot)) => report(app, Ok((shot.path, shot.copied))),
                Ok(None) => {}
                Err(e) => report(app, Err(e)),
            },
        ),
    }
}

/// 操作視窗看得到時由截圖狀態顯示提示；隱藏時用系統通知
fn report(app: &mut UiApp, res: Result<(String, bool), String>) {
    match res {
        Ok((path, copied)) => {
            if !app.visible {
                let name = super::dialogs::file_name(&path);
                app.core.notify(
                    tr!("已截圖", "Screenshot taken"),
                    &if copied { trf!("已複製到剪貼簿，存成 {name}", "Copied to the clipboard and saved as {name}") } else { trf!("已存成 {name}", "Saved as {name}") },
                    false,
                );
            }
        }
        Err(e) => {
            if app.visible {
                app.toast(trf!("截圖失敗：{e}", "Screenshot failed: {e}"), true);
            } else {
                app.core.notify(tr!("截圖失敗", "Screenshot failed"), &e, true);
            }
        }
    }
}
