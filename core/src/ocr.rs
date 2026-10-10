//! 文字辨識（OCR）：用 Windows 內建的 Windows.Media.Ocr（不用另外下載），優先用繁體中文，
//! 沒有安裝時改用使用者的語言。其他平台不支援。
//! 也用來自動遮個資：從辨識出的字找出 Email、電話、身分證字號、信用卡號的位置。

use regex::Regex;
use std::sync::LazyLock;

/// 辨識出的一個字（英文是一個單字）與它在圖上的範圍（像素）
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 個資的種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pii {
    Email,
    Phone,
    Id,
    Card,
}

impl Pii {
    pub fn name(self) -> &'static str {
        match self {
            Pii::Email => "Email",
            Pii::Phone => "電話",
            Pii::Id => "身分證字號",
            Pii::Card => "卡號",
        }
    }
}

/// 找到的個資與要遮的範圍（像素：x, y, 寬, 高；四周多留一點）
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub kind: Pii,
    pub text: String,
    pub rect: (f64, f64, f64, f64),
}

static PATTERNS: LazyLock<Vec<(Pii, Regex)>> = LazyLock::new(|| {
    let re = |s: &str| Regex::new(s).expect("個資的規則");
    vec![
        (Pii::Email, re(r"[A-Za-z0-9._%+\-]+\s?@\s?[A-Za-z0-9\-]+(?:\s?\.\s?[A-Za-z0-9\-]+)+")),
        // 卡號先找（16 碼，避免被當成電話）
        (Pii::Card, re(r"\b(?:\d{4}[\s\-]?){3}\d{4}\b")),
        // 身分證字號、居留證號
        (Pii::Id, re(r"\b[A-Z]\s?[12ABCD89]\s?\d{8}\b")),
        // 手機（含 +886）、市話（02-2345-6789、(02)2345-6789、037-123456）
        (Pii::Phone, re(r"(?:\+\s?886[\s\-]?|\b0)9\d{2}[\s\-]?\d{3}[\s\-]?\d{3}\b")),
        (Pii::Phone, re(r"(?:\(0\d{1,3}\)|\b0[2-8]\d{0,2}[\s\-])\s?\d{3,4}[\s\-]?\d{3,4}\b")),
    ]
});

/// 信用卡號的檢查碼（Luhn）
fn luhn(digits: &str) -> bool {
    let d: Vec<u32> = digits.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() < 13 {
        return false;
    }
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &v)| {
            if i % 2 == 1 {
                if v * 2 > 9 {
                    v * 2 - 9
                } else {
                    v * 2
                }
            } else {
                v
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// 從辨識出的每一行找出個資。一行的字用空白接起來比對，再把比對到的那幾個字的範圍合起來
pub fn find_pii(lines: &[Vec<Word>]) -> Vec<Found> {
    let mut out: Vec<Found> = vec![];
    for line in lines {
        let mut joined = String::new();
        let mut spans = vec![];
        for (i, w) in line.iter().enumerate() {
            if i > 0 {
                joined.push(' ');
            }
            let start = joined.len();
            joined.push_str(w.text.trim());
            spans.push((start, joined.len()));
        }
        let mut taken: Vec<(usize, usize)> = vec![];
        for (kind, re) in PATTERNS.iter() {
            for m in re.find_iter(&joined) {
                let (a, b) = (m.start(), m.end());
                if taken.iter().any(|&(x, y)| a < y && x < b) {
                    continue;
                }
                if *kind == Pii::Card && !luhn(m.as_str()) {
                    continue;
                }
                let words: Vec<&Word> = line.iter().zip(&spans).filter(|(_, &(x, y))| a < y && x < b).map(|(w, _)| w).collect();
                let Some(first) = words.first() else { continue };
                let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x + first.w, first.y + first.h);
                for w in &words {
                    x0 = x0.min(w.x);
                    y0 = y0.min(w.y);
                    x1 = x1.max(w.x + w.w);
                    y1 = y1.max(w.y + w.h);
                }
                let pad = ((y1 - y0) * 0.2).max(2.0);
                taken.push((a, b));
                out.push(Found { kind: *kind, text: m.as_str().to_string(), rect: (x0 - pad, y0 - pad, x1 - x0 + pad * 2.0, y1 - y0 + pad * 2.0) });
            }
        }
    }
    out
}

/// Windows 的辨識結果在中文字之間會加空白：中文字（含全形標點）之間的空白拿掉，英文單字之間的保留
pub fn tidy(line: &str) -> String {
    let cjk = |c: char| matches!(c as u32, 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF);
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let prev = out.chars().last();
            let next = chars[i + 1..].iter().copied().find(|c| *c != ' ');
            if prev.is_some_and(cjk) || next.is_some_and(cjk) || prev.is_none() || prev == Some(' ') {
                continue;
            }
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

#[cfg(windows)]
fn fail(e: windows::core::Error) -> String {
    format!("文字辨識失敗：{}", e.message())
}

/// 辨識 RGBA 圖裡的文字；回傳 (文字（一行一行）, 用的語言)
#[cfg(windows)]
pub fn recognize(rgba: &[u8], w: u32, h: u32) -> Result<(String, String), String> {
    let (res, _, lang) = run(rgba, w, h)?;
    let lines = res.Lines().map_err(fail)?;
    let mut out = Vec::new();
    for i in 0..lines.Size().map_err(fail)? {
        if let Ok(t) = lines.GetAt(i).and_then(|l| l.Text()) {
            out.push(tidy(&t.to_string()));
        }
    }
    Ok((out.join("\n"), lang))
}

/// 辨識 RGBA 圖裡的字與位置（原圖的像素），一行一組
#[cfg(windows)]
pub fn recognize_words(rgba: &[u8], w: u32, h: u32) -> Result<Vec<Vec<Word>>, String> {
    let (res, k, _) = run(rgba, w, h)?;
    let lines = res.Lines().map_err(fail)?;
    let mut out = Vec::new();
    for i in 0..lines.Size().map_err(fail)? {
        let Ok(words) = lines.GetAt(i).and_then(|l| l.Words()) else { continue };
        let mut line = vec![];
        for j in 0..words.Size().unwrap_or(0) {
            let Ok(word) = words.GetAt(j) else { continue };
            let (Ok(t), Ok(r)) = (word.Text(), word.BoundingRect()) else { continue };
            line.push(Word { text: t.to_string(), x: r.X as f64 / k, y: r.Y as f64 / k, w: r.Width as f64 / k, h: r.Height as f64 / k });
        }
        out.push(line);
    }
    Ok(out)
}

/// 辨識；回傳 (結果, 辨識用的圖是原圖的幾倍, 語言)
#[cfg(windows)]
fn run(rgba: &[u8], w: u32, h: u32) -> Result<(windows::Media::Ocr::OcrResult, f64, String), String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
    }
    let engine = engine()?;
    let lang = engine.RecognizerLanguage().and_then(|l| l.DisplayName()).map(|s| s.to_string()).unwrap_or_default();
    // 太大的圖縮小；小字放大兩倍比較認得出來
    let max = OcrEngine::MaxImageDimension().unwrap_or(4096).max(256);
    let k = if w.max(h) > max {
        max as f64 / w.max(h) as f64
    } else if w.max(h) * 2 <= max {
        2.0
    } else {
        1.0
    };
    let ow = w.max(1);
    let (data, w, h) = crate::shot_edit::scale_rgba(rgba, w, h, k);
    let bgra: Vec<u8> = data.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], 255]).collect();
    let writer = DataWriter::new().map_err(fail)?;
    writer.WriteBytes(&bgra).map_err(fail)?;
    let buf = writer.DetachBuffer().map_err(fail)?;
    let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32).map_err(fail)?;
    let res = engine.RecognizeAsync(&bmp).map_err(fail)?.join().map_err(fail)?;
    // 縮放後的實際倍率（寬度取整過）
    let k = w as f64 / ow as f64;
    Ok((res, k, lang))
}

#[cfg(windows)]
fn engine() -> Result<windows::Media::Ocr::OcrEngine, String> {
    use windows::core::HSTRING;
    use windows::Globalization::Language;
    use windows::Media::Ocr::OcrEngine;
    for tag in ["zh-Hant-TW", "zh-TW", "zh-Hant"] {
        if let Ok(l) = Language::CreateLanguage(&HSTRING::from(tag)) {
            if OcrEngine::IsLanguageSupported(&l).unwrap_or(false) {
                if let Ok(e) = OcrEngine::TryCreateFromLanguage(&l) {
                    return Ok(e);
                }
            }
        }
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "Windows 沒有可用的文字辨識語言：請到「設定 → 時間與語言 → 語言與地區」新增「中文（繁體，台灣）」，並在語言選項裡安裝「光學字元辨識」".to_string())
}

#[cfg(not(windows))]
pub fn recognize(_rgba: &[u8], _w: u32, _h: u32) -> Result<(String, String), String> {
    Err("文字辨識只支援 Windows".into())
}

#[cfg(not(windows))]
pub fn recognize_words(_rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<Vec<Word>>, String> {
    Err("自動遮個資要用 Windows 的文字辨識，只支援 Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(words: &[&str]) -> Vec<Word> {
        // 每個字 10 像素寬、20 像素高，字之間空 5 像素
        let mut x = 0.0;
        words
            .iter()
            .map(|t| {
                let w = Word { text: t.to_string(), x, y: 100.0, w: 10.0 * t.chars().count() as f64, h: 20.0 };
                x += w.w + 5.0;
                w
            })
            .collect()
    }

    #[test]
    fn finds_personal_data() {
        let lines = vec![
            line(&["聯", "絡", "：", "0912-345-678"]),
            line(&["Email:", "amy.chen@example.com.tw", "謝謝"]),
            line(&["身", "分", "證", "A123456789"]),
            line(&["卡號", "4111", "1111", "1111", "1111", "有效"]),
            line(&["市話", "(02)", "2345-6789"]),
            line(&["訂單", "1234", "5678", "9012", "3456"]),
            line(&["2026-10-10", "版本", "3.1.0", "共", "1234", "筆"]),
        ];
        let found = find_pii(&lines);
        let kinds: Vec<(Pii, &str)> = found.iter().map(|f| (f.kind, f.text.as_str())).collect();
        assert_eq!(kinds, vec![(Pii::Phone, "0912-345-678"), (Pii::Email, "amy.chen@example.com.tw"), (Pii::Id, "A123456789"), (Pii::Card, "4111 1111 1111 1111"), (Pii::Phone, "(02) 2345-6789"),]);
        // 範圍只包含那幾個字（四周多留一點），不含前後的字
        let (x, y, w, h) = found[0].rect;
        assert!(x < 45.0 && x > 35.0 && y < 100.0 && y + h > 120.0);
        assert!((x + w - (45.0 + 120.0)).abs() < 6.0);
        let (x, _, w, _) = found[3].rect;
        // 「卡號」20 像素寬：卡號從 25 開始，到 200 結束
        assert!(x > 15.0 && x < 25.0 && x + w > 200.0 && x + w < 210.0);
        assert!(luhn("4111111111111111") && !luhn("1234567890123456"));
    }

    #[test]
    fn removes_spaces_between_chinese_characters() {
        assert_eq!(tidy("開 始 錄 影"), "開始錄影");
        assert_eq!(tidy("檔 案 Rec_2026 .mp4"), "檔案Rec_2026 .mp4");
        assert_eq!(tidy("Hello world"), "Hello world");
        assert_eq!(tidy("設 定 ， 完 成 。"), "設定，完成。");
        assert_eq!(tidy("  OK  "), "OK");
    }
}
