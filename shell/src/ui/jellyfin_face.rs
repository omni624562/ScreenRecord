//! 人物照片：在播放 Jellyfin 影片時按「人物照片」，用正在看的這一格框出臉，設成 Jellyfin 上那個人物的照片
//! （「依這些找」的人物清單、Jellyfin 的演員頁都看得到）。框是 2:3 直的，拖曳移動、拖右下角調整大小。
//! 見 core::cover::frame_at、core::jellyfin::Jellyfin::set_person_photo（人物也是一個項目，照片就是它的主要圖片）

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Id, Layout, Rect, RichText, Sense, Stroke};
use screenrecorder_core::jellyfin;
use screenrecorder_core::{tr, trf};
use tiny_skia::Pixmap;

/// 框的寬高比（Jellyfin 的人物照片是直的 2:3）
const ASPECT: f32 = 2.0 / 3.0;

pub struct FaceDlg {
    frame: Pixmap,
    tex: egui::TextureHandle,
    /// 這支影片的人物（編號, 名稱）；編號找不到（伺服器上還沒有這個人物）時 None
    people: Vec<(Option<String>, String)>,
    pick: usize,
    /// 框（以圖片寬高為 1 的比例）
    crop: Rect,
    /// 正在拖曳：（調整大小？, 按下時的框）
    drag: Option<(bool, Rect)>,
    saving: bool,
}

/// 打開（frame 是正在看的這一格；people 是這支影片的人物與它們在伺服器上的編號）
pub fn open(app: &mut UiApp, frame: Pixmap, people: Vec<(Option<String>, String)>) {
    let img = egui::ColorImage::from_rgba_premultiplied([frame.width() as usize, frame.height() as usize], frame.data());
    let tex = app.ctx.load_texture("jf-face-frame", img, egui::TextureOptions::LINEAR);
    // 一開始：高度 70%、置中
    let (iw, ih) = (frame.width() as f32, frame.height() as f32);
    let h = 0.7;
    let w = (h * ih * ASPECT / iw).min(1.0);
    let crop = Rect::from_center_size(pos2(0.5, 0.5), vec2(w, h));
    app.jf_face = Some(FaceDlg { frame, tex, people, pick: 0, crop, drag: None, saving: false });
}

/// 框在圖片範圍內、維持 2:3（size_w：以寬為準調整高）
fn clamp(c: Rect, iw: f32, ih: f32) -> Rect {
    let min_w = 40.0 / iw;
    let mut w = c.width().clamp(min_w, 1.0);
    let mut h = w * iw / ASPECT / ih;
    if h > 1.0 {
        h = 1.0;
        w = h * ih * ASPECT / iw;
    }
    let x = c.min.x.clamp(0.0, 1.0 - w);
    let y = c.min.y.clamp(0.0, 1.0 - h);
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

/// 切出框起來的那一塊
fn crop(pm: &Pixmap, c: Rect) -> Option<Pixmap> {
    let (iw, ih) = (pm.width() as f32, pm.height() as f32);
    let (x0, y0) = ((c.min.x * iw) as u32, (c.min.y * ih) as u32);
    let (w, h) = (((c.width() * iw) as u32).max(1).min(pm.width() - x0), ((c.height() * ih) as u32).max(1).min(pm.height() - y0));
    let mut out = Pixmap::new(w, h)?;
    let stride = pm.width() as usize * 4;
    for row in 0..h as usize {
        let from = (y0 as usize + row) * stride + x0 as usize * 4;
        out.data_mut()[row * w as usize * 4..(row + 1) * w as usize * 4].copy_from_slice(&pm.data()[from..from + w as usize * 4]);
    }
    Some(out)
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_face.take() else { return };
    let mut close = false;
    let mut save = false;
    let (iw, ih) = (d.frame.width() as f32, d.frame.height() as f32);
    let modal = egui::Modal::new(Id::new("jellyfin-face")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width((ctx.content_rect().width() - 80.0).clamp(380.0, 640.0));
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("人物照片", "Person photo")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.label(
            RichText::new(tr!(
                "把框拖到臉上（拖右下角調整大小），選這是誰，設成 Jellyfin 上的人物照片。",
                "Drag the box over the face (drag its corner to resize), choose who it is, and set it as their photo on Jellyfin."
            ))
            .font(theme::font(12.0))
            .color(p.muted),
        );
        ui.add_space(8.0);
        // 圖片（放得下的最大）與框
        let max_w = ui.available_width();
        let scale = (max_w / iw).min(360.0 / ih);
        let (img_rect, resp) = ui.allocate_exact_size(vec2(iw * scale, ih * scale), Sense::drag());
        ui.painter().image(d.tex.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
        let to_screen = |c: Rect| Rect::from_min_max(img_rect.min + c.min.to_vec2() * img_rect.size(), img_rect.min + c.max.to_vec2() * img_rect.size());
        let box_rect = to_screen(d.crop);
        let handle = Rect::from_center_size(box_rect.max, vec2(16.0, 16.0));
        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                d.drag = if handle.contains(pos) {
                    Some((true, d.crop))
                } else if box_rect.contains(pos) {
                    Some((false, d.crop))
                } else {
                    None
                };
            }
        }
        if let (Some((resize, start)), Some(origin), Some(now)) = (d.drag, ui.input(|i| i.pointer.press_origin()), resp.interact_pointer_pos()) {
            let delta = (now - origin) / img_rect.size();
            d.crop = if resize { clamp(Rect::from_min_size(start.min, start.size() + vec2(delta.x, delta.x)), iw, ih) } else { clamp(start.translate(delta), iw, ih) };
        }
        if resp.drag_stopped() {
            d.drag = None;
        }
        // 框外變暗、框線、右下角把手
        let box_rect = to_screen(d.crop);
        let dim = egui::Color32::from_black_alpha(130);
        for r in [
            Rect::from_min_max(img_rect.min, pos2(img_rect.max.x, box_rect.min.y)),
            Rect::from_min_max(pos2(img_rect.min.x, box_rect.max.y), img_rect.max),
            Rect::from_min_max(pos2(img_rect.min.x, box_rect.min.y), pos2(box_rect.min.x, box_rect.max.y)),
            Rect::from_min_max(pos2(box_rect.max.x, box_rect.min.y), pos2(img_rect.max.x, box_rect.max.y)),
        ] {
            ui.painter().rect_filled(r, 0.0, dim);
        }
        ui.painter().rect_stroke(box_rect, 2.0, Stroke::new(2.0, egui::Color32::WHITE), egui::StrokeKind::Inside);
        ui.painter().rect_filled(Rect::from_center_size(box_rect.max - vec2(3.0, 3.0), vec2(12.0, 12.0)), 2.0, egui::Color32::WHITE);
        if ui.rect_contains_pointer(Rect::from_center_size(box_rect.max, vec2(16.0, 16.0))) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
        } else if ui.rect_contains_pointer(box_rect) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            ui.label(RichText::new(tr!("這是：", "This is:")).font(theme::font_bold(12.5)).color(p.muted));
            if d.people.is_empty() {
                ui.label(RichText::new(tr!("這支影片還沒有設人物：先在「影片資訊」加上人物", "This video has no people yet: add them in Details first")).font(theme::font(12.5)).color(p.warn));
            }
            for (i, (id, name)) in d.people.iter().enumerate() {
                let r = ui.add_enabled(id.is_some(), egui::Button::selectable(d.pick == i, name));
                let r = if id.is_none() {
                    r.on_disabled_hover_text(tr!("Jellyfin 上還找不到這個人物（剛加的話等一下再試）", "Jellyfin doesn't have this person yet (if just added, try again shortly)"))
                } else {
                    r
                };
                if r.clicked() {
                    d.pick = i;
                }
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let who = d.people.get(d.pick).filter(|(id, _)| id.is_some()).map(|(_, n)| n.clone());
                let label = match (&who, d.saving) {
                    (_, true) => tr!("正在設定…", "Saving…").to_string(),
                    (Some(n), _) => trf!("設成「{n}」的照片", "Set as {n}'s photo"),
                    (None, _) => tr!("設成人物照片", "Set as person photo").to_string(),
                };
                if Btn::new(label).primary().enabled(who.is_some() && !d.saving).show(ui).clicked() {
                    save = true;
                }
                if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                    close = true;
                }
            });
        });
    });
    if (close || modal.should_close()) && !d.saving {
        return;
    }
    if save {
        let Some((Some(id), name)) = d.people.get(d.pick).cloned() else {
            app.jf_face = Some(d);
            return;
        };
        let Some(jf) = app.jellyfin() else { return };
        let Some(pm) = crop(&d.frame, d.crop) else { return };
        d.saving = true;
        let id2 = id.clone();
        app.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    let jpeg = screenrecorder_core::shot_edit::encode_jpeg(&pm, 90)?;
                    jf.set_person_photo(&id, &jpeg)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |app, r| match r {
                Ok(()) => {
                    app.jf_face = None;
                    super::jellyfin_browse::face_changed(app, &id2);
                    app.toast(trf!("已設成「{name}」的照片", "Set as {name}'s photo"), false);
                }
                Err(e) => {
                    if let Some(d) = app.jf_face.as_mut() {
                        d.saving = false;
                    }
                    app.toast(trf!("沒有設成人物照片：{e}", "Couldn't set the person photo: {e}"), true);
                }
            },
        );
    }
    app.jf_face = Some(d);
}

/// 這支影片的人物與伺服器上的編號（「依這些找」的人物清單裡找；只差空白、大小寫、全形半形的算同一個）
pub fn people_with_ids(names: &[String], persons: &[(String, String, usize)]) -> Vec<(Option<String>, String)> {
    names.iter().map(|n| (persons.iter().find(|(_, p, _)| jellyfin::name_key(p) == jellyfin::name_key(n)).map(|(id, _, _)| id.clone()), n.clone())).collect()
}
