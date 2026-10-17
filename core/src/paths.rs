//! 資料夾位置與時間戳記。

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

/// 程式資料：%LOCALAPPDATA%\ScreenRecorder（設定、記錄檔、縮圖、剪輯專案、自動下載的 FFmpeg）
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

/// 預設的截圖資料夾：Windows 的「圖片\螢幕擷取畫面」（Win+PrtSc 存的地方）；
/// 找不到時是「圖片」裡的 Screenshots，再不行用 %USERPROFILE%\Pictures\Screenshots
pub fn default_shot_dir() -> PathBuf {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{FOLDERID_Pictures, FOLDERID_Screenshots};
        if let Some(p) = known_folder(&FOLDERID_Screenshots) {
            return p;
        }
        if let Some(p) = known_folder(&FOLDERID_Pictures) {
            return p.join("Screenshots");
        }
    }
    home().join("Pictures").join("Screenshots")
}

/// Windows 的特殊資料夾（圖片、螢幕擷取畫面…），使用者搬過位置（例如 OneDrive）也找得到
#[cfg(windows)]
fn known_folder(id: &windows_core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: 回傳的字串由系統配置，讀完就用 CoTaskMemFree 釋放
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

/// 2026-10-05_14-30-05（當地時間）
pub fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// 現在時間（Unix 毫秒）
/// 暫存檔、暫存資料夾的路徑（系統暫存資料夾裡）：程序編號＋時間＋流水號，同一毫秒同時開始的工作也不會撞名
pub fn unique_temp(prefix: &str, ext: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{}-{n}{ext}", std::process::id(), now_ms()))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 檔案修改時間（Unix 毫秒，含小數，與 Node 的 mtimeMs 相同）
pub fn mtime_ms(meta: &std::fs::Metadata) -> f64 {
    meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// 兩個修改時間（毫秒）是不是同一個：存進 JSON 再讀回來的小數可能差最後一位，不能用 == 比
pub fn same_mtime(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

/// 建立時間（毫秒）；檔案系統不支援時 0
pub fn created_ms(meta: &std::fs::Metadata) -> f64 {
    meta.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// 改建立時間（毫秒）；只有 Windows 能改，其他系統不做事。失敗（例如唯讀）就算了
pub fn set_created(path: &Path, ms: f64) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTimesExt;
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms.max(0.0) as u64);
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = f.set_times(std::fs::FileTimes::new().set_created(t));
        }
    }
    #[cfg(not(windows))]
    let _ = (path, ms);
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

/// dir 是不是 root 本身或它底下的資料夾（不分大小寫、斜線方向）
pub fn inside(dir: &std::path::Path, root: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| p.display().to_string().replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (d, r) = (norm(dir), norm(root));
    !r.is_empty() && (d == r || d.starts_with(&format!("{r}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_never_repeat() {
        let names: std::collections::HashSet<PathBuf> = (0..1000).map(|_| unique_temp("ScreenRecorder-x", ".png")).collect();
        assert_eq!(names.len(), 1000);
        assert!(names.iter().all(|p| p.extension().is_some_and(|e| e == "png")));
    }

    #[test]
    fn inside_matches_the_folder_and_below() {
        assert!(inside(std::path::Path::new("C:\\Videos\\Timelapse\\sub"), std::path::Path::new("C:\\Videos\\Timelapse")));
        assert!(inside(std::path::Path::new("c:/videos/timelapse/"), std::path::Path::new("C:\\Videos\\Timelapse")));
        assert!(!inside(std::path::Path::new("C:\\Videos\\Timelapse2"), std::path::Path::new("C:\\Videos\\Timelapse")));
    }

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

    #[test]
    fn mtime_survives_a_json_round_trip_off_by_the_last_digit() {
        let t = 1_760_501_234_567.890_1_f64;
        assert!(same_mtime(t, t + t * f64::EPSILON));
        assert!(!same_mtime(t, t + 1.0), "差 1 毫秒就是改過了");
    }
}
