//! 畫面調色與日期時間浮水印（剪輯視窗「畫面」分頁）：
//! - 調色：亮度、對比、飽和度（FFmpeg eq）與銳利化（unsharp）；剪輯視窗的預覽用 apply_rgba 在 CPU 上近似
//! - 日期時間：畫面角落顯示錄影當下的時間（drawtext 的 %{pts:localtime}），在原影片的時間畫上去，
//!   剪掉片段、改順序後顯示的仍是錄影當時的時間（暫停的時間不算）

use crate::format::num;
use serde::{Deserialize, Serialize};

/// 調色：數值都是 -50～50（0 = 不變）；銳利化 0～3
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Look {
    pub brightness: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub sharpen: u32,
}

/// 調色數值的範圍
pub const RANGE: i32 = 50;
/// 銳利化的等級
pub const SHARPEN_MAX: u32 = 3;

impl Look {
    pub fn is_default(&self) -> bool {
        self.clamped() == Look::default()
    }

    /// 限制在可用的範圍內（專案檔可能被改過）
    pub fn clamped(&self) -> Look {
        Look {
            brightness: self.brightness.clamp(-RANGE, RANGE),
            contrast: self.contrast.clamp(-RANGE, RANGE),
            saturation: self.saturation.clamp(-RANGE, RANGE),
            sharpen: self.sharpen.min(SHARPEN_MAX),
        }
    }

    /// FFmpeg 的 eq 參數：亮度 ±0.25、對比 0.5～1.5、飽和度 0～2
    fn eq(&self) -> (f64, f64, f64) {
        let l = self.clamped();
        (l.brightness as f64 / 200.0, 1.0 + l.contrast as f64 / 100.0, 1.0 + l.saturation as f64 / RANGE as f64)
    }

    /// 輸出用的濾鏡；沒有調整時 None
    pub fn filter(&self) -> Option<String> {
        let l = self.clamped();
        let mut f = vec![];
        if l.brightness != 0 || l.contrast != 0 || l.saturation != 0 {
            let (b, c, s) = self.eq();
            f.push(format!("eq=brightness={}:contrast={}:saturation={}", num(b), num(c), num(s)));
        }
        if l.sharpen > 0 {
            f.push(format!("unsharp=5:5:{}", num(0.2 + 0.4 * l.sharpen as f64)));
        }
        (!f.is_empty()).then(|| f.join(","))
    }

    /// 預覽：在 RGBA 畫面上做和 eq 差不多的調整（銳利化不預覽）
    pub fn apply_rgba(&self, rgba: &mut [u8]) {
        let l = self.clamped();
        if l.brightness == 0 && l.contrast == 0 && l.saturation == 0 {
            return;
        }
        let (b, c, s) = self.eq();
        // 亮度、對比：每個值查表
        let lut: Vec<f32> = (0..256).map(|v| (((v as f64 / 255.0 - 0.5) * c + 0.5 + b) * 255.0) as f32).collect();
        let s = s as f32;
        for px in rgba.chunks_exact_mut(4) {
            let (r, g, bl) = (lut[px[0] as usize], lut[px[1] as usize], lut[px[2] as usize]);
            let y = 0.299 * r + 0.587 * g + 0.114 * bl;
            px[0] = (y + (r - y) * s).clamp(0.0, 255.0) as u8;
            px[1] = (y + (g - y) * s).clamp(0.0, 255.0) as u8;
            px[2] = (y + (bl - y) * s).clamp(0.0, 255.0) as u8;
        }
    }
}

/// 日期時間浮水印：start_ms = 錄影開始的時間（1970 年起的毫秒）；corner：0 右下、1 左下、2 右上、3 左上；
/// size：0 小、1 中、2 大
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Stamp {
    pub start_ms: i64,
    pub corner: u8,
    pub size: u8,
}

impl Default for Stamp {
    fn default() -> Self {
        Stamp { start_ms: 0, corner: 0, size: 1 }
    }
}

/// 浮水印的文字檔內容（drawtext 的 textfile）：錄影開始的時間加上原影片的播放時間
pub fn stamp_text(start_ms: i64) -> String {
    // 冒號要跳脫（%{...} 的參數用冒號分開）
    format!("%{{pts:localtime:{}:%Y-%m-%d %H\\:%M\\:%S}}", num(start_ms as f64 / 1000.0))
}

/// 浮水印的 drawtext 濾鏡：font、text 是 FFmpeg 工作資料夾裡的檔名；area = 畫面上看得到的範圍（裁切後），原影片的座標
pub fn stamp_filter(s: &Stamp, font: &str, text: &str, area: crate::types::Rect) -> String {
    let px = (area.height as f64 / [36.0, 26.0, 18.0][s.size.min(2) as usize]).round().max(10.0) as i32;
    let m = (px as f64 * 0.7).round() as i32;
    let (l, t) = (area.x + m, area.y + m);
    let (r, b) = (area.x + area.width - m, area.y + area.height - m);
    let x = if s.corner.is_multiple_of(2) { format!("{r}-tw") } else { l.to_string() };
    let y = if s.corner < 2 { format!("{b}-th") } else { t.to_string() };
    format!("drawtext=fontfile={font}:textfile={text}:fontsize={px}:fontcolor=white:borderw={}:bordercolor=black@0.7:x={x}:y={y}", (px / 12).max(1))
}

/// 預覽用的文字：錄影開始的時間加上 t 秒（當地時間）
pub fn stamp_preview(start_ms: i64, t: f64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(start_ms + (t * 1000.0) as i64).single().map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

/// 浮水印用的字型（等寬、粗體，數字不會跳動）
pub fn stamp_font() -> Option<std::path::PathBuf> {
    let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let fonts = std::path::Path::new(&win).join("Fonts");
    [fonts.join("consolab.ttf"), fonts.join("consola.ttf"), fonts.join("arialbd.ttf"), "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf".into()].into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Rect;

    #[test]
    fn look_filters() {
        assert_eq!(Look::default().filter(), None);
        assert!(Look::default().is_default());
        let l = Look { brightness: 20, contrast: -10, saturation: 50, sharpen: 0 };
        assert_eq!(l.filter().unwrap(), "eq=brightness=0.1:contrast=0.9:saturation=2");
        assert_eq!(Look { sharpen: 2, ..Default::default() }.filter().unwrap(), "unsharp=5:5:1");
        // 超出範圍的數值限制住
        assert_eq!(Look { brightness: 999, sharpen: 9, ..Default::default() }.clamped(), Look { brightness: 50, sharpen: 3, ..Default::default() });
        let s: Look = serde_json::from_str("{}").unwrap();
        assert!(s.is_default());
    }

    #[test]
    fn preview_matches_direction() {
        let mut px = vec![100, 150, 200, 255];
        Look { brightness: 50, ..Default::default() }.apply_rgba(&mut px);
        assert!(px[0] > 100 && px[1] > 150 && px[2] > 200 && px[3] == 255);
        let mut gray = vec![50, 100, 200, 255];
        Look { saturation: -50, ..Default::default() }.apply_rgba(&mut gray);
        assert!(gray[0] == gray[1] && gray[1] == gray[2], "{gray:?}");
        let mut same = vec![1, 2, 3, 4];
        Look::default().apply_rgba(&mut same);
        assert_eq!(same, vec![1, 2, 3, 4]);
    }

    #[test]
    fn stamp_text_and_place() {
        assert_eq!(stamp_text(1_760_600_000_500), "%{pts:localtime:1760600000.5:%Y-%m-%d %H\\:%M\\:%S}");
        let area = Rect { x: 100, y: 50, width: 1280, height: 720 };
        let f = stamp_filter(&Stamp { start_ms: 0, corner: 0, size: 1 }, "stamp.ttf", "stamp.txt", area);
        assert!(f.contains("fontsize=28") && f.contains("x=1360-tw") && f.contains("y=750-th"), "{f}");
        let f = stamp_filter(&Stamp { start_ms: 0, corner: 3, size: 0 }, "stamp.ttf", "stamp.txt", area);
        assert!(f.contains("fontsize=20") && f.contains("x=114:") && f.contains("y=64"), "{f}");
    }
}
