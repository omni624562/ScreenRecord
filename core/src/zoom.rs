//! 跟著點擊自動放大：錄影時記下的滑鼠點擊，剪輯輸出時在點擊前後把畫面放大到點擊的地方，
//! 連續的點擊之間平移過去，沒有點擊時慢慢拉回全畫面。預覽（Rust）與輸出（FFmpeg zoompan 的運算式）用同一套公式。

use crate::format::num;

/// 點擊前多久開始放大（秒）
const RAMP_IN: f64 = 0.5;
/// 最後一次點擊後維持放大多久（秒）
const HOLD: f64 = 1.6;
/// 拉回全畫面花多久（秒）
const RAMP_OUT: f64 = 0.6;
/// 兩次點擊之間平移的時間（秒，在後一次點擊之前完成）
const PAN: f64 = 0.5;
/// 運算式太長時 FFmpeg 的命令列會超過 Windows 的上限：最多用這麼多個點
pub const MAX_POINTS: usize = 150;

/// 放大的倍率可選
pub const FACTORS: [f64; 3] = [1.5, 2.0, 2.5];

/// 一個要放大的點：(秒, x, y)，x、y 是畫面裡的位置 0～1
pub type Point = [f64; 3];

/// 整理點擊：依時間排序；0.8 秒內的連續點擊只留最後一個；太多時只留前面的
pub fn focus_points(clicks: &[Point]) -> Vec<Point> {
    let mut v: Vec<Point> = clicks.iter().copied().filter(|p| p.iter().all(|v| v.is_finite()) && p[0] >= 0.0).collect();
    v.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut out: Vec<Point> = vec![];
    for p in v {
        let p = [p[0], p[1].clamp(0.0, 1.0), p[2].clamp(0.0, 1.0)];
        match out.last_mut() {
            Some(l) if p[0] - l[0] < 0.8 => *l = p,
            _ => out.push(p),
        }
    }
    out.truncate(MAX_POINTS);
    out
}

/// 放大的時段：每個點 [t, t + HOLD]，靠得近的（拉回又放大來不及）合併
fn windows(points: &[Point]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = vec![];
    for p in points {
        let (a, b) = (p[0], p[0] + HOLD);
        match out.last_mut() {
            Some(l) if a - RAMP_IN < l.1 + RAMP_OUT => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn smooth(e: f64) -> f64 {
    let e = e.clamp(0.0, 1.0);
    e * e * (3.0 - 2.0 * e)
}

/// 時間 t 的 (倍率, 中心 x, 中心 y)
pub fn at(points: &[Point], factor: f64, t: f64) -> (f64, f64, f64) {
    let Some(first) = points.first() else { return (1.0, 0.5, 0.5) };
    let env: f64 = windows(points).iter().map(|&(a, b)| smooth(((t - (a - RAMP_IN)) / RAMP_IN).min((b + RAMP_OUT - t) / RAMP_OUT))).sum();
    let z = 1.0 + (factor - 1.0) * env.min(1.0);
    let (mut x, mut y) = (first[1], first[2]);
    for w in points.windows(2) {
        let k = smooth((t - w[1][0] + PAN) / PAN);
        x += (w[1][1] - w[0][1]) * k;
        y += (w[1][2] - w[0][2]) * k;
    }
    (z, x, y)
}

/// FFmpeg 的 zoompan（每張輸入一張輸出；it = 輸入的秒數），輸出 w × h
pub fn zoompan(points: &[Point], factor: f64, w: i32, h: i32, fps: f64) -> Option<String> {
    let first = points.first()?;
    // smooth(e) = e*e*(3-2*e)，用 st / ld 暫存，運算式短一點
    let s = |e: String| format!("st(1,clip({e},0,1));ld(1)*ld(1)*(3-2*ld(1))");
    let env: Vec<String> = windows(points).iter().map(|&(a, b)| format!("({})", s(format!("min((it-{})/{},({}-it)/{})", num(a - RAMP_IN), num(RAMP_IN), num(b + RAMP_OUT), num(RAMP_OUT))))).collect();
    let z = format!("1+{}*min(1,{})", num(factor - 1.0), env.join("+"));
    let axis = |i: usize| {
        let mut e = num(first[i]);
        for w in points.windows(2) {
            let d = w[1][i] - w[0][i];
            if d.abs() > 1e-4 {
                e.push_str(&format!("+{}*({})", num(d), s(format!("(it-{})/{}", num(w[1][0] - PAN), num(PAN)))));
            }
        }
        e
    };
    Some(format!("zoompan=z='{z}':x='({})*iw-iw/zoom/2':y='({})*ih-ih/zoom/2':d=1:s={w}x{h}:fps={}", axis(1), axis(2), num(fps)))
}

/// 原影片的時間 → 剪輯後的時間（刪掉的片段裡的點不算；加速的片段時間變短）
pub fn map_time(parts: &[(f64, f64, u32)], t: f64) -> Option<f64> {
    let mut out = 0.0;
    for &(a, b, s) in parts {
        let s = s.max(1) as f64;
        if t >= a && t < b {
            return Some(out + (t - a) / s);
        }
        out += (b - a) / s;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_windows_and_zoom_curve() {
        let pts = focus_points(&[[5.0, 0.2, 0.3], [1.0, 0.8, 0.8], [5.5, 0.25, 0.35], [20.0, 2.0, -1.0], [f64::NAN, 0.0, 0.0]]);
        // 0.8 秒內的連續點擊只留最後一個；位置限制在畫面內
        assert_eq!(pts, vec![[1.0, 0.8, 0.8], [5.5, 0.25, 0.35], [20.0, 1.0, 0.0]]);
        assert_eq!(windows(&pts), vec![(1.0, 2.6), (5.5, 7.1), (20.0, 21.6)]);
        // 遠離點擊時是全畫面；點擊時放大到指定倍率
        assert_eq!(at(&pts, 2.0, 0.0).0, 1.0);
        assert!((at(&pts, 2.0, 1.0).0 - 2.0).abs() < 1e-9);
        assert!((at(&pts, 2.0, 2.6).0 - 2.0).abs() < 1e-9);
        assert_eq!(at(&pts, 2.0, 4.0).0, 1.0);
        // 往下一個點平移：點擊時剛好到
        let (_, x, y) = at(&pts, 2.0, 5.5);
        assert!((x - 0.25).abs() < 1e-9 && (y - 0.35).abs() < 1e-9);
        assert!(at(&[], 2.0, 3.0) == (1.0, 0.5, 0.5));
        // 剪掉的地方沒有時間；加速的片段變短
        let parts = [(0.0, 4.0, 1), (6.0, 10.0, 2)];
        assert_eq!(map_time(&parts, 3.0), Some(3.0));
        assert_eq!(map_time(&parts, 5.0), None);
        assert_eq!(map_time(&parts, 8.0), Some(5.0));
    }

    #[test]
    fn zoompan_expression() {
        let pts = [[1.0, 0.8, 0.8], [5.5, 0.25, 0.8]];
        let f = zoompan(&pts, 2.0, 1920, 1080, 30.0).unwrap();
        assert!(f.starts_with("zoompan=z='1+1*min(1,(st(1,clip(min((it-0.5)/0.5,(3.2-it)/0.6),0,1));"), "{f}");
        // y 沒變就不用加平移
        assert!(f.contains(":x='(0.8+-0.55*(st(1,clip((it-5)/0.5,0,1));ld(1)*ld(1)*(3-2*ld(1))))*iw-iw/zoom/2':y='(0.8)*ih-ih/zoom/2':d=1:s=1920x1080:fps=30"), "{f}");
        assert!(zoompan(&[], 2.0, 1920, 1080, 30.0).is_none());
    }
}
