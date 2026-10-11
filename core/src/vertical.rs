//! 直式影片（9:16）：給 YouTube Shorts、Instagram Reels、TikTok。剪輯視窗「畫面裁切」分頁選：
//! - 放在中間：整個畫面縮小放在直式畫面中間，上下用同一個畫面放大、模糊當背景（什麼都不會被裁掉）
//! - 跟著游標：從畫面裁出 9:16 的一塊，像攝影機一樣跟著游標左右移動（沒有記下游標時跟著點擊）。
//!   游標在中間一半的範圍內移動時鏡頭不動，出了這個範圍才平順地移過去，看起來不會晃。

use crate::cursor::{clean, raw_at, Sample};
use crate::format::num;
use serde::{Deserialize, Serialize};
use std::fmt::Write;

/// 剪輯的設定（輸出時用）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum Vertical {
    Fit,
    /// track：游標的位置（或點擊），原影片的時間
    Follow {
        track: Vec<Sample>,
    },
}

/// 鏡頭的彈簧（比游標慢：約 0.3 秒跟上）
const SPRING: f64 = 4.0;

fn even(v: f64) -> i32 {
    ((v / 2.0).floor() as i32 * 2).max(2)
}

/// 放在中間：輸出的大小（來源高度 1080 以上用 1080×1920，否則 720×1280）
pub fn fit_size(src_h: i32) -> (i32, i32) {
    if src_h >= 1080 {
        (1080, 1920)
    } else {
        (720, 1280)
    }
}

/// 放在中間的濾鏡（接在剪輯的濾鏡後面；輸出 w × h）
pub fn fit_filter(w: i32, h: i32) -> String {
    format!(
        "split=2[vta][vtb];[vta]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},boxblur=20:2,eq=brightness=-0.08[vtbg];\
         [vtb]scale={w}:{h}:force_original_aspect_ratio=decrease[vtfg];[vtbg][vtfg]overlay=(W-w)/2:(H-h)/2"
    )
}

/// 跟著游標：裁切的大小（整個高度、寬 = 高 × 9 / 16；畫面本來就比 9:16 窄時改裁高度）
pub fn follow_size(src_w: i32, src_h: i32) -> (i32, i32) {
    let w = even((src_h as f64 * 9.0 / 16.0).min(src_w as f64));
    let h = even((w as f64 * 16.0 / 9.0).min(src_h as f64));
    (w, h)
}

/// 跟著游標：輸出太大時縮小到 1080×1920
pub fn follow_scale(w: i32, h: i32) -> Option<(i32, i32)> {
    (h > 1920).then(|| (even(1920.0 * w as f64 / h as f64), 1920))
}

/// 鏡頭的位置（裁切範圍中心的 x，0～1），每 1/rate 秒一點；view = 裁切寬度佔畫面寬度的比例
pub fn pan(track: &[Sample], rate: f64, view: f64) -> Vec<[f64; 2]> {
    let pts = clean(track);
    let (Some(first), Some(last)) = (pts.first().copied(), pts.last().copied()) else { return vec![] };
    if !(0.0..1.0).contains(&view) {
        return vec![];
    }
    let half = view / 2.0;
    // 游標在鏡頭中間一半的範圍內移動時，鏡頭不動
    let dead = half / 2.0;
    let dt = 1.0 / rate.clamp(10.0, 120.0);
    let sub = 4;
    let h = dt / sub as f64;
    let mut target = first[1].clamp(half, 1.0 - half);
    let (mut c, mut v) = (target, 0.0);
    let n = ((last[0] + 1.0 - first[0]) / dt).ceil() as usize + 1;
    let mut out = Vec::with_capacity(n);
    let mut j = 0;
    for i in 0..n {
        let t = first[0] + i as f64 * dt;
        out.push([t, c]);
        for k in 1..=sub {
            let (x, _) = raw_at(&pts, &mut j, t + h * k as f64);
            if x > target + dead {
                target = x - dead;
            } else if x < target - dead {
                target = x + dead;
            }
            target = target.clamp(half, 1.0 - half);
            v += (SPRING * SPRING * (target - c) - 2.0 * SPRING * v) * h;
            c += v * h;
        }
    }
    out
}

/// 時間 t 的鏡頭位置（預覽用）
pub fn pan_at(pan: &[[f64; 2]], t: f64) -> Option<f64> {
    let first = pan.first()?;
    if t <= first[0] {
        return Some(first[1]);
    }
    let i = pan.partition_point(|p| p[0] <= t).saturating_sub(1);
    let a = pan[i];
    Some(match pan.get(i + 1) {
        Some(b) => a[1] + (b[1] - a[1]) * ((t - a[0]) / (b[0] - a[0]).max(1e-6)).clamp(0.0, 1.0),
        None => a[1],
    })
}

/// 裁切範圍的左邊（像素）
pub fn crop_x(center: f64, src_w: i32, w: i32) -> i32 {
    ((center * src_w as f64 - w as f64 / 2.0).round() as i32).clamp(0, (src_w - w).max(0))
}

/// FFmpeg sendcmd 的指令：位置有變才更新 crop@vertical 的 x（原影片的時間）
pub fn commands(pan: &[[f64; 2]], src_w: i32, w: i32) -> String {
    let mut s = String::new();
    let mut last = None;
    for p in pan {
        let x = crop_x(p[1], src_w, w);
        if last != Some(x) {
            let _ = writeln!(s, "{} crop@vertical x {x};", num((p[0] * 1000.0).round() / 1000.0));
            last = Some(x);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(fit_size(1080), (1080, 1920));
        assert_eq!(fit_size(720), (720, 1280));
        assert_eq!(follow_size(1920, 1080), (606, 1076));
        assert_eq!(follow_size(3840, 2160), (1214, 2158));
        assert_eq!(follow_scale(1214, 2158), Some((1080, 1920)));
        assert_eq!(follow_scale(606, 1076), None);
        // 本來就是直的（比 9:16 窄）：寬不變、裁高度
        assert_eq!(follow_size(500, 1200), (500, 888));
        assert!(fit_filter(1080, 1920).ends_with("[vtbg][vtfg]overlay=(W-w)/2:(H-h)/2"));
    }

    #[test]
    fn camera_ignores_small_moves_and_follows_big_ones() {
        let view = 606.0 / 1920.0;
        // 一開始在中間附近小幅移動，3 秒時移到右邊
        let mut track = vec![];
        for i in 0..90 {
            let t = i as f64 / 30.0;
            track.push([t, 0.5 + 0.03 * (t * 7.0).sin(), 0.5]);
        }
        track.push([3.0, 0.5, 0.5]);
        track.push([3.1, 0.95, 0.5]);
        track.push([6.0, 0.95, 0.5]);
        let p = pan(&track, 30.0, view);
        let at = |t: f64| pan_at(&p, t).unwrap();
        // 小幅移動：鏡頭不動
        assert!((0..=29).map(|i| at(i as f64 * 0.1)).all(|c| (c - 0.5).abs() < 1e-6), "{:?}", &p[..5]);
        // 大幅移動：平順地跟過去，停在不超出畫面的地方
        assert!(at(3.2) < 0.7);
        assert!((at(5.5) - (1.0 - view / 2.0)).abs() < 0.01, "{}", at(5.5));
        assert!(p.iter().all(|c| c[1] >= view / 2.0 - 1e-9 && c[1] <= 1.0 - view / 2.0 + 1e-9));
        // 指令：位置有變才寫
        let cmds = commands(&p, 1920, 606);
        assert!(cmds.starts_with("0 crop@vertical x 657;\n"), "{}", &cmds[..60]);
        assert!(cmds.trim_end().ends_with("crop@vertical x 1314;"));
        assert!(cmds.lines().count() < p.len());
        assert!(pan(&track, 30.0, 1.0).is_empty());
    }
}
