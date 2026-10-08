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
    let path = FILE.lock().unwrap().clone();
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
