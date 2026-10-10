//! 外觀：顏色（淺色 / 深色跟著 Windows）、字型、圖示與共用的小元件（按鈕、分段選擇、開關、標籤）。

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Painter, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, TextStyle, Ui, Vec2,
    Visuals, WidgetText,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Pal {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_ink: Color32,
    pub accent_soft: Color32,
    pub rec: Color32,
    pub rec_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const LIGHT: Pal = Pal {
    bg: hex(0xf4f4f2),
    surface: hex(0xffffff),
    surface2: hex(0xf0f0ee),
    border: hex(0xe2e1dd),
    border_strong: hex(0xcfcdc7),
    text: hex(0x1d1d1b),
    muted: hex(0x6b6a66),
    accent: hex(0x2563eb),
    accent_ink: hex(0xffffff),
    accent_soft: hex(0xe8efff),
    rec: hex(0xe5484d),
    rec_soft: hex(0xfdecec),
    warn: hex(0xb25e00),
    warn_soft: hex(0xfff4e2),
    ok: hex(0x1a7f4b),
    ok_soft: hex(0xe6f5ec),
};

pub const DARK: Pal = Pal {
    bg: hex(0x161615),
    surface: hex(0x1f1f1e),
    surface2: hex(0x2a2a28),
    border: hex(0x33332f),
    border_strong: hex(0x46453f),
    text: hex(0xecebe7),
    muted: hex(0x9b9a94),
    accent: hex(0x6b9bff),
    accent_ink: hex(0x0d1530),
    accent_soft: hex(0x1c2847),
    rec: hex(0xff6369),
    rec_soft: hex(0x3a1d1f),
    warn: hex(0xf5a524),
    warn_soft: hex(0x3a2a12),
    ok: hex(0x4cc38a),
    ok_soft: hex(0x15301f),
};

pub fn pal(ui: &Ui) -> &'static Pal {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn pal_ctx(ctx: &egui::Context) -> &'static Pal {
    if ctx.theme() == egui::Theme::Dark {
        &DARK
    } else {
        &LIGHT
    }
}

pub const RADIUS: u8 = 12;
pub const RADIUS_SM: u8 = 8;

/// 粗體字型家族
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn font_bold(size: f32) -> FontId {
    FontId::new(size, bold())
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// 載入系統字型（Segoe UI + 微軟正黑體 UI），保留 egui 內建字型當作最後的備援（含表情符號）
pub fn setup_fonts(ctx: &egui::Context) {
    let (regular, bold_fonts) = screenrecorder_core::fonts::ui_fonts();
    let mut defs = FontDefinitions::default();
    let builtin_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let builtin_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mut add = |prefix: &str, list: &[screenrecorder_core::fonts::FontFile]| -> Vec<String> {
        list.iter()
            .enumerate()
            .map(|(i, f)| {
                let name = format!("{prefix}{i}-{}", f.name);
                let mut fd = FontData::from_owned((*f.data).clone());
                fd.index = f.index;
                defs.font_data.insert(name.clone(), Arc::new(fd));
                name
            })
            .collect()
    };
    let reg = add("r", &regular);
    let bol = add("b", &bold_fonts);
    let mono_files: Vec<_> = ["CascadiaMono.ttf", "consola.ttf"]
        .iter()
        .find_map(|n| {
            let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
            std::fs::read(std::path::Path::new(&win).join("Fonts").join(n)).ok().map(|d| screenrecorder_core::fonts::FontFile { name: n.to_string(), data: Arc::new(d), index: 0 })
        })
        .into_iter()
        .collect();
    let mon = add("m", &mono_files);

    let prop: Vec<String> = reg.iter().cloned().chain(builtin_prop.iter().cloned()).collect();
    let bold_list: Vec<String> = bol.iter().cloned().chain(reg.iter().cloned()).chain(builtin_prop.iter().cloned()).collect();
    let mono_list: Vec<String> = mon.iter().cloned().chain(builtin_mono.iter().cloned()).chain(reg.iter().cloned()).collect();
    defs.families.insert(FontFamily::Proportional, prop);
    defs.families.insert(bold(), bold_list);
    defs.families.insert(FontFamily::Monospace, mono_list);
    ctx.set_fonts(defs);
}

/// 淺色、深色兩套外觀（egui 依 Windows 的設定自動切換）
pub fn setup_visuals(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Light, visuals(false));
    ctx.set_visuals_of(egui::Theme::Dark, visuals(true));
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 5.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.combo_height = 300.0;
        s.text_styles.insert(TextStyle::Body, font(14.0));
        s.text_styles.insert(TextStyle::Button, font(14.0));
        s.text_styles.insert(TextStyle::Small, font(12.0));
        s.text_styles.insert(TextStyle::Heading, font_bold(17.0));
        s.text_styles.insert(TextStyle::Monospace, mono(13.0));
    });
}

/// 調整 egui 內建元件（下拉選單、輸入框、捲軸）的外觀
fn visuals(dark: bool) -> Visuals {
    let p = if dark { &DARK } else { &LIGHT };
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface2;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(RADIUS);
    v.menu_corner_radius = CornerRadius::same(RADIUS_SM);
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.override_text_color = Some(p.text);
    let w = &mut v.widgets;
    for (st, fill, stroke) in [
        (&mut w.noninteractive, p.surface, p.border),
        (&mut w.inactive, p.surface, p.border_strong),
        (&mut w.hovered, p.surface2, p.border_strong),
        (&mut w.active, p.surface2, p.accent),
        (&mut w.open, p.surface2, p.accent),
    ] {
        st.bg_fill = fill;
        st.weak_bg_fill = fill;
        st.bg_stroke = Stroke::new(1.0, stroke);
        st.corner_radius = CornerRadius::same(6);
        st.fg_stroke = Stroke::new(1.0, p.text);
        st.expansion = 0.0;
    }
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v
}

// ───────────── 圖示（16×16 的線條圖，與 2.x 版相同的設計） ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Cut,
    Export,
    Edit,
    Settings,
    Folder,
    Refresh,
    ChevL,
    ChevR,
    List,
    Pause,
    Stop,
    Trash,
    Mic,
    Speaker,
    Close,
    Camera,
    Eye,
    More,
    /// 視窗（有標題列的方框）
    Window,
    /// 向下的箭頭（下拉選單）
    ChevD,
    /// 圖片（山與太陽）
    Image,
}

/// 在 rect（正方形）裡畫圖示
pub fn paint_icon(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let k = rect.width() / 16.0;
    let o = rect.min;
    let at = |x: f32, y: f32| pos2(o.x + x * k, o.y + y * k);
    let st = Stroke::new(1.6 * k.max(0.8), color);
    let line = |pts: &[(f32, f32)]| {
        p.add(Shape::line(pts.iter().map(|(x, y)| at(*x, *y)).collect(), st));
    };
    let circle = |x: f32, y: f32, r: f32| {
        p.circle_stroke(at(x, y), r * k, st);
    };
    // 弧線：中心、半徑、起訖角度（弧度）
    let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| {
        let n = 16;
        let pts: Vec<Pos2> = (0..=n).map(|i| a0 + (a1 - a0) * i as f32 / n as f32).map(|a| at(cx + r * a.cos(), cy + r * a.sin())).collect();
        p.add(Shape::line(pts, st));
    };
    match icon {
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![at(5.0, 3.5), at(12.5, 8.0), at(5.0, 12.5)], color, Stroke::NONE));
        }
        Icon::Cut => {
            circle(4.5, 4.5, 2.0);
            circle(4.5, 11.5, 2.0);
            line(&[(6.2, 5.6), (13.5, 12.0)]);
            line(&[(6.2, 10.4), (13.5, 4.0)]);
        }
        Icon::Export => {
            line(&[(9.0, 2.5), (13.5, 2.5), (13.5, 7.0)]);
            line(&[(13.5, 2.5), (7.5, 8.5)]);
            line(&[(11.5, 9.5), (11.5, 12.5), (10.5, 13.5), (3.5, 13.5), (2.5, 12.5), (2.5, 5.5), (3.5, 4.5), (6.5, 4.5)]);
        }
        Icon::Edit => {
            line(&[(10.5, 2.5), (13.5, 5.5), (6.0, 13.0), (3.0, 13.0), (3.0, 10.0), (10.5, 2.5)]);
            line(&[(9.0, 4.0), (12.0, 7.0)]);
        }
        Icon::Settings => {
            for (y, x) in [(4.0, 5.5), (8.0, 10.5), (12.0, 6.5)] {
                line(&[(2.5, y), (13.5, y)]);
                p.circle(at(x, y), 1.6 * k, pal_from(color), st);
            }
        }
        Icon::Folder => {
            line(&[(1.5, 4.5), (1.5, 12.5), (2.5, 13.5), (13.5, 13.5), (14.5, 12.5), (14.5, 6.5), (13.5, 5.5), (8.0, 5.5), (6.5, 3.5), (2.5, 3.5), (1.5, 4.5)]);
        }
        Icon::Refresh => {
            arc(8.0, 8.0, 5.6, -0.6, 4.9);
            line(&[(14.0, 2.5), (14.0, 6.3), (10.2, 6.3)]);
        }
        Icon::ChevL => line(&[(10.0, 3.0), (5.0, 8.0), (10.0, 13.0)]),
        Icon::ChevR => line(&[(6.0, 3.0), (11.0, 8.0), (6.0, 13.0)]),
        Icon::ChevD => line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)]),
        Icon::List => {
            for y in [4.0, 8.0, 12.0] {
                line(&[(5.5, y), (13.5, y)]);
                p.circle_filled(at(2.5, y), 0.9 * k, color);
            }
        }
        Icon::Pause => {
            line(&[(5.0, 3.0), (5.0, 13.0)]);
            line(&[(11.0, 3.0), (11.0, 13.0)]);
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_min_max(at(4.0, 4.0), at(12.0, 12.0)), CornerRadius::same((1.5 * k) as u8), color);
        }
        Icon::Trash => {
            line(&[(2.5, 4.5), (13.5, 4.5)]);
            line(&[(6.0, 4.5), (6.0, 3.0), (10.0, 3.0), (10.0, 4.5)]);
            line(&[(4.0, 4.5), (4.7, 13.5), (11.3, 13.5), (12.0, 4.5)]);
        }
        Icon::Mic => {
            p.rect_stroke(Rect::from_min_max(at(6.0, 2.0), at(10.0, 9.5)), CornerRadius::same((2.0 * k) as u8), st, StrokeKind::Middle);
            arc(8.0, 8.0, 4.5, 0.0, std::f32::consts::PI);
            line(&[(8.0, 12.5), (8.0, 14.0)]);
        }
        Icon::Speaker => {
            line(&[(2.5, 6.0), (5.0, 6.0), (8.5, 3.0), (8.5, 13.0), (5.0, 10.0), (2.5, 10.0), (2.5, 6.0)]);
            arc(11.0, 8.0, 2.5, -1.1, 1.1);
        }
        Icon::Camera => {
            p.rect_stroke(Rect::from_min_max(at(2.0, 5.0), at(14.0, 13.5)), CornerRadius::same((2.0 * k) as u8), st, StrokeKind::Middle);
            line(&[(5.5, 5.0), (6.5, 3.0), (9.5, 3.0), (10.5, 5.0)]);
            circle(8.0, 9.2, 2.4);
        }
        Icon::Image => {
            p.rect_stroke(Rect::from_min_max(at(2.0, 3.0), at(14.0, 13.0)), CornerRadius::same((1.5 * k) as u8), st, StrokeKind::Middle);
            line(&[(3.5, 11.5), (7.0, 7.5), (9.5, 10.0), (11.0, 8.5), (13.0, 11.0)]);
            p.circle_filled(at(10.8, 5.8), 1.2 * k, color);
        }
        Icon::Window => {
            p.rect_stroke(Rect::from_min_max(at(2.0, 3.0), at(14.0, 13.0)), CornerRadius::same((1.5 * k) as u8), st, StrokeKind::Middle);
            line(&[(2.0, 6.0), (14.0, 6.0)]);
        }
        Icon::More => {
            for x in [3.5, 8.0, 12.5] {
                p.circle_filled(at(x, 8.0), 1.4 * k, color);
            }
        }
        Icon::Eye => {
            arc(8.0, 13.0, 7.6, -2.42, -0.72);
            arc(8.0, 3.0, 7.6, 0.72, 2.42);
            p.circle_filled(at(8.0, 8.0), 2.2 * k, color);
        }
        Icon::Close => {
            line(&[(4.0, 4.0), (12.0, 12.0)]);
            line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
    }
}

/// 設定圖示圓點的底色（與按鈕底色相近）
fn pal_from(c: Color32) -> Color32 {
    if c.r() as u32 + c.g() as u32 + c.b() as u32 > 380 {
        DARK.surface
    } else {
        LIGHT.surface
    }
}

// ───────────── 按鈕 ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Primary,
    Ghost,
    Danger,
    /// 開始錄影（紅色、較大）
    Record,
}

pub struct Btn {
    text: String,
    icon: Option<Icon>,
    kind: Kind,
    small: bool,
    enabled: bool,
    min_width: f32,
    tooltip: Option<String>,
    selected: bool,
    /// 指定高度（與旁邊的大按鈕對齊）
    height: Option<f32>,
    /// 圖示與文字靠左（上下排的按鈕對齊用）
    left: bool,
    /// 平常淡灰色，滑鼠移上去才變深（一整排操作按鈕時不搶眼）
    quiet: bool,
}

impl Btn {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), icon: None, kind: Kind::Normal, small: false, enabled: true, min_width: 0.0, tooltip: None, selected: false, height: None, left: false, quiet: false }
    }
    pub fn quiet(mut self, q: bool) -> Self {
        self.quiet = q;
        self
    }
    pub fn left(mut self) -> Self {
        self.left = true;
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.height = Some(h);
        self
    }
    pub fn icon_only(icon: Icon) -> Self {
        Self::new("").icon(icon)
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn kind(mut self, k: Kind) -> Self {
        self.kind = k;
        self
    }
    pub fn primary(self) -> Self {
        self.kind(Kind::Primary)
    }
    pub fn ghost(self) -> Self {
        self.kind(Kind::Ghost)
    }
    pub fn danger(self) -> Self {
        self.kind(Kind::Danger)
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
    pub fn tooltip(mut self, t: impl Into<String>) -> Self {
        let t = t.into();
        self.tooltip = (!t.is_empty()).then_some(t);
        self
    }
    pub fn selected(mut self, s: bool) -> Self {
        self.selected = s;
        self
    }

    fn metrics(&self) -> (f32, f32, f32, FontId) {
        let (h, fs, pad) = match (self.kind, self.small) {
            (Kind::Record, _) => (40.0, 15.0, 18.0),
            (_, true) => (28.0, 13.0, 10.0),
            _ => (34.0, 14.0, 14.0),
        };
        let fid = if matches!(self.kind, Kind::Primary | Kind::Record | Kind::Danger) { font_bold(fs) } else { font(fs) };
        (self.height.unwrap_or(h), fs, pad, fid)
    }

    /// 不設最小寬度時的寬度（讓一組按鈕取一樣寬）
    pub fn width(&self, ui: &Ui) -> f32 {
        let (_, _, pad, fid) = self.metrics();
        let text = (!self.text.is_empty()).then(|| ui.painter().layout_no_wrap(self.text.clone(), fid, Color32::WHITE).size().x);
        pad * 2.0 + text.unwrap_or(0.0) + if self.icon.is_some() { 16.0 + if text.is_some() { 6.0 } else { 0.0 } } else { 0.0 }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = pal(ui);
        let (h, _, pad, fid) = self.metrics();
        let text_color = |hover: bool| match self.kind {
            Kind::Primary => p.accent_ink,
            Kind::Record => Color32::WHITE,
            Kind::Danger => {
                if hover {
                    Color32::WHITE
                } else {
                    p.rec
                }
            }
            _ => p.text,
        };
        let galley = (!self.text.is_empty()).then(|| ui.painter().layout_no_wrap(self.text.clone(), fid.clone(), text_color(false)));
        let icon_w = 16.0;
        let gap = 6.0;
        let mut w = pad * 2.0;
        if let Some(g) = &galley {
            w += g.size().x;
        }
        if self.icon.is_some() {
            w += icon_w + if galley.is_some() { gap } else { 0.0 };
        }
        if self.kind == Kind::Record {
            w += 10.0 + gap;
        }
        if galley.is_none() && self.icon.is_some() {
            w = h;
        }
        let size = vec2(w.max(self.min_width), h);
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(size, sense);
        if ui.is_rect_visible(rect) {
            let hover = self.enabled && resp.hovered();
            let down = self.enabled && resp.is_pointer_button_down_on();
            let (mut fill, mut stroke) = match self.kind {
                Kind::Normal => (if hover { p.surface2 } else { p.surface }, Stroke::new(1.0, p.border_strong)),
                Kind::Ghost => (if hover || self.selected { p.surface2 } else { Color32::TRANSPARENT }, Stroke::NONE),
                Kind::Primary => (if hover { p.accent.gamma_multiply(0.9) } else { p.accent }, Stroke::NONE),
                Kind::Record => (if hover { p.rec.gamma_multiply(0.9) } else { p.rec }, Stroke::NONE),
                Kind::Danger => (if hover { p.rec } else { p.surface }, Stroke::new(1.0, p.rec.gamma_multiply(0.6))),
            };
            if self.selected && self.kind == Kind::Normal {
                fill = p.accent_soft;
                stroke = Stroke::new(1.0, p.accent);
            }
            if down {
                fill = fill.gamma_multiply(0.92);
            }
            let painter = ui.painter();
            let r = CornerRadius::same(if self.kind == Kind::Record { RADIUS_SM + 2 } else { RADIUS_SM });
            painter.rect(rect, r, fill, stroke, StrokeKind::Inside);
            let color = if !self.enabled {
                text_color(false).gamma_multiply(0.45)
            } else if self.quiet && !hover {
                p.muted.gamma_multiply(0.85)
            } else {
                text_color(hover)
            };
            let mut x = if self.left { rect.min.x + pad } else { rect.center().x - (w - pad * 2.0) / 2.0 };
            if galley.is_none() && self.icon.is_some() {
                x = rect.center().x - icon_w / 2.0;
            }
            if self.kind == Kind::Record {
                painter.circle_filled(pos2(x + 5.0, rect.center().y), 5.0, Color32::WHITE);
                x += 10.0 + gap;
            }
            if let Some(i) = self.icon {
                paint_icon(painter, Rect::from_min_size(pos2(x, rect.center().y - icon_w / 2.0), Vec2::splat(icon_w)), i, color);
                x += icon_w + gap;
            }
            if let Some(g) = &galley {
                painter.galley_with_override_text_color(pos2(x, rect.center().y - g.size().y / 2.0), g.clone(), color);
            }
        }
        let resp = if self.enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        match self.tooltip {
            Some(t) => resp.on_hover_text(t),
            None => resp,
        }
    }
}

// ───────────── 分段選擇、開關、標籤 ─────────────

/// 分段選擇（例如：單一螢幕｜所有螢幕｜自訂範圍）；回傳是否改變
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, items: &[(T, &str)], enabled: bool) -> bool {
    let p = pal(ui);
    let mut changed = false;
    let frame = egui::Frame::new().fill(p.surface2).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(10)).inner_margin(3.0);
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for (v, label) in items {
                let on = *value == *v;
                let g = ui.painter().layout_no_wrap(label.to_string(), font(13.5), p.text);
                let size = vec2(g.size().x + 22.0, 28.0);
                let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
                let hover = enabled && resp.hovered();
                if on {
                    ui.painter().rect(rect, CornerRadius::same(7), p.surface, Stroke::new(1.0, p.border), StrokeKind::Inside);
                }
                let color = if on || hover { p.text } else { p.muted };
                let color = if enabled { color } else { color.gamma_multiply(0.5) };
                ui.painter().galley_with_override_text_color(rect.center() - g.size() / 2.0, g, color);
                if resp.clicked() && !on {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// 開關（右邊是文字）；回傳是否改變
pub fn switch(ui: &mut Ui, on: &mut bool, label: impl Into<WidgetText>, enabled: bool) -> Response {
    let p = pal(ui);
    let label: WidgetText = label.into();
    let galley = label.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Body);
    let size = vec2(34.0 + 8.0 + galley.size().x, galley.size().y.max(20.0));
    let (rect, mut resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let track = Rect::from_min_size(pos2(rect.min.x, rect.center().y - 9.0), vec2(34.0, 18.0));
    let fill = if *on { p.accent } else { p.border_strong };
    let fill = if enabled { fill } else { fill.gamma_multiply(0.5) };
    ui.painter().rect_filled(track, CornerRadius::same(9), fill);
    let knob_x = egui::lerp(track.left() + 9.0..=track.right() - 9.0, t);
    ui.painter().circle_filled(pos2(knob_x, track.center().y), 7.0, Color32::WHITE);
    let text_color = if enabled { p.text } else { p.muted };
    ui.painter().galley_with_override_text_color(pos2(track.right() + 8.0, rect.center().y - galley.size().y / 2.0), galley, text_color);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Warn,
    Bad,
    Accent,
}

/// 小標籤（系統狀態、聲音、剪輯版…）
pub fn chip(ui: &mut Ui, text: &str, tone: Tone, clickable: bool) -> Response {
    let p = pal(ui);
    let (fill, fg) = match tone {
        Tone::Plain => (p.surface2, p.muted),
        Tone::Ok => (p.ok_soft, p.ok),
        Tone::Warn => (p.warn_soft, p.warn),
        Tone::Bad => (p.rec_soft, p.rec),
        Tone::Accent => (p.accent_soft, p.accent),
    };
    let g = ui.painter().layout_no_wrap(text.to_string(), font_bold(11.5), fg);
    let size = vec2(g.size().x + 14.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, if clickable { Sense::click() } else { Sense::hover() });
    let fill = if clickable && resp.hovered() { fill.gamma_multiply(1.15) } else { fill };
    ui.painter().rect_filled(rect, CornerRadius::same(11), fill);
    ui.painter().galley_with_override_text_color(rect.center() - g.size() / 2.0, g, fg);
    if clickable {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// 對話框的框
pub fn modal_frame(ctx: &egui::Context) -> egui::Frame {
    let p = pal_ctx(ctx);
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(RADIUS)).inner_margin(20.0).shadow(egui::Shadow {
        offset: [0, 12],
        blur: 40,
        spread: 0,
        color: Color32::from_black_alpha(70),
    })
}

/// 卡片 / 面板的框
pub fn card(ui: &Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(RADIUS)).inner_margin(14.0)
}

pub fn muted(ui: &Ui, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(pal(ui).muted).font(font(12.5))
}

/// 進度條（細）
pub fn progress(ui: &mut Ui, frac: f32, color: Color32, height: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same((height / 2.0) as u8), p.surface2);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, CornerRadius::same((height / 2.0) as u8), color);
}

/// 文字放在 rect 中間
/// 下拉選單的外觀：與按鈕相同（白底、細框、圓角、高 30）
pub fn field_style(ui: &mut Ui) {
    let p = pal(ui);
    ui.spacing_mut().interact_size.y = 30.0;
    ui.spacing_mut().button_padding = vec2(10.0, 6.0);
    let w = &mut ui.visuals_mut().widgets;
    for (st, fill) in [(&mut w.inactive, p.surface), (&mut w.hovered, p.surface2), (&mut w.active, p.surface2), (&mut w.open, p.surface2)] {
        st.weak_bg_fill = fill;
        st.bg_fill = fill;
        st.bg_stroke = Stroke::new(1.0, p.border_strong);
        st.corner_radius = CornerRadius::same(RADIUS_SM);
        st.expansion = 0.0;
    }
}

/// 縮圖依原比例放進 rect 置中（不拉伸；兩側或上下留底色）
pub fn paint_thumb(p: &Painter, rect: Rect, tex: &egui::TextureHandle) {
    let [w, h] = tex.size();
    let s = (rect.width() / w.max(1) as f32).min(rect.height() / h.max(1) as f32);
    let r = Rect::from_center_size(rect.center(), egui::vec2(w as f32 * s, h as f32 * s));
    p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

pub fn centered_text(p: &Painter, rect: Rect, text: &str, f: FontId, color: Color32) {
    p.text(rect.center(), Align2::CENTER_CENTER, text, f, color);
}
