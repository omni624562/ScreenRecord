//! 介面語言：繁體中文或英文。字串直接寫在用到的地方，兩種語言並排：
//! `tr!("開始錄影", "Start recording")`、`trf!("已錄 {} 秒", "Recorded {} s", n)`（格式化，支援 `{name}`）。
//! 程式啟動時依設定（或 Windows 的顯示語言）呼叫 `set_lang`，之後畫的介面、選單、通知都用這個語言；
//! 記錄檔（ScreenRecorder.log）固定用中文，方便回報問題。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 繁體中文（台灣）
    Zh,
    English,
}

/// 0 = 中文（預設；測試也是中文）、1 = 英文
static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::English
    } else {
        Lang::Zh
    }
}

pub fn is_en() -> bool {
    lang() == Lang::English
}

pub fn set_lang(lang: Lang) {
    LANG.store(if lang == Lang::English { 1 } else { 0 }, Ordering::Relaxed);
}

/// 設定裡的語言："zh-TW"、"en"，其他（"auto" 或沒設定）依 Windows 的顯示語言
pub fn resolve(setting: Option<&str>) -> Lang {
    match setting {
        Some("zh-TW") => Lang::Zh,
        Some("en") => Lang::English,
        _ => system_lang(),
    }
}

/// 系統的顯示語言：中文（台灣、香港、中國…）用繁體中文，其他用英文
pub fn system_lang() -> Lang {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        // 主要語言是 LANG_CHINESE（0x04）
        let id = unsafe { GetUserDefaultUILanguage() };
        if id & 0x3ff == 0x04 {
            Lang::Zh
        } else {
            Lang::English
        }
    }
    #[cfg(not(windows))]
    {
        let v = std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).unwrap_or_default();
        if v.is_empty() || v.starts_with("zh") || v == "C" || v.starts_with("C.") {
            Lang::Zh
        } else {
            Lang::English
        }
    }
}

/// 依介面語言選字串：`tr!("中文", "English")`
#[macro_export]
macro_rules! tr {
    ($zh:expr, $en:expr $(,)?) => {
        if $crate::i18n::is_en() {
            $en
        } else {
            $zh
        }
    };
}

/// 依介面語言格式化：`trf!("已錄 {n} 秒", "Recorded {n} s")`、`trf!("{} 個", "{} items", n)`。
/// 兩個樣板都在編譯時檢查（參數、`{name}` 都要對得上）
#[macro_export]
macro_rules! trf {
    ($zh:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::i18n::is_en() {
            format!($en $(, $($arg)*)?)
        } else {
            format!($zh $(, $($arg)*)?)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_setting() {
        assert_eq!(resolve(Some("zh-TW")), Lang::Zh);
        assert_eq!(resolve(Some("en")), Lang::English);
        // 測試時沒有設定語言：中文
        assert_eq!(tr!("中", "en"), "中");
        let n = 3;
        assert_eq!(trf!("{n} 個", "{n} items"), "3 個");
        assert_eq!(trf!("{} 個", "{} items", n + 1), "4 個");
    }
}
