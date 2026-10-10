//! 計算與格式化：檔名、倍率、長度、大小（結果與 2.x 版相同，見 tests/vectors.json）。

use crate::types::ExportFormat;
use crate::{tr, trf};
use regex::Regex;
use std::sync::LazyLock;

pub const FPS_MIN: f64 = 1.0;
pub const FPS_MAX: f64 = 60.0;
pub const SPEED_MIN: f64 = 1.1;
pub const SPEED_MAX: f64 = 1000.0;
pub const MAX_MINUTES_MAX: f64 = 7.0 * 24.0 * 60.0;

/// 加速版可選的寬度（0 = 原尺寸）
pub const MP4_WIDTHS: [u32; 3] = [0, 1920, 1280];

/// JavaScript 的 Math.round：.5 一律往正無限大方向進位（f64::round 是遠離 0）
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// JavaScript 的 String(number)：整數不帶小數點、其餘用最短表示（Rust 的 `{}` 規則相同）
pub fn num(x: f64) -> String {
    if x == 0.0 {
        return "0".into(); // -0 也輸出 0
    }
    format!("{x}")
}

/// 字串長度（UTF-16 單位，與 JavaScript 的 .length 相同）
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 取偶數（至少 2）：yuv420p 要求寬高為偶數
pub fn even(n: f64) -> i32 {
    ((n / 2.0).floor() * 2.0).max(2.0) as i32
}

/// 輸出尺寸：依縮放比例計算並取偶數
pub fn output_size(width: i32, height: i32, scale: f64) -> (i32, i32) {
    (even(js_round(width as f64 * scale / 100.0)), even(js_round(height as f64 * scale / 100.0)))
}

/// 倍率顯示：4 → "4"、1.5 → "1.5"、2.25 → "2.25"
pub fn speed_label(speed: f64) -> String {
    num(js_round(speed * 100.0) / 100.0)
}

static MP4_EXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.mp4$").unwrap());

/// 去掉 .mp4
pub fn strip_mp4(name: &str) -> String {
    MP4_EXT.replace(name, "").into_owned()
}

/// 匯出檔名：Rec_xxx.mp4 → Rec_xxx_4x.mp4 / Rec_xxx_4x.gif；原速 GIF 為 Rec_xxx.gif
pub fn export_file_name(source_name: &str, speed: f64, format: ExportFormat) -> String {
    let base = strip_mp4(source_name);
    if format == ExportFormat::Gif && speed <= 1.0 {
        format!("{base}.gif")
    } else {
        format!("{base}_{}x.{}", speed_label(speed), format.ext())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedExport {
    /// 原片檔名（含 .mp4）
    pub base: String,
    pub speed: f64,
    pub format: ExportFormat,
}

static EXPORT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*)_(\d+(?:\.\d+)?)x(?:_\d+)?\.(mp4|gif)$").unwrap());
static GIF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*?)(?:_\d+)?\.gif$").unwrap());

/// 從匯出檔名解析倍率與格式；不是匯出檔回傳 None（原速 GIF 也算，倍率為 1）
pub fn parse_export_name(name: &str) -> Option<ParsedExport> {
    if let Some(m) = EXPORT_RE.captures(name) {
        let format = if m[3].eq_ignore_ascii_case("gif") { ExportFormat::Gif } else { ExportFormat::Mp4 };
        return Some(ParsedExport { base: format!("{}.mp4", &m[1]), speed: m[2].parse().unwrap_or(1.0), format });
    }
    GIF_RE.captures(name).map(|g| ParsedExport { base: format!("{}.mp4", &g[1]), speed: 1.0, format: ExportFormat::Gif })
}

/// 縮小後的尺寸（高度等比、取偶數）；width 為 0 或不小於原寬時維持原尺寸
pub fn scaled_size(src_width: i32, src_height: i32, width: i32) -> (i32, i32) {
    if width == 0 || width >= src_width {
        return (src_width, src_height);
    }
    (even(width as f64), even(js_round(src_height as f64 * width as f64 / src_width as f64)))
}

pub struct EstimateInput {
    pub format: ExportFormat,
    pub src_bytes: f64,
    pub src_sec: f64,
    pub src_width: f64,
    pub src_height: f64,
    pub speed: f64,
    pub width: f64,
    pub height: f64,
    pub gif_fps: Option<f64>,
}

/// 粗估成品大小，回傳 (下限, 上限)（位元組）
pub fn estimate_bytes(o: &EstimateInput) -> (f64, f64) {
    let out_sec = o.src_sec / o.speed;
    if o.format == ExportFormat::Gif {
        let frames = (out_sec * o.gif_fps.unwrap_or(10.0)).max(1.0);
        let px = o.width * o.height * frames;
        return (px * 0.03, px * 0.2);
    }
    let area = if o.src_width > 0.0 && o.src_height > 0.0 { o.width * o.height / (o.src_width * o.src_height) } else { 1.0 };
    let mid = o.src_bytes / o.src_sec * out_sec * o.speed.sqrt().min(4.0) * area.powf(0.75);
    (mid * 0.5, mid * 1.8)
}

static BAD_CHARS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[\\/:*?"<>|\x00-\x1f]"#).unwrap());
static RESERVED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(con|prn|aux|nul|com\d|lpt\d)$").unwrap());

/// 錄影改名時的名稱檢查（不含 .mp4）；沒問題回傳 None
pub fn check_recording_name(base: &str) -> Option<&'static str> {
    let b = base.trim();
    if b.is_empty() {
        return Some(tr!("請輸入名稱", "Enter a name"));
    }
    if BAD_CHARS.is_match(b) {
        return Some(tr!("名稱不能包含 \\ / : * ? \" < > |", "Name can't contain \\ / : * ? \" < > |"));
    }
    if b.ends_with('.') || b.ends_with(' ') {
        return Some(tr!("名稱結尾不能是句點或空白", "Name can't end with a period or space"));
    }
    if RESERVED.is_match(b) {
        return Some(tr!("這是 Windows 保留的名稱，請換一個", "This name is reserved by Windows. Choose another one."));
    }
    if js_len(b) > 120 {
        return Some(tr!("名稱太長（最多 120 個字）", "Name is too long (max 120 characters)"));
    }
    if parse_export_name(&format!("{b}.mp4")).is_some() {
        return Some(tr!("名稱結尾不能是「_數字x」（例如 _4x），會被當成加速版", "Name can't end with “_<number>x” (e.g. _4x); it would be treated as a sped-up version"));
    }
    None
}

static CLOCK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(?::\d{1,2}){0,2}(?:\.\d+)?$").unwrap());

/// 「1:30」「90」「1:02:03」→ 秒數；格式不對回傳 None
pub fn parse_clock(text: &str) -> Option<f64> {
    let t = text.trim();
    if !CLOCK_RE.is_match(t) {
        return None;
    }
    let sec = t.split(':').fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap_or(f64::NAN));
    (sec.is_finite() && sec > 0.0).then_some(sec)
}

/// 依目標長度算倍率（限制在可用範圍，取兩位小數）
pub fn speed_for_target(duration_sec: f64, target_sec: f64, allow_original: bool) -> f64 {
    let min = if allow_original { 1.0 } else { SPEED_MIN };
    let raw = duration_sec / target_sec;
    (js_round(raw * 100.0) / 100.0).max(min).min(SPEED_MAX)
}

/// 秒數 → 「1 小時 2 分 3 秒」
pub fn human_duration(sec: f64) -> String {
    if !sec.is_finite() {
        return "—".into();
    }
    if sec < 10.0 {
        return trf!("{} 秒", "{} s", num(js_round(sec * 10.0) / 10.0));
    }
    let s = js_round(sec) as i64;
    let (h, m, r) = (s / 3600, (s % 3600) / 60, s % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(trf!("{h} 小時", "{h} h"));
    }
    if m > 0 {
        parts.push(trf!("{m} 分", "{m} min"));
    }
    if r > 0 || parts.is_empty() {
        parts.push(trf!("{r} 秒", "{r} s"));
    }
    parts.join(" ")
}

/// 毫秒 → HH:MM:SS
pub fn clock(ms: f64) -> String {
    let s = (ms / 1000.0).floor().max(0.0) as i64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// 秒 → MM:SS.s（超過 1 小時加上小時）
pub fn video_clock(sec: f64) -> String {
    let tenth = (sec * 10.0).floor().max(0.0) as i64;
    let h = tenth / 36000;
    let mmss = format!("{:02}:{:02}.{}", (tenth % 36000) / 600, (tenth % 600) / 10, tenth % 10);
    if h > 0 {
        format!("{h}:{mmss}")
    } else {
        mmss
    }
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.2} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_javascript() {
        assert_eq!(num(4.0), "4");
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(-180.0), "-180");
        assert_eq!(num(5.5), "5.5");
        assert_eq!(speed_label(1.0 / 3.0 * 10.0), "3.33");
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
    }

    #[test]
    fn export_names() {
        assert_eq!(export_file_name("Rec_2026-10-05_14-30-00.mp4", 4.0, ExportFormat::Mp4), "Rec_2026-10-05_14-30-00_4x.mp4");
        assert_eq!(export_file_name("Rec_a.mp4", 1.5, ExportFormat::Mp4), "Rec_a_1.5x.mp4");
        let p = |base: &str, speed: f64, format| Some(ParsedExport { base: base.into(), speed, format });
        assert_eq!(parse_export_name("Rec_a_1.5x.mp4"), p("Rec_a.mp4", 1.5, ExportFormat::Mp4));
        assert_eq!(parse_export_name("Rec_a_8x_2.mp4"), p("Rec_a.mp4", 8.0, ExportFormat::Mp4));
        assert_eq!(parse_export_name("Rec_2026-10-05_14-30-00.mp4"), None);
        assert_eq!(export_file_name("Rec_a.mp4", 4.0, ExportFormat::Gif), "Rec_a_4x.gif");
        assert_eq!(export_file_name("Rec_a.mp4", 1.0, ExportFormat::Gif), "Rec_a.gif");
        assert_eq!(parse_export_name("Rec_a_4x.gif"), p("Rec_a.mp4", 4.0, ExportFormat::Gif));
        assert_eq!(parse_export_name("Rec_a.gif"), p("Rec_a.mp4", 1.0, ExportFormat::Gif));
        assert_eq!(parse_export_name("Rec_a_2.gif"), p("Rec_a.mp4", 1.0, ExportFormat::Gif));
        // 剪輯版本身是獨立的錄影；它的加速版歸在剪輯版底下
        assert_eq!(parse_export_name("Rec_2026-10-06_08-00-00_cut.mp4"), None);
        assert_eq!(parse_export_name("Rec_2026-10-06_08-00-00_cut_4x.mp4"), p("Rec_2026-10-06_08-00-00_cut.mp4", 4.0, ExportFormat::Mp4));
    }

    #[test]
    fn clock_and_target_speed() {
        assert_eq!(parse_clock("1:30"), Some(90.0));
        assert_eq!(parse_clock("90"), Some(90.0));
        assert_eq!(parse_clock("1:02:03"), Some(3723.0));
        assert_eq!(parse_clock("abc"), None);
        assert_eq!(parse_clock("0"), None);
        assert_eq!(speed_for_target(3600.0, 60.0, false), 60.0);
        assert_eq!(speed_for_target(100.0, 30.0, false), 3.33);
        assert_eq!(speed_for_target(10.0, 60.0, false), 1.1);
        assert_eq!(speed_for_target(10.0, 60.0, true), 1.0);
        assert_eq!(speed_for_target(36000.0, 1.0, false), 1000.0);
    }

    #[test]
    fn sizes() {
        assert_eq!(scaled_size(3840, 1080, 1920), (1920, 540));
        assert_eq!(scaled_size(1920, 1080, 1280), (1280, 720));
        assert_eq!(scaled_size(1366, 768, 1280), (1280, 720));
        assert_eq!(scaled_size(1280, 720, 1920), (1280, 720));
        assert_eq!(scaled_size(1920, 1080, 0), (1920, 1080));
        assert_eq!(output_size(1921, 1081, 100.0), (1920, 1080));
        assert_eq!(output_size(1366, 768, 75.0), (1024, 576));
        assert_eq!(output_size(10, 10, 25.0), (2, 2));
    }

    #[test]
    fn estimates() {
        let base = EstimateInput { format: ExportFormat::Mp4, src_bytes: 60e6, src_sec: 600.0, src_width: 1920.0, src_height: 1080.0, speed: 4.0, width: 1920.0, height: 1080.0, gif_fps: None };
        let (lo, hi) = estimate_bytes(&base);
        assert!(lo < hi && lo > 0.0);
        assert!((lo + hi) / 2.0 > 15e6 && (lo + hi) / 2.0 < 60e6);
        assert!(estimate_bytes(&EstimateInput { width: 1280.0, height: 720.0, ..base }).1 < hi);
        assert!(estimate_bytes(&EstimateInput { speed: 16.0, ..base }).1 < hi);
        let gif = estimate_bytes(&EstimateInput { format: ExportFormat::Gif, speed: 1.0, width: 640.0, height: 360.0, gif_fps: Some(10.0), src_sec: 10.0, ..base });
        assert!((gif.0 - 640.0 * 360.0 * 100.0 * 0.03).abs() < 1.0);
    }

    #[test]
    fn recording_names() {
        assert_eq!(check_recording_name("操作示範"), None);
        assert!(check_recording_name("a:b").unwrap().contains("不能包含"));
        assert!(check_recording_name("Demo_4x").unwrap().contains("加速版"));
        assert!(check_recording_name("demo.").is_some());
        assert!(check_recording_name("CON").is_some());
        assert!(check_recording_name("  ").is_some());
        assert!(check_recording_name(&"字".repeat(121)).is_some());
        assert_eq!(check_recording_name(&"字".repeat(120)), None);
    }

    #[test]
    fn human_readable() {
        assert_eq!(human_duration(2.54), "2.5 秒");
        assert_eq!(human_duration(3.0), "3 秒");
        assert_eq!(human_duration(225.0), "3 分 45 秒");
        assert_eq!(human_duration(3600.0), "1 小時");
        assert_eq!(clock(3_725_000.0), "01:02:05");
        assert_eq!(video_clock(25.75), "00:25.7");
        assert_eq!(video_clock(3725.0), "1:02:05.0");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(5_368_709), "5.12 MB");
        assert_eq!(format_bytes(104_857_600), "100.0 MB");
    }
}
