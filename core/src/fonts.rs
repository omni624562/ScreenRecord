//! 字型：從系統字型資料夾載入（不打包進執行檔），供操作介面與標註繪製使用。
//! - 中文：微軟正黑體（msjh.ttc / msjhbd.ttc）
//! - 表情符號：Segoe UI Emoji（彩色，COLR）
//! - 其他符號：Segoe UI Symbol、Segoe UI
//! 在 Linux 上（開發、測試）改用文泉驛正黑與 Noto Color Emoji。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// 一個字型檔（.ttc 時以 index 指定其中一個字型）
#[derive(Clone)]
pub struct FontFile {
    pub name: String,
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

impl FontFile {
    pub fn face(&self) -> Option<ttf_parser::Face<'_>> {
        ttf_parser::Face::parse(&self.data, self.index).ok()
    }
}

/// 標註文字用的字型（依序找字）
pub struct TextFonts {
    /// 一般文字（粗體）
    pub text: Vec<FontFile>,
    /// 彩色表情符號
    pub emoji: Vec<FontFile>,
}

fn windows_fonts() -> PathBuf {
    let win = std::env::var("WINDIR").or_else(|_| std::env::var("SystemRoot")).unwrap_or_else(|_| "C:\\Windows".into());
    Path::new(&win).join("Fonts")
}

/// 依序找第一個存在的檔案
fn load(candidates: &[(&str, u32)]) -> Option<FontFile> {
    let win = windows_fonts();
    candidates.iter().find_map(|(name, index)| {
        let path = if name.starts_with('/') { PathBuf::from(name) } else { win.join(name) };
        let data = std::fs::read(&path).ok()?;
        ttf_parser::Face::parse(&data, *index).ok()?;
        Some(FontFile { name: path.file_name()?.to_string_lossy().into_owned(), data: Arc::new(data), index: *index })
    })
}

const CJK_LINUX: &[(&str, u32)] = &[
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc", 3),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 3),
    ("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc", 0),
    ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
];

/// 介面用的字型（一般、粗體），依序找字；第一個用來顯示英數字
pub fn ui_fonts() -> (Vec<FontFile>, Vec<FontFile>) {
    let regular = [
        load(&[("segoeui.ttf", 0)]),
        // msjh.ttc 的第 2 個字型是「Microsoft JhengHei UI」（行距較適合介面）
        load(&[("msjh.ttc", 1), ("msjh.ttf", 0), ("mingliu.ttc", 0)]).or_else(|| load(CJK_LINUX)),
        load(&[("seguisym.ttf", 0)]),
    ];
    let bold = [
        load(&[("segoeuib.ttf", 0), ("seguisb.ttf", 0)]),
        load(&[("msjhbd.ttc", 1), ("msjhbd.ttf", 0)]).or_else(|| load(&[("msjh.ttc", 1)])).or_else(|| load(CJK_LINUX)),
    ];
    (regular.into_iter().flatten().collect(), bold.into_iter().flatten().collect())
}

/// 標註文字的字型（載入一次）
pub fn text_fonts() -> &'static TextFonts {
    static FONTS: OnceLock<TextFonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        let text = [
            load(&[("msjhbd.ttc", 1), ("msjhbd.ttf", 0), ("msjh.ttc", 1)]).or_else(|| load(CJK_LINUX)),
            load(&[("segoeuib.ttf", 0), ("segoeui.ttf", 0)]),
            load(&[("seguisym.ttf", 0)]),
        ];
        let emoji = [load(&[("seguiemj.ttf", 0), ("/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf", 0)])];
        TextFonts { text: text.into_iter().flatten().collect(), emoji: emoji.into_iter().flatten().collect() }
    })
}

/// 預設以彩色表情顯示的字元（Emoji_Presentation），或後面接著 U+FE0F 的字元
pub fn wants_emoji(c: char, next: Option<char>) -> bool {
    if next == Some('\u{FE0F}') {
        return true;
    }
    let u = c as u32;
    u >= 0x1F000
        || matches!(
            u,
            0x231A..=0x231B
                | 0x23E9..=0x23EC
                | 0x23F0
                | 0x23F3
                | 0x25FD..=0x25FE
                | 0x2614..=0x2615
                | 0x2648..=0x2653
                | 0x267F
                | 0x2693
                | 0x26A1
                | 0x26AA..=0x26AB
                | 0x26BD..=0x26BE
                | 0x26C4..=0x26C5
                | 0x26CE
                | 0x26D4
                | 0x26EA
                | 0x26F2..=0x26F3
                | 0x26F5
                | 0x26FA
                | 0x26FD
                | 0x2705
                | 0x270A..=0x270B
                | 0x2728
                | 0x274C
                | 0x274E
                | 0x2753..=0x2755
                | 0x2757
                | 0x2795..=0x2797
                | 0x27B0
                | 0x27BF
                | 0x2B1B..=0x2B1C
                | 0x2B50
                | 0x2B55
        )
}

/// 不顯示的字元（變體選擇符、零寬連接符）
pub fn invisible(c: char) -> bool {
    matches!(c, '\u{FE0E}' | '\u{FE0F}' | '\u{200D}' | '\u{200B}')
}

/// 一個字要用哪個字型：（字型, 字形編號, 是否彩色表情）
pub fn pick<'a>(fonts: &'a TextFonts, c: char, next: Option<char>) -> Option<(&'a FontFile, ttf_parser::GlyphId, bool)> {
    let find = |list: &'a [FontFile]| list.iter().find_map(|f| Some((f, f.face()?.glyph_index(c)?)));
    if wants_emoji(c, next) {
        if let Some((f, g)) = find(&fonts.emoji) {
            return Some((f, g, true));
        }
    }
    if let Some((f, g)) = find(&fonts.text) {
        return Some((f, g, false));
    }
    find(&fonts.emoji).map(|(f, g)| (f, g, true))
}
