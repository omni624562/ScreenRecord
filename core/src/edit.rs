//! 剪輯計算：保留哪些時間區段、裁切範圍、輸出檔名（結果與 2.x 版相同，見 tests/vectors.json）。

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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
    /// 畫面上的標註（文字、箭頭、框線、編號由介面畫成透明 PNG；馬賽克 / 模糊由 FFmpeg 處理）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<Overlay>,
    /// 聲音處理
    #[serde(default, skip_serializing_if = "AudioFx::is_default")]
    pub audio: AudioFx,
    /// 加速的片段（原影片的時間）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fast: Vec<FastRange>,
    /// 跟著點擊放大
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<ClickZoom>,
    /// 背景與圓角（影片縮小放在背景中間）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<crate::video_frame::VideoFrame>,
}

/// 跟著點擊放大：倍率與錄影時記下的點擊（原影片的秒數, x, y；x、y 為 0～1）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClickZoom {
    pub factor: f64,
    pub clicks: Vec<[f64; 3]>,
}

/// 局部加速：這段時間以 speed 倍播放（加速的片段沒有聲音）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FastRange {
    /// 毫秒（整數，方便比對與存檔）
    pub start_ms: u64,
    pub end_ms: u64,
    pub speed: u32,
}

impl FastRange {
    pub fn start(&self) -> f64 {
        self.start_ms as f64 / 1000.0
    }
    pub fn end(&self) -> f64 {
        self.end_ms as f64 / 1000.0
    }
}

/// 局部加速可選的倍率（整數倍：每 N 張留一張）
pub const FAST_SPEEDS: [u32; 4] = [2, 4, 8, 16];

/// 把 [a, b) 設成 speed 倍（1 = 恢復原速）；和原有的加速片段重疊的部分以新的為準
pub fn set_fast(list: &mut Vec<FastRange>, a: f64, b: f64, speed: u32) {
    let (a, b) = (a.min(b).max(0.0), a.max(b).max(0.0));
    let (am, bm) = ((a * 1000.0).round() as u64, (b * 1000.0).round() as u64);
    if bm <= am {
        return;
    }
    let mut out = vec![];
    for r in list.drain(..) {
        if r.end_ms <= am || r.start_ms >= bm {
            out.push(r);
            continue;
        }
        if r.start_ms < am {
            out.push(FastRange { end_ms: am, ..r });
        }
        if r.end_ms > bm {
            out.push(FastRange { start_ms: bm, ..r });
        }
    }
    if speed > 1 {
        out.push(FastRange { start_ms: am, end_ms: bm, speed });
    }
    out.retain(|r| r.end_ms >= r.start_ms + 20);
    out.sort_by_key(|r| r.start_ms);
    // 相鄰而且倍率相同的合併
    let mut merged: Vec<FastRange> = vec![];
    for r in out {
        match merged.last_mut() {
            Some(l) if l.end_ms >= r.start_ms && l.speed == r.speed => l.end_ms = l.end_ms.max(r.end_ms),
            _ => merged.push(r),
        }
    }
    *list = merged;
}

/// 保留的片段再依加速切開：(開始, 結束, 倍率)；倍率 1 = 原速
pub fn keep_parts(duration: f64, spec: &EditSpec) -> Vec<(f64, f64, u32)> {
    let mut fast = spec.fast.clone();
    fast.sort_by_key(|f| f.start_ms);
    let mut out = vec![];
    for (a, b) in keep_ranges(duration, spec) {
        let mut t = a;
        for f in fast.iter().filter(|f| f.speed > 1 && f.end() > a && f.start() < b) {
            let (fa, fb) = (f.start().max(a), f.end().min(b));
            if fa - t >= MIN_RANGE {
                out.push((t, fa, 1));
            }
            if fb - fa >= MIN_RANGE {
                out.push((fa.max(t), fb, f.speed));
            }
            t = t.max(fb);
        }
        if b - t >= MIN_RANGE {
            out.push((t, b, 1));
        }
    }
    out.into_iter().map(|(a, b, s)| (ms(a), ms(b), s)).collect()
}

/// 輸出的長度（加速的片段變短）
pub fn output_length(parts: &[(f64, f64, u32)]) -> f64 {
    parts.iter().map(|&(a, b, s)| (b - a) / s.max(1) as f64).sum()
}

/// 剪輯時的聲音處理
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFx {
    /// 降噪（減少風扇、冷氣等穩定的背景雜音）
    #[serde(default)]
    pub denoise: bool,
    /// 音量平衡（忽大忽小變平均，整體調到適合聆聽的音量）
    #[serde(default)]
    pub normalize: bool,
    /// 不要聲音
    #[serde(default)]
    pub mute: bool,
}

impl AudioFx {
    pub fn is_default(&self) -> bool {
        *self == AudioFx::default()
    }

    /// FFmpeg 的聲音濾鏡（接在剪輯之後）
    pub fn filters(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        if self.denoise {
            f.push("afftdn=nf=-25");
        }
        if self.normalize {
            // loudnorm 會把取樣率改成 192k：轉回 48k
            f.push("loudnorm=I=-16:TP=-1.5:LRA=11");
            f.push("aresample=48000");
        }
        f
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayKind {
    /// 介面畫好的透明 PNG（png 欄位）
    Image,
    Blur,
    Mosaic,
}

/// 一個標註：位置與大小為原影片的像素座標，start / end 為原影片的時間（秒，剪輯前）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    pub kind: OverlayKind,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub start: f64,
    pub end: f64,
    /// PNG 檔案內容
    #[serde(skip)]
    pub png: Option<Vec<u8>>,
    /// 馬賽克 / 模糊的形狀遮罩（白色 = 範圍內，PNG）；沒有就是方形
    #[serde(skip)]
    pub mask: Option<Vec<u8>>,
    /// 反過來：範圍外模糊（或馬賽克），範圍內清楚
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
}

/// 片段短於這個長度就忽略（約一張畫面以下）
const MIN_RANGE: f64 = 0.02;

fn ms(t: f64) -> f64 {
    js_round(t * 1000.0) / 1000.0
}

/// 合併重疊或相鄰的區段，並限制在 [0, duration] 內
pub fn normalize_ranges(ranges: &[Range], duration: f64) -> Vec<Range> {
    let mut sorted: Vec<Range> = ranges.iter().map(|&(a, b)| (a.min(b).max(0.0), a.max(b).min(duration))).filter(|&(a, b)| b - a >= MIN_RANGE).collect();
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

    #[test]
    fn fast_ranges_split_the_kept_parts() {
        let mut fast = vec![];
        set_fast(&mut fast, 2.0, 6.0, 4);
        set_fast(&mut fast, 8.0, 9.0, 2);
        assert_eq!(fast.len(), 2);
        // 重疊的部分以新的為準；設回原速就拿掉
        set_fast(&mut fast, 5.0, 8.5, 8);
        assert_eq!(fast.iter().map(|f| (f.start_ms, f.end_ms, f.speed)).collect::<Vec<_>>(), vec![(2000, 5000, 4), (5000, 8500, 8), (8500, 9000, 2)]);
        set_fast(&mut fast, 0.0, 10.0, 1);
        assert!(fast.is_empty());
        // 相鄰而且倍率相同的合併
        set_fast(&mut fast, 1.0, 2.0, 4);
        set_fast(&mut fast, 2.0, 3.0, 4);
        assert_eq!(fast.iter().map(|f| (f.start_ms, f.end_ms)).collect::<Vec<_>>(), vec![(1000, 3000)]);

        let mut sp = spec(0.0, 10.0, &[(4.0, 5.0)]);
        set_fast(&mut sp.fast, 2.0, 6.0, 4);
        let parts = keep_parts(10.0, &sp);
        assert_eq!(parts, vec![(0.0, 2.0, 1), (2.0, 4.0, 4), (5.0, 6.0, 4), (6.0, 10.0, 1)]);
        assert!((output_length(&parts) - (2.0 + 0.5 + 0.25 + 4.0)).abs() < 1e-9);
        // 沒有加速時和 keep_ranges 一樣
        let plain = spec(1.0, 9.0, &[(3.0, 4.0)]);
        assert_eq!(keep_parts(10.0, &plain), keep_ranges(10.0, &plain).into_iter().map(|(a, b)| (a, b, 1)).collect::<Vec<_>>());
    }

    fn spec(start: f64, end: f64, removed: &[Range]) -> EditSpec {
        EditSpec { start, end, removed: removed.to_vec(), crop: None, overlays: vec![], ..Default::default() }
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
