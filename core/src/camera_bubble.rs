//! 攝影機小窗：錄影時在螢幕上（擷取範圍的角落）顯示攝影機畫面，可以拖曳，直接被錄進影片（看到的就是錄到的）。
//! 攝影機同時只能給一個程式用，所以錄影的 FFmpeg 不再開攝影機，改由這個小窗讀畫面、顯示在螢幕上。
//! 這裡是不分平台的部分：小窗的位置與大小、讀攝影機畫面的 FFmpeg 參數、圓形 / 圓角方形的遮罩與白框。

use crate::types::{CameraConfig, MonitorInfo, Rect};

/// 錄影器提供給小窗的資訊（倒數、錄影、暫停中，而且選了攝影機才有）
#[derive(Debug, Clone, PartialEq)]
pub struct BubbleInfo {
    /// 擷取範圍（虛擬桌面的實體像素座標）
    pub area: Rect,
    /// 範圍涵蓋到的螢幕
    pub monitors: Vec<MonitorInfo>,
    pub camera: CameraConfig,
}

/// 小窗大小可調的範圍：擷取範圍短邊的百分比
pub const SIZE_MIN: u32 = 8;
pub const SIZE_MAX: u32 = 40;

/// 小窗的邊長：擷取範圍短邊的 size%（8～40%），取偶數、至少 64
pub fn bubble_size(area: &Rect, cam: &CameraConfig) -> i32 {
    size_px(area, cam.size)
}

/// 擷取範圍短邊的 pct% 是幾個像素（取偶數、至少 64）
pub fn size_px(area: &Rect, pct: u32) -> i32 {
    let d = (area.width.min(area.height) as f64 * pct.clamp(SIZE_MIN, SIZE_MAX) as f64 / 100.0).round() as i32;
    (d / 2 * 2).max(64)
}

/// 拖曳邊緣調整大小：游標離中心的距離決定新的邊長（圓形用直線距離、方形用較遠的那一軸），
/// 換算成短邊的整數百分比（存設定用；小窗也直接用這個大小，存檔後不會再跳一下）
pub fn resize_pct(area: &Rect, center: (i32, i32), cursor: (i32, i32), circle: bool) -> u32 {
    let (dx, dy) = ((cursor.0 - center.0) as f64, (cursor.1 - center.1) as f64);
    let half = if circle { dx.hypot(dy) } else { dx.abs().max(dy.abs()) };
    let short = area.width.min(area.height).max(1) as f64;
    ((half * 2.0 / short * 100.0).round() as u32).clamp(SIZE_MIN, SIZE_MAX)
}

/// 按在小窗的哪裡：外圍一圈（邊長的 12%，至少 10 像素）拖曳是調整大小，其他地方是移動。
/// x、y 是小窗內的座標；不在形狀裡（圓形的角落）時是 None
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Move,
    Resize,
}

pub fn grab_at(x: i32, y: i32, size: i32, circle: bool) -> Option<Grab> {
    let c = size as f64 / 2.0;
    let (dx, dy) = ((x as f64 + 0.5 - c).abs(), (y as f64 + 0.5 - c).abs());
    let edge = (size as f64 * 0.12).max(10.0);
    let dist = if circle { dx.hypot(dy) } else { dx.max(dy) };
    if dist > c {
        return None;
    }
    Some(if dist >= c - edge { Grab::Resize } else { Grab::Move })
}

/// 讀攝影機畫面用的大小：這個範圍裡小窗最大時的邊長（最多 720），調整大小時不用重開攝影機，在程式裡縮放
pub fn source_size(area: &Rect) -> i32 {
    size_px(area, SIZE_MAX).clamp(128, 720)
}

/// 把 s×s 的 BGRA 縮放成 d×d：縮小時取範圍內的平均（不會有鋸齒），放大時取最近的像素
pub fn resample(src: &[u8], s: usize, dst: &mut [u8], d: usize) {
    if s == d {
        dst.copy_from_slice(&src[..d * d * 4]);
        return;
    }
    let span = |i: usize| {
        let a = i * s / d;
        (a, ((i + 1) * s / d).max(a + 1).min(s))
    };
    let cols: Vec<(usize, usize)> = (0..d).map(span).collect();
    for y in 0..d {
        let (y0, y1) = span(y);
        for (x, &(x0, x1)) in cols.iter().enumerate() {
            let mut acc = [0u32; 4];
            for sy in y0..y1 {
                let row = &src[(sy * s + x0) * 4..(sy * s + x1) * 4];
                for p in row.as_chunks::<4>().0 {
                    for (a, v) in acc.iter_mut().zip(p) {
                        *a += *v as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = &mut dst[(y * d + x) * 4..(y * d + x) * 4 + 4];
            for (v, a) in o.iter_mut().zip(acc) {
                *v = (a / n) as u8;
            }
        }
    }
}

/// 開始時的位置：拖曳過就放在記住的相對位置（落在沒有螢幕的地方時改回角落）；
/// 否則放在設定的角落（0 右下、1 左下、2 右上、3 左上），離邊為短邊的 3%。
/// 範圍跨好幾個螢幕（所有螢幕）時，放在最靠那個角落的螢幕上，不會落在沒有螢幕的地方
pub fn bubble_rect(info: &BubbleInfo) -> Rect {
    let a = &info.area;
    let d = bubble_size(a, &info.camera);
    if let Some([px, py]) = info.camera.pos {
        let (cx, cy) = (a.x + (a.width as f64 * px.min(SCALE) as f64 / SCALE as f64).round() as i32, a.y + (a.height as f64 * py.min(SCALE) as f64 / SCALE as f64).round() as i32);
        let r = clamp_into(Rect { x: cx - d / 2, y: cy - d / 2, width: d, height: d }, a);
        let on_screen = info.monitors.is_empty() || info.monitors.iter().any(|m| r.x >= m.x && r.y >= m.y && r.x + r.width <= m.x + m.width && r.y + r.height <= m.y + m.height);
        if on_screen {
            return r;
        }
    }
    let m = (a.width.min(a.height) as f64 * 0.03).round() as i32;
    let (right, bottom) = match info.camera.corner {
        1 => (false, true),
        2 => (true, false),
        3 => (false, false),
        _ => (true, true),
    };
    // 每個螢幕和範圍的交集；挑出那個角落最外側的一塊
    let parts: Vec<Rect> = info.monitors.iter().filter_map(|mon| intersect(a, &Rect { x: mon.x, y: mon.y, width: mon.width, height: mon.height })).filter(|r| r.width >= d && r.height >= d).collect();
    let score = |r: &Rect| (if right { r.x + r.width } else { -r.x }) + (if bottom { r.y + r.height } else { -r.y });
    let part = parts.iter().max_by_key(|r| score(r)).copied().unwrap_or(*a);
    let x = if right { part.x + part.width - d - m } else { part.x + m };
    let y = if bottom { part.y + part.height - d - m } else { part.y + m };
    clamp_into(Rect { x, y, width: d, height: d }, a)
}

fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let (r, btm) = ((a.x + a.width).min(b.x + b.width), (a.y + a.height).min(b.y + b.height));
    (r > x && btm > y).then_some(Rect { x, y, width: r - x, height: btm - y })
}

/// 相對位置的單位：萬分比（4K 寬也能準確回到同一個像素）
const SCALE: u16 = 10000;

/// 拖曳後要記住的位置：小窗中心在擷取範圍內的相對位置（萬分比）
pub fn relative_pos(r: &Rect, area: &Rect) -> [u16; 2] {
    let f = |c: i32, start: i32, len: i32| (((c - start) as f64 / len.max(1) as f64) * SCALE as f64).round().clamp(0.0, SCALE as f64) as u16;
    [f(r.x + r.width / 2, area.x, area.width), f(r.y + r.height / 2, area.y, area.height)]
}

/// 讓小窗整個留在擷取範圍內（拖曳、範圍移動後），才會被錄到
pub fn clamp_into(r: Rect, area: &Rect) -> Rect {
    let x = r.x.clamp(area.x, (area.x + area.width - r.width).max(area.x));
    let y = r.y.clamp(area.y, (area.y + area.height - r.height).max(area.y));
    Rect { x, y, ..r }
}

/// 讀攝影機畫面：裁成正方形、縮成 d×d、左右翻轉（像照鏡子），BGRA 原始像素送到 stdout。
/// test_source：用 FFmpeg 的測試畫面代替攝影機（沒有攝影機的電腦上測試小窗用）
pub fn reader_args(device: &str, d: i32, test_source: bool) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error"].iter().map(|s| s.to_string()).collect();
    if test_source {
        a.extend(["-re", "-f", "lavfi", "-i", "testsrc2=s=640x480:r=30"].iter().map(|s| s.to_string()));
    } else {
        a.extend(["-f", "dshow", "-rtbufsize", "64M", "-i"].iter().map(|s| s.to_string()));
        a.push(format!("video={device}"));
    }
    a.extend(["-an", "-vf"].iter().map(|s| s.to_string()));
    a.push(format!("fps=30,crop='min(iw,ih)':'min(iw,ih)',scale={d}:{d}:flags=bicubic,hflip,format=bgra"));
    a.extend(["-f", "rawvideo", "-"].iter().map(|s| s.to_string()));
    a
}

/// 小窗的形狀：每個像素的透明度與白框的比例（事先算好，每張畫面只要套用）
pub struct Mask {
    pub size: usize,
    /// (不透明度, 白框比例)，0～255
    px: Vec<(u8, u8)>,
}

impl Mask {
    /// 圓形或圓角方形；外圍一圈白框（邊長的 1/64，至少 2 像素），邊緣反鋸齒
    pub fn new(size: usize, circle: bool) -> Mask {
        let s = size as f32;
        let c = s / 2.0;
        let border = (s / 64.0).round().max(2.0);
        let radius = if circle { c } else { s * 0.12 };
        let mut px = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = ((x as f32 + 0.5 - c).abs(), (y as f32 + 0.5 - c).abs());
                // 到圓角方形外緣的距離（裡面是負的）；圓形就是圓角半徑 = 邊長一半
                let (qx, qy) = (dx - (c - radius), dy - (c - radius));
                let dist = (qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0)) - radius;
                let alpha = (0.5 - dist).clamp(0.0, 1.0);
                let white = (dist + border + 0.5).clamp(0.0, 1.0);
                px.push(((alpha * 255.0).round() as u8, (white * 255.0).round() as u8));
            }
        }
        Mask { size, px }
    }

    /// 攝影機畫面（BGRA，size×size）套上形狀與白框，寫成預乘透明度的 BGRA（分層視窗用）
    pub fn apply(&self, src: &[u8], out: &mut [u8]) {
        let (src, _) = src.as_chunks::<4>();
        let (out, _) = out.as_chunks_mut::<4>();
        for ((s, o), &(a, w)) in src.iter().zip(out.iter_mut()).zip(&self.px) {
            let (a, w) = (a as u32, w as u32);
            for i in 0..3 {
                let c = (s[i] as u32 * (255 - w) + 255 * w) / 255;
                o[i] = (c * a / 255) as u8;
            }
            o[3] = a as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: i32, h: i32) -> MonitorInfo {
        MonitorInfo { id: format!("0:{x}"), adapter: 0, output: 0, adapter_name: "GPU".into(), device_name: String::new(), display_number: 1, x, y, width: w, height: h, primary: x == 0, rotation: 1 }
    }

    fn cam(corner: u8, size: u32) -> CameraConfig {
        CameraConfig { device: "Cam".into(), corner, size, circle: true, pos: None }
    }

    #[test]
    fn placed_in_the_chosen_corner() {
        let area = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        let info = |corner| BubbleInfo { area, monitors: vec![mon(0, 0, 1920, 1080)], camera: cam(corner, 20) };
        // 短邊 1080 的 20% = 216，離邊 3% ≈ 32
        assert_eq!(bubble_rect(&info(0)), Rect { x: 1920 - 216 - 32, y: 1080 - 216 - 32, width: 216, height: 216 });
        assert_eq!(bubble_rect(&info(1)), Rect { x: 32, y: 1080 - 216 - 32, width: 216, height: 216 });
        assert_eq!(bubble_rect(&info(2)), Rect { x: 1920 - 216 - 32, y: 32, width: 216, height: 216 });
        assert_eq!(bubble_rect(&info(3)), Rect { x: 32, y: 32, width: 216, height: 216 });
        // 自訂範圍：在範圍的角落
        let region = BubbleInfo { area: Rect { x: 100, y: 200, width: 800, height: 600 }, monitors: vec![mon(0, 0, 1920, 1080)], camera: cam(0, 20) };
        assert_eq!(bubble_rect(&region), Rect { x: 100 + 800 - 120 - 18, y: 200 + 600 - 120 - 18, width: 120, height: 120 });
        // 很小的範圍：至少 64
        assert_eq!(bubble_size(&Rect { x: 0, y: 0, width: 200, height: 150 }, &cam(0, 8)), 64);
    }

    #[test]
    fn all_screens_uses_a_real_monitor() {
        // 左邊 1920×1080、右邊 1280×1024 上緣對齊：整個桌面的右下角（2559, 1079）在右邊螢幕外面
        let monitors = vec![mon(0, 0, 1920, 1080), mon(1920, 0, 1280, 1024)];
        let area = Rect { x: 0, y: 0, width: 3200, height: 1080 };
        let r = bubble_rect(&BubbleInfo { area, monitors: monitors.clone(), camera: cam(0, 20) });
        let on = |m: &MonitorInfo| r.x >= m.x && r.y >= m.y && r.x + r.width <= m.x + m.width && r.y + r.height <= m.y + m.height;
        assert!(monitors.iter().any(on), "{r:?}");
        // 右邊螢幕的右下角
        assert_eq!((r.x + r.width + 32, r.y + r.height + 32), (3200, 1024));
    }

    #[test]
    fn remembers_the_dragged_position() {
        let mons = vec![mon(0, 0, 1920, 1080)];
        let area = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        // 拖到畫面中間偏左上：記成相對位置
        let dragged = Rect { x: 400, y: 200, width: 216, height: 216 };
        let pos = relative_pos(&dragged, &area);
        assert_eq!(pos, [2646, 2852]);
        // 下次錄同一個範圍：回到同一個地方
        let info = |area: Rect, pos| BubbleInfo { area, monitors: mons.clone(), camera: CameraConfig { pos, ..cam(0, 20) } };
        assert_eq!(bubble_rect(&info(area, Some(pos))), dragged);
        // 錄比較小的自訂範圍：相對位置一樣、大小跟著範圍
        let region = Rect { x: 100, y: 100, width: 800, height: 600 };
        let r = bubble_rect(&info(region, Some(pos)));
        assert_eq!((r.width, r.x + r.width / 2, r.y + r.height / 2), (120, 100 + 212, 100 + 171));
        // 貼著邊拖：仍整個在範圍內
        let edge = bubble_rect(&info(area, Some([10000, 0])));
        assert_eq!((edge.x + edge.width, edge.y), (1920, 0));
        // 所有螢幕時落在沒有螢幕的地方（右邊螢幕比較矮）：改回角落
        let two = vec![mon(0, 0, 1920, 1080), mon(1920, 0, 1280, 1024)];
        let all = BubbleInfo { area: Rect { x: 0, y: 0, width: 3200, height: 1080 }, monitors: two, camera: CameraConfig { pos: Some([9500, 9900]), ..cam(0, 20) } };
        let r = bubble_rect(&all);
        assert_eq!((r.x + r.width + 32, r.y + r.height + 32), (3200, 1024));
    }

    #[test]
    fn resize_by_dragging_the_edge() {
        let area = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        // 中心在 (500, 500)，游標拉到右邊 162 像素：邊長 324 = 短邊 30%
        assert_eq!(resize_pct(&area, (500, 500), (662, 500), true), 30);
        // 圓形看直線距離、方形看較遠的一軸
        assert_eq!(resize_pct(&area, (500, 500), (600, 600), true), 26);
        assert_eq!(resize_pct(&area, (500, 500), (600, 600), false), 19);
        // 有上下限
        assert_eq!(resize_pct(&area, (500, 500), (505, 500), true), SIZE_MIN);
        assert_eq!(resize_pct(&area, (500, 500), (1500, 500), true), SIZE_MAX);
        assert_eq!(size_px(&area, 30), 324);
        // 外圍一圈是調整大小，中間是移動，圓形的角落點不到
        assert_eq!(grab_at(100, 100, 200, true), Some(Grab::Move));
        assert_eq!(grab_at(100, 4, 200, true), Some(Grab::Resize));
        assert_eq!(grab_at(2, 2, 200, true), None);
        assert_eq!(grab_at(2, 2, 200, false), Some(Grab::Resize));
        // 讀攝影機的大小：小窗最大時的邊長，最多 720
        assert_eq!(source_size(&area), 432);
        assert_eq!(source_size(&Rect { x: 0, y: 0, width: 3840, height: 2160 }), 720);
    }

    #[test]
    fn resample_averages_when_shrinking() {
        // 4×4：左半黑、右半白 → 2×2 每格取平均
        let mut src = vec![0u8; 4 * 4 * 4];
        for y in 0..4 {
            for x in 2..4 {
                src[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let mut out = vec![0u8; 2 * 2 * 4];
        resample(&src, 4, &mut out, 2);
        assert_eq!(&out[0..4], &[0, 0, 0, 0]);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
        // 3×3 → 2×2：跨在中間的那一欄、列都算進去
        let gray: Vec<u8> = (0..9).flat_map(|i| [i as u8 * 10; 4]).collect();
        let mut o = vec![0u8; 16];
        resample(&gray, 3, &mut o, 2);
        assert_eq!(o[0], 0);
        assert_eq!(o[12], (40 + 50 + 70 + 80) / 4);
        // 一樣大：直接複製；放大：取最近的像素
        let mut same = vec![0u8; 16];
        resample(&out, 2, &mut same, 2);
        assert_eq!(same, out);
        let mut big = vec![0u8; 4 * 4 * 4];
        resample(&out, 2, &mut big, 4);
        assert_eq!(&big[0..4], &[0, 0, 0, 0]);
        assert_eq!(&big[(4 + 3) * 4..(4 + 3) * 4 + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn stays_inside_the_area() {
        let area = Rect { x: 100, y: 100, width: 800, height: 600 };
        let r = Rect { x: 850, y: -50, width: 120, height: 120 };
        assert_eq!(clamp_into(r, &area), Rect { x: 780, y: 100, width: 120, height: 120 });
        assert_eq!(clamp_into(Rect { x: 300, y: 300, ..r }, &area), Rect { x: 300, y: 300, ..r });
    }

    #[test]
    fn reader_reads_mirrored_squares() {
        let a = reader_args("USB Camera", 216, false).join(" ");
        assert!(a.contains("-f dshow -rtbufsize 64M -i video=USB Camera -an"), "{a}");
        assert!(a.contains("crop='min(iw,ih)':'min(iw,ih)',scale=216:216:flags=bicubic,hflip,format=bgra"), "{a}");
        assert!(a.ends_with("-f rawvideo -"));
        assert!(reader_args("x", 64, true).join(" ").contains("-re -f lavfi -i testsrc2"));
    }

    #[test]
    fn circle_mask_with_white_border() {
        let n = 128;
        let m = Mask::new(n, true);
        let src = vec![[10u8, 20, 30, 255]; n * n].concat();
        let mut out = vec![0u8; n * n * 4];
        m.apply(&src, &mut out);
        let at = |x: usize, y: usize| &out[(y * n + x) * 4..(y * n + x) * 4 + 4];
        // 角落透明（點得穿）；正中間是攝影機畫面、不透明
        assert_eq!(at(0, 0), &[0, 0, 0, 0]);
        assert_eq!(at(64, 64), &[10, 20, 30, 255]);
        // 邊緣內側一點是白框
        assert_eq!(at(64, 1), &[255, 255, 255, 255]);
        // 圓角方形：角落附近仍透明，靠邊的中間是白框
        let sq = Mask::new(n, false);
        sq.apply(&src, &mut out);
        assert_eq!(out[3], 0);
        assert_eq!(&out[(64 * n) * 4..(64 * n) * 4 + 4], &[255, 255, 255, 255]);
        assert_eq!(&out[(64 * n + 64) * 4..(64 * n + 64) * 4 + 4], &[10, 20, 30, 255]);
    }
}
