//! 版本資訊：版本號以 Cargo.toml（workspace）為唯一來源，更新說明為 CHANGELOG.md（建置時一併打包進執行檔）。

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

#[cfg(test)]
mod tests {
    #[test]
    fn version_and_changelog_are_embedded() {
        assert!(super::APP_VERSION.split('.').count() == 3);
        assert!(super::CHANGELOG.contains(&format!("## {}", super::APP_VERSION)));
    }
}
