//! 版本資訊：版本號以 package.json 為唯一來源，更新說明為 CHANGELOG.md（建置時一併打包進執行檔）。

pub const APP_VERSION: &str = env!("APP_VERSION");
pub const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

include!(concat!(env!("OUT_DIR"), "/ui_files.rs"));

/// 內嵌介面的檔案
pub fn ui_file(name: &str) -> Option<(&'static str, &'static [u8])> {
    UI_FILES.iter().find(|(n, _, _)| *n == name).map(|(_, m, b)| (*m, *b))
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_and_ui_are_embedded() {
        assert!(super::APP_VERSION.split('.').count() == 3);
        let (mime, body) = super::ui_file("index.html").unwrap();
        assert!(mime.starts_with("text/html"));
        assert!(!body.is_empty());
    }
}
