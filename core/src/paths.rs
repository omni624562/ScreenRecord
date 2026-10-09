//! 資料夾位置與時間戳。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 程式所在資料夾（exe 所在處）；可由主程式在啟動時指定（開發時為專案根目錄）
pub fn app_dir() -> PathBuf {
    APP_DIR.get_or_init(|| std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))).clone()
}

pub fn set_app_dir(dir: PathBuf) {
    let _ = APP_DIR.set(dir);
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// 程式資料：%LOCALAPPDATA%\ScreenRecorder（設定、記錄檔、縮圖、自動下載的 FFmpeg、WebView2 資料）
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SCREENRECORDER_DATA_DIR") {
        return PathBuf::from(dir); // 測試用
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local"));
    base.join("ScreenRecorder")
}

/// 預設儲存位置：%USERPROFILE%\Videos\Timelapse
pub fn default_output_dir() -> PathBuf {
    home().join("Videos").join("Timelapse")
}

/// 2026-10-05_14-30-05（當地時間）
pub fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// 現在時間（Unix 毫秒）
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 檔案修改時間（Unix 毫秒，含小數，與 Node 的 mtimeMs 相同）
pub fn mtime_ms(meta: &std::fs::Metadata) -> f64 {
    meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// 檔名已存在時加上 _2、_3…，避免覆蓋既有檔案
pub fn unique_path(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}{ext}"));
    let mut i = 2;
    while p.exists() {
        p = dir.join(format!("{base}_{i}{ext}"));
        i += 1;
    }
    p
}

/// 是否為 Windows 的絕對路徑（C:\… 或 \\server\…）：API 只接受這種路徑
pub fn is_windows_abs(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') || p.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a", ".mp4"), dir.path().join("a.mp4"));
        std::fs::write(dir.path().join("a.mp4"), "").unwrap();
        std::fs::write(dir.path().join("a_2.mp4"), "").unwrap();
        assert_eq!(unique_path(dir.path(), "a", ".mp4"), dir.path().join("a_3.mp4"));
    }

    #[test]
    fn windows_paths() {
        assert!(is_windows_abs("C:\\Videos\\a.mp4"));
        assert!(is_windows_abs("\\\\nas\\share\\a.mp4"));
        assert!(!is_windows_abs("/home/a.mp4"));
        assert!(!is_windows_abs("C:/a.mp4"));
        assert!(!is_windows_abs("a.mp4"));
    }

    #[test]
    fn timestamp_format() {
        let t = timestamp();
        assert_eq!(t.len(), 19);
        assert_eq!(&t[10..11], "_");
    }
}
