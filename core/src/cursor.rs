//! 平滑、放大的游標（後製）：錄影時記下游標在畫面裡的位置（約每 30 毫秒），剪輯時換成大的箭頭，
//! 位置像彈簧一樣平滑地跟過去（手抖、急停、跳動都變得流暢）。錄影時關掉「含游標」，影片裡才不會同時有原本的小游標。
//!
//! 預覽（介面依時間畫箭頭）與輸出（FFmpeg 的 sendcmd 依時間移動 overlay）用同一份平滑後的位置。

use crate::format::num;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// 一個位置：(影片的秒數, x, y)，x、y 是在畫面裡的位置（0～1；游標在範圍外時超出）
pub type Sample = [f64; 3];

/// 大小可選（一般游標的幾倍）
pub const SIZES: [f64; 3] = [1.5, 2.0, 3.0];
/// 一般大小的游標在 1080p 畫面上的高度（像素）
const BASE_H: f64 = 22.0;
/// 錄影時最多記幾點（約 4 小時一直在動）
pub const MAX_SAMPLES: usize = 500_000;
/// 彈簧的自然頻率（越大跟得越緊）：約 0.1 秒追上
const SPRING: f64 = 14.0;
/// 箭頭的形狀（單位：一般游標的 1/19 高）：尖端在 (0, 0)
const ARROW: [(f32, f32); 7] = [(0.0, 0.0), (0.0, 16.0), (4.0, 12.3), (6.8, 18.6), (9.6, 17.4), (6.9, 11.3), (11.8, 11.3)];
const ARROW_H: f32 = 19.0;
/// 圖的四周留白（框線與陰影；同樣的單位）
const PAD: f32 = 2.0;

/// 剪輯的設定：大小（SIZES 之一）與錄影時記下的位置（原影片的時間）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorFx {
    pub size: f64,
    pub track: Vec<Sample>,
}

fn round(v: f64, k: f64) -> f64 {
    (v * k).round() / k
}

/// 錄影時記下一點：位置有變才記；停了一陣子才動時，先補一點舊的位置（否則平滑時會在停住的期間慢慢飄過去）
pub fn push(track: &mut Vec<Sample>, t: f64, x: f64, y: f64) {
    if track.len() >= MAX_SAMPLES || ![t, x, y].iter().all(|v| v.is_finite()) {
        return;
    }
    let (t, x, y) = (round(t, 1000.0), round(x.clamp(-0.5, 1.5), 10_000.0), round(y.clamp(-0.5, 1.5), 10_000.0));
    if let Some(l) = track.last().copied() {
        if (l[1], l[2]) == (x, y) || t <= l[0] {
            return;
        }
        if t - l[0] > 0.1 {
            track.push([round(t - 0.03, 1000.0), l[1], l[2]]);
        }
    }
    track.push([t, x, y]);
}

/// 原始位置在時間 t（兩點之間直線內插；j 是上次找到的位置，往後找）
fn raw_at(pts: &[Sample], j: &mut usize, t: f64) -> (f64, f64) {
    while *j + 1 < pts.len() && pts[*j + 1][0] <= t {
        *j += 1;
    }
    let a = pts[*j];
    match pts.get(*j + 1) {
        Some(b) if t > a[0] => {
            let k = ((t - a[0]) / (b[0] - a[0]).max(1e-6)).clamp(0.0, 1.0);
            (a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k)
        }
        _ => (a[1], a[2]),
    }
}

/// 平滑後的位置：每 1/rate 秒一點，從第一點到最後一點後 0.5 秒（讓它停穩）
pub fn smooth(track: &[Sample], rate: f64) -> Vec<Sample> {
    let mut pts: Vec<Sample> = track.iter().copied().filter(|p| p.iter().all(|v| v.is_finite())).collect();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (Some(first), Some(last)) = (pts.first().copied(), pts.last().copied()) else { return vec![] };
    let dt = 1.0 / rate.clamp(10.0, 120.0);
    // 每一點之間再細分幾步（彈簧的計算才穩定）
    let sub = 4;
    let h = dt / sub as f64;
    let (mut x, mut y, mut vx, mut vy) = (first[1], first[2], 0.0, 0.0);
    let n = ((last[0] + 0.5 - first[0]) / dt).ceil() as usize + 1;
    let mut out = Vec::with_capacity(n);
    let mut j = 0;
    for i in 0..n {
        let t = first[0] + i as f64 * dt;
        out.push([t, x, y]);
        for k in 1..=sub {
            let (tx, ty) = raw_at(&pts, &mut j, t + h * k as f64);
            // 臨界阻尼的彈簧：不會晃來晃去，也不會拖太久
            vx += (SPRING * SPRING * (tx - x) - 2.0 * SPRING * vx) * h;
            vy += (SPRING * SPRING * (ty - y) - 2.0 * SPRING * vy) * h;
            x += vx * h;
            y += vy * h;
        }
    }
    out
}

/// 平滑後時間 t 的位置（預覽用；第一點之前沒有）
pub fn at(smoothed: &[Sample], t: f64) -> Option<(f64, f64)> {
    let first = smoothed.first()?;
    if t < first[0] {
        return None;
    }
    let i = smoothed.partition_point(|p| p[0] <= t).saturating_sub(1);
    let a = smoothed[i];
    Some(match smoothed.get(i + 1) {
        Some(b) => {
            let k = ((t - a[0]) / (b[0] - a[0]).max(1e-6)).clamp(0.0, 1.0);
            (a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k)
        }
        None => (a[1], a[2]),
    })
}

/// 畫面高 video_h 時游標的高度（像素）
pub fn height_for(video_h: i32, size: f64) -> u32 {
    ((video_h.max(1) as f64 / 1080.0 * BASE_H * size).round() as u32).clamp(12, 400)
}

/// 箭頭的輪廓（尖端在 tip）
fn arrow_path(k: f32, tip: (f32, f32)) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in ARROW.iter().enumerate() {
        let (px, py) = (tip.0 + x * k, tip.1 + y * k);
        if i == 0 {
            pb.move_to(px, py);
        } else {
            pb.line_to(px, py);
        }
    }
    pb.close();
    pb.finish()
}

/// 游標圖（白色箭頭、黑框、淡淡的陰影），高 h 像素（不含留白）；回傳 (圖, 尖端在圖裡的位置)
pub fn image(h: u32) -> Option<(Pixmap, (i32, i32))> {
    let k = h as f32 / ARROW_H;
    let pad = (PAD * k).ceil();
    let (w, ht) = ((12.0 * k + pad * 2.0).ceil() as u32, (ARROW_H * k + pad * 2.0).ceil() as u32);
    let mut pm = Pixmap::new(w, ht)?;
    let tip = (pad, pad);
    let mut paint = Paint { anti_alias: true, ..Default::default() };
    // 陰影：往右下偏一點的半透明黑色
    if let Some(shadow) = arrow_path(k, (tip.0 + 0.8 * k, tip.1 + 1.0 * k)) {
        paint.set_color(Color::from_rgba8(0, 0, 0, 70));
        pm.fill_path(&shadow, &paint, FillRule::Winding, Transform::identity(), None);
    }
    let path = arrow_path(k, tip)?;
    paint.set_color(Color::WHITE);
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    paint.set_color(Color::BLACK);
    let stroke = Stroke { width: (0.9 * k).max(1.0), line_join: tiny_skia::LineJoin::Round, ..Default::default() };
    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    Some((pm, (tip.0 as i32, tip.1 as i32)))
}

/// 游標圖左上角的位置（畫面 w × h 像素）
pub fn origin(p: (f64, f64), w: i32, h: i32, tip: (i32, i32)) -> (i32, i32) {
    ((p.0 * w as f64).round() as i32 - tip.0, (p.1 * h as f64).round() as i32 - tip.1)
}

/// FFmpeg sendcmd 的指令：位置有變才更新 overlay@cursor 的 x、y（原影片的時間）
pub fn commands(smoothed: &[Sample], w: i32, h: i32, tip: (i32, i32)) -> String {
    let mut s = String::new();
    let mut last = None;
    for p in smoothed {
        let xy = origin((p[1], p[2]), w, h, tip);
        if last != Some(xy) {
            let _ = writeln!(s, "{} overlay@cursor x {}, overlay@cursor y {};", num((p[0] * 1000.0).round() / 1000.0), xy.0, xy.1);
            last = Some(xy);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_changes_and_holds_still_periods() {
        let mut tr = vec![];
        push(&mut tr, 0.0, 0.1, 0.1);
        push(&mut tr, 0.03, 0.1, 0.1); // 沒動
        push(&mut tr, 0.06, 0.2, 0.1);
        // 停了 2 秒才動：先補一點舊的位置
        push(&mut tr, 2.06, 0.5, 0.5);
        push(&mut tr, 2.0, 0.9, 0.9); // 時間倒退：不記
        push(&mut tr, 2.09, f64::NAN, 0.5);
        push(&mut tr, 2.12, 3.0, -2.0); // 超出太多：限制
        assert_eq!(tr, vec![[0.0, 0.1, 0.1], [0.06, 0.2, 0.1], [2.03, 0.2, 0.1], [2.06, 0.5, 0.5], [2.12, 1.5, -0.5]]);
    }

    #[test]
    fn smoothing_follows_without_overshoot() {
        // 0.5 秒時從左邊瞬間跳到右邊
        let track = vec![[0.0, 0.1, 0.5], [0.5, 0.1, 0.5], [0.51, 0.9, 0.5], [2.0, 0.9, 0.5]];
        let s = smooth(&track, 30.0);
        assert!((s[0][0], s[0][1]) == (0.0, 0.1));
        // 跳的瞬間不會跟著跳，之後平順地追上，不會衝過頭
        let x = |t: f64| at(&s, t).unwrap().0;
        assert!(x(0.55) < 0.3, "{}", x(0.55));
        assert!(x(0.7) > 0.4 && x(0.7) < 0.9);
        assert!(x(1.2) > 0.88);
        assert!(s.iter().all(|p| p[1] <= 0.9 + 1e-9));
        // 一直遞增（沒有來回晃）
        assert!(s.windows(2).all(|w| w[1][1] >= w[0][1] - 1e-12));
        // 最後停在終點
        assert!((s.last().unwrap()[1] - 0.9).abs() < 0.001);
        assert_eq!(at(&s, -1.0), None);
        assert!(smooth(&[], 30.0).is_empty());
    }

    #[test]
    fn image_and_commands() {
        assert_eq!(height_for(1080, 2.0), 44);
        assert_eq!(height_for(720, 1.5), 22);
        let (pm, tip) = image(44).unwrap();
        assert!(tip.0 > 0 && tip.1 > 0 && pm.height() > 44 && pm.width() > 20);
        // 尖端是白色或黑框（不透明），左上角的留白是透明的
        assert!(pm.pixel(tip.0 as u32 + 1, tip.1 as u32 + 3).unwrap().alpha() > 200);
        assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
        let s = vec![[0.0, 0.5, 0.5], [0.033, 0.5, 0.5], [0.066, 0.6, 0.5]];
        let c = commands(&s, 1000, 800, (4, 4));
        assert_eq!(c, "0 overlay@cursor x 496, overlay@cursor y 396;\n0.066 overlay@cursor x 596, overlay@cursor y 396;\n");
    }
}
