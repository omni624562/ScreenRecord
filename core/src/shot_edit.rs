//! 截圖編輯：在原圖上套用標註（含馬賽克 / 模糊 / 放大鏡）、裁切、縮放、外框與陰影，存成新的 PNG（原圖保留）。
//! 編輯設定另外存成專案（projects.rs，與剪輯版相同），之後開啟編輯過的圖可以從原圖重新套用並修改。
//! 預覽與輸出用同一個 render：預覽時畫面可以比原圖小，位置與大小依比例換算。

use crate::annotate::{self, Ann};
use crate::edit::CropInput;
use serde::{Deserialize, Serialize};
use tiny_skia::{Color, FilterQuality, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

/// 輸出的縮放比例（%）
pub const SCALES: [u32; 4] = [100, 75, 50, 25];
/// 外框的顏色
pub const BORDER_COLORS: [&str; 4] = ["#d0d4da", "#111111", "#e5484d", "#0090ff"];

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
}

fn default_border_width() -> f64 {
    2.0
}

fn default_scale() -> u32 {
    100
}

impl Default for ShotSpec {
    fn default() -> Self {
        Self { crop: None, anns: vec![], border: None, border_width: default_border_width(), shadow: false, scale: default_scale() }
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
        }))
        .ok()?;
        s.anns = spec.get("anns").and_then(|a| a.as_array()).map(|l| l.iter().filter_map(|a| serde_json::from_value(a.clone()).ok()).collect()).unwrap_or_default();
        Some(s)
    }
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
    let (_, _, w, h) = crop_rect(spec, vw, vh);
    let k = spec.scale.clamp(10, 100) as f64 / 100.0;
    let (w, h) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    let m = if spec.shadow { shadow_margin(w, h) * 2 } else { 0 };
    (w + m, h + m)
}

/// 套用編輯：src 是畫面（RGBA，不透明；可以是縮小的預覽），vw × vh 是原圖大小。
/// preview = true 時不縮放（預覽另外縮放顯示），外框粗細與陰影依畫面比例換算
pub fn render(src: &[u8], fw: u32, fh: u32, vw: u32, vh: u32, spec: &ShotSpec, preview: bool) -> Option<Pixmap> {
    if fw == 0 || fh == 0 || vw == 0 || vh == 0 || src.len() < (fw * fh * 4) as usize {
        return None;
    }
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

/// 外框與陰影；unit = 輸出圖一像素在這張圖上是多少像素
fn decorate(body: Pixmap, spec: &ShotSpec, unit: f64) -> Pixmap {
    let (w, h) = (body.width(), body.height());
    let margin = if spec.shadow { (shadow_margin((w as f64 / unit) as u32, (h as f64 / unit) as u32) as f64 * unit).round().max(1.0) as u32 } else { 0 };
    let Some(mut out) = Pixmap::new(w + margin * 2, h + margin * 2) else { return body };
    let m = margin as f32;
    if spec.shadow {
        // 往下偏一點的柔和陰影：先畫半透明的方塊，再把透明度模糊
        let mut sh = vec![0u8; (out.width() * out.height() * 4) as usize];
        let ow = out.width() as usize;
        let off = (margin as f64 * 0.25).round() as usize;
        for y in 0..h as usize {
            let row = (y + margin as usize + off).min(out.height() as usize - 1);
            for x in 0..w as usize {
                let p = (row * ow + x + margin as usize) * 4;
                sh[p + 3] = 110;
            }
        }
        crate::effects::box_blur(&mut sh, ow, out.height() as usize, (margin as usize / 3).max(1));
        // 透明度只留在 alpha（顏色是黑色，預乘後 RGB 為 0）
        for (d, s) in out.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(sh.as_chunks::<4>().0) {
            *d = [0, 0, 0, s[3]];
        }
    }
    out.draw_pixmap(margin as i32, margin as i32, body.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    if let Some(c) = &spec.border {
        let bw = (spec.border_width.clamp(1.0, 20.0) * unit).max(1.0) as f32;
        if let Some(r) = Rect::from_xywh(m + bw / 2.0, m + bw / 2.0, w as f32 - bw, h as f32 - bw) {
            let (r8, g8, b8) = annotate::parse_color(c);
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(r8, g8, b8, 255));
            paint.anti_alias = true;
            let path = PathBuilder::from_rect(r);
            out.stroke_path(&path, &paint, &Stroke { width: bw, ..Default::default() }, Transform::identity(), None);
        }
    }
    out
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
