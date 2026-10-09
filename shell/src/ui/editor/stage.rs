//! 影片畫面：影片（含馬賽克 / 模糊）、標註、選取框、裁切框，以及在畫面上放置 / 移動 / 調整標註與框選裁切範圍。

use super::{hash_of, Drag, Editor, Tab, Tool};
use crate::ui::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions};
use screenrecorder_core::annotate::{self, Ann, AnnKind, Shape};
use screenrecorder_core::edit::CropInput;
use std::time::{Duration, Instant};

/// 標註畫成與畫面同大小的圖（內容變了才重畫）
#[derive(Default)]
pub struct Overlay {
    /// 每個標註畫成自己的小圖（依內容快取）：只改時間或位置時不用重畫，拖曳才順
    sprites: std::collections::HashMap<u64, Sprite>,
}

struct Sprite {
    /// 內容（不含位置、時間）與縮放比例的雜湊；變了才重畫
    key: u64,
    tex: TextureHandle,
    /// 圖的左上角相對於標註 (x, y) 的位移與大小（影片像素）
    off: (f64, f64),
    size: (f64, f64),
}

const SEL_BLUE: Color32 = Color32::from_rgb(0x00, 0x90, 0xff);

pub fn show(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, stage_h: f32) {
    let avail_w = ui.available_width();
    let ar = if ed.vw > 0.0 && ed.vh > 0.0 { (ed.vw / ed.vh) as f32 } else { 16.0 / 9.0 };
    let (w, h) = if avail_w / stage_h > ar { (stage_h * ar, stage_h) } else { (avail_w, avail_w / ar) };
    let (outer, _) = ui.allocate_exact_size(vec2(avail_w, stage_h), Sense::hover());
    let rect = Rect::from_center_size(outer.center(), vec2(w, h));
    let resp = ui.interact(rect, Id::new("ed-stage"), Sense::click_and_drag());
    let painter = ui.painter_at(outer);
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
    if let Some(tex) = &ed.video_tex {
        painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if ed.vw <= 0.0 {
        interact(ed, ui, rect, &resp);
        return;
    }
    let t = ed.now();
    let playing = ed.player.is_playing();

    // 標註（馬賽克 / 模糊已套用在影片上）
    let css = (rect.width() as f64) / ed.vw;
    update_sprites(ed, ctx, rect);
    for a in ed.ordered() {
        if a.kind.is_effect() {
            continue;
        }
        let visible = t >= a.start && t <= a.end;
        if !visible && ed.ann_sel != Some(a.id) {
            continue;
        }
        if let Some(sp) = ed.overlay.sprites.get(&a.id) {
            let min = pos2(rect.left() + ((a.x + sp.off.0) * css) as f32, rect.top() + ((a.y + sp.off.1) * css) as f32);
            let r = Rect::from_min_size(min, vec2((sp.size.0 * css) as f32, (sp.size.1 * css) as f32));
            // 選取中、但目前時間看不到的標註畫淡一點
            let tint = if visible { Color32::WHITE } else { Color32::from_white_alpha(89) };
            painter.with_clip_rect(rect).image(sp.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
    }
    let to_screen = |x: f64, y: f64| pos2(rect.left() + (x * css) as f32, rect.top() + (y * css) as f32);

    // 馬賽克 / 模糊的範圍框與名稱（播放時隱藏，看到的就是輸出的樣子）
    if !playing {
        for a in ed.anns.iter().filter(|a| a.kind.is_effect()) {
            let visible = t >= a.start && t <= a.end;
            if !visible && ed.ann_sel != Some(a.id) {
                continue;
            }
            let alpha = if visible { 1.0 } else { 0.4 };
            let r = Rect::from_min_max(to_screen(a.x, a.y), to_screen(a.x + a.w, a.y + a.h));
            let pts = outline(a.shape, r, (annotate::round_radius(a.w, a.h) * css) as f32);
            painter.add(egui::Shape::closed_line(pts.clone(), Stroke::new(1.0, Color32::from_black_alpha((128.0 * alpha) as u8))));
            painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.0, Color32::from_white_alpha((230.0 * alpha) as u8)), 4.0, 3.0));
            // 名稱放在形狀裡面（橢圓、圓角時置中靠上，不會跑到框外）；形狀太小放不下就不顯示
            if r.width() >= 120.0 && r.height() >= 48.0 {
                let g = painter.layout_no_wrap(annotate::label(a), theme::font(11.0), Color32::WHITE);
                let pill = match a.shape.unwrap_or_default() {
                    Shape::Rect => Rect::from_min_size(r.min + vec2(4.0, 4.0), g.size() + vec2(12.0, 2.0)),
                    _ => Rect::from_center_size(pos2(r.center().x, r.top() + r.height() * 0.14 + g.size().y / 2.0), g.size() + vec2(12.0, 2.0)),
                };
                painter.rect_filled(pill, CornerRadius::same(4), Color32::from_black_alpha((140.0 * alpha) as u8));
                painter.galley(pill.min + vec2(6.0, 1.0), g, Color32::WHITE);
            }
        }
    }

    // 選取框與調整大小的把手
    if let Some(a) = ed.selected() {
        let (bx, by, bw, bh) = annotate::bbox(a);
        let r = Rect::from_min_max(to_screen(bx, by), to_screen(bx + bw, by + bh));
        let pts = vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.5, SEL_BLUE), 5.0, 4.0));
        for (hx, hy) in annotate::handles(a) {
            let c = to_screen(hx, hy);
            let hr = Rect::from_center_size(c, vec2(10.0, 10.0));
            painter.rect(hr, CornerRadius::ZERO, Color32::WHITE, Stroke::new(1.5, SEL_BLUE), egui::StrokeKind::Middle);
        }
    }

    // 裁切範圍：外面變暗
    if ed.crop_on {
        if let Some(c) = ed.spec.crop {
            let cr = Rect::from_min_max(to_screen(c.x, c.y), to_screen(c.x + c.width, c.y + c.height));
            let dim = Color32::from_black_alpha(128);
            for r in [
                Rect::from_min_max(rect.min, pos2(rect.right(), cr.top())),
                Rect::from_min_max(pos2(rect.left(), cr.bottom()), rect.max),
                Rect::from_min_max(pos2(rect.left(), cr.top()), pos2(cr.left(), cr.bottom())),
                Rect::from_min_max(pos2(cr.right(), cr.top()), pos2(rect.right(), cr.bottom())),
            ] {
                if r.is_positive() {
                    painter.rect_filled(r, CornerRadius::ZERO, dim);
                }
            }
            painter.rect_stroke(cr, CornerRadius::ZERO, Stroke::new(2.0, theme::pal(ui).rec), egui::StrokeKind::Outside);
        }
    }
    interact(ed, ui, rect, &resp);
}

/// 形狀的外框（畫面座標）
fn outline(shape: Option<Shape>, r: Rect, radius: f32) -> Vec<Pos2> {
    match shape.unwrap_or_default() {
        Shape::Rect => vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()],
        Shape::Ellipse => (0..72)
            .map(|i| {
                let a = i as f32 / 72.0 * std::f32::consts::TAU;
                pos2(r.center().x + a.cos() * r.width() / 2.0, r.center().y + a.sin() * r.height() / 2.0)
            })
            .collect(),
        Shape::Round => {
            let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0);
            let corners = [(r.right() - rad, r.top() + rad, -90.0f32), (r.right() - rad, r.bottom() - rad, 0.0), (r.left() + rad, r.bottom() - rad, 90.0), (r.left() + rad, r.top() + rad, 180.0)];
            corners
                .iter()
                .flat_map(|&(cx, cy, start)| {
                    (0..=8).map(move |k| {
                        let a = (start + k as f32 * 90.0 / 8.0).to_radians();
                        pos2(cx + a.cos() * rad, cy + a.sin() * rad)
                    })
                })
                .collect()
        }
    }
}

/// 標註的小圖：內容或大小變了才重畫（表情符號的彩色字形畫起來特別慢，拖曳時不能每一格都重畫）
fn update_sprites(ed: &mut Editor, ctx: &egui::Context, rect: Rect) {
    let s = (rect.width() * ctx.pixels_per_point()) as f64 / ed.vw;
    let ids: Vec<u64> = ed.anns.iter().map(|a| a.id).collect();
    ed.overlay.sprites.retain(|id, _| ids.contains(id));
    for a in ed.anns.iter().filter(|a| !a.kind.is_effect()) {
        // 內容：位置、時間、編號以外的欄位
        let mut look = a.clone();
        (look.x, look.y, look.start, look.end, look.id) = (0.0, 0.0, 0.0, 0.0, 0);
        let key = hash_of(&(serde_json::to_string(&look).unwrap_or_default(), s.to_bits()));
        if ed.overlay.sprites.get(&a.id).is_some_and(|sp| sp.key == key) {
            continue;
        }
        // 範圍：標註實際佔的地方，再留一點邊（文字外框、箭頭頭部）
        let (bx, by, bw, bh) = annotate::bbox(&look);
        let m = a.size * 0.3 + 4.0;
        let (ox, oy, ow, oh) = (bx - m, by - m, bw + m * 2.0, bh + m * 2.0);
        let (pw, ph) = ((ow * s).ceil().max(1.0) as u32, (oh * s).ceil().max(1.0) as u32);
        let Some(mut pm) = tiny_skia::Pixmap::new(pw.min(8192), ph.min(8192)) else { continue };
        let tf = tiny_skia::Transform::from_scale(s as f32, s as f32).pre_translate(-ox as f32, -oy as f32);
        annotate::draw(&mut pm, &look, tf);
        let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
        let size = (pm.width() as f64 / s, pm.height() as f64 / s);
        match ed.overlay.sprites.get_mut(&a.id) {
            Some(sp) => {
                sp.tex.set(img, TextureOptions::LINEAR);
                (sp.key, sp.off, sp.size) = (key, (ox, oy), size);
            }
            None => {
                let tex = ctx.load_texture(format!("ann-{}", a.id), img, TextureOptions::LINEAR);
                ed.overlay.sprites.insert(a.id, Sprite { key, tex, off: (ox, oy), size });
            }
        }
    }
}

/// 點到的標註（目前時間看得到的，最上面的優先；選取中的也算）
fn ann_at(ed: &Editor, x: f64, y: f64, tol: f64) -> Option<u64> {
    let t = ed.now();
    ed.anns.iter().rev().find(|a| (ed.ann_sel == Some(a.id) || (t >= a.start && t <= a.end)) && annotate::hit(a, x, y, tol)).map(|a| a.id)
}

fn handle_at(a: &Ann, x: f64, y: f64, tol: f64) -> Option<usize> {
    annotate::handles(a).iter().position(|&(hx, hy)| (hx - x).abs() <= tol && (hy - y).abs() <= tol)
}

fn interact(ed: &mut Editor, ui: &egui::Ui, rect: Rect, resp: &egui::Response) {
    let (pressed, down, released, pos, scroll, shift) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.interact_pos(), super::wheel_delta(i), i.modifiers.shift));
    let (vw, vh) = (ed.vw, ed.vh);
    let to_video = |p: Pos2| (((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * vw, ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0) as f64 * vh);
    let k = vw / rect.width().max(1.0) as f64;

    // 影片上轉滾輪：前後一張（Shift 一秒）
    if resp.hovered() && scroll != egui::Vec2::ZERO && ed.wheel_at.elapsed() > Duration::from_millis(40) {
        ed.wheel_at = Instant::now();
        let d = if scroll.y.abs() >= scroll.x.abs() { scroll.y } else { scroll.x };
        let forward = d < 0.0;
        if shift {
            ed.step(None, if forward { 1.0 } else { -1.0 });
        } else {
            ed.step(Some(if forward { 1 } else { -1 }), 0.0);
        }
    }

    // 游標
    if resp.hovered() && matches!(ed.drag, Drag::None) {
        if let Some(p) = pos {
            let (x, y) = to_video(p);
            let cursor = if vw <= 0.0 {
                CursorIcon::PointingHand
            } else if ed.tool.is_some() {
                CursorIcon::Crosshair
            } else if ed.selected().is_some_and(|a| handle_at(a, x, y, k * 9.0).is_some()) {
                CursorIcon::ResizeNwSe
            } else if ann_at(ed, x, y, k * 6.0).is_some() {
                CursorIcon::Move
            } else if ed.crop_on {
                CursorIcon::Crosshair
            } else {
                CursorIcon::PointingHand
            };
            ui.ctx().set_cursor_icon(cursor);
        }
    }

    if pressed && resp.hovered() {
        let Some(p) = pos else { return };
        if vw <= 0.0 {
            return ed.toggle_play();
        }
        let (x, y) = to_video(p);
        // 連續放置編號 / 表情時，點到已放好的同類標註 = 選取、移動它，不再新增
        let hit_same = ed.tool.filter(|t| t.sticky()).and_then(|t| ann_at(ed, x, y, k * 6.0).filter(|id| ed.anns.iter().any(|a| a.id == *id && a.kind == t.kind())));
        if let (Some(tool), None) = (ed.tool, hit_same) {
            ed.player.pause();
            ed.stop_preview();
            let kind = tool.kind();
            let a = ed.new_ann(kind, x, y);
            let id = a.id;
            let default_text = a.text.as_deref() == Some("說明文字");
            ed.anns.push(a);
            ed.ann_sel = Some(id);
            ed.tab = Tab::Ann;
            if matches!(kind, AnnKind::Text | AnnKind::Step) {
                // 點一下就放好；編號、表情繼續放下一個
                if !tool.sticky() {
                    ed.tool = None;
                }
                if kind == AnnKind::Text && default_text {
                    ed.focus_text = true;
                }
            } else {
                ed.drag = Drag::Create { id, from: (x, y) };
            }
            return;
        }
        if let Some(cur) = ed.selected() {
            if let Some(h) = handle_at(cur, x, y, k * 9.0) {
                ed.drag = Drag::Resize { id: cur.id, handle: h, orig: cur.clone() };
                return;
            }
        }
        if let Some(id) = ann_at(ed, x, y, k * 6.0) {
            ed.player.pause();
            ed.stop_preview();
            ed.select_ann(Some(id));
            let orig = ed.anns.iter().find(|a| a.id == id).cloned();
            if let Some(orig) = orig {
                ed.drag = Drag::Move { id, from: (x, y), orig };
            }
            return;
        }
        if ed.ann_sel.is_some() {
            // 點空白處：取消選取標註（裁切模式下同時開始框選）
            ed.ann_sel = None;
            if !ed.crop_on {
                return;
            }
        } else if !ed.crop_on {
            return ed.toggle_play();
        }
        ed.drag = Drag::Crop { from: (x, y) };
        return;
    }

    let stage_drag = matches!(ed.drag, Drag::Crop { .. } | Drag::Create { .. } | Drag::Move { .. } | Drag::Resize { .. });
    if !stage_drag {
        return;
    }
    if down {
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            drag_to(ed, to_video(p));
        }
    }
    if released || !down {
        end_drag(ed);
    }
}

fn drag_to(ed: &mut Editor, (px, py): (f64, f64)) {
    match std::mem::replace(&mut ed.drag, Drag::None) {
        Drag::Crop { from } => {
            ed.spec.crop = Some(CropInput { x: from.0.min(px), y: from.1.min(py), width: (px - from.0).abs(), height: (py - from.1).abs() });
            ed.drag = Drag::Crop { from };
        }
        Drag::Create { id, from } => {
            if let Some(a) = ed.ann_mut(id) {
                if a.kind == AnnKind::Arrow {
                    a.w = px - from.0;
                    a.h = py - from.1;
                } else {
                    a.x = from.0.min(px);
                    a.y = from.1.min(py);
                    a.w = (px - from.0).abs();
                    a.h = (py - from.1).abs();
                }
            }
            ed.drag = Drag::Create { id, from };
        }
        Drag::Move { id, from, orig } => {
            if let Some(a) = ed.ann_mut(id) {
                a.x = orig.x + (px - from.0);
                a.y = orig.y + (py - from.1);
            }
            ed.drag = Drag::Move { id, from, orig };
        }
        Drag::Resize { id, handle, orig } => {
            if let Some(a) = ed.ann_mut(id) {
                let o = &orig;
                match a.kind {
                    AnnKind::Arrow if handle == 0 => {
                        // 移動起點，終點不動
                        a.x = px;
                        a.y = py;
                        a.w = o.x + o.w - px;
                        a.h = o.y + o.h - py;
                    }
                    AnnKind::Arrow => {
                        a.w = px - o.x;
                        a.h = py - o.y;
                    }
                    AnnKind::Text | AnnKind::Step => {
                        // 拉角：依寬度等比例調整字級 / 大小
                        let ratio = ((px - o.x) / o.w.max(1.0)).max(0.2);
                        a.size = (o.size * ratio).round().max(8.0);
                        annotate::measure(a);
                    }
                    _ => {
                        a.w = (px - o.x).max(8.0);
                        a.h = (py - o.y).max(8.0);
                    }
                }
            }
            ed.drag = Drag::Resize { id, handle, orig };
        }
        other => ed.drag = other,
    }
}

fn end_drag(ed: &mut Editor) {
    match std::mem::replace(&mut ed.drag, Drag::None) {
        Drag::Crop { .. } => {
            if let Some(c) = ed.spec.crop {
                ed.set_crop(c);
            }
        }
        Drag::Create { id, .. } => {
            // 拖曳太短：給個預設大小
            let k = (if ed.vh > 0.0 { ed.vh } else { 1080.0 }) / 1080.0;
            if let Some(a) = ed.ann_mut(id) {
                if a.kind == AnnKind::Arrow && a.w.hypot(a.h) < 20.0 * k {
                    a.w = 160.0 * k;
                    a.h = -100.0 * k;
                } else if a.kind != AnnKind::Arrow && (a.w < 12.0 * k || a.h < 12.0 * k) {
                    a.w = 320.0 * k;
                    a.h = 180.0 * k;
                }
            }
            ed.tool = None;
        }
        _ => {}
    }
}

/// 工具按鈕上的小圖示
pub fn paint_tool_icon(p: &egui::Painter, c: Pos2, tool: Tool, color: Color32, emoji: Option<&TextureHandle>) {
    let s = Stroke::new(1.8, color);
    match tool {
        Tool::Ann(AnnKind::Text) => {
            p.text(c, Align2::CENTER_CENTER, "T", theme::font_bold(19.0), color);
        }
        Tool::Emoji => {
            if let Some(t) = emoji {
                let sz = t.size_vec2();
                let k = 22.0 / sz.y.max(1.0);
                p.image(t.id(), Rect::from_center_size(c, sz * k), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        Tool::Ann(AnnKind::Arrow) => {
            let (a, b) = (c + vec2(-7.0, 7.0), c + vec2(7.0, -7.0));
            p.line_segment([a, b], s);
            p.line_segment([b, b + vec2(-7.0, 0.0)], s);
            p.line_segment([b, b + vec2(0.0, 7.0)], s);
        }
        Tool::Ann(AnnKind::Rect) => {
            p.rect_stroke(Rect::from_center_size(c, vec2(18.0, 14.0)), CornerRadius::same(2), s, egui::StrokeKind::Middle);
        }
        Tool::Ann(AnnKind::Ellipse) => {
            p.add(egui::Shape::ellipse_stroke(c, vec2(9.0, 7.5), s));
        }
        Tool::Ann(AnnKind::Highlight) => {
            p.rect_filled(Rect::from_center_size(c, vec2(20.0, 8.0)), CornerRadius::same(1), if color == Color32::WHITE { color } else { Color32::from_rgb(0xf5, 0xb3, 0x01) });
        }
        Tool::Ann(AnnKind::Step) => {
            p.circle_stroke(c, 9.0, s);
            p.text(c + vec2(0.0, 0.5), Align2::CENTER_CENTER, "1", theme::font_bold(12.0), color);
        }
        Tool::Ann(AnnKind::Mosaic) => {
            for i in 0..3 {
                for j in 0..3 {
                    let r = Rect::from_min_size(c + vec2(-9.0 + i as f32 * 6.0, -9.0 + j as f32 * 6.0), vec2(6.0, 6.0));
                    let a = if (i + j) % 2 == 0 { 220 } else { 90 };
                    p.rect_filled(r, CornerRadius::ZERO, color.gamma_multiply(a as f32 / 255.0));
                }
            }
        }
        Tool::Ann(AnnKind::Blur) => {
            for (r, a) in [(9.5, 0.25), (7.0, 0.5), (4.5, 0.85)] {
                p.circle_filled(c, r, color.gamma_multiply(a));
            }
        }
    }
}
