//! 影片畫面：影片（含馬賽克 / 模糊）、標註、選取框、裁切框，以及在畫面上放置 / 移動 / 調整標註與框選裁切範圍。

use super::{hash_of, Drag, Editor, Tab, Tool};
use crate::ui::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions};
use screenrecorder_core::annotate::{self, Ann, AnnKind, Shape};
use screenrecorder_core::edit::CropInput;
use screenrecorder_core::zoom;
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
/// 旋轉把手在選取框上方多遠（畫面像素）
/// 旋轉把手離外框的距離、可以點到的半徑（畫面像素）
const ROT_HANDLE_PX: f64 = 30.0;
const ROT_HIT_PX: f64 = 16.0;

pub fn show(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, stage_h: f32) {
    let avail_w = ui.available_width();
    // 截圖的「輸出」分頁：大畫面顯示輸出的樣子（背景、圓角、陰影、外框都看得清楚）
    if ed.is_shot() && ed.tab == Tab::Output {
        if let Some(tex) = super::shot::output_texture(ed) {
            let (outer, _) = ui.allocate_exact_size(vec2(avail_w, stage_h), Sense::hover());
            let sz = tex.size_vec2();
            let k = ((outer.width() - 16.0) / sz.x).min((outer.height() - 16.0) / sz.y).min(ed.vw as f32 / sz.x.max(1.0) * 1.5).max(0.01);
            let r = Rect::from_center_size(outer.center(), sz * k);
            let painter = ui.painter_at(outer);
            super::shot::checker(&painter, r);
            painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            return;
        }
    }
    let ar = if ed.vw > 0.0 && ed.vh > 0.0 { (ed.vw / ed.vh) as f32 } else { 16.0 / 9.0 };
    let (w, h) = if avail_w / stage_h > ar { (stage_h * ar, stage_h) } else { (avail_w, avail_w / ar) };
    let (outer, _) = ui.allocate_exact_size(vec2(avail_w, stage_h), Sense::hover());
    let rect = Rect::from_center_size(outer.center(), vec2(w, h));
    let resp = ui.interact(rect, Id::new("ed-stage"), Sense::click_and_drag());
    let painter = ui.painter_at(outer);
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
    if let Some(tex) = &ed.video_tex {
        // 預覽結果時顯示跟著點擊放大的效果（只放大影片；標註照原本的位置畫）
        let uv = if ed.previewing && ed.zoom_on() {
            let (z, cx, cy) = zoom::at(&zoom::focus_points(&ed.clicks), ed.zoom, ed.now());
            let (hw, hh) = (0.5 / z as f32, 0.5 / z as f32);
            let (cx, cy) = ((cx as f32).clamp(hw, 1.0 - hw), (cy as f32).clamp(hh, 1.0 - hh));
            Rect::from_min_max(pos2(cx - hw, cy - hh), pos2(cx + hw, cy + hh))
        } else {
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
        };
        painter.image(tex.id(), rect, uv, Color32::WHITE);
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
            // 放大鏡本身就看得出範圍（選取時才有外框）
            if a.kind == AnnKind::Magnify {
                continue;
            }
            let alpha = if visible { 1.0 } else { 0.4 };
            let r = Rect::from_min_max(to_screen(a.x, a.y), to_screen(a.x + a.w, a.y + a.h));
            let mut pts = outline(a.shape, r, (annotate::round_radius(a.w, a.h) * css) as f32);
            // 旋轉：外框的點繞中心轉
            let rot = annotate::rotation(a);
            if rot != 0.0 {
                let c = r.center();
                for q in &mut pts {
                    let (x, y) = annotate::rotate_point(q.x as f64, q.y as f64, c.x as f64, c.y as f64, rot);
                    *q = pos2(x as f32, y as f32);
                }
            }
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
        // 未旋轉的外框四個角轉到畫面上（旋轉時選取框跟著轉）
        let lb = annotate::local_bbox(a);
        let mut pts: Vec<Pos2> = annotate::corners(a, lb).iter().map(|&(x, y)| to_screen(x, y)).collect();
        pts.push(pts[0]);
        painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.5, SEL_BLUE), 5.0, 4.0));
        if a.kind.rotatable() {
            // 旋轉把手：外框上方（放不下時在下方）的圓鈕，與外框中間連一條線
            let (anchor, (rx, ry)) = rot_handle(a, 1.0 / css, ed.vw, ed.vh);
            let (anchor, rh) = (to_screen(anchor.0, anchor.1), to_screen(rx, ry));
            painter.line_segment([anchor, rh], Stroke::new(1.5, SEL_BLUE));
            let rotating = matches!(ed.drag, Drag::Rotate { .. });
            painter.circle(rh, 10.0, if rotating { SEL_BLUE } else { Color32::WHITE }, Stroke::new(1.5, SEL_BLUE));
            theme::paint_icon(&painter, Rect::from_center_size(rh, vec2(13.0, 13.0)), theme::Icon::Refresh, if rotating { Color32::WHITE } else { SEL_BLUE });
            // 轉動中：顯示角度
            if rotating {
                let g = painter.layout_no_wrap(format!("{}°", a.rot.round()), theme::font_bold(12.0), Color32::WHITE);
                let tag = Rect::from_min_size(rh + vec2(16.0, -g.size().y / 2.0 - 3.0), g.size() + vec2(10.0, 6.0));
                painter.rect_filled(tag, CornerRadius::same(4), SEL_BLUE);
                painter.galley(tag.min + vec2(5.0, 3.0), g, Color32::WHITE);
            }
        }
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
            if ed.tab == Tab::Crop {
                // 三等分線（構圖參考）與四個角的 L 形把手
                let faint = Stroke::new(1.0, Color32::from_white_alpha(90));
                for i in 1..3 {
                    let t = i as f32 / 3.0;
                    painter.vline(cr.left() + cr.width() * t, cr.y_range(), faint);
                    painter.hline(cr.x_range(), cr.top() + cr.height() * t, faint);
                }
                let len = (cr.width().min(cr.height()) / 4.0).clamp(6.0, 18.0);
                for (c, sx, sy) in [(cr.left_top(), 1.0, 1.0), (cr.right_top(), -1.0, 1.0), (cr.left_bottom(), 1.0, -1.0), (cr.right_bottom(), -1.0, -1.0)] {
                    let pts = vec![c + vec2(0.0, len * sy), c, c + vec2(len * sx, 0.0)];
                    painter.add(egui::Shape::line(pts.clone(), Stroke::new(6.0, Color32::from_black_alpha(110))));
                    painter.add(egui::Shape::line(pts, Stroke::new(3.5, Color32::WHITE)));
                }
            }
        }
    } else if ed.tab == Tab::Crop && ed.vw > 0.0 && ed.tool.is_none() {
        let g = painter.layout_no_wrap("在圖上拖曳，框出要保留的範圍".to_string(), theme::font(13.0), Color32::WHITE);
        let pill = Rect::from_center_size(pos2(rect.center().x, rect.top() + 24.0), g.size() + vec2(24.0, 12.0));
        painter.rect_filled(pill, CornerRadius::same(255), Color32::from_black_alpha(170));
        painter.galley(pill.min + vec2(12.0, 6.0), g, Color32::WHITE);
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
        // 聚光燈蓋住整個畫面：位置也算在內容裡，圖就是整個畫面
        let spot = a.kind == AnnKind::Spotlight;
        // 內容：位置、時間、編號以外的欄位
        let mut look = a.clone();
        (look.start, look.end, look.id) = (0.0, 0.0, 0);
        if !spot {
            (look.x, look.y) = (0.0, 0.0);
        }
        let key = hash_of(&(serde_json::to_string(&look).unwrap_or_default(), s.to_bits(), ed.vw.to_bits(), ed.vh.to_bits()));
        if ed.overlay.sprites.get(&a.id).is_some_and(|sp| sp.key == key) {
            continue;
        }
        // 範圍：標註實際佔的地方，再留一點邊（文字外框、箭頭頭部）；聚光燈是整個畫面（位置換算回標註的座標）
        let (ox, oy, ow, oh) = if spot {
            (-a.x, -a.y, ed.vw, ed.vh)
        } else {
            let (bx, by, bw, bh) = annotate::bbox(&look);
            let m = if a.kind == AnnKind::Image { 2.0 } else { a.size * 0.3 + 4.0 };
            (bx - m, by - m, bw + m * 2.0, bh + m * 2.0)
        };
        let (pw, ph) = ((ow * s).ceil().max(1.0) as u32, (oh * s).ceil().max(1.0) as u32);
        let Some(mut pm) = tiny_skia::Pixmap::new(pw.min(8192), ph.min(8192)) else { continue };
        // 聚光燈的 look 保留位置，畫在整個畫面上，不用平移
        let tf = if spot { tiny_skia::Transform::from_scale(s as f32, s as f32) } else { tiny_skia::Transform::from_scale(s as f32, s as f32).pre_translate(-ox as f32, -oy as f32) };
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

/// 旋轉把手的位置（影片座標）：(外框邊的中點, 把手)。平常在外框上方；超出影片時改放下方（才點得到）。
/// k = 畫面一像素是多少影片像素
fn rot_handle(a: &Ann, k: f64, vw: f64, vh: f64) -> ((f64, f64), (f64, f64)) {
    let (bx, by, bw, bh) = annotate::local_bbox(a);
    let d = ROT_HANDLE_PX * k;
    let inside = |(x, y): (f64, f64)| x >= 0.0 && x <= vw && y >= 0.0 && y <= vh;
    let above = annotate::to_world(a, bx + bw / 2.0, by - d);
    if inside(above) {
        return (annotate::to_world(a, bx + bw / 2.0, by), above);
    }
    (annotate::to_world(a, bx + bw / 2.0, by + bh), annotate::to_world(a, bx + bw / 2.0, by + bh + d))
}

/// 點到旋轉把手
fn on_rotate_handle(a: &Ann, x: f64, y: f64, k: f64, vw: f64, vh: f64) -> bool {
    if !a.kind.rotatable() {
        return false;
    }
    let (_, (rx, ry)) = rot_handle(a, k, vw, vh);
    (rx - x).hypot(ry - y) <= ROT_HIT_PX * k
}

/// 游標相對於標註中心的角度（度）
fn pointer_angle(a: &Ann, x: f64, y: f64) -> f64 {
    let (cx, cy) = annotate::center(a);
    (y - cy).atan2(x - cx).to_degrees()
}

/// 在裁切範圍上按下的位置：角（回傳對角當固定點，以及是不是左上—右下方向）或框內
enum CropGrab {
    Corner((f64, f64), bool),
    Inside,
}

fn crop_grab(ed: &Editor, x: f64, y: f64, tol: f64) -> Option<CropGrab> {
    if !ed.crop_on {
        return None;
    }
    let c = ed.spec.crop?;
    let (l, t, r, b) = (c.x, c.y, c.x + c.width, c.y + c.height);
    for (cx, cy, ax, ay, diag) in [(l, t, r, b, true), (r, t, l, b, false), (l, b, r, t, false), (r, b, l, t, true)] {
        if (x - cx).abs() <= tol && (y - cy).abs() <= tol {
            return Some(CropGrab::Corner((ax, ay), diag));
        }
    }
    (x > l && x < r && y > t && y < b).then_some(CropGrab::Inside)
}

/// 從固定的角拖到 (px, py) 的範圍；有比例時保持比例，並限制在畫面內
fn crop_from_drag(from: (f64, f64), (px, py): (f64, f64), ratio: Option<f64>, vw: f64, vh: f64) -> CropInput {
    let (dx, dy) = (px - from.0, py - from.1);
    let (mut w, mut h) = (dx.abs(), dy.abs());
    if let Some(r) = ratio.filter(|r| *r > 0.0) {
        if w / h.max(1e-9) > r {
            w = h * r;
        } else {
            h = w / r;
        }
        let room_w = if dx >= 0.0 { vw - from.0 } else { from.0 };
        let room_h = if dy >= 0.0 { vh - from.1 } else { from.1 };
        if w > room_w {
            (w, h) = (room_w, room_w / r);
        }
        if h > room_h {
            (w, h) = (room_h * r, room_h);
        }
    }
    let x = if dx >= 0.0 { from.0 } else { from.0 - w };
    let y = if dy >= 0.0 { from.1 } else { from.1 - h };
    CropInput { x, y, width: w, height: h }
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

    // 影片上轉滾輪：前後一張（Shift 一秒）；截圖沒有
    if !ed.is_shot() && resp.hovered() && scroll != egui::Vec2::ZERO && ed.wheel_at.elapsed() > Duration::from_millis(40) {
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
            } else if ed.selected().is_some_and(|a| on_rotate_handle(a, x, y, k, vw, vh)) {
                CursorIcon::Grab
            } else if ed.selected().is_some_and(|a| handle_at(a, x, y, k * 9.0).is_some()) {
                CursorIcon::ResizeNwSe
            } else if ann_at(ed, x, y, k * 6.0).is_some() {
                CursorIcon::Move
            } else if ed.tab == Tab::Crop {
                match crop_grab(ed, x, y, k * 12.0) {
                    Some(CropGrab::Corner(_, diag)) => {
                        if diag {
                            CursorIcon::ResizeNwSe
                        } else {
                            CursorIcon::ResizeNeSw
                        }
                    }
                    Some(CropGrab::Inside) => CursorIcon::Move,
                    None => CursorIcon::Crosshair,
                }
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
        // 選取中標註的旋轉 / 調整大小把手優先（連續放置表情、編號時也一樣，不會變成再放一個）
        if let Some(cur) = ed.selected() {
            if on_rotate_handle(cur, x, y, k, vw, vh) {
                ed.drag = Drag::Rotate { id: cur.id, a0: pointer_angle(cur, x, y), r0: cur.rot };
                return;
            }
            if let Some(h) = handle_at(cur, x, y, k * 9.0) {
                ed.drag = Drag::Resize { id: cur.id, handle: h, orig: cur.clone() };
                return;
            }
        }
        // 連續放置編號 / 表情時，點到已放好的同類標註 = 選取、移動它，不再新增
        let hit_same =
            ed.tool.filter(|t| matches!(t, Tool::Emoji | Tool::Ann(AnnKind::Step))).and_then(|t| ann_at(ed, x, y, k * 6.0).filter(|id| ed.anns.iter().any(|a| a.id == *id && a.kind == t.kind())));
        if let (Some(tool), None) = (ed.tool, hit_same) {
            ed.player.pause();
            ed.stop_preview();
            let kind = tool.kind();
            let mut a = ed.new_ann(kind, x, y);
            if kind == AnnKind::Pen {
                // 畫筆：一筆一個標註，畫完繼續畫下一筆
                annotate::set_pen_points(&mut a, &[(x, y)]);
                let id = a.id;
                ed.anns.push(a);
                ed.ann_sel = None;
                ed.drag = Drag::Pen { id, pts: vec![(x, y)] };
                return;
            }
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
        // 只有「裁切」分頁會框選裁切範圍，其他分頁點空白處不會動到裁切
        let cropping = ed.tab == Tab::Crop;
        if ed.ann_sel.is_some() {
            // 點空白處：取消選取標註（裁切分頁同時開始框選）
            ed.ann_sel = None;
            if !cropping {
                return;
            }
        } else if !cropping {
            return ed.toggle_play();
        }
        ed.stop_preview();
        ed.drag = match crop_grab(ed, x, y, k * 12.0) {
            // 拉角：以對角為固定點重新框選
            Some(CropGrab::Corner(anchor, _)) => Drag::Crop { from: anchor, prev_on: ed.crop_on, prev: ed.spec.crop },
            Some(CropGrab::Inside) => Drag::CropMove { from: (x, y), orig: ed.crop_rect() },
            None => Drag::Crop { from: (x, y), prev_on: ed.crop_on, prev: ed.spec.crop },
        };
        return;
    }

    let stage_drag = matches!(ed.drag, Drag::Crop { .. } | Drag::CropMove { .. } | Drag::Create { .. } | Drag::Move { .. } | Drag::Resize { .. } | Drag::Rotate { .. } | Drag::Pen { .. });
    if !stage_drag {
        return;
    }
    if down {
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            // 轉動時游標可以移到影片外面（不夾在影片範圍內，角度才準）
            let v = if matches!(ed.drag, Drag::Rotate { .. }) { (((p.x - rect.left()) / rect.width()) as f64 * vw, ((p.y - rect.top()) / rect.height()) as f64 * vh) } else { to_video(p) };
            drag_to(ed, v, ui.input(|i| i.modifiers.shift));
        }
    }
    if released || !down {
        end_drag(ed);
    }
}

/// shift：調整圖片大小時不保持比例
fn drag_to(ed: &mut Editor, (px, py): (f64, f64), shift: bool) {
    match std::mem::replace(&mut ed.drag, Drag::None) {
        Drag::Crop { from, prev_on, prev } => {
            let ratio = super::CROP_RATIOS.get(ed.crop_ratio).and_then(|c| c.1);
            let c = crop_from_drag(from, (px, py), ratio, ed.vw, ed.vh);
            // 拖超過一點點才算框選（只點一下不會把裁切清掉）
            if c.width >= 8.0 && c.height >= 8.0 {
                ed.crop_on = true;
                ed.spec.crop = Some(c);
            }
            ed.drag = Drag::Crop { from, prev_on, prev };
        }
        Drag::CropMove { from, orig } => {
            let x = (orig.x + px - from.0).clamp(0.0, (ed.vw - orig.width).max(0.0));
            let y = (orig.y + py - from.1).clamp(0.0, (ed.vh - orig.height).max(0.0));
            ed.spec.crop = Some(CropInput { x, y, ..orig });
            ed.drag = Drag::CropMove { from, orig };
        }
        Drag::Pen { id, mut pts } => {
            // 移動超過一點點才加點（線比較平滑、資料比較少）
            let k = (if ed.vh > 0.0 { ed.vh } else { 1080.0 }) / 1080.0;
            if pts.last().is_none_or(|&(lx, ly)| (px - lx).hypot(py - ly) >= 2.5 * k) {
                pts.push((px, py));
                if let Some(a) = ed.ann_mut(id) {
                    annotate::set_pen_points(a, &pts);
                }
            }
            ed.drag = Drag::Pen { id, pts };
        }
        Drag::Create { id, from } => {
            if let Some(a) = ed.ann_mut(id) {
                if a.kind == AnnKind::Magnify {
                    // 放大鏡是正圓：取寬高中比較大的
                    let d = (px - from.0).abs().max((py - from.1).abs());
                    a.x = if px < from.0 { from.0 - d } else { from.0 };
                    a.y = if py < from.1 { from.1 - d } else { from.1 };
                    a.w = d;
                    a.h = d;
                } else if a.kind == AnnKind::Arrow {
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
                    _ => {
                        // 在標註自己的方向（未旋轉）裡計算：拉右下角，左上角固定不動
                        let (lx, ly) = annotate::to_local(o, px, py);
                        if matches!(a.kind, AnnKind::Text | AnnKind::Step) {
                            // 依寬度等比例調整字級 / 大小
                            let ratio = ((lx - o.x) / o.w.max(1.0)).max(0.2);
                            a.size = (o.size * ratio).round().max(8.0);
                            annotate::measure(a);
                        } else if a.kind == AnnKind::Magnify {
                            let d = (lx - o.x).max(ly - o.y).max(16.0);
                            (a.w, a.h) = (d, d);
                        } else if a.kind == AnnKind::Image && !shift {
                            // 圖片保持比例（按住 Shift 可以自由調整）
                            let k = ((lx - o.x) / o.w.max(1.0)).max((ly - o.y) / o.h.max(1.0)).max(8.0 / o.w.min(o.h).max(1.0));
                            (a.w, a.h) = ((o.w * k).round(), (o.h * k).round());
                        } else {
                            a.w = (lx - o.x).max(8.0);
                            a.h = (ly - o.y).max(8.0);
                        }
                        // 旋轉時中心會移動：讓原本的左上角留在原位
                        let (cx, cy) = annotate::to_world(o, o.x + a.w / 2.0, o.y + a.h / 2.0);
                        a.x = cx - a.w / 2.0;
                        a.y = cy - a.h / 2.0;
                    }
                }
            }
            ed.drag = Drag::Resize { id, handle, orig };
        }
        Drag::Rotate { id, a0, r0 } => {
            if let Some(a) = ed.ann_mut(id) {
                // 轉的量 = 游標繞中心轉了多少（從哪裡按下去都不會跳）
                let mut deg = super::norm_deg(r0 + pointer_angle(a, px, py) - a0);
                // 靠近 15 度的倍數時吸附（容易轉回水平、轉成 45 / 90 度）
                let snap = (deg / 15.0).round() * 15.0;
                if (deg - snap).abs() < 4.0 {
                    deg = snap;
                }
                a.rot = super::norm_deg(deg.round());
            }
            ed.drag = Drag::Rotate { id, a0, r0 };
        }
        other => ed.drag = other,
    }
}

fn end_drag(ed: &mut Editor) {
    match std::mem::replace(&mut ed.drag, Drag::None) {
        Drag::Crop { prev_on, prev, .. } => {
            let tiny = !ed.crop_on || ed.spec.crop.is_none_or(|c| c.width < 8.0 || c.height < 8.0);
            if tiny {
                // 只點一下：還原；影片照舊是播放 / 暫停
                (ed.crop_on, ed.spec.crop) = (prev_on, prev);
                if !ed.is_shot() {
                    ed.toggle_play();
                }
            } else if let Some(c) = ed.spec.crop {
                ed.set_crop(c);
            }
        }
        Drag::CropMove { .. } => {
            if let Some(c) = ed.spec.crop {
                ed.set_crop(c);
            }
        }
        Drag::Pen { id, pts } => {
            // 只點一下：畫一個點
            if pts.len() == 1 {
                if let Some(a) = ed.ann_mut(id) {
                    annotate::set_pen_points(a, &[pts[0], (pts[0].0 + 0.5, pts[0].1)]);
                }
            }
        }
        Drag::Create { id, .. } => {
            // 拖曳太短：給個預設大小
            let k = (if ed.vh > 0.0 { ed.vh } else { 1080.0 }) / 1080.0;
            if let Some(a) = ed.ann_mut(id) {
                if a.kind == AnnKind::Magnify && a.w < 24.0 * k {
                    (a.w, a.h) = (240.0 * k, 240.0 * k);
                    a.x -= a.w / 2.0;
                    a.y -= a.h / 2.0;
                } else if a.kind == AnnKind::Arrow && a.w.hypot(a.h) < 20.0 * k {
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
        Tool::Ann(AnnKind::Pen) => {
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0;
                    c + vec2(-9.0 + t * 18.0, (t * std::f32::consts::TAU * 1.2).sin() * 5.0)
                })
                .collect();
            p.add(egui::Shape::line(pts, s));
        }
        Tool::Ann(AnnKind::Spotlight) => {
            p.rect_filled(Rect::from_center_size(c, vec2(20.0, 16.0)), CornerRadius::same(2), color.gamma_multiply(0.45));
            p.circle_filled(c, 5.0, Color32::WHITE);
            p.circle_stroke(c, 5.0, Stroke::new(1.2, color));
        }
        Tool::Ann(AnnKind::Image) => {
            theme::paint_icon(p, Rect::from_center_size(c, vec2(20.0, 20.0)), theme::Icon::Image, color);
        }
        Tool::Ann(AnnKind::Magnify) => {
            p.circle_stroke(c + vec2(-2.0, -2.0), 7.0, s);
            p.line_segment([c + vec2(3.0, 3.0), c + vec2(9.0, 9.0)], Stroke::new(2.6, color));
        }
    }
}
