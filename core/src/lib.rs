//! 螢幕錄影的後端（由 src/*.ts 移植）。
//!
//! 平台無關的部分（FFmpeg 參數、剪輯計算、錄影清單、HTTP API…）可以在任何平台編譯與測試；
//! 呼叫 Windows API 的部分放在 `#[cfg(windows)]` 底下。

pub mod args;
pub mod audio;
pub mod audiopipe;
pub mod clock;
pub mod desktop;
pub mod downloader;
pub mod edit;
pub mod error;
pub mod exporter;
pub mod ffmpeg;
pub mod format;
pub mod http;
pub mod job;
pub mod library;
pub mod monitors;
pub mod log;
pub mod paths;
pub mod process;
pub mod recycle;
pub mod settings;
pub mod thumbs;
pub mod types;
pub mod updater;
pub mod winui;

pub use error::{Error, Result};
