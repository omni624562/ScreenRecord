//! 版本資訊：版本號以 Cargo.toml（workspace）為唯一來源，更新說明為 CHANGELOG.md（建置時一併打包進執行檔）。

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHANGELOG: &str = include_str!("../../CHANGELOG.md");
/// 專案網頁（「關於」的連結；更新檢查也只接受這個網址底下的發佈頁）
pub const PROJECT_URL: &str = "https://github.com/omni624562/ScreenRecord/";
/// 回報問題
pub const ISSUES_URL: &str = "https://github.com/omni624562/ScreenRecord/issues";

/// 某一版的更新說明：(標題那一行「## 」後面的文字, 內容)；CHANGELOG 裡沒有這一版時為 None
pub fn release_notes(version: &str) -> Option<(&'static str, &'static str)> {
    let mut start = None;
    let mut offset = 0;
    for line in CHANGELOG.split_inclusive('\n') {
        let end = offset + line.len();
        if let Some(h) = line.trim_end().strip_prefix("## ") {
            if let Some((title, body_start)) = start {
                return Some((title, CHANGELOG[body_start..offset].trim()));
            }
            // 「3.2.1（2026-10-14）」：版本號後面接日期等文字
            if h.strip_prefix(version).is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '.')) {
                start = Some((h, end));
            }
        }
        offset = end;
    }
    start.map(|(title, body_start)| (title, CHANGELOG[body_start..].trim()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_and_changelog_are_embedded() {
        assert!(super::APP_VERSION.split('.').count() == 3);
        assert!(super::CHANGELOG.contains(&format!("## {}", super::APP_VERSION)));
    }

    #[test]
    fn notes_of_one_version() {
        let (title, body) = super::release_notes(super::APP_VERSION).expect("目前版本的更新說明");
        assert!(title.starts_with(super::APP_VERSION));
        assert!(!body.is_empty());
        assert!(!body.contains("\n## "), "只包含這一版");
        // 版本號只比對完整的一段：3.2 不會找到 3.2.1
        assert!(super::release_notes("3.2").is_none());
        assert!(super::release_notes("0.0.0").is_none());
    }
}
