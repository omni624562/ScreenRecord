//! 截圖編輯：原圖先旋轉（90 度的倍數），再套用標註（含馬賽克 / 模糊 / 放大鏡）、裁切、縮放、外框與陰影，存成新的 PNG（原圖保留）。
//! 標註與裁切的座標都是旋轉後的圖的像素。
//! 編輯設定另外存成專案（projects.rs，與剪輯版相同），之後開啟編輯過的圖可以從原圖重新套用並修改。
//! 預覽與輸出用同一個 render：預覽時畫面可以比原圖小，位置與大小依比例換算。

use crate::annotate::{self, Ann};
use crate::edit::CropInput;
use serde::{Deserialize, Serialize};
use tiny_skia::{Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Point, Rect, SpreadMode, Stroke, Transform};

/// 輸出的縮放比例（%）
pub const SCALES: [u32; 4] = [100, 75, 50, 25];
/// 外框的顏色
pub const BORDER_COLORS: [&str; 4] = ["#d0d4da", "#111111", "#e5484d", "#0090ff"];
/// 背景：單色「#rrggbb」或斜向漸層「#左上,#右下」
pub const BACKGROUNDS: [&str; 8] = ["#a1c4fd,#c2e9fb", "#ff9a9e,#fecfef", "#f6d365,#fda085", "#84fab0,#8fd3f4", "#667eea,#764ba2", "#2b5876,#4e4376", "#f1f3f5", "#1f2328"];
/// 圓角半徑（輸出圖的像素）
pub const RADII: [u32; 4] = [0, 8, 16, 32];
/// 背景留白（圖的短邊的百分比）
pub const PADDINGS: [u32; 3] = [4, 8, 14];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotSpec {
    /// 裁切範圍（原圖像素）；None = 整張
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropInput>,
    #[serde(default)]
    pub anns: Vec<Ann>,
    /// 外框顏色（#rrggbb）；None = 沒有外框
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    /// 外框粗細（輸出圖的像素）
    #[serde(default = "default_border_width")]
    pub border_width: f64,
    /// 外圍加陰影（四周留透明的邊）
    #[serde(default)]
    pub shadow: bool,
    /// 縮放比例（%）
    #[serde(default = "default_scale")]
    pub scale: u32,
    /// 整張圖順時針旋轉的角度（0 / 90 / 180 / 270）
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: u32,
    /// 圓角半徑（輸出圖的像素；0 = 直角）
    #[serde(default, skip_serializing_if = "is_zero")]
    pub radius: u32,
    /// 放在背景上（單色或漸層，見 BACKGROUNDS）；None = 沒有背景
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// 背景留白（圖的短邊的百分比）
    #[serde(default = "default_padding")]
    pub padding: u32,
}

fn default_padding() -> u32 {
    8
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn default_border_width() -> f64 {
    2.0
}

fn default_scale() -> u32 {
    100
}

impl Default for ShotSpec {
    fn default() -> Self {
        Self { crop: None, anns: vec![], border: None, border_width: default_border_width(), shadow: false, scale: default_scale(), rotate: 0, radius: 0, background: None, padding: default_padding() }
    }
}

/// 存起來的截圖編輯（kind 用來和影片的剪輯專案區分）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotProject {
    pub v: u32,
    pub kind: String,
    pub spec: ShotSpec,
}

impl ShotProject {
    pub fn new(spec: ShotSpec) -> Self {
        Self { v: 1, kind: "shot".into(), spec }
    }

    /// 讀取存起來的專案（不是截圖的專案、格式不對時為 None；格式不對的標註略過）
    pub fn parse(v: &serde_json::Value) -> Option<ShotSpec> {
        if v.get("kind").and_then(|k| k.as_str()) != Some("shot") {
            return None;
        }
        let spec = v.get("spec")?;
        let mut s: ShotSpec = serde_json::from_value(serde_json::json!({
            "crop": spec.get("crop"),
            "border": spec.get("border"),
            "borderWidth": spec.get("borderWidth").cloned().unwrap_or(serde_json::json!(2.0)),
            "shadow": spec.get("shadow").cloned().unwrap_or(serde_json::json!(false)),
            "scale": spec.get("scale").cloned().unwrap_or(serde_json::json!(100)),
            "rotate": spec.get("rotate").cloned().unwrap_or(serde_json::json!(0)),
        }))
        .ok()?;
        s.rotate = norm_rotate(s.rotate);
        s.anns = spec.get("anns").and_then(|a| a.as_array()).map(|l| l.iter().filter_map(|a| serde_json::from_value(a.clone()).ok()).collect()).unwrap_or_default();
        Some(s)
    }
}

/// 角度換成 0 / 90 / 180 / 270
pub fn norm_rotate(deg: u32) -> u32 {
    (deg / 90 % 4) * 90
}

/// 旋轉後的寬高（vw × vh 是原圖）
pub fn rotated_size(vw: u32, vh: u32, rotate: u32) -> (u32, u32) {
    if norm_rotate(rotate) % 180 == 90 {
        (vh, vw)
    } else {
        (vw, vh)
    }
}

/// 把 RGBA 順時針轉 deg 度（90 的倍數）
pub fn rotate_rgba(rgba: &[u8], w: u32, h: u32, deg: u32) -> (Vec<u8>, u32, u32) {
    let deg = norm_rotate(deg);
    if deg == 0 {
        return (rgba.to_vec(), w, h);
    }
    let (w, h) = (w as usize, h as usize);
    let (nw, nh) = if deg == 180 { (w, h) } else { (h, w) };
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..h {
        for x in 0..w {
            // 順時針 90：(x, y) → (h - 1 - y, x)；180：(w - 1 - x, h - 1 - y)；270：(y, w - 1 - x)
            let (dx, dy) = match deg {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (s, d) = ((y * w + x) * 4, (dy * nw + dx) * 4);
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out, nw as u32, nh as u32)
}

/// 陰影留的邊（輸出圖的像素）
pub fn shadow_margin(out_w: u32, out_h: u32) -> u32 {
    ((out_w.min(out_h) as f64 * 0.03).round() as u32).clamp(8, 32)
}

/// 裁切後的範圍（原圖像素，限制在圖內，至少 1×1）
pub fn crop_rect(spec: &ShotSpec, vw: u32, vh: u32) -> (u32, u32, u32, u32) {
    match spec.crop {
        Some(c) => {
            let x = c.x.round().clamp(0.0, vw.saturating_sub(1) as f64) as u32;
            let y = c.y.round().clamp(0.0, vh.saturating_sub(1) as f64) as u32;
            let w = c.width.round().clamp(1.0, (vw - x) as f64) as u32;
            let h = c.height.round().clamp(1.0, (vh - y) as f64) as u32;
            (x, y, w, h)
        }
        None => (0, 0, vw, vh),
    }
}

/// 輸出圖的大小（含外框與陰影）
pub fn output_size(spec: &ShotSpec, vw: u32, vh: u32) -> (u32, u32) {
    let (vw, vh) = rotated_size(vw, vh, spec.rotate);
    let (_, _, w, h) = crop_rect(spec, vw, vh);
    let k = spec.scale.clamp(10, 100) as f64 / 100.0;
    let (w, h) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    let m = margin(spec, w, h) * 2;
    (w + m, h + m)
}

/// 圖四周加的邊（輸出圖的像素）：陰影要的空間、背景的留白，取大的
pub fn margin(spec: &ShotSpec, out_w: u32, out_h: u32) -> u32 {
    let shadow = if spec.shadow { shadow_margin(out_w, out_h) } else { 0 };
    let bg = if spec.background.is_some() { (out_w.min(out_h) as f64 * spec.padding.min(40) as f64 / 100.0).round() as u32 } else { 0 };
    shadow.max(bg)
}

/// 套用編輯：src 是畫面（RGBA，不透明；可以是縮小的預覽；還沒旋轉），vw × vh 是原圖大小。
/// preview = true 時不縮放（預覽另外縮放顯示），外框粗細與陰影依畫面比例換算
pub fn render(src: &[u8], fw: u32, fh: u32, vw: u32, vh: u32, spec: &ShotSpec, preview: bool) -> Option<Pixmap> {
    if fw == 0 || fh == 0 || vw == 0 || vh == 0 || src.len() < (fw * fh * 4) as usize {
        return None;
    }
    // 0. 旋轉（之後的座標都是旋轉後的）
    let (rotated, fw, fh) = rotate_rgba(&src[..(fw * fh * 4) as usize], fw, fh, spec.rotate);
    let src = &rotated[..];
    let (vw, vh) = rotated_size(vw, vh, spec.rotate);
    let (sx, sy) = (fw as f64 / vw as f64, fh as f64 / vh as f64);
    // 1. 馬賽克 / 模糊 / 放大鏡（依清單順序）
    let mut rgba = src[..(fw * fh * 4) as usize].to_vec();
    for a in spec.anns.iter().filter(|a| a.kind.is_effect()) {
        crate::effects::apply(&mut rgba, fw as usize, fh as usize, a, vw as f64, vh as f64);
    }
    let mut pm = Pixmap::new(fw, fh)?;
    pm.data_mut().copy_from_slice(&rgba);
    // 2. 其他標註
    let t = Transform::from_scale(sx as f32, sy as f32);
    for a in spec.anns.iter().filter(|a| !a.kind.is_effect()) {
        annotate::draw(&mut pm, a, t);
    }
    // 3. 裁切
    let (cx, cy, cw, ch) = crop_rect(spec, vw, vh);
    let (px, py) = ((cx as f64 * sx).round() as i32, (cy as f64 * sy).round() as i32);
    let (pw, ph) = (((cw as f64 * sx).round() as u32).max(1), ((ch as f64 * sy).round() as u32).max(1));
    let mut cropped = Pixmap::new(pw, ph)?;
    cropped.draw_pixmap(-px, -py, pm.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    // 4. 縮放（預覽不縮）
    let k = if preview { 1.0 } else { spec.scale.clamp(10, 100) as f64 / 100.0 };
    let body = if k < 0.999 {
        let (w, h) = (((pw as f64 * k).round() as u32).max(1), ((ph as f64 * k).round() as u32).max(1));
        let mut out = Pixmap::new(w, h)?;
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
        out.draw_pixmap(0, 0, cropped.as_ref(), &paint, Transform::from_scale(w as f32 / pw as f32, h as f32 / ph as f32), None);
        out
    } else {
        cropped
    };
    // 預覽時外框與陰影依「預覽 / 輸出」的比例換算
    let unit = if preview { sx * 100.0 / spec.scale.clamp(10, 100) as f64 } else { 1.0 };
    Some(decorate(body, spec, unit))
}

/// 圓角、背景、陰影與外框；unit = 輸出圖一像素在這張圖上是多少像素
fn decorate(mut body: Pixmap, spec: &ShotSpec, unit: f64) -> Pixmap {
    let (w, h) = (body.width(), body.height());
    let (ow, oh) = ((w as f64 / unit) as u32, (h as f64 / unit) as u32);
    let scaled = |v: u32| if v > 0 { (v as f64 * unit).round().max(1.0) as u32 } else { 0 };
    let margin = scaled(margin(spec, ow, oh));
    let radius = (spec.radius.min(400) as f64 * unit) as f32;
    // 圓角：四個角變透明（陰影、外框都跟著圓）
    if radius >= 0.5 {
        if let (Some(path), Some(mut mask)) = (annotate::round_rect(0.0, 0.0, w as f32, h as f32, radius), Mask::new(w, h)) {
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
            body.apply_mask(&mask);
        }
    }
    if margin == 0 && spec.border.is_none() {
        return body;
    }
    let Some(mut out) = Pixmap::new(w + margin * 2, h + margin * 2) else { return body };
    let m = margin as f32;
    if let Some(bg) = &spec.background {
        fill_background(&mut out, bg);
    }
    if spec.shadow {
        // 往下偏一點的柔和陰影：照圖的形狀（含圓角）畫半透明的黑色，再把透明度模糊
        let sm = scaled(shadow_margin(ow, oh)) as usize;
        let (ow, oh) = (out.width() as usize, out.height() as usize);
        let mut sh = vec![0u8; ow * oh * 4];
        let off = (sm as f64 * 0.25).round() as usize;
        for (y, row) in body.data().chunks_exact(w as usize * 4).enumerate() {
            let oy = (y + margin as usize + off).min(oh - 1);
            for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
                sh[(oy * ow + x + margin as usize) * 4 + 3] = (px[3] as u32 * 110 / 255) as u8;
            }
        }
        crate::effects::box_blur(&mut sh, ow, oh, (sm / 3).max(1));
        if let Some(mut layer) = Pixmap::new(ow as u32, oh as u32) {
            // 透明度只留在 alpha（顏色是黑色，預乘後 RGB 為 0）
            for (d, s) in layer.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(sh.as_chunks::<4>().0) {
                *d = [0, 0, 0, s[3]];
            }
            out.draw_pixmap(0, 0, layer.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    }
    out.draw_pixmap(margin as i32, margin as i32, body.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    if let Some(c) = &spec.border {
        let bw = (spec.border_width.clamp(1.0, 20.0) * unit).max(1.0) as f32;
        let path = if radius >= 0.5 {
            annotate::round_rect(m + bw / 2.0, m + bw / 2.0, w as f32 - bw, h as f32 - bw, (radius - bw / 2.0).max(0.0))
        } else {
            Rect::from_xywh(m + bw / 2.0, m + bw / 2.0, w as f32 - bw, h as f32 - bw).map(PathBuilder::from_rect)
        };
        if let Some(path) = path {
            let (r8, g8, b8) = annotate::parse_color(c);
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(r8, g8, b8, 255));
            paint.anti_alias = true;
            out.stroke_path(&path, &paint, &Stroke { width: bw, ..Default::default() }, Transform::identity(), None);
        }
    }
    out
}

/// 填滿背景：「#rrggbb」單色，「#a,#b」從左上到右下的漸層
pub(crate) fn fill_background(out: &mut Pixmap, bg: &str) {
    let colors: Vec<Color> = bg
        .split(',')
        .map(|c| {
            let (r, g, b) = annotate::parse_color(c.trim());
            Color::from_rgba8(r, g, b, 255)
        })
        .collect();
    let (w, h) = (out.width() as f32, out.height() as f32);
    let mut paint = Paint::default();
    match colors.as_slice() {
        [] => return,
        [c] => paint.set_color(*c),
        [a, .., b] => {
            let stops = vec![GradientStop::new(0.0, *a), GradientStop::new(1.0, *b)];
            match LinearGradient::new(Point::from_xy(0.0, 0.0), Point::from_xy(w, h), stops, SpreadMode::Pad, Transform::identity()) {
                Some(sh) => paint.shader = sh,
                None => paint.set_color(*a),
            }
        }
    }
    if let Some(r) = Rect::from_xywh(0.0, 0.0, w, h) {
        out.fill_rect(r, &paint, Transform::identity(), None);
    }
}

/// 多張圖拼成一張：左右（vertical = false）或上下排，不縮放、置中對齊，中間與四周留 gap 像素的白邊
pub fn combine(images: &[Pixmap], vertical: bool, gap: u32) -> Option<Pixmap> {
    if images.is_empty() {
        return None;
    }
    let n = images.len() as u32;
    let along: u32 = images.iter().map(|i| if vertical { i.height() } else { i.width() }).sum::<u32>() + gap * (n + 1);
    let across: u32 = images.iter().map(|i| if vertical { i.width() } else { i.height() }).max()? + gap * 2;
    let (w, h) = if vertical { (across, along) } else { (along, across) };
    let mut out = Pixmap::new(w, h)?;
    out.fill(Color::WHITE);
    let mut pos = gap;
    for img in images {
        let (iw, ih) = (img.width(), img.height());
        let (x, y) = if vertical { ((w - iw) / 2, pos) } else { (pos, (h - ih) / 2) };
        out.draw_pixmap(x as i32, y as i32, img.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        pos += if vertical { ih } else { iw } + gap;
    }
    Some(out)
}

/// 編輯過的圖的檔名：Shot_…_編輯.png（已有同名時加 _2、_3…）
pub fn edited_path(source: &std::path::Path) -> std::path::PathBuf {
    let dir = source.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let stem = source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Shot".into());
    // 編輯過的圖再編輯：不要變成 _編輯_編輯
    let stem = stem.strip_suffix("_編輯").unwrap_or(&stem).to_string();
    (1..).map(|i| dir.join(if i == 1 { format!("{stem}_編輯.png") } else { format!("{stem}_編輯_{i}.png") })).find(|p| !p.exists()).unwrap_or_else(|| dir.join(format!("{stem}_編輯.png")))
}

/// 依比例 k 縮放 RGBA（不透明）；k = 1 時原樣回傳
pub fn scale_rgba(rgba: &[u8], w: u32, h: u32, k: f64) -> (Vec<u8>, u32, u32) {
    if (k - 1.0).abs() < 1e-6 || w == 0 || h == 0 {
        return (rgba.to_vec(), w, h);
    }
    let (nw, nh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    let (Some(mut src), Some(mut dst)) = (Pixmap::new(w, h), Pixmap::new(nw, nh)) else {
        return (rgba.to_vec(), w, h);
    };
    src.data_mut().copy_from_slice(&rgba[..(w * h * 4) as usize]);
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
    dst.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_scale(nw as f32 / w as f32, nh as f32 / h as f32), None);
    (dst.data().to_vec(), nw, nh)
}

/// 預乘 alpha 的畫面 → 一般 RGBA（存檔、剪貼簿用）
pub fn straight_rgba(pm: &Pixmap) -> Vec<u8> {
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::AnnKind;

    fn img(w: u32, h: u32, v: u8) -> Vec<u8> {
        vec![[v, v, v, 255]; (w * h) as usize].concat()
    }

    #[test]
    fn spotlight_dims_outside() {
        let src = img(200, 100, 200);
        let mut a: Ann = serde_json::from_value(serde_json::json!({
            "kind": "spotlight", "x": 50.0, "y": 20.0, "w": 100.0, "h": 60.0, "start": 0.0, "end": 1.0, "color": "#000000", "size": 8.0, "shape": "rect"
        }))
        .unwrap();
        a.id = 1;
        let spec = ShotSpec { anns: vec![a.clone()], ..Default::default() };
        let pm = render(&src, 200, 100, 200, 100, &spec, false).unwrap();
        // 框外變暗、框內不變
        assert!(pm.pixel(5, 5).unwrap().red() < 120);
        assert_eq!(pm.pixel(100, 50).unwrap().red(), 200);
        // 影片匯出：範圍限制在畫面內
        let o = annotate::to_overlay(&a, 200.0, 100.0).unwrap();
        assert_eq!((o.x, o.y, o.w, o.h), (0.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn combine_side_by_side_and_stacked() {
        let a = Pixmap::new(40, 20).map(|mut p| {
            p.fill(Color::BLACK);
            p
        });
        let b = Pixmap::new(10, 30).map(|mut p| {
            p.fill(Color::from_rgba8(255, 0, 0, 255));
            p
        });
        let (a, b) = (a.unwrap(), b.unwrap());
        let row = combine(&[a.clone(), b.clone()], false, 5).unwrap();
        assert_eq!((row.width(), row.height()), (5 + 40 + 5 + 10 + 5, 30 + 10));
        // 矮的置中，上下是白色
        assert_eq!(row.pixel(10, 7).unwrap().red(), 255);
        assert_eq!(row.pixel(10, 12).unwrap().red(), 0);
        assert_eq!(row.pixel(52, 6).unwrap().green(), 0);
        let col = combine(&[a, b], true, 0).unwrap();
        assert_eq!((col.width(), col.height()), (40, 50));
        assert!(combine(&[], true, 5).is_none());
    }

    #[test]
    fn rounded_corners_and_background() {
        let src = img(100, 60, 128);
        // 圓角：角落透明，中間不透明
        let spec = ShotSpec { radius: 16, ..Default::default() };
        let pm = render(&src, 100, 60, 100, 60, &spec, false).unwrap();
        assert_eq!((pm.width(), pm.height()), (100, 60));
        assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(50, 30).unwrap().alpha(), 255);
        // 背景：短邊的 10% 留白，四周填滿背景色
        let spec = ShotSpec { radius: 16, background: Some("#ff0000".into()), padding: 10, ..Default::default() };
        assert_eq!(output_size(&spec, 100, 60), (112, 72));
        let pm = render(&src, 100, 60, 100, 60, &spec, false).unwrap();
        assert_eq!((pm.width(), pm.height()), (112, 72));
        let c = pm.pixel(0, 0).unwrap();
        assert_eq!((c.red(), c.green(), c.blue(), c.alpha()), (255, 0, 0, 255));
        // 圖的圓角處露出背景
        let c = pm.pixel(7, 7).unwrap();
        assert_eq!((c.red(), c.alpha()), (255, 255));
        assert_eq!(pm.pixel(56, 36).unwrap().red(), 128);
        // 漸層：左上和右下的顏色不同；陰影比留白大時用陰影的邊
        let spec = ShotSpec { background: Some("#000000,#ffffff".into()), padding: 4, shadow: true, ..Default::default() };
        let pm = render(&src, 100, 60, 100, 60, &spec, false).unwrap();
        let m = shadow_margin(100, 60);
        assert_eq!(pm.width(), 100 + m * 2);
        assert!(pm.pixel(0, 0).unwrap().red() < 30 && pm.pixel(pm.width() - 1, pm.height() - 1).unwrap().red() > 225);
    }

    #[test]
    fn crop_scale_and_decorations_set_the_size() {
        let src = img(200, 100, 128);
        let mut spec = ShotSpec { crop: Some(CropInput { x: 10.0, y: 10.0, width: 100.0, height: 60.0 }), ..Default::default() };
        let pm = render(&src, 200, 100, 200, 100, &spec, false).unwrap();
        assert_eq!((pm.width(), pm.height()), (100, 60));
        spec.scale = 50;
        assert_eq!(output_size(&spec, 200, 100), (50, 30));
        let pm = render(&src, 200, 100, 200, 100, &spec, false).unwrap();
        assert_eq!((pm.width(), pm.height()), (50, 30));
        spec.shadow = true;
        let m = shadow_margin(50, 30);
        assert_eq!(output_size(&spec, 200, 100), (50 + m * 2, 30 + m * 2));
        let pm = render(&src, 200, 100, 200, 100, &spec, false).unwrap();
        assert_eq!((pm.width(), pm.height()), (50 + m * 2, 30 + m * 2));
        // 四周是透明的陰影邊，角落完全透明
        assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(m + 5, m + 5).unwrap().alpha(), 255);
    }

    #[test]
    fn annotations_and_border_are_drawn() {
        let src = img(100, 100, 255);
        let a = Ann {
            id: 1,
            kind: AnnKind::Highlight,
            x: 10.0,
            y: 10.0,
            w: 30.0,
            h: 30.0,
            start: 0.0,
            end: 1.0,
            color: "#000000".into(),
            size: 4.0,
            text: None,
            bg: false,
            n: None,
            shape: None,
            invert: false,
            rot: 0.0,
            pts: vec![],
        };
        let spec = ShotSpec { anns: vec![a], border: Some("#e5484d".into()), border_width: 3.0, ..Default::default() };
        let pm = render(&src, 100, 100, 100, 100, &spec, false).unwrap();
        // 螢光筆（35% 黑）讓白底變暗；外框是紅色
        assert!(pm.pixel(20, 20).unwrap().red() < 200);
        let b = pm.pixel(1, 50).unwrap();
        assert!(b.red() > 200 && b.green() < 100, "{b:?}");
    }

    #[test]
    fn rotation_turns_the_picture_and_swaps_the_size() {
        // 2×1：左紅右藍；順時針轉 90 度後是 1×2：上紅下藍
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        let (r, w, h) = rotate_rgba(&src, 2, 1, 90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&r[..4], &[255, 0, 0, 255]);
        let (r, w, h) = rotate_rgba(&src, 2, 1, 270);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&r[..4], &[0, 0, 255, 255]);
        let (r, _, _) = rotate_rgba(&src, 2, 1, 180);
        assert_eq!(&r[..4], &[0, 0, 255, 255]);
        let spec = ShotSpec { rotate: 90, ..Default::default() };
        assert_eq!(output_size(&spec, 200, 100), (100, 200));
        let pm = render(&img(200, 100, 128), 200, 100, 200, 100, &spec, false).unwrap();
        assert_eq!((pm.width(), pm.height()), (100, 200));
        let v = serde_json::to_value(ShotProject::new(spec.clone())).unwrap();
        assert_eq!(ShotProject::parse(&v), Some(spec));
    }

    #[test]
    fn project_round_trip_and_edited_name() {
        let spec = ShotSpec { shadow: true, scale: 75, ..Default::default() };
        let v = serde_json::to_value(ShotProject::new(spec.clone())).unwrap();
        assert_eq!(ShotProject::parse(&v), Some(spec));
        assert_eq!(ShotProject::parse(&serde_json::json!({ "v": 1, "spec": {} })), None);
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Shot_2026-10-10_11-20-00.png");
        assert_eq!(edited_path(&src).file_name().unwrap(), "Shot_2026-10-10_11-20-00_編輯.png");
        std::fs::write(dir.path().join("Shot_2026-10-10_11-20-00_編輯.png"), b"x").unwrap();
        assert_eq!(edited_path(&src).file_name().unwrap(), "Shot_2026-10-10_11-20-00_編輯_2.png");
        assert_eq!(edited_path(&dir.path().join("Shot_2026-10-10_11-20-00_編輯.png")).file_name().unwrap(), "Shot_2026-10-10_11-20-00_編輯_2.png");
    }
}
