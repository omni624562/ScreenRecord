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
    /// 平滑、放大的游標（錄影時記下的位置）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<crate::cursor::CursorFx>,
    /// 直式影片（9:16）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical: Option<crate::vertical::Vertical>,
    /// 片頭卡片
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<crate::cards::TitleCard>,
    /// 片尾卡片
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outro: Option<crate::cards::TitleCard>,
    /// 背景音樂
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music: Option<crate::cards::Music>,
    /// 刪掉片段的接縫淡入淡出
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fade_cuts: bool,
    /// 定格畫面（原影片的時間）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub freezes: Vec<Freeze>,
    /// 手動放大（原影片的時間）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zoom_regions: Vec<crate::zoom::Region>,
    /// 整支影片加速（倍率；1 = 不加速）：和局部變速相乘，定格不變
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub speed: f64,
    /// 整支加速時保留聲音（變快、音調不變）；局部變速、定格的地方仍然沒有原本的聲音
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub speed_audio: bool,
    /// 保留的片段的順序：各片段開頭的時間（毫秒）；空的 = 原本的順序
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<u64>,
    /// 旁白（錄的或文字轉語音）、點擊音效
    #[serde(default, skip_serializing_if = "crate::narration::Narration::is_empty")]
    pub narration: crate::narration::Narration,
}

fn one() -> f64 {
    1.0
}
fn is_one(v: &f64) -> bool {
    (*v - 1.0).abs() < 1e-9
}

/// 整支加速可選的倍率
pub const WHOLE_SPEEDS: [f64; 7] = [1.0, 1.25, 1.5, 2.0, 4.0, 8.0, 16.0];

/// 整支加速的倍率（不合理的當成 1）
pub fn whole_speed(spec: &EditSpec) -> f64 {
    if spec.speed.is_finite() && (1.0..=16.0).contains(&spec.speed) {
        spec.speed
    } else {
        1.0
    }
}

/// 定格畫面：在原影片 at_ms 的畫面停住 ms 毫秒
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freeze {
    pub at_ms: u64,
    pub ms: u64,
}

impl Freeze {
    pub fn at(&self) -> f64 {
        self.at_ms as f64 / 1000.0
    }
    pub fn secs(&self) -> f64 {
        self.ms as f64 / 1000.0
    }
}

/// 定格可選的秒數
pub const FREEZE_SECS: [f64; 4] = [1.0, 2.0, 3.0, 5.0];

/// 剪好的影片裡的一段：原影片 [a, b) 以 rate 倍播放（大於 1 加速、小於 1 放慢）；
/// hold 大於 0 時是定格（a == b，停住 hold 秒）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    pub a: f64,
    pub b: f64,
    pub rate: f64,
    pub hold: f64,
    /// 變速了還保留原本的聲音（整支加速、選了保留聲音）：聲音跟著變快、音調不變
    pub keep_audio: bool,
}

impl Part {
    pub fn new(a: f64, b: f64, rate: f64) -> Part {
        Part { a, b, rate, hold: 0.0, keep_audio: false }
    }
    /// 在剪好的影片裡多長
    pub fn len(&self) -> f64 {
        if self.hold > 0.0 {
            self.hold
        } else {
            (self.b - self.a) / self.rate.max(0.01)
        }
    }
    pub fn is_hold(&self) -> bool {
        self.hold > 0.0
    }
    /// 不是原速（加速、放慢或定格）
    pub fn retimed(&self) -> bool {
        self.is_hold() || (self.rate - 1.0).abs() > 1e-9
    }
    /// 沒有原本的聲音（變速、定格，除非是保留聲音的整支加速）
    pub fn silent(&self) -> bool {
        self.retimed() && !(self.keep_audio && !self.is_hold())
    }
}

/// 跟著點擊放大：倍率與錄影時記下的點擊（原影片的秒數, x, y；x、y 為 0～1）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClickZoom {
    pub factor: f64,
    pub clicks: Vec<[f64; 3]>,
}

/// 局部加速或放慢：這段時間以 speed 倍播放（大於 1 加速、小於 1 放慢；變速的片段沒有原本的聲音）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FastRange {
    /// 毫秒（整數，方便比對與存檔）
    pub start_ms: u64,
    pub end_ms: u64,
    pub speed: f64,
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

/// 局部放慢可選的倍率
pub const SLOW_SPEEDS: [f64; 2] = [0.5, 0.25];

/// 倍率可以用：加速 2～16 倍、放慢 0.25～0.5 倍
pub fn valid_speed(s: f64) -> bool {
    s.is_finite() && ((1.01..=16.0).contains(&s) || (0.25..=0.99).contains(&s))
}

/// 把 [a, b) 設成 speed 倍（1 = 恢復原速）；和原有的變速片段重疊的部分以新的為準
pub fn set_fast(list: &mut Vec<FastRange>, a: f64, b: f64, speed: f64) {
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
    if valid_speed(speed) {
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

/// 保留的片段再依變速切開，並插入定格
pub fn keep_parts(duration: f64, spec: &EditSpec) -> Vec<Part> {
    let mut fast = spec.fast.clone();
    fast.sort_by_key(|f| f.start_ms);
    let mut out: Vec<Part> = vec![];
    for (a, b) in ordered_ranges(duration, spec) {
        let mut t = a;
        for f in fast.iter().filter(|f| valid_speed(f.speed) && f.end() > a && f.start() < b) {
            let (fa, fb) = (f.start().max(a), f.end().min(b));
            if fa - t >= MIN_RANGE {
                out.push(Part::new(t, fa, 1.0));
            }
            if fb - fa >= MIN_RANGE {
                out.push(Part::new(fa.max(t), fb, f.speed));
            }
            t = t.max(fb);
        }
        if b - t >= MIN_RANGE {
            out.push(Part::new(t, b, 1.0));
        }
    }
    // 整支加速：每段的倍率乘上去（局部加速、放慢一起變快）；原速的片段可以保留聲音
    let g = whole_speed(spec);
    let mut out: Vec<Part> = out.into_iter().map(|p| Part { keep_audio: spec.speed_audio && g > 1.0 && (p.rate - 1.0).abs() < 1e-9, ..Part::new(ms(p.a), ms(p.b), p.rate * g) }).collect();
    // 定格：落在保留的片段裡（或最後一段的結尾）才有；在那裡把片段切開
    let mut freezes = spec.freezes.clone();
    freezes.sort_by_key(|f| f.at_ms);
    for f in freezes.iter().filter(|f| f.ms >= 100) {
        let t = ms(f.at());
        let hold = Part { a: t, b: t, rate: 1.0, hold: ms(f.secs()), keep_audio: false };
        if let Some(i) = out.iter().position(|p| !p.is_hold() && t >= p.a && t < p.b) {
            let p = out[i];
            if t - p.a < MIN_RANGE {
                out.insert(i, hold);
            } else {
                out[i] = Part { b: t, ..p };
                out.insert(i + 1, hold);
                out.insert(i + 2, Part { a: t, ..p });
            }
        } else if out.last().is_some_and(|p| !p.is_hold() && (t - p.b).abs() < 0.05) {
            out.push(Part { a: out.last().unwrap().b, b: out.last().unwrap().b, ..hold });
        }
    }
    out
}

/// 保留的片段，依 spec.order 排好。順序裡記的是片段的開頭；之後又刪掉一段、片段被切開時，
/// 後半段跟著原本的那一段（開頭在它之前、最近的那個）；更前面沒有的放在最後
pub fn ordered_ranges(duration: f64, spec: &EditSpec) -> Vec<Range> {
    let mut list = keep_ranges(duration, spec);
    if spec.order.is_empty() {
        return list;
    }
    let key = |r: &Range| {
        let ms = (r.0 * 1000.0).round() as u64;
        let parent = spec.order.iter().enumerate().filter(|(_, &o)| o <= ms + 20).max_by_key(|(_, &o)| o).map(|(i, _)| i);
        (parent.unwrap_or(spec.order.len()), ms)
    };
    list.sort_by_key(key);
    list
}

/// 片段的順序和原本不一樣（有片段往前移）
pub fn reordered(parts: &[Part]) -> bool {
    parts.windows(2).any(|w| w[1].a < w[0].b - 0.001)
}

/// 接縫淡出、淡入各多久（秒）
pub const CUT_FADE: f64 = 0.2;

/// 接縫淡入淡出：剪好的影片裡每個接縫的時間與淡出（淡入）長度。
/// 只有中間刪掉片段的地方才算接縫（變速、定格的交界不算）；片段很短時縮短，不超過前後片段的一半
pub fn cut_fades(parts: &[Part]) -> Vec<(f64, f64)> {
    let mut out = vec![];
    let mut t = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let len = p.len();
        if i > 0 {
            let prev = parts[i - 1];
            if (p.a - prev.b).abs() > 0.001 {
                let d = CUT_FADE.min(prev.len() / 2.0).min(len / 2.0);
                if d >= 0.04 {
                    out.push((ms(t), ms(d)));
                }
            }
        }
        t += len;
    }
    out
}

/// 輸出的長度（加速的片段變短、放慢的變長，加上定格）
pub fn output_length(parts: &[Part]) -> f64 {
    parts.iter().map(Part::len).sum()
}

/// 原影片的時間 → 剪輯後的時間（刪掉的片段裡的點不算）
pub fn map_time(parts: &[Part], t: f64) -> Option<f64> {
    let mut out = 0.0;
    for p in parts {
        if !p.is_hold() && t >= p.a && t < p.b {
            return Some(out + (t - p.a) / p.rate.max(0.01));
        }
        out += p.len();
    }
    None
}

/// 剪輯後的時間 → 原影片的時間（定格時是定格的那一格）
pub fn source_time(parts: &[Part], out_t: f64) -> Option<f64> {
    let mut o = 0.0;
    for p in parts {
        let len = p.len();
        if out_t < o + len {
            return Some(if p.is_hold() { p.a } else { p.a + (out_t - o).max(0.0) * p.rate });
        }
        o += len;
    }
    None
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
    /// 人聲加強（濾掉低頻的嗡嗡聲、講話的頻段稍微提高、大小聲壓得比較平均）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub voice: bool,
    /// 分軌錄影：系統聲音與麥克風分別調整音量後重新混合（降噪只處理麥克風）；None = 用混好的那條
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<TrackMix>,
}

/// 人聲加強：濾掉 80 Hz 以下（冷氣、桌面震動），3 kHz 附近（咬字）提高 3 dB，再壓縮大小聲
pub const VOICE_FX: &str = "highpass=f=80,equalizer=f=3000:t=q:w=1.2:g=3,acompressor=threshold=0.1:ratio=3:attack=10:release=200:makeup=2";

/// 分軌錄影的音量（%）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMix {
    pub system: u32,
    pub mic: u32,
}

impl Default for TrackMix {
    fn default() -> Self {
        TrackMix { system: 100, mic: 100 }
    }
}

impl AudioFx {
    pub fn is_default(&self) -> bool {
        *self == AudioFx::default() || AudioFx { tracks: None, ..*self } == AudioFx::default() && self.tracks == Some(TrackMix::default())
    }

    /// 分軌時重新混合系統聲音與麥克風的濾鏡（輸入 [0:a:1]、[0:a:2]，輸出 [amixed]）
    pub fn track_graph(&self) -> Option<String> {
        let t = self.tracks?;
        let mut mic = vec![];
        if self.denoise {
            mic.push("afftdn=nf=-25".to_string());
        }
        if self.voice {
            mic.push(VOICE_FX.to_string());
        }
        mic.push(format!("volume={}", t.mic as f64 / 100.0));
        let mic = mic.join(",");
        Some(format!("[0:a:1]volume={}[tsys];[0:a:2]{mic}[tmic];[tsys][tmic]amix=inputs=2:duration=longest:normalize=0[amixed]", t.system as f64 / 100.0))
    }

    /// FFmpeg 的聲音濾鏡（接在剪輯之後）
    pub fn filters(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        // 分軌時降噪只套用在麥克風（見 track_graph）
        if self.denoise && self.tracks.is_none() {
            f.push("afftdn=nf=-25");
        }
        // 分軌時人聲加強只套用在麥克風（見 track_graph）
        if self.voice && self.tracks.is_none() {
            f.push(VOICE_FX);
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

/// 把 cut 從刪除的區段裡拿掉（還原這一段；它前後還是刪除的部分留著）
pub fn unremove(removed: &[Range], cut: Range, duration: f64) -> Vec<Range> {
    let mut out = vec![];
    for &(a, b) in removed {
        if cut.1 <= a || cut.0 >= b {
            out.push((a, b));
            continue;
        }
        out.push((a, cut.0.max(a)));
        out.push((cut.1.min(b), b));
    }
    normalize_ranges(&out, duration)
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
    fn unremove_restores_only_that_part() {
        let removed = vec![(1.0, 5.0), (8.0, 9.0)];
        assert_eq!(unremove(&removed, (2.0, 3.0), 10.0), vec![(1.0, 2.0), (3.0, 5.0), (8.0, 9.0)]);
        // 整段還原、沒有重疊的不動
        assert_eq!(unremove(&removed, (0.5, 5.5), 10.0), vec![(8.0, 9.0)]);
        assert_eq!(unremove(&removed, (6.0, 7.0), 10.0), removed);
    }

    #[test]
    fn fast_ranges_split_the_kept_parts() {
        let mut fast = vec![];
        set_fast(&mut fast, 2.0, 6.0, 4.0);
        set_fast(&mut fast, 8.0, 9.0, 2.0);
        assert_eq!(fast.len(), 2);
        // 重疊的部分以新的為準；設回原速就拿掉
        set_fast(&mut fast, 5.0, 8.5, 8.0);
        assert_eq!(fast.iter().map(|f| (f.start_ms, f.end_ms, f.speed)).collect::<Vec<_>>(), vec![(2000, 5000, 4.0), (5000, 8500, 8.0), (8500, 9000, 2.0)]);
        set_fast(&mut fast, 0.0, 10.0, 1.0);
        assert!(fast.is_empty());
        // 相鄰而且倍率相同的合併
        set_fast(&mut fast, 1.0, 2.0, 4.0);
        set_fast(&mut fast, 2.0, 3.0, 4.0);
        assert_eq!(fast.iter().map(|f| (f.start_ms, f.end_ms)).collect::<Vec<_>>(), vec![(1000, 3000)]);
        // 不能用的倍率不會加
        set_fast(&mut fast, 5.0, 6.0, 0.1);
        assert_eq!(fast.len(), 1);

        let mut sp = spec(0.0, 10.0, &[(4.0, 5.0)]);
        set_fast(&mut sp.fast, 2.0, 6.0, 4.0);
        let parts = keep_parts(10.0, &sp);
        assert_eq!(parts, vec![Part::new(0.0, 2.0, 1.0), Part::new(2.0, 4.0, 4.0), Part::new(5.0, 6.0, 4.0), Part::new(6.0, 10.0, 1.0)]);
        assert!((output_length(&parts) - (2.0 + 0.5 + 0.25 + 4.0)).abs() < 1e-9);
        // 沒有變速時和 keep_ranges 一樣
        let plain = spec(1.0, 9.0, &[(3.0, 4.0)]);
        assert_eq!(keep_parts(10.0, &plain), keep_ranges(10.0, &plain).into_iter().map(|(a, b)| Part::new(a, b, 1.0)).collect::<Vec<_>>());
    }

    #[test]
    fn whole_video_speed_multiplies() {
        let mut sp = spec(0.0, 10.0, &[]);
        set_fast(&mut sp.fast, 2.0, 4.0, 4.0);
        sp.freezes = vec![Freeze { at_ms: 6000, ms: 1000 }];
        sp.speed = 2.0;
        sp.speed_audio = true;
        let parts = keep_parts(10.0, &sp);
        // 2 秒 /2 + 2 秒 /8 + 2 秒 /2 + 定格 1 秒 + 4 秒 /2
        assert!((output_length(&parts) - (1.0 + 0.25 + 1.0 + 1.0 + 2.0)).abs() < 1e-9);
        // 原速的片段保留聲音，局部加速的、定格的沒有
        assert_eq!(parts.iter().map(|p| p.silent()).collect::<Vec<_>>(), vec![false, true, false, true, false]);
        sp.speed_audio = false;
        assert!(keep_parts(10.0, &sp).iter().all(|p| p.silent()));
        // 不合理的倍率當成 1；舊的專案沒有這個欄位
        sp.speed = 99.0;
        assert_eq!(whole_speed(&sp), 1.0);
        let old: EditSpec = serde_json::from_str(r#"{"start":0,"end":5,"removed":[]}"#).unwrap();
        assert_eq!(whole_speed(&old), 1.0);
    }

    #[test]
    fn segments_can_be_reordered() {
        let mut sp = spec(0.0, 10.0, &[(3.0, 4.0), (6.0, 7.0)]);
        assert!(!reordered(&keep_parts(10.0, &sp)));
        // 最後一段移到最前面；順序裡沒有的照原本的先後放在後面
        sp.order = vec![7000, 0];
        let parts = keep_parts(10.0, &sp);
        assert_eq!(parts.iter().map(|p| (p.a, p.b)).collect::<Vec<_>>(), vec![(7.0, 10.0), (0.0, 3.0), (4.0, 6.0)]);
        assert!(reordered(&parts));
        assert_eq!(map_time(&parts, 1.0), Some(4.0));
        assert_eq!(map_time(&parts, 8.0), Some(1.0));
        // 順序改了的地方都是接縫
        assert_eq!(cut_fades(&parts).len(), 2);
        assert!((output_length(&parts) - 8.0).abs() < 1e-9);
        // 之後在移到前面的那一段中間又刪掉一段：後半段跟著它
        sp.removed.push((8.0, 8.5));
        let parts = keep_parts(10.0, &sp);
        assert_eq!(parts.iter().map(|p| (p.a, p.b)).collect::<Vec<_>>(), vec![(7.0, 8.0), (8.5, 10.0), (0.0, 3.0), (4.0, 6.0)]);
    }

    #[test]
    fn slow_parts_and_freezes() {
        let mut sp = spec(0.0, 10.0, &[(8.0, 9.0)]);
        set_fast(&mut sp.fast, 2.0, 4.0, 0.5);
        // 定格：2 秒處停 3 秒、刪掉的片段裡的不算、結尾也可以停
        sp.freezes = vec![Freeze { at_ms: 6000, ms: 3000 }, Freeze { at_ms: 8500, ms: 2000 }, Freeze { at_ms: 10000, ms: 1000 }];
        let parts = keep_parts(10.0, &sp);
        let hold = |t: f64, h: f64| Part { a: t, b: t, rate: 1.0, hold: h, keep_audio: false };
        assert_eq!(parts, vec![Part::new(0.0, 2.0, 1.0), Part::new(2.0, 4.0, 0.5), Part::new(4.0, 6.0, 1.0), hold(6.0, 3.0), Part::new(6.0, 8.0, 1.0), Part::new(9.0, 10.0, 1.0), hold(10.0, 1.0)]);
        // 2 + 4（放慢）+ 2 + 3（定格）+ 2 + 1 + 1
        assert!((output_length(&parts) - 15.0).abs() < 1e-9);
        assert_eq!(map_time(&parts, 3.0), Some(4.0));
        assert_eq!(map_time(&parts, 6.0), Some(11.0));
        assert_eq!(map_time(&parts, 8.5), None);
        assert_eq!(source_time(&parts, 4.0), Some(3.0));
        assert_eq!(source_time(&parts, 9.5), Some(6.0));
        assert_eq!(source_time(&parts, 11.5), Some(6.5));
        assert_eq!(source_time(&parts, 99.0), None);
        // 定格、變速的交界不是接縫
        assert_eq!(cut_fades(&parts).len(), 1);
        // 舊的專案檔：倍率是整數
        let f: FastRange = serde_json::from_str(r#"{"start_ms":1000,"end_ms":2000,"speed":4}"#).unwrap();
        assert_eq!(f.speed, 4.0);
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

    #[test]
    fn fades_only_at_removed_joins() {
        // 0～2 原速、2～6 加速 4 倍（長 1 秒）、刪掉 6～7、7～9 原速：只有 6～7 的接縫（剪好的 3 秒）
        let p = Part::new;
        let parts = [p(0.0, 2.0, 1.0), p(2.0, 6.0, 4.0), p(7.0, 9.0, 1.0)];
        assert_eq!(cut_fades(&parts), vec![(3.0, CUT_FADE)]);
        // 前後片段很短：淡出淡入縮短成片段的一半；太短就不做
        assert_eq!(cut_fades(&[p(0.0, 0.2, 1.0), p(1.0, 3.0, 1.0)]), vec![(0.2, 0.1)]);
        assert!(cut_fades(&[p(0.0, 0.05, 1.0), p(1.0, 3.0, 1.0)]).is_empty());
        assert!(cut_fades(&[p(0.0, 5.0, 1.0)]).is_empty());
    }
}
