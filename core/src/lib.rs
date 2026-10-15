//! 螢幕錄影的後端：FFmpeg 參數、錄影、轉檔、錄影清單、標註繪製、播放器、系統匣。
//!
//! 平台無關的部分（FFmpeg 參數、剪輯計算、錄影清單、標註繪製…）可以在任何平台編譯與測試；
//! 呼叫 Windows API 的部分放在 `#[cfg(windows)]` 底下。

pub mod actions;
pub mod after_record;
pub mod annotate;
pub mod app;
pub mod args;
pub mod audio;
pub mod audio_card;
pub mod audio_out;
pub mod audiopipe;
pub mod bubble;
#[cfg(windows)]
pub mod bubble_win;
pub mod camera_bubble;
#[cfg(windows)]
pub mod camera_bubble_win;
pub mod chapters;
pub mod clipboard;
pub mod clock;
pub mod cursor;
pub mod desktop;
pub mod downloader;
pub mod dragout;
pub mod edit;
pub mod effects;
pub mod error;
pub mod exporter;
pub mod ffmpeg;
pub mod filepick;
pub mod fonts;
pub mod format;
pub mod http;
pub mod i18n;
pub mod icon;
pub mod idle;
pub mod input_overlay;
#[cfg(windows)]
pub mod input_overlay_win;
pub mod instance;
pub mod ipc;
pub mod job;
pub mod library;
#[cfg(windows)]
pub mod llhook_win;
pub mod log;
pub mod longshot;
pub mod meter;
pub mod monitors;
pub mod ocr;
pub mod ocr_index;
pub mod paths;
pub mod picture;
#[cfg(windows)]
pub mod pin_win;
pub mod pins;
pub mod player;
pub mod power;
pub mod presets;
pub mod process;
pub mod projects;
pub mod qr;
#[cfg(windows)]
pub mod rec_frame_win;
pub mod recorder;
pub mod recovery;
pub mod recycle;
pub mod replay;
pub mod schedule;
pub mod screen_pen;
#[cfg(windows)]
pub mod screen_pen_win;
#[cfg(windows)]
pub mod scroll_win;
pub mod selfupdate;
pub mod settings;
pub mod shot_edit;
pub mod shot_toast;
#[cfg(windows)]
pub mod shot_toast_win;
pub mod similar;
pub mod snip_tools;
#[cfg(windows)]
pub mod snip_win;
pub mod steps;
#[cfg(windows)]
pub mod steps_win;
pub mod subtitles;
pub mod thumbs;
pub mod tray;
#[cfg(windows)]
pub mod tray_win;
pub mod types;
pub mod updater;
pub mod version;
pub mod vertical;
pub mod video_frame;
pub mod winui;
pub mod zoom;

#[cfg(test)]
mod e2e_tests;

pub use error::{Error, Result};
