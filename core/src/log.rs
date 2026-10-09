//! 記錄檔：%LOCALAPPDATA%\ScreenRecorder\ScreenRecorder.log（超過 1 MB 時改名為 .old.log 重新開始）。
//! 隱藏主控台的版本看不到訊息，出問題時可從這裡查。同時也輸出到 stderr。

use crate::paths::data_dir;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn log_file() -> PathBuf {
    data_dir().join("ScreenRecorder.log")
}

/// 啟動時呼叫：輪替並開始寫入記錄檔（無法寫入時只輸出到 stderr）
pub fn setup() {
    let dir = data_dir();
    let file = log_file();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if std::fs::metadata(&file).map(|m| m.len() > 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&file, dir.join("ScreenRecorder.old.log"));
    }
    *FILE.lock().unwrap() = Some(file);
    write("INFO ", "──── 程式啟動 ────");
}

pub fn write(level: &str, msg: &str) {
    eprintln!("{msg}");
    write_file(level, msg);
}

/// 只寫進記錄檔（不輸出到 stderr）
fn write_file(level: &str, msg: &str) {
    let path = FILE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(path) = path {
        let stamp = chrono::Local::now().format("%Y/%m/%d %H:%M:%S");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{stamp} {level} {msg}");
        }
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write("INFO ", &format!($($arg)*)) };
}
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write("WARN ", &format!($($arg)*)) };
}
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", &format!($($arg)*)) };
}

/// 程式內部出錯（panic）時把位置與訊息寫進記錄檔；原本的處理（輸出到 stderr）照常進行。
/// 發佈版不中止整個程式：出錯的只有那個背景工作，錄影等其他工作繼續。
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "（無訊息）".into());
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("未命名").to_string();
        write_file("ERROR", &format!("程式內部錯誤（執行緒 {thread}，{at}）：{msg}"));
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn panics_are_written_to_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.log");
        *super::FILE.lock().unwrap() = Some(file.clone());
        super::install_panic_hook();
        let r = std::thread::Builder::new().name("背景工作".into()).spawn(|| panic!("測試用的錯誤 {}", 42)).unwrap().join();
        assert!(r.is_err());
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("程式內部錯誤（執行緒 背景工作，"), "{text}");
        assert!(text.contains("log.rs:"), "{text}");
        assert!(text.contains("測試用的錯誤 42"), "{text}");
        *super::FILE.lock().unwrap() = None;
    }
}
