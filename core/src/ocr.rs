//! 文字辨識（OCR）：用 Windows 內建的 Windows.Media.Ocr（不用另外下載），優先用繁體中文，
//! 沒有安裝時改用使用者的語言。其他平台不支援。

/// Windows 的辨識結果在中文字之間會加空格：中文字（含全形標點）之間的空格拿掉，英文單字之間的保留
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

/// 辨識 RGBA 圖裡的文字；回傳 (文字（一行一行）, 用的語言)
#[cfg(windows)]
pub fn recognize(rgba: &[u8], w: u32, h: u32) -> Result<(String, String), String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
    }
    let fail = |e: windows::core::Error| format!("文字辨識失敗：{}", e.message());
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
    let (data, w, h) = crate::shot_edit::scale_rgba(rgba, w, h, k);
    let bgra: Vec<u8> = data.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], 255]).collect();
    let writer = DataWriter::new().map_err(fail)?;
    writer.WriteBytes(&bgra).map_err(fail)?;
    let buf = writer.DetachBuffer().map_err(fail)?;
    let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32).map_err(fail)?;
    let res = engine.RecognizeAsync(&bmp).map_err(fail)?.join().map_err(fail)?;
    let lines = res.Lines().map_err(fail)?;
    let mut out = Vec::new();
    for i in 0..lines.Size().map_err(fail)? {
        if let Ok(t) = lines.GetAt(i).and_then(|l| l.Text()) {
            out.push(tidy(&t.to_string()));
        }
    }
    Ok((out.join("\n"), lang))
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

#[cfg(test)]
mod tests {
    use super::tidy;

    #[test]
    fn removes_spaces_between_chinese_characters() {
        assert_eq!(tidy("開 始 錄 影"), "開始錄影");
        assert_eq!(tidy("檔 案 Rec_2026 .mp4"), "檔案Rec_2026 .mp4");
        assert_eq!(tidy("Hello world"), "Hello world");
        assert_eq!(tidy("設 定 ， 完 成 。"), "設定，完成。");
        assert_eq!(tidy("  OK  "), "OK");
    }
}
