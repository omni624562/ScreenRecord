//! 人物照片：在播放 Jellyfin 影片時按「人物照片」，用正在看的這一格（或電腦裡的圖片、拖進來的圖片、剪貼簿的圖片）框出臉，
//! 設成 Jellyfin 上那個人物的照片（「依這些找」的人物清單、Jellyfin 的演員頁都看得到）。
//! 框可以拖曳移動、拖右下角調整大小、拖上方的圓點旋轉；比例可選 2:3（Jellyfin 的人物照片是直的）、1:1 或自由。
//! 見 core::cover::frame_at、core::jellyfin::Jellyfin::set_person_photo（人物也是一個項目，照片就是它的主要圖片）

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use screenrecorder_core::jellyfin;
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;
use tiny_skia::Pixmap;

/// 框的比例
#[derive(Clone, Copy, PartialEq)]
enum Ratio {
    /// 2:3 直的（Jellyfin 人物照片的比例）
    Portrait,
    Square,
    Free,
}

impl Ratio {
    /// 寬 / 高；自由時 None
    fn value(self) -> Option<f32> {
        match self {
            Ratio::Portrait => Some(2.0 / 3.0),
            Ratio::Square => Some(1.0),
            Ratio::Free => None,
        }
    }
}

/// 框（圖片像素）：中心、寬高、角度（弧度，順時針）
#[derive(Clone, Copy, PartialEq)]
struct Frame {
    c: Pos2,
    w: f32,
    h: f32,
    a: f32,
}

/// 拖的是哪裡
#[derive(Clone, Copy)]
enum Grab {
    Move,
    Resize,
    Rotate,
}

/// 框的最小邊長（像素）
const MIN_SIDE: f32 = 40.0;
/// 旋轉把手在框上緣再往上多遠（螢幕點）
const ROT_GAP: f32 = 22.0;

pub struct FaceDlg {
    frame: Pixmap,
    tex: egui::TextureHandle,
    /// 影片的那一格（換成其他圖片後可以換回來）；on_video = 正在用它
    video: Option<Pixmap>,
    on_video: bool,
    /// 這支影片的人物（編號, 名稱）；編號找不到（伺服器上還沒有這個人物）時 None
    people: Vec<(Option<String>, String)>,
    /// 人物現在的照片（編號 → 小圖；沒有照片時 None；還沒讀到的不在裡面）
    faces: HashMap<String, Option<egui::TextureHandle>>,
    pick: usize,
    /// 這支影片的編號與看的使用者（向伺服器重讀人物用）
    item: String,
    user: String,
    /// 正在向伺服器讀人物
    loading: bool,
    crop: Frame,
    /// 開啟（或換圖片）時的框（框過了又按 Esc、點外面時不直接關）
    crop0: Frame,
    ratio: Ratio,
    /// 正在拖曳：拖哪裡、按下時的框
    drag: Option<(Grab, Frame)>,
    saving: bool,
    /// 框過了又按 Esc、點外面：不關，提示要放棄請按「取消」
    keep_hint: bool,
    /// 換圖片失敗的原因
    error: Option<String>,
}

/// 一開始的框：高度 70%、置中（比例照目前選的，自由時用 2:3）
fn first_frame(pm: &Pixmap, ratio: Ratio) -> Frame {
    let (iw, ih) = (pm.width() as f32, pm.height() as f32);
    let r = ratio.value().unwrap_or(2.0 / 3.0);
    let mut h = ih * 0.7;
    let mut w = h * r;
    if w > iw * 0.9 {
        w = iw * 0.9;
        h = w / r;
    }
    clamp(Frame { c: pos2(iw / 2.0, ih / 2.0), w, h, a: 0.0 }, ratio, iw, ih)
}

fn texture(ctx: &egui::Context, pm: &Pixmap) -> egui::TextureHandle {
    let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
    ctx.load_texture("jf-face-frame", img, egui::TextureOptions::LINEAR)
}

/// 開啟（frame 是正在看的這一格；people 是清單上記得的人物，接著向伺服器重讀）
pub fn open(app: &mut UiApp, frame: Pixmap, item: String, user: String, people: Vec<(Option<String>, String)>) {
    let tex = texture(&app.ctx, &frame);
    let crop = first_frame(&frame, Ratio::Portrait);
    app.jf_face = Some(FaceDlg {
        video: Some(frame.clone()),
        on_video: true,
        frame,
        tex,
        people,
        faces: HashMap::new(),
        pick: 0,
        item,
        user,
        loading: false,
        crop,
        crop0: crop,
        ratio: Ratio::Portrait,
        drag: None,
        saving: false,
        keep_hint: false,
        error: None,
    });
    reload(app);
}

/// 換成另一張圖片（電腦裡的、拖進來的、剪貼簿的，或換回影片畫面）：框重新放在中間
fn use_image(d: &mut FaceDlg, ctx: &egui::Context, pm: Pixmap) {
    d.tex = texture(ctx, &pm);
    d.crop = first_frame(&pm, d.ratio);
    d.crop0 = d.crop;
    d.frame = pm;
    d.on_video = false;
    d.error = None;
    d.keep_hint = false;
}

/// 讀圖片檔換上去
fn use_file(d: &mut FaceDlg, ctx: &egui::Context, path: &std::path::Path) {
    match std::fs::read(path).ok().and_then(|b| screenrecorder_core::picture::decode_pixmap(&b)) {
        Some(pm) => use_image(d, ctx, pm),
        None => d.error = Some(trf!("讀不到這張圖片：{}", "Couldn't read this image: {}", path.display())),
    }
}

/// 讀這支影片人物現在的照片（標出哪些人已經有照片）
fn load_faces(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_face.as_ref() else { return };
    let ids: Vec<String> = d.people.iter().filter_map(|(id, _)| id.clone()).filter(|id| !d.faces.contains_key(id)).collect();
    for id in ids {
        let (jf, ctx) = (jf.clone(), app.ctx.clone());
        app.spawn(
            async move {
                let key = id.clone();
                let img = tokio::task::spawn_blocking(move || {
                    let pm = screenrecorder_core::picture::decode_pixmap(&jf.poster(&key)?)?;
                    Some(egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
                })
                .await
                .ok()
                .flatten();
                (id.clone(), img.map(|i| ctx.load_texture(format!("jf-face-has-{id}"), i, egui::TextureOptions::LINEAR)))
            },
            |app, (id, tex): (String, Option<egui::TextureHandle>)| {
                if let Some(d) = app.jf_face.as_mut() {
                    d.faces.insert(id, tex);
                }
            },
        );
    }
}

/// 向伺服器重讀這支影片的人物與編號（清單上記得的可能是舊的：剛在「影片資訊」加的人物、剛建立的人物編號）
pub fn reload(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_face.as_mut() else { return };
    d.loading = true;
    let (item, user) = (d.item.clone(), d.user.clone());
    let item2 = item.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.item_people(&user, &item)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        let Some(d) = app.jf_face.as_mut().filter(|d| d.item == item2) else { return };
        d.loading = false;
        match r {
            Ok(people) => {
                // 原本選的人還在就繼續選他
                let picked = d.people.get(d.pick).map(|(_, n)| n.clone());
                d.pick = picked.and_then(|n| people.iter().position(|(_, p)| *p == n)).or_else(|| people.iter().position(|(id, _)| id.is_some())).unwrap_or(0);
                d.people = people;
                load_faces(app);
            }
            Err(e) => app.toast(trf!("讀不到這支影片的人物：{e}", "Couldn't read this video's people: {e}"), true),
        }
    });
}

/// 「影片資訊」存好了：人物照片開著、是同一支影片就重讀人物
pub fn info_saved(app: &mut UiApp, item: &str) {
    if app.jf_face.as_ref().is_some_and(|d| d.item == item) {
        reload(app);
    }
}

/// 轉 a 弧度（順時針，螢幕座標 y 朝下）
fn rot(v: Vec2, a: f32) -> Vec2 {
    let (s, c) = a.sin_cos();
    vec2(v.x * c - v.y * s, v.x * s + v.y * c)
}

/// 框的中心留在圖片裡、邊長至少 MIN_SIDE、至多圖片較長邊的 1.5 倍；有固定比例時維持比例（以高為準）
fn clamp(mut f: Frame, ratio: Ratio, iw: f32, ih: f32) -> Frame {
    let max = iw.max(ih) * 1.5;
    if let Some(r) = ratio.value() {
        f.h = f.h.clamp(MIN_SIDE.max(MIN_SIDE / r), (max / r).min(max));
        f.w = f.h * r;
    } else {
        f.w = f.w.clamp(MIN_SIDE, max);
        f.h = f.h.clamp(MIN_SIDE, max);
    }
    f.c = pos2(f.c.x.clamp(0.0, iw), f.c.y.clamp(0.0, ih));
    f
}

/// 切出框起來的那一塊（轉正；框超出圖片的地方填黑）
fn crop(pm: &Pixmap, f: Frame) -> Option<Pixmap> {
    let (w, h) = (f.w.round().max(1.0) as u32, f.h.round().max(1.0) as u32);
    let mut out = Pixmap::new(w, h)?;
    out.fill(tiny_skia::Color::BLACK);
    let t = tiny_skia::Transform::from_translate(w as f32 / 2.0, h as f32 / 2.0).pre_rotate(-f.a.to_degrees()).pre_translate(-f.c.x, -f.c.y);
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
    out.draw_pixmap(0, 0, pm.as_ref(), &paint, t, None);
    Some(out)
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_face.take() else { return };
    let mut close = false;
    let mut save = false;
    let mut reload_now = false;
    let mut pick_file = false;
    // 拖進來的圖片檔（開著這個視窗時不算匯入影片）、Ctrl+V 貼上剪貼簿的圖片
    let (hovering, dropped, paste) = ctx.input(|i| {
        let dropped = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| !p.as_os_str().is_empty());
        let paste = i.modifiers.command && i.key_pressed(egui::Key::V) || i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
        (!i.raw.hovered_files.is_empty(), dropped, paste)
    });
    if let Some(path) = dropped {
        use_file(&mut d, ctx, &path);
    }
    if paste && !d.saving {
        match screenrecorder_core::clipboard::read_image() {
            Some(pm) => use_image(&mut d, ctx, pm),
            None => {
                d.error = Some(
                    tr!("剪貼簿裡沒有圖片（在網頁的圖片上按右鍵「複製圖片」再貼上）", "There's no image on the clipboard (right-click an image on a web page, choose Copy image, then paste)").into(),
                )
            }
        }
    }
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
                "把框拖到臉上：拖右下角調整大小、拖上方的圓點旋轉。也可以換成電腦裡的圖片（或拖進來、按 Ctrl+V 貼上）。",
                "Drag the box over the face: drag its corner to resize, drag the dot above it to rotate. You can also use an image from your computer (or drag one in, or press Ctrl+V to paste)."
            ))
            .font(theme::font(12.0))
            .color(p.muted),
        );
        ui.add_space(6.0);
        // 圖片來源、框的比例、轉正
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if Btn::new(tr!("從電腦選圖片…", "Choose image…")).icon(Icon::Image).small().enabled(!d.saving).show(ui).clicked() {
                pick_file = true;
            }
            if Btn::new(tr!("貼上圖片", "Paste image")).small().enabled(!d.saving).tooltip(tr!("貼上剪貼簿裡的圖片（Ctrl+V）", "Paste the image on the clipboard (Ctrl+V)")).show(ui).clicked()
            {
                match screenrecorder_core::clipboard::read_image() {
                    Some(pm) => use_image(&mut d, ui.ctx(), pm),
                    None => {
                        d.error = Some(
                            tr!("剪貼簿裡沒有圖片（在網頁的圖片上按右鍵「複製圖片」再貼上）", "There's no image on the clipboard (right-click an image on a web page, choose Copy image, then paste)")
                                .into(),
                        )
                    }
                }
            }
            if !d.on_video {
                if let Some(v) = d.video.clone() {
                    if Btn::new(tr!("用影片畫面", "Use video frame")).small().ghost().enabled(!d.saving).show(ui).clicked() {
                        use_image(&mut d, ui.ctx(), v);
                        d.on_video = true;
                    }
                }
            }
            ui.add_space(8.0);
            ui.label(RichText::new(tr!("比例", "Ratio")).font(theme::font(12.0)).color(p.muted));
            for (r, label) in [(Ratio::Portrait, "2:3"), (Ratio::Square, "1:1"), (Ratio::Free, tr!("自由", "Free"))] {
                if ui.add(egui::Button::selectable(d.ratio == r, label)).clicked() && d.ratio != r {
                    d.ratio = r;
                    d.crop = clamp(d.crop, r, iw, ih);
                }
            }
            if d.crop.a != 0.0 && Btn::new(tr!("轉正", "Straighten")).small().ghost().show(ui).clicked() {
                d.crop.a = 0.0;
            }
        });
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).font(theme::font(12.0)).color(p.err));
        }
        ui.add_space(6.0);
        // 圖片（放得下的最大）與框；四周留一點空間給旋轉把手
        let pad = ROT_GAP + 8.0;
        let max_w = ui.available_width() - pad * 2.0;
        let scale = (max_w / iw).min(360.0 / ih);
        // 圖片置中（直的圖片不要靠左）
        let (area, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ih * scale + pad * 2.0), Sense::drag());
        let img_rect = Rect::from_center_size(area.center(), vec2(iw * scale, ih * scale));
        let painter = ui.painter_at(area);
        painter.image(d.tex.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
        let to_screen = |p: Pos2| img_rect.min + p.to_vec2() * scale;
        // 框的四角（螢幕）、旋轉把手
        let corners = |f: Frame| {
            let c = to_screen(f.c);
            let (hw, hh) = (f.w * scale / 2.0, f.h * scale / 2.0);
            [vec2(-hw, -hh), vec2(hw, -hh), vec2(hw, hh), vec2(-hw, hh)].map(|v| c + rot(v, f.a))
        };
        let rot_handle = |f: Frame| to_screen(f.c) + rot(vec2(0.0, -f.h * scale / 2.0 - ROT_GAP), f.a);
        let local = |f: Frame, pos: Pos2| rot(pos - to_screen(f.c), -f.a);
        let hit = |f: Frame, pos: Pos2| {
            let l = local(f, pos);
            let (hw, hh) = (f.w * scale / 2.0, f.h * scale / 2.0);
            if pos.distance(rot_handle(f)) <= 10.0 {
                Some(Grab::Rotate)
            } else if pos.distance(corners(f)[2]) <= 10.0 {
                Some(Grab::Resize)
            } else if l.x.abs() <= hw && l.y.abs() <= hh {
                Some(Grab::Move)
            } else {
                None
            }
        };
        if resp.drag_started() {
            // 用按下去的位置判斷（開始拖的時候滑鼠已經移動了一點，小把手會判斷不到）
            d.drag = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).and_then(|pos| hit(d.crop, pos)).map(|g| (g, d.crop));
        }
        if let (Some((grab, start)), Some(origin), Some(now)) = (d.drag, ui.input(|i| i.pointer.press_origin()), resp.interact_pointer_pos()) {
            let mut f = start;
            match grab {
                Grab::Move => f.c = start.c + (now - origin) / scale,
                Grab::Resize => {
                    // 以中心為準：拖到哪裡，框的一半就到哪裡
                    let l = local(start, now) / scale;
                    f.w = l.x.abs() * 2.0;
                    f.h = l.y.abs() * 2.0;
                    if let Some(r) = d.ratio.value() {
                        f.h = f.h.max(f.w / r);
                    }
                }
                Grab::Rotate => {
                    let v = now - to_screen(start.c);
                    let mut a = v.x.atan2(-v.y);
                    // 接近正的時候吸過去（每 90 度）
                    let q = (a / std::f32::consts::FRAC_PI_2).round() * std::f32::consts::FRAC_PI_2;
                    if (a - q).abs() < 3f32.to_radians() {
                        a = q;
                    }
                    f.a = a;
                }
            }
            d.crop = clamp(f, d.ratio, iw, ih);
        }
        if resp.drag_stopped() {
            d.drag = None;
        }
        // 框外變暗；框裡用原本的亮度再畫一次（框可以是斜的）
        painter.rect_filled(img_rect, 0.0, egui::Color32::from_black_alpha(130));
        let pts = corners(d.crop);
        {
            let mut mesh = egui::Mesh::with_texture(d.tex.id());
            for q in pts {
                let px = (q - img_rect.min) / scale;
                mesh.vertices.push(egui::epaint::Vertex { pos: q, uv: pos2(px.x / iw, px.y / ih), color: egui::Color32::WHITE });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            ui.painter_at(img_rect).add(egui::Shape::mesh(mesh));
        }
        let white = egui::Color32::WHITE;
        painter.add(egui::Shape::closed_line(pts.to_vec(), Stroke::new(2.0, white)));
        // 右下角把手（調整大小）、上方圓點（旋轉）
        let corner = pts[2];
        painter.add(egui::Shape::convex_polygon([vec2(-6.0, -6.0), vec2(6.0, -6.0), vec2(6.0, 6.0), vec2(-6.0, 6.0)].map(|v| corner + rot(v, d.crop.a)).to_vec(), white, Stroke::NONE));
        let top_mid = to_screen(d.crop.c) + rot(vec2(0.0, -d.crop.h * scale / 2.0), d.crop.a);
        let rh = rot_handle(d.crop);
        painter.line_segment([top_mid, rh], Stroke::new(1.5, white));
        painter.circle(rh, 6.5, white, Stroke::new(1.5, p.accent));
        // 拖著圖片檔經過：放開換成這張
        if hovering {
            painter.rect_filled(img_rect, 4.0, egui::Color32::from_black_alpha(150));
            painter.text(img_rect.center(), egui::Align2::CENTER_CENTER, tr!("放開換成這張圖片", "Release to use this image"), theme::font_bold(16.0), white);
        }
        // 游標
        let cursor = match d.drag.map(|(g, _)| g).or_else(|| ui.ctx().pointer_hover_pos().filter(|p| area.contains(*p)).and_then(|p| hit(d.crop, p))) {
            Some(Grab::Resize) => Some(egui::CursorIcon::ResizeNwSe),
            Some(Grab::Rotate) => Some(egui::CursorIcon::Alias),
            Some(Grab::Move) => Some(if d.drag.is_some() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab }),
            None => None,
        };
        if let Some(c) = cursor {
            ui.ctx().set_cursor_icon(c);
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            ui.label(RichText::new(tr!("這是：", "This is:")).font(theme::font_bold(12.5)).color(p.muted));
            if d.loading {
                ui.spinner();
                ui.label(RichText::new(tr!("正在讀取人物…", "Loading people…")).font(theme::font(12.5)).color(p.muted));
            } else if d.people.is_empty() {
                ui.label(
                    RichText::new(tr!("這支影片還沒有設人物：先在「影片資訊」加上人物並儲存", "This video has no people yet: add them in Details and save first"))
                        .font(theme::font(12.5))
                        .color(p.warn),
                );
            }
            for (i, (id, name)) in d.people.iter().enumerate() {
                // 已經有照片的：名字前面放小頭像（設定後會換掉）
                let face = id.as_ref().and_then(|id| d.faces.get(id)).cloned().flatten();
                let r = match &face {
                    Some(tex) => {
                        let img = egui::Image::new(tex).fit_to_exact_size(vec2(18.0, 18.0)).corner_radius(9.0);
                        ui.add_enabled(id.is_some(), egui::Button::selectable(d.pick == i, egui::Atoms::new((img, name.as_str()))))
                    }
                    None => ui.add_enabled(id.is_some(), egui::Button::selectable(d.pick == i, name)),
                };
                let r = if id.is_none() {
                    r.on_disabled_hover_text(tr!("Jellyfin 上還找不到這個人物（剛加的話等一下再試）", "Jellyfin doesn't have this person yet (if just added, try again shortly)"))
                } else if face.is_some() {
                    r.on_hover_text(tr!("已經有照片：設定後會換成新的", "Already has a photo: setting one replaces it"))
                } else if id.as_ref().is_some_and(|id| d.faces.contains_key(id)) {
                    r.on_hover_text(tr!("還沒有照片", "No photo yet"))
                } else {
                    r
                };
                if r.clicked() {
                    d.pick = i;
                }
            }
            // 沒有人物、或有人物還沒有編號：可以再讀一次（存好「影片資訊」後也會自動重讀）
            if !d.loading && (d.people.is_empty() || d.people.iter().any(|(id, _)| id.is_none())) && Btn::new(tr!("重新讀取", "Reload")).ghost().small().show(ui).clicked() {
                reload_now = true;
            }
        });
        if d.faces.values().any(Option::is_some) {
            ui.label(RichText::new(tr!("名字前有小頭像的已經有照片", "People with a small picture already have a photo")).font(theme::font(11.5)).color(p.muted));
        }
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
                if d.keep_hint && d.crop != d.crop0 {
                    ui.label(RichText::new(tr!("要放棄請按「取消」", "Click Cancel to discard")).font(theme::font(12.0)).color(theme::pal(ui).warn));
                }
            });
        });
    });
    // Esc、點外面（拖框時很容易點到外面）：框過了就不直接關，提示按「取消」
    let dismiss = modal.should_close();
    if dismiss && d.crop != d.crop0 {
        d.keep_hint = true;
    }
    if (close || dismiss && d.crop == d.crop0) && !d.saving {
        return;
    }
    if pick_file {
        app.jf_face = Some(d);
        app.spawn(async { tokio::task::spawn_blocking(screenrecorder_core::picture::pick_file).await.unwrap_or_else(|e| Err(e.to_string())) }, |app, r| {
            let ctx = app.ctx.clone();
            let Some(d) = app.jf_face.as_mut() else { return };
            match r {
                Ok(Some(path)) => use_file(d, &ctx, &path),
                Ok(None) => {}
                Err(e) => d.error = Some(e),
            }
        });
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
    if reload_now {
        reload(app);
    }
}

/// 這支影片的人物與伺服器上的編號（「依這些找」的人物清單裡找；只差空白、大小寫、全形半形的算同一個）
pub fn people_with_ids(names: &[String], persons: &[(String, String, usize)]) -> Vec<(Option<String>, String)> {
    names.iter().map(|n| (persons.iter().find(|(_, p, _)| jellyfin::name_key(p) == jellyfin::name_key(n)).map(|(id, _, _)| id.clone()), n.clone())).collect()
}
