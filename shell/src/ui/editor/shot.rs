//! 截圖編輯（剪輯視窗的截圖模式）：沒有時間軸；標註與裁切和剪輯影片相同，另有輸出設定（外框、陰影、大小）。
//! 存成 *_編輯.png（原圖保留）並複製到剪貼簿；編輯設定記在專案裡，之後開啟編輯過的圖可以從原圖重新套用並修改。

use super::{BannerInfo, Editor, Pl, Tab};
use crate::ui::dialogs::file_name;
use crate::ui::theme::{self, segmented, switch, Btn, Icon};
use crate::ui::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, CursorIcon, Layout, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions};
use screenrecorder_core::actions::{self, ProjectMatch, ShotProjectInfo};
use screenrecorder_core::annotate::{self, AnnKind};
use screenrecorder_core::edit::{normalize_crop, CropInput, EditSpec};
use screenrecorder_core::ocr;
use screenrecorder_core::picture;
use screenrecorder_core::player::Frame;
use screenrecorder_core::shot_edit::{self, ShotSpec, BACKGROUNDS, BORDER_COLORS, PADDINGS, RADII, SCALES};
use screenrecorder_core::types::{LibraryEntry, MediaInfo};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 編輯時顯示的畫面最大邊長（輸出用原圖）
const PREVIEW_MAX: u32 = 2560;

/// 輸出設定（復原 / 重做也記這些）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotOpts {
    pub border: Option<String>,
    pub border_width: f64,
    pub shadow: bool,
    pub scale: u32,
    /// 整張圖順時針轉的角度（0 / 90 / 180 / 270）
    #[serde(default)]
    pub rotate: u32,
    #[serde(default)]
    pub radius: u32,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default = "default_padding")]
    pub padding: u32,
}

fn default_padding() -> u32 {
    ShotSpec::default().padding
}

impl Default for ShotOpts {
    fn default() -> Self {
        let d = ShotSpec::default();
        Self { border: d.border, border_width: d.border_width, shadow: d.shadow, scale: d.scale, rotate: 0, radius: d.radius, background: d.background, padding: d.padding }
    }
}

pub struct Shot {
    /// 原圖（編輯過的圖從原圖重新套用）
    path: String,
    opts: ShotOpts,
    /// 開啟的那張編輯過的圖（可以改回直接編輯它）
    opened_edit: Option<String>,
    /// 「輸出」分頁的預覽：(內容, 圖)
    preview: Option<(String, TextureHandle)>,
    /// 還沒旋轉的畫面（預覽解析度）與原圖大小
    base: Frame,
    orig: (u32, u32),
    /// 目前 ed.frame 轉了幾度（和 opts.rotate 不同時重新轉）
    applied: u32,
}

impl Shot {
    pub fn opts(&self) -> ShotOpts {
        self.opts.clone()
    }
    pub fn set_opts(&mut self, o: ShotOpts) {
        self.opts = o;
    }
}

/// 需要 app 的動作（按鈕與快速鍵）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Save,
    Copy,
    Pin,
    Ocr,
    /// 不從原圖重新套用，直接編輯開啟的那張
    Plain,
    /// 影片：目前這一格存成截圖
    Grab,
    /// 找出個資並打上馬賽克
    FindPii,
    /// 影片：找出沒動靜的片段並刪掉
    FindIdle,
    /// 選一張圖片檔加上去（replace = 換掉選取的圖片）
    PickPicture {
        replace: bool,
    },
    /// 加上剪貼簿裡的圖片
    PastePicture,
    /// 再加一次上次用的圖片
    LastPicture,
    /// 影片：自動產生字幕（語音辨識）
    AutoSubs,
    /// 影片：匯入 SRT 字幕檔
    ImportSrt,
    /// 停止產生字幕
    CancelSubs,
}

/// 開啟截圖編輯：編輯過的圖會從原圖重新套用上次的編輯
pub fn open(app: &mut UiApp, path: String) {
    open_with(app, path, true);
}

fn open_with(app: &mut UiApp, path: String, use_project: bool) {
    if app.editor.is_some() {
        return app.toast("編輯視窗已經開著，請先關閉", true);
    }
    let core = app.core.clone();
    let p = path.clone();
    app.spawn(
        async move {
            let info = if use_project { actions::shot_project(&core, &p).await } else { None };
            // 開啟的是編輯過的圖、原圖還在：改用原圖，套用上次的編輯
            let src = match &info {
                Some(i) if i.matched == ProjectMatch::Output && i.source_ok => i.project.source.clone(),
                _ => p.clone(),
            };
            let img = actions::load_image(&src).await;
            (info, src, img)
        },
        move |app, (info, src, img)| match img {
            Err(e) => app.toast(e.message().to_string(), true),
            Ok((rgba, w, h)) => {
                if app.editor.is_none() {
                    app.editor = Some(new_editor(app, path, src, rgba, w, h, info));
                }
            }
        },
    );
}

fn new_editor(app: &UiApp, opened: String, src: String, rgba: Vec<u8>, w: u32, h: u32, info: Option<ShotProjectInfo>) -> Editor {
    let entry = LibraryEntry { media: MediaInfo { path: src.clone(), name: file_name(&src), width: Some(w), height: Some(h), ..Default::default() }, exports: vec![] };
    let mut ed = Editor::base(app, entry, PathBuf::new(), Pl(None), Tab::Ann);
    ed.duration = 1.0;
    ed.fps = 1.0;
    ed.vw = w as f64;
    ed.vh = h as f64;
    ed.spec = EditSpec { start: 0.0, end: 1.0, removed: vec![], crop: None, overlays: vec![], ..Default::default() };
    ed.view = (0.0, 1.0);
    let (prgba, pw, ph) = downscale(&rgba, w, h, PREVIEW_MAX);
    let base = Frame { time: 0.0, width: pw, height: ph, rgba: prgba };
    ed.frame = Some(base.clone());
    ed.shot = Some(Shot { path: src, opts: ShotOpts::default(), opened_edit: None, preview: None, base, orig: (w, h), applied: 0 });
    match info {
        Some(i) if i.matched == ProjectMatch::Output && i.source_ok => {
            apply_spec(&mut ed, &i.spec);
            ed.replace_target = Some(opened.clone());
            if let Some(s) = &mut ed.shot {
                s.opened_edit = Some(opened);
            }
        }
        Some(i) if i.matched == ProjectMatch::Output => ed.banner = BannerInfo::MissingSource(file_name(&i.project.source)),
        Some(i) => ed.banner = BannerInfo::HasProject { output: i.project.output, data: i.project.data },
        None => {}
    }
    ed.mark_saved();
    ed
}

/// 縮小到最長邊不超過 max（RGBA）
fn downscale(rgba: &[u8], w: u32, h: u32, max: u32) -> (Vec<u8>, u32, u32) {
    shot_edit::scale_rgba(rgba, w, h, (max as f64 / w.max(h) as f64).min(1.0))
}

/// 套用存起來的編輯
fn apply_spec(ed: &mut Editor, spec: &ShotSpec) {
    ed.spec.crop = spec.crop;
    ed.crop_on = spec.crop.is_some();
    ed.anns = spec.anns.iter().filter(|a| [a.x, a.y, a.w, a.h, a.size].iter().all(|v| v.is_finite())).cloned().collect();
    for a in &mut ed.anns {
        a.id = ed.next_id;
        ed.next_id += 1;
        (a.start, a.end) = (0.0, 1.0);
        annotate::measure(a);
    }
    if let Some(s) = &mut ed.shot {
        s.opts = ShotOpts {
            border: spec.border.clone(),
            border_width: spec.border_width,
            shadow: spec.shadow,
            scale: spec.scale,
            rotate: shot_edit::norm_rotate(spec.rotate),
            radius: spec.radius,
            background: spec.background.clone(),
            padding: spec.padding,
        };
    }
    sync_rotation(ed);
}

/// 畫面轉成 opts.rotate 的角度（旋轉、復原 / 重做之後）；ed.vw × ed.vh 是旋轉後的大小
pub fn sync_rotation(ed: &mut Editor) {
    let Some(s) = &mut ed.shot else { return };
    let deg = shot_edit::norm_rotate(s.opts.rotate);
    if deg == s.applied {
        return;
    }
    let (rgba, w, h) = shot_edit::rotate_rgba(&s.base.rgba, s.base.width, s.base.height, deg);
    let (vw, vh) = shot_edit::rotated_size(s.orig.0, s.orig.1, deg);
    s.applied = deg;
    ed.frame = Some(Frame { time: 0.0, width: w, height: h, rgba });
    (ed.vw, ed.vh) = (vw as f64, vh as f64);
    ed.video_key = 0;
}

/// 整張圖向右（順時針）或向左轉 90 度：標註與裁切範圍跟著轉
pub fn rotate(ed: &mut Editor, clockwise: bool) {
    let (w, h) = (ed.vw, ed.vh);
    // 旋轉前 → 旋轉後的座標
    let t = |x: f64, y: f64| if clockwise { (h - y, x) } else { (y, w - x) };
    for a in &mut ed.anns {
        if a.kind == annotate::AnnKind::Arrow {
            let (p0, p1) = (t(a.x, a.y), t(a.x + a.w, a.y + a.h));
            (a.x, a.y, a.w, a.h) = (p0.0, p0.1, p1.0 - p0.0, p1.1 - p0.1);
            continue;
        }
        // 其他：中心跟著轉，標註本身多轉 90 度（放大鏡是圓的，不用轉）
        let (cx, cy) = t(a.x + a.w / 2.0, a.y + a.h / 2.0);
        a.x = cx - a.w / 2.0;
        a.y = cy - a.h / 2.0;
        if a.kind.rotatable() {
            a.rot = super::norm_deg(a.rot + if clockwise { 90.0 } else { -90.0 });
        }
    }
    if let Some(c) = ed.spec.crop {
        ed.spec.crop = Some(if clockwise {
            CropInput { x: h - (c.y + c.height), y: c.x, width: c.height, height: c.width }
        } else {
            CropInput { x: c.y, y: w - (c.x + c.width), width: c.height, height: c.width }
        });
    }
    if let Some(s) = &mut ed.shot {
        s.opts.rotate = (s.opts.rotate + if clockwise { 90 } else { 270 }) % 360;
    }
    sync_rotation(ed);
}

/// 目前的編輯（輸出用）
pub fn spec(ed: &Editor) -> ShotSpec {
    let crop =
        if ed.crop_on { normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32).map(|r| CropInput { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 }) } else { None };
    let o = ed.shot.as_ref().map(|s| s.opts()).unwrap_or_default();
    ShotSpec {
        crop,
        anns: ed.ordered().into_iter().cloned().collect(),
        border: o.border,
        border_width: o.border_width,
        shadow: o.shadow,
        scale: o.scale,
        rotate: o.rotate,
        radius: o.radius,
        background: o.background,
        padding: o.padding,
    }
}

fn source(ed: &Editor) -> String {
    ed.shot.as_ref().map(|s| s.path.clone()).unwrap_or_default()
}

/// 什麼都還沒改（不用存）
fn unchanged(ed: &Editor) -> bool {
    let s = spec(ed);
    s.crop.is_none() && s.anns.is_empty() && s.border.is_none() && !s.shadow && s.scale == 100 && s.rotate == 0 && s.radius == 0 && s.background.is_none()
}

// ───────────── 畫面 ─────────────

/// 說明列：正在修改哪張編輯過的圖、找不到原圖、或原圖有上次的編輯可以載入
pub fn banner(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let (text, warn): (String, bool) = if let Some(target) = &ed.replace_target {
        (format!("正在修改「{}」：已從原圖「{}」載入上次的標註，可以直接修改。儲存時會取代這張。", file_name(target), ed.entry.media.name), false)
    } else {
        match &ed.banner {
            BannerInfo::None => return,
            BannerInfo::MissingSource(src) => (format!("找不到原圖「{src}」，之前的標註已經存進這張圖、無法修改；只能在這張圖上繼續編輯。"), true),
            BannerInfo::HasProject { output, .. } => (format!("這張截圖之前編輯成「{}」。", file_name(output)), false),
        }
    };
    egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 0)).show(ui, |ui| {
        egui::Frame::new().fill(if warn { p.warn_soft } else { p.accent_soft }).corner_radius(theme::RADIUS_SM).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).color(if warn { p.warn } else { p.text }));
                if ed.replace_target.is_some() {
                    if Btn::new("改為另存新的一張").small().show(ui).clicked() {
                        ed.replace_target = None;
                        ed.banner = BannerInfo::None;
                    }
                    if Btn::new("改成直接編輯這張").small().show(ui).clicked() {
                        ed.pending = Some(Act::Plain);
                    }
                } else if let BannerInfo::HasProject { output, data } = &ed.banner {
                    if Btn::new("載入上次的編輯來修改").small().show(ui).clicked() {
                        let (output, data) = (output.clone(), data.clone());
                        if let Some(spec) = shot_edit::ShotProject::parse(&data) {
                            apply_spec(ed, &spec);
                            ed.replace_target = Some(output);
                            ed.banner = BannerInfo::None;
                            ed.mark_saved();
                        }
                    }
                }
            });
        });
    });
}

/// 下方：輸出大小與按鈕
pub fn footer(ed: &mut Editor, ui: &mut egui::Ui) -> Option<Act> {
    let p = theme::pal(ui);
    let s = spec(ed);
    let (w, h) = output_size(ed, &s);
    let mut act = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("輸出 ");
            ui.label(RichText::new(format!("{w}×{h}")).font(theme::font_bold(13.5)));
            let (ow, oh) = ed.shot.as_ref().map(|s| s.orig).unwrap_or_default();
            ui.label(format!("（原圖 {ow}×{oh}）"));
            if !ed.anns.is_empty() {
                ui.label(format!("・標註 {} 個", ed.anns.len()));
            }
        });
        let line = match &ed.replace_target {
            Some(t) => format!("儲存後取代 {}，並複製到剪貼簿", file_name(t)),
            None => format!("另存成 {}（原圖保留），並複製到剪貼簿", file_name(&shot_edit::edited_path(std::path::Path::new(&source(ed))).display().to_string())),
        };
        ui.label(RichText::new(line).font(theme::font(12.0)).color(p.muted));
    });
    // 右邊：儲存（主要）、複製；不常用的收進「更多」，全部重設也不會跟儲存並排被誤按
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let same = unchanged(ed);
        let mut b = Btn::new(if ed.replace_target.is_some() { "儲存修改" } else { "儲存" }).primary().enabled(!same && !ed.saving).tooltip("Ctrl+S");
        if same {
            b = b.tooltip("還沒有任何標註、裁切或輸出設定");
        }
        if b.show(ui).clicked() {
            act = Some(Act::Save);
        }
        if Btn::new("複製").tooltip("複製編輯後的圖到剪貼簿，不存檔（Ctrl+C）").show(ui).clicked() {
            act = Some(Act::Copy);
        }
        let more = Btn::new("更多").icon(Icon::More).ghost().tooltip("文字辨識、釘在桌面、全部重設").show(ui);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(220.0);
            if ui.button("文字辨識（複製圖裡的文字）").on_hover_text("把圖裡的文字轉成可以複製的文字（Windows 內建的文字辨識）").clicked() {
                act = Some(Act::Ocr);
            }
            if ui.button("釘在桌面").on_hover_text("把編輯後的圖變成浮在最上層的小視窗，方便對照").clicked() {
                act = Some(Act::Pin);
            }
            ui.separator();
            if ui.add_enabled(!unchanged(ed), egui::Button::new("全部重設（回到原圖）")).on_hover_text("清掉標註、裁切、旋轉與輸出設定；可以按 Ctrl+Z 復原").clicked() {
                ed.reset();
                if let Some(s) = &mut ed.shot {
                    s.opts = ShotOpts::default();
                }
            }
        });
    });
    act
}

/// 「輸出」分頁：外框、陰影、大小，以及輸出的樣子
pub fn output_panel(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let p = theme::pal(ui);
    let Some(mut o) = ed.shot.as_ref().map(|s| s.opts()) else { return };
    let label = |ui: &mut egui::Ui, t: &str| {
        ui.label(theme::muted(ui, t).font(theme::font(12.0)));
    };
    // 外框
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, "外框");
        let mut kind = match &o.border {
            None => 0,
            Some(_) if o.border_width <= 3.0 => 1,
            Some(_) => 2,
        };
        if segmented(ui, &mut kind, &[(0, "無"), (1, "細"), (2, "粗")], true) {
            match kind {
                0 => o.border = None,
                k => {
                    o.border = Some(o.border.clone().unwrap_or_else(|| BORDER_COLORS[0].into()));
                    o.border_width = if k == 1 { 2.0 } else { 6.0 };
                }
            }
        }
        if o.border.is_some() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for c in BORDER_COLORS {
                    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                    let (cr, cg, cb) = annotate::parse_color(c);
                    if o.border.as_deref() == Some(c) {
                        ui.painter().circle_stroke(r.center(), 13.5, Stroke::new(2.0, p.accent));
                    }
                    ui.painter().circle(r.center(), 10.5, Color32::from_rgb(cr, cg, cb), Stroke::new(1.0, p.border_strong));
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        o.border = Some(c.to_string());
                    }
                }
            });
        }
    });
    // 圓角
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, "圓角");
        let names = ["無", "小", "中", "大"];
        let items: Vec<(u32, &str)> = RADII.iter().copied().zip(names).collect();
        segmented(ui, &mut o.radius, &items, true);
    });
    // 背景
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, "背景");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
            let sw = r.shrink(2.0);
            ui.painter().rect(sw, CornerRadius::same(5), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
            ui.painter().line_segment([sw.left_bottom() + vec2(4.0, -4.0), sw.right_top() + vec2(-4.0, 4.0)], Stroke::new(1.5, p.muted));
            if o.background.is_none() {
                ui.painter().rect_stroke(r.expand(1.0), CornerRadius::same(7), Stroke::new(2.0, p.accent), StrokeKind::Inside);
            }
            if resp.on_hover_text("不要背景").on_hover_cursor(CursorIcon::PointingHand).clicked() {
                o.background = None;
            }
            for bg in BACKGROUNDS {
                let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                gradient_swatch(ui.painter(), r.shrink(2.0), bg);
                if o.background.as_deref() == Some(bg) {
                    ui.painter().rect_stroke(r.expand(1.0), CornerRadius::same(7), Stroke::new(2.0, p.accent), StrokeKind::Inside);
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    o.background = Some(bg.to_string());
                }
            }
        });
        if o.background.is_some() {
            let items: Vec<(u32, &str)> = PADDINGS.iter().copied().zip(["留白少", "留白中", "留白多"]).collect();
            segmented(ui, &mut o.padding, &items, true);
        }
    });
    // 陰影
    switch(ui, &mut o.shadow, "加上陰影", true).on_hover_text("圖的四周加上柔和的陰影（沒有背景時四周留透明的邊），貼到文件或簡報比較立體");
    // 大小
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, "大小");
        let items: Vec<(u32, String)> = SCALES.iter().map(|s| (*s, format!("{s}%"))).collect();
        let refs: Vec<(u32, &str)> = items.iter().map(|(v, t)| (*v, t.as_str())).collect();
        segmented(ui, &mut o.scale, &refs, true);
    });
    if let Some(s) = &mut ed.shot {
        s.opts = o;
    }
    let sp = spec(ed);
    // 輸出的樣子（與存檔用同一個 render）
    let key = serde_json::to_string(&sp).unwrap_or_default();
    let stale = ed.shot.as_ref().is_none_or(|s| s.preview.as_ref().is_none_or(|(k, _)| *k != key));
    if stale {
        // 用還沒旋轉的畫面與原圖大小（render 自己轉）
        if let Some((f, (ow, oh))) = ed.shot.as_ref().map(|s| (&s.base, s.orig)) {
            if let Some(pm) = shot_edit::render(&f.rgba, f.width, f.height, ow, oh, &sp, true) {
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                let tex = ctx.load_texture("shot-output", img, TextureOptions::LINEAR);
                if let Some(s) = &mut ed.shot {
                    s.preview = Some((key, tex));
                }
            }
        }
    }
    ui.label(theme::muted(ui, "左邊顯示的就是輸出的樣子；輸出的大小顯示在下方。").font(theme::font(12.0)));
}

/// 輸出的大小（原圖大小 + 旋轉 + 裁切 + 縮放 + 陰影）
fn output_size(ed: &Editor, sp: &ShotSpec) -> (u32, u32) {
    let (ow, oh) = ed.shot.as_ref().map(|s| s.orig).unwrap_or((ed.vw as u32, ed.vh as u32));
    shot_edit::output_size(sp, ow, oh)
}

/// 旋轉按鈕（只在「裁切與旋轉」分頁）
pub fn rotate_buttons(ed: &mut Editor, ui: &mut egui::Ui) {
    if Btn::new("向左轉").icon(Icon::RotL).small().tooltip("整張圖逆時針轉 90 度（標註跟著轉）").show(ui).clicked() {
        rotate(ed, false);
    }
    if Btn::new("向右轉").icon(Icon::RotR).small().tooltip("整張圖順時針轉 90 度（標註跟著轉）").show(ui).clicked() {
        rotate(ed, true);
    }
}

/// 透明處的棋盤格底
/// 背景的色塊：單色或從左上到右下的漸層
pub(super) fn gradient_swatch(p: &egui::Painter, r: Rect, bg: &str) {
    let cols: Vec<Color32> = bg
        .split(',')
        .map(|c| {
            let (r, g, b) = annotate::parse_color(c.trim());
            Color32::from_rgb(r, g, b)
        })
        .collect();
    let (a, b) = (cols.first().copied().unwrap_or(Color32::GRAY), cols.last().copied().unwrap_or(Color32::GRAY));
    let mid = a.lerp_to_gamma(b, 0.5);
    let mut mesh = egui::Mesh::default();
    for (pt, c) in [(r.left_top(), a), (r.right_top(), mid), (r.right_bottom(), b), (r.left_bottom(), mid)] {
        mesh.colored_vertex(pt, c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
    p.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_black_alpha(40)), StrokeKind::Inside);
}

/// 「輸出」分頁的預覽圖（左邊大畫面顯示）
pub fn output_texture(ed: &Editor) -> Option<TextureHandle> {
    ed.shot.as_ref().and_then(|s| s.preview.as_ref()).map(|(_, t)| t.clone())
}

pub fn checker(p: &egui::Painter, r: Rect) {
    let n = 8.0;
    let (a, b) = (Color32::from_gray(236), Color32::from_gray(214));
    p.rect_filled(r, CornerRadius::ZERO, a);
    let mut y = r.top();
    let mut row = 0;
    while y < r.bottom() {
        let mut x = r.left() + if row % 2 == 0 { 0.0 } else { n };
        while x < r.right() {
            p.rect_filled(Rect::from_min_max(pos2(x, y), pos2((x + n).min(r.right()), (y + n).min(r.bottom()))), CornerRadius::ZERO, b);
            x += n * 2.0;
        }
        y += n;
        row += 1;
    }
}

// ───────────── 動作 ─────────────

/// 在找到的個資上加馬賽克（已經遮住的不重複加）；回傳要顯示的訊息
fn cover_pii(ed: &mut Editor, found: &[ocr::Found]) -> String {
    if found.is_empty() {
        return "沒有找到 Email、電話、身分證字號或卡號".into();
    }
    let covered = |ed: &Editor, (x, y, w, h): (f64, f64, f64, f64)| {
        ed.anns.iter().filter(|a| a.kind.is_effect() && !a.invert).any(|a| {
            let ix = (a.x + a.w).min(x + w) - a.x.max(x);
            let iy = (a.y + a.h).min(y + h) - a.y.max(y);
            ix > 0.0 && iy > 0.0 && ix * iy >= w * h * 0.6
        })
    };
    let mut counts: Vec<(ocr::Pii, usize)> = vec![];
    for f in found {
        let (x, y, w, h) = f.rect;
        let x = x.clamp(0.0, ed.vw);
        let y = y.clamp(0.0, ed.vh);
        let (w, h) = (w.min(ed.vw - x), h.min(ed.vh - y));
        if w < 2.0 || h < 2.0 || covered(ed, (x, y, w, h)) {
            continue;
        }
        let mut a = ed.new_ann(AnnKind::Mosaic, x, y);
        (a.w, a.h) = (w, h);
        (a.shape, a.invert) = (Some(annotate::Shape::Rect), false);
        ed.anns.push(a);
        match counts.iter_mut().find(|(k, _)| *k == f.kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((f.kind, 1)),
        }
    }
    if counts.is_empty() {
        return "找到的個資都已經遮住了".into();
    }
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{} {n}", k.name())).collect();
    format!("已遮住 {total} 處（{}）；請再確認有沒有漏掉的", parts.join("、"))
}

/// 儲存、複製、釘在桌面、文字辨識、直接編輯開啟的那張
pub fn run(app: &mut UiApp, ed: &mut Editor, act: Act) {
    let (core, src, sp) = (app.core.clone(), source(ed), spec(ed));
    match act {
        Act::Save => {
            if unchanged(ed) || ed.saving {
                return;
            }
            ed.saving = true;
            let replace = ed.replace_target.clone();
            app.spawn(async move { actions::shot_save(&core, &src, &sp, replace.as_deref()).await }, |app, r| match r {
                Ok(info) => {
                    app.editor = None;
                    app.toast(format!("已存成 {}{}", file_name(&info.path), if info.copied { "，並複製到剪貼簿" } else { "" }), false);
                    app.shots_changed();
                }
                Err(e) => {
                    if let Some(ed) = &mut app.editor {
                        ed.saving = false;
                    }
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        Act::Copy => app.spawn(async move { actions::shot_copy(&src, &sp).await }, |app, r| match r {
            Ok((w, h)) => app.toast(format!("已複製到剪貼簿（{w}×{h}）"), false),
            Err(e) => app.toast(e.message().to_string(), true),
        }),
        Act::Pin => app.spawn(async move { actions::shot_pin(&src, &sp).await }, |app, r| {
            if let Err(e) = r {
                app.toast(e.message().to_string(), true);
            }
        }),
        Act::Ocr => super::super::ocr::start_spec(app, src, sp),
        Act::FindPii => {
            if ed.finding_pii {
                return;
            }
            ed.finding_pii = true;
            let rotate = sp.rotate;
            app.spawn(async move { actions::shot_find_pii(&src, rotate).await }, |app, r| {
                let Some(ed) = &mut app.editor else { return };
                ed.finding_pii = false;
                match r {
                    Ok(found) => {
                        let msg = cover_pii(ed, &found);
                        app.toast(msg, false);
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                }
            });
        }
        Act::PickPicture { replace } => {
            app.spawn(async move { tokio::task::spawn_blocking(picture::pick_file).await.unwrap_or(Ok(None)) }, move |app, r| match r {
                Ok(Some(path)) => use_picture(app, &path.to_string_lossy(), replace),
                Ok(None) => {}
                Err(e) => app.toast(e, true),
            });
        }
        Act::PastePicture => {
            app.spawn(async move { tokio::task::spawn_blocking(picture::save_clipboard_image).await.unwrap_or_else(|e| Err(e.to_string())) }, |app, r| match r {
                Ok(path) => use_picture(app, &path.to_string_lossy(), false),
                Err(e) => app.toast(e, true),
            });
        }
        Act::AutoSubs | Act::ImportSrt | Act::CancelSubs => super::subs::run(app, ed, act),
        Act::LastPicture => {
            let path = ed.last_picture.clone();
            match ed.add_picture(&path) {
                Ok(()) => {}
                Err(e) => app.toast(e, true),
            }
        }
        Act::FindIdle => {
            if ed.finding_idle {
                return;
            }
            ed.finding_idle = true;
            let (ffmpeg, video, dur, audio) = (ed.ffmpeg.clone(), ed.entry.media.path.clone(), ed.duration, ed.entry.media.has_audio == Some(true));
            app.spawn(async move { screenrecorder_core::idle::find(&ffmpeg, &video, dur, audio).await }, |app, r| {
                let Some(ed) = &mut app.editor else { return };
                ed.finding_idle = false;
                match r {
                    Ok(found) => {
                        let msg = ed.remove_idle(&found);
                        app.toast(msg, false);
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                }
            });
        }
        Act::Grab => {
            let (video, t) = (ed.entry.media.path.clone(), ed.now());
            ed.pause();
            // 完成後由狀態更新顯示「已截圖」並更新清單
            app.spawn(async move { core.grab_frame(&video, t).await }, |app, r| {
                if let Err(e) = r {
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        Act::Plain => {
            let Some(path) = ed.shot.as_ref().and_then(|s| s.opened_edit.clone()) else { return };
            app.editor = None;
            open_with(app, path, false);
        }
    }
}

/// 加上（或換成）這張圖片，並記住它（下次可以從選單直接再用）
pub fn use_picture(app: &mut UiApp, path: &str, replace: bool) {
    let Some(ed) = &mut app.editor else { return };
    let r = if replace { ed.replace_picture(path) } else { ed.add_picture(path) };
    match r {
        Ok(()) => {
            if app.s.last_picture != path {
                app.s.last_picture = path.to_string();
                app.save_settings();
            }
        }
        Err(e) => app.toast(e, true),
    }
}
