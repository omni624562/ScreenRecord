//! 剪輯計算（對應 src/shared/edit.ts）：保留哪些時間區段、裁切範圍、輸出檔名。

use crate::format::{js_round, strip_mp4};
use crate::types::Rect;
use serde::{Deserialize, Serialize};

/// [開始秒, 結束秒)
pub type Range = (f64, f64);

/// 介面送來的裁切範圍（可能有小數）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropInput {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditSpec {
    /// 保留的開頭（剪掉之前的部分）
    pub start: f64,
    /// 保留的結尾（剪掉之後的部分）
    pub end: f64,
    /// 中間要刪除的片段
    pub removed: Vec<Range>,
    /// 畫面裁切（影片像素座標）；None = 不裁切
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropInput>,
}

/// 片段短於這個長度就忽略（約一張畫面以下）
const MIN_RANGE: f64 = 0.02;

fn ms(t: f64) -> f64 {
    js_round(t * 1000.0) / 1000.0
}

/// 合併重疊或相鄰的區段，並限制在 [0, duration] 內
pub fn normalize_ranges(ranges: &[Range], duration: f64) -> Vec<Range> {
    let mut sorted: Vec<Range> = ranges
        .iter()
        .map(|&(a, b)| (a.min(b).max(0.0), a.max(b).min(duration)))
        .filter(|&(a, b)| b - a >= MIN_RANGE)
        .collect();
    sorted.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut out: Vec<Range> = Vec::new();
    for r in sorted {
        match out.last_mut() {
            Some(last) if r.0 <= last.1 => last.1 = last.1.max(r.1),
            _ => out.push(r),
        }
    }
    out.into_iter().map(|(a, b)| (ms(a), ms(b))).collect()
}

/// 實際保留的區段：[start, end] 扣掉 removed
pub fn keep_ranges(duration: f64, spec: &EditSpec) -> Vec<Range> {
    let start = spec.start.min(duration).max(0.0);
    let end = spec.end.min(duration).max(start);
    let mut keep: Vec<Range> = vec![(start, end)];
    for (a, b) in normalize_ranges(&spec.removed, duration) {
        keep = keep
            .into_iter()
            .flat_map(|(s, e)| {
                if b <= s || a >= e {
                    return vec![(s, e)];
                }
                let mut parts = Vec::new();
                if a > s {
                    parts.push((s, a));
                }
                if b < e {
                    parts.push((b, e));
                }
                parts
            })
            .collect();
    }
    keep.into_iter().filter(|&(a, b)| b - a >= MIN_RANGE).map(|(a, b)| (ms(a), ms(b))).collect()
}

pub fn total_length(ranges: &[Range]) -> f64 {
    ranges.iter().map(|(a, b)| b - a).sum()
}

/// 裁切範圍：限制在畫面內、寬高取偶數（yuv420p 需要），至少 16px；等於整個畫面時回傳 None
pub fn normalize_crop(crop: Option<CropInput>, width: i32, height: i32) -> Option<Rect> {
    let crop = crop?;
    let (wf, hf) = (width as f64, height as f64);
    let x = js_round(crop.x).min(wf - 16.0).max(0.0);
    let y = js_round(crop.y).min(hf - 16.0).max(0.0);
    let w = (js_round(crop.width).min(wf - x).max(16.0) / 2.0).floor() * 2.0;
    let h = (js_round(crop.height).min(hf - y).max(16.0) / 2.0).floor() * 2.0;
    if x == 0.0 && y == 0.0 && w >= wf - 1.0 && h >= hf - 1.0 {
        return None;
    }
    Some(Rect { x: x as i32, y: y as i32, width: w as i32, height: h as i32 })
}

/// Rec_xxx.mp4 → Rec_xxx_cut.mp4
pub fn cut_file_name(source_name: &str) -> String {
    format!("{}_cut.mp4", strip_mp4(source_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(start: f64, end: f64, removed: &[Range]) -> EditSpec {
        EditSpec { start, end, removed: removed.to_vec(), crop: None }
    }

    #[test]
    fn keep_head_and_tail() {
        assert_eq!(keep_ranges(60.0, &spec(5.0, 50.0, &[])), vec![(5.0, 50.0)]);
    }

    #[test]
    fn remove_multiple_middle_parts() {
        let keep = keep_ranges(60.0, &spec(2.0, 58.0, &[(40.0, 45.0), (10.0, 20.0), (15.0, 25.0), (55.0, 70.0)]));
        assert_eq!(keep, vec![(2.0, 10.0), (25.0, 40.0), (45.0, 55.0)]);
        assert_eq!(total_length(&keep), 33.0);
    }

    #[test]
    fn removal_covering_start() {
        assert_eq!(keep_ranges(30.0, &spec(0.0, 30.0, &[(0.0, 12.5)])), vec![(12.5, 30.0)]);
    }

    #[test]
    fn clamp_to_duration() {
        assert_eq!(keep_ranges(10.0, &spec(-3.0, 99.0, &[])), vec![(0.0, 10.0)]);
    }

    #[test]
    fn merge_adjacent_and_drop_tiny() {
        assert_eq!(normalize_ranges(&[(1.0, 2.0), (2.0, 3.0), (5.0, 5.001)], 10.0), vec![(1.0, 3.0)]);
    }

    #[test]
    fn crop() {
        let c = |x, y, w, h| Some(CropInput { x, y, width: w, height: h });
        assert_eq!(normalize_crop(c(101.0, 50.0, 301.0, 9999.0), 1920, 1080), Some(Rect { x: 101, y: 50, width: 300, height: 1030 }));
        assert_eq!(normalize_crop(c(0.0, 0.0, 1920.0, 1080.0), 1920, 1080), None);
        assert_eq!(normalize_crop(c(10.0, 10.0, 3.0, 3.0), 1920, 1080), Some(Rect { x: 10, y: 10, width: 16, height: 16 }));
    }

    #[test]
    fn cut_name() {
        assert_eq!(cut_file_name("Rec_2026-10-06_08-00-00.mp4"), "Rec_2026-10-06_08-00-00_cut.mp4");
    }

    #[test]
    fn spec_json_matches_ui() {
        let s: EditSpec = serde_json::from_str(r#"{"start":1,"end":5.5,"removed":[[2,3]],"crop":{"x":1,"y":2,"width":30,"height":40}}"#).unwrap();
        assert_eq!(s.removed, vec![(2.0, 3.0)]);
        assert_eq!(s.crop.unwrap().width, 30.0);
    }
}
