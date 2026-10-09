//! 螢幕錄影的後端：FFmpeg 參數、錄影、轉檔、錄影清單、標註繪製、播放器、系統匣。
//!
//! 平台無關的部分（FFmpeg 參數、剪輯計算、錄影清單、標註繪製…）可以在任何平台編譯與測試；
//! 呼叫 Windows API 的部分放在 `#[cfg(windows)]` 底下。

pub mod actions;
pub mod annotate;
pub mod app;
pub mod args;
pub mod audio;
pub mod audio_out;
pub mod audiopipe;
pub mod clipboard;
pub mod clock;
pub mod desktop;
pub mod downloader;
pub mod edit;
pub mod effects;
pub mod error;
pub mod exporter;
pub mod ffmpeg;
pub mod fonts;
pub mod format;
pub mod http;
pub mod icon;
pub mod instance;
pub mod ipc;
pub mod job;
pub mod library;
pub mod log;
pub mod monitors;
pub mod paths;
pub mod player;
pub mod process;
pub mod projects;
pub mod recorder;
pub mod recycle;
pub mod selfupdate;
pub mod settings;
pub mod thumbs;
pub mod tray;
#[cfg(windows)]
pub mod tray_win;
pub mod types;
pub mod updater;
pub mod version;
pub mod winui;

pub use error::{Error, Result};
