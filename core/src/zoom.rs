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

/// 手動放大的一塊：從 start 到 end（秒）放大到畫面上的範圍（x、y、w、h 是 0～1）
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Region {
    pub start: f64,
    pub end: f64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 手動放大：拉近、拉遠各花多久（秒；放大的時間很短時縮短成一半）
const REGION_RAMP: f64 = 0.6;
/// 最多放大幾倍
pub const REGION_MAX: f64 = 6.0;

impl Region {
    /// 放大的倍率：整塊放得進畫面
    pub fn factor(&self) -> f64 {
        (1.0 / self.w.max(1e-3)).min(1.0 / self.h.max(1e-3)).clamp(1.0, REGION_MAX)
    }
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    /// 時間、範圍合理
    pub fn valid(&self) -> bool {
        [self.start, self.end, self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite()) && self.end - self.start >= 0.3 && self.w > 0.02 && self.h > 0.02
    }
}

/// 整理手動放大：去掉不合理的，依時間排序，和前一塊重疊的從前一塊結束後開始
pub fn clean_regions(list: &[Region]) -> Vec<Region> {
    let mut v: Vec<Region> = list.iter().copied().filter(Region::valid).collect();
    v.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Region> = vec![];
    for mut r in v {
        if let Some(l) = out.last() {
            r.start = r.start.max(l.end);
        }
        if r.valid() {
            out.push(r);
        }
    }
    out
}

/// 一段放大（輸出用）：開始、結束（秒）、倍率、中心 x、y
pub type Span = (f64, f64, f64, f64, f64);

fn ramp(a: f64, b: f64) -> f64 {
    REGION_RAMP.min((b - a) / 2.0).max(0.05)
}

/// 時間 t 的 (倍率, 中心 x, 中心 y)；沒有在放大時 None
pub fn region_at(spans: &[Span], t: f64) -> Option<(f64, f64, f64)> {
    spans.iter().find(|s| t > s.0 && t < s.1).map(|&(a, b, f, cx, cy)| {
        let r = ramp(a, b);
        let e = smooth(((t - a) / r).min((b - t) / r));
        (1.0 + (f - 1.0) * e, 0.5 + (cx - 0.5) * e, 0.5 + (cy - 0.5) * e)
    })
}

/// 手動放大的 zoompan（it = 輸入的秒數，剪輯後的時間），輸出 w × h；spans 不能重疊
pub fn regions_zoompan(spans: &[Span], w: i32, h: i32, fps: f64) -> Option<String> {
    if spans.is_empty() {
        return None;
    }
    let s = |e: String| format!("st(1,clip({e},0,1));ld(1)*ld(1)*(3-2*ld(1))");
    let env = |a: f64, b: f64| {
        let r = ramp(a, b);
        s(format!("min((it-{})/{},({}-it)/{})", num(a), num(r), num(b), num(r)))
    };
    let sum = |f: &dyn Fn(&Span) -> f64| spans.iter().map(|sp| format!("{}*({})", num(f(sp)), env(sp.0, sp.1))).collect::<Vec<_>>().join("+");
    let z = format!("1+{}", sum(&|sp| sp.2 - 1.0));
    let x = format!("0.5+{}", sum(&|sp| sp.3 - 0.5));
    let y = format!("0.5+{}", sum(&|sp| sp.4 - 0.5));
    Some(format!("zoompan=z='{z}':x='({x})*iw-iw/zoom/2':y='({y})*ih-ih/zoom/2':d=1:s={w}x{h}:fps={}", num(fps)))
}

/// 手動放大換成剪輯後的時間：開始落在刪掉的片段裡時從下一段保留的開始，結束落在刪掉的片段裡時到上一段保留的結束
pub fn map_regions(parts: &[crate::edit::Part], regions: &[Region]) -> Vec<Span> {
    clean_regions(regions)
        .into_iter()
        .filter_map(|r| {
            let a = map_time(parts, r.start).or_else(|| parts.iter().find(|p| !p.is_hold() && p.a > r.start).and_then(|p| map_time(parts, p.a)))?;
            let b = map_time(parts, r.end - 0.001).or_else(|| {
                let p = parts.iter().rev().find(|p| !p.is_hold() && p.b < r.end)?;
                map_time(parts, p.b - 0.001)
            })?;
            let (cx, cy) = r.center();
            (b - a >= 0.3).then_some((a, b, r.factor(), cx, cy))
        })
        .collect()
}

/// 跟著點擊放大時，手動放大的前後不要再放大點擊（兩種不會同時發生）
pub fn outside_spans(points: &[Point], spans: &[Span]) -> Vec<Point> {
    points.iter().copied().filter(|p| spans.iter().all(|s| p[0] < s.0 - HOLD - RAMP_OUT || p[0] > s.1 + RAMP_IN)).collect()
}

/// 原影片的時間 → 剪輯後的時間（刪掉的片段裡的點不算；變速、定格都算進去）
pub fn map_time(parts: &[crate::edit::Part], t: f64) -> Option<f64> {
    crate::edit::map_time(parts, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_regions_zoom_in_and_out() {
        let r = Region { start: 2.0, end: 5.0, x: 0.5, y: 0.5, w: 0.25, h: 0.5 };
        assert_eq!(r.factor(), 2.0);
        assert_eq!(r.center(), (0.625, 0.75));
        // 重疊的往後挪、太短的移除
        let list = clean_regions(&[Region { start: 4.0, end: 6.0, ..r }, r, Region { start: 9.0, end: 9.1, ..r }]);
        assert_eq!(list.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(), vec![(2.0, 5.0), (5.0, 6.0)]);
        // 剪掉 0～1：剪輯後 1～4
        let parts = [crate::edit::Part::new(1.0, 10.0, 1.0)];
        let spans = map_regions(&parts, &[r]);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].0 - 1.0).abs() < 1e-9 && (spans[0].1 - 4.0).abs() < 0.01 && spans[0].2 == 2.0, "{spans:?}");
        // 中間放大到位，頭尾慢慢拉近拉遠
        assert_eq!(region_at(&spans, 0.5), None);
        let (z, cx, _) = region_at(&spans, 2.5).unwrap();
        assert!((z - 2.0).abs() < 1e-9 && (cx - 0.625).abs() < 1e-9);
        assert!(region_at(&spans, 1.1).unwrap().0 < 1.2);
        let f = regions_zoompan(&spans, 1280, 720, 30.0).unwrap();
        assert!(f.starts_with("zoompan=z='1+1*(") && f.ends_with(":d=1:s=1280x720:fps=30"), "{f}");
        // 點擊在手動放大附近的不放大
        let pts = [[0.5, 0.1, 0.1], [2.0, 0.2, 0.2], [9.0, 0.3, 0.3]];
        assert_eq!(outside_spans(&pts, &spans), vec![[9.0, 0.3, 0.3]]);
    }

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
        let parts = [crate::edit::Part::new(0.0, 4.0, 1.0), crate::edit::Part::new(6.0, 10.0, 2.0)];
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
