//! 在螢幕上框選截圖（系統匣「截圖 → 框選範圍或視窗」、Ctrl+Alt+A）：
//! 先截下整個桌面（凍結），每個螢幕開一個全螢幕視窗顯示變暗的畫面，
//! 拖曳框選範圍，或點一下擷取游標下的視窗；Esc 或右鍵取消。
//! 從凍結的畫面裁切，所以截到的就是框選時看到的樣子。

use super::theme;
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Key, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, ViewportBuilder, ViewportCommand, ViewportId};
use screenrecorder_core::app::SnipSource;
use screenrecorder_core::types::{MonitorInfo, Rect as DRect};
use screenrecorder_core::{tr, trf};

/// 小於這個大小（實體像素）的拖曳當成點一下
const MIN_DRAG: i32 = 4;

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
    /// 原本操作視窗是隱藏的（結束後再隱藏）
    was_hidden: bool,
}

enum Done {
    Cancel,
    Save(DRect),
}

/// 收到框選要求：取走凍結的畫面，在背景切成每個螢幕的材質
pub fn start(app: &mut UiApp, ctx: &egui::Context) {
    let Some(src) = app.core.take_snip() else { return };
    if app.snip.is_some() {
        app.core.snip_end(Some(&src));
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
    app.snip = Some(Snip { src, mons, drag: None, was_hidden });
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
            let builder = ViewportBuilder::default()
                .with_title(tr!("框選截圖", "Select area"))
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

    if ui.input(|i| i.key_pressed(Key::Escape)) || resp.secondary_clicked() {
        return Some(Done::Cancel);
    }
    let pointer = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
    // 拖曳：從開始的那台螢幕框選（限制在那台螢幕內）
    if resp.drag_started() {
        // 從按下的位置算起（開始拖曳時游標已經移動了幾點）
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos()) {
            let d = to_desk(p);
            s.drag = Some((i, d, d));
        }
    }
    if let (Some((si, a, _)), Some(p)) = (s.drag, resp.interact_pointer_pos()) {
        if si == i {
            s.drag = Some((si, a, to_desk(p)));
        }
    }
    let selection = s.drag.filter(|d| d.0 == i).map(|(_, a, b)| DRect { x: a.0.min(b.0), y: a.1.min(b.1), width: (a.0 - b.0).abs(), height: (a.1 - b.1).abs() });
    // 沒有在拖曳：游標下的視窗
    let window = if s.drag.is_none() { pointer.map(to_desk).and_then(|(x, y)| s.src.windows.iter().find(|w| x >= w.x && x < w.x + w.width && y >= w.y && y < w.y + w.height).copied()) } else { None };
    if resp.drag_stopped() {
        let sel = selection.filter(|r| r.width >= MIN_DRAG && r.height >= MIN_DRAG);
        s.drag = None;
        if let Some(r) = sel {
            return Some(Done::Save(r));
        }
    }
    if resp.clicked() {
        if let Some(w) = window {
            return Some(Done::Save(clamp_to(w, s.src.desktop)));
        }
    }

    // 選取範圍外變暗
    let hl = selection.or(window).map(|r| Rect::from_min_max(to_screen(r.x, r.y), to_screen(r.x + r.width, r.y + r.height)).intersect(rect));
    let dim = Color32::from_black_alpha(120);
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
            let accent = theme::pal_ctx(ui.ctx()).accent;
            painter.rect_stroke(h, CornerRadius::ZERO, Stroke::new(2.0, accent), StrokeKind::Outside);
            let r = selection.or(window).unwrap_or_default();
            let text = if selection.is_some() { format!("{} × {}", r.width, r.height) } else { trf!("視窗 {} × {}・點一下擷取", "Window {} × {} · Click to capture", r.width, r.height) };
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
    // 上方的說明
    let g = painter.layout_no_wrap(
        tr!("拖曳框選範圍，或點一下擷取視窗　　Esc 或右鍵取消", "Drag to select an area, or click a window to capture it    Esc or right-click to cancel").to_string(),
        theme::font(14.0),
        Color32::WHITE,
    );
    let tip = Rect::from_center_size(pos2(rect.center().x, rect.min.y + 40.0), g.size() + vec2(28.0, 16.0));
    if !pointer.is_some_and(|p| tip.expand(20.0).contains(p)) || s.drag.is_some() {
        painter.rect_filled(tip, CornerRadius::same(18), Color32::from_black_alpha(190));
        painter.galley(tip.center() - g.size() / 2.0, g, Color32::WHITE);
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    None
}

fn clamp_to(r: DRect, b: DRect) -> DRect {
    let x0 = r.x.max(b.x);
    let y0 = r.y.max(b.y);
    let x1 = (r.x + r.width).min(b.x + b.width);
    let y1 = (r.y + r.height).min(b.y + b.height);
    DRect { x: x0, y: y0, width: (x1 - x0).max(1), height: (y1 - y0).max(1) }
}

/// 結束框選：關掉框選視窗，存檔（或取消），操作視窗回到原本的樣子
fn finish(app: &mut UiApp, ctx: &egui::Context, done: Done) {
    let Some(s) = app.snip.take() else { return };
    if s.was_hidden {
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(false));
    }
    let core = app.core.clone();
    match done {
        Done::Cancel => core.snip_end(Some(&s.src)),
        Done::Save(r) => app.spawn(
            async move {
                let res = core.snip_save(&s.src, r).await;
                core.snip_end(Some(&s.src));
                res.map_err(|e| e.message().to_string())
            },
            |app, res| report(app, res.map(|shot| (shot.path, shot.copied))),
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
                    &if copied { trf!("已複製到剪貼簿，存成 {name}", "Copied to clipboard and saved as {name}") } else { trf!("已存成 {name}", "Saved as {name}") },
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
