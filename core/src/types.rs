//! 共用的型別（設定、狀態、錄影清單…）；序列化格式與 2.x 版相同，舊的設定檔可以直接沿用。
//!
//! JSON 欄位名稱一律 camelCase；沒有值的欄位不輸出（介面以 `!== undefined` 判斷，不能送 null）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// `${adapter}:${output}`
    pub id: String,
    pub adapter: u32,
    pub output: u32,
    pub adapter_name: String,
    /// 例如 \\.\DISPLAY1
    pub device_name: String,
    pub display_number: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
    /// DXGI_MODE_ROTATION：1 = 不旋轉
    pub rotation: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMethod {
    Ddagrab,
    Gdigrab,
}

impl CaptureMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMethod::Ddagrab => "ddagrab",
            CaptureMethod::Gdigrab => "gdigrab",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MethodPreference {
    #[default]
    Auto,
    Ddagrab,
    Gdigrab,
}

pub const SCALE_OPTIONS: [u32; 4] = [100, 75, 50, 25];

/// 錄影範圍。自訂範圍的座標由介面送來，可能是小數，驗證時要求整數。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    Monitor {
        #[serde(rename = "monitorId")]
        monitor_id: String,
    },
    /// 所有螢幕（整個延伸桌面）
    All,
    Region {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
    /// 電腦播放的聲音（WASAPI loopback）
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub mic: bool,
    /// 空字串 = 預設麥克風
    #[serde(default)]
    pub mic_id: String,
}

/// 全域快捷鍵是否登記成功（false = 已被其他程式占用；停用的視為成功）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HotkeyStatus {
    pub record: bool,
    pub pause: bool,
    #[serde(default)]
    pub shot: bool,
    #[serde(default)]
    pub snip: bool,
}

/// 最近一次的截圖（seq 每次加一，介面看到變了就更新清單）
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShotInfo {
    pub seq: u64,
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// 已複製到剪貼簿
    pub copied: bool,
}

/// 一組全域快捷鍵：修飾鍵＋按鍵（Windows 虛擬鍵碼）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub win: bool,
    pub key: u32,
}

impl Hotkey {
    pub const fn ctrl_alt(key: u32) -> Hotkey {
        Hotkey { ctrl: true, alt: true, shift: false, win: false, key }
    }

    /// 例如「Ctrl+Alt+R」
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = vec![];
        for (on, name) in [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift"), (self.win, "Win")] {
            if on {
                parts.push(name.into());
            }
        }
        parts.push(key_name(self.key).unwrap_or_else(|| format!("0x{:02X}", self.key)));
        parts.join("+")
    }

    /// 可以使用的組合：支援的按鍵，且有 Ctrl、Alt 或 Win（只有 Shift 會擋到一般打字）
    pub fn valid(&self) -> bool {
        key_name(self.key).is_some() && (self.ctrl || self.alt || self.win)
    }
}

/// 支援的按鍵名稱（Windows 虛擬鍵碼）：A–Z、0–9、F1–F12 與幾個常用的功能鍵
pub fn key_name(vk: u32) -> Option<String> {
    Some(match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk)?.to_string(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        0x20 => "Space".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x2C => "PrintScreen".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        _ => return None,
    })
}

/// 四個全域快捷鍵（None = 停用）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    pub record: Option<Hotkey>,
    pub pause: Option<Hotkey>,
    pub shot: Option<Hotkey>,
    pub snip: Option<Hotkey>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys { record: Some(Hotkey::ctrl_alt(0x52)), pause: Some(Hotkey::ctrl_alt(0x50)), shot: Some(Hotkey::ctrl_alt(0x53)), snip: Some(Hotkey::ctrl_alt(0x41)) }
    }
}

impl Hotkeys {
    /// 依序：開始 / 停止錄影、暫停 / 繼續、截圖、框選截圖
    pub fn all(&self) -> [Option<Hotkey>; 4] {
        [self.record, self.pause, self.shot, self.snip]
    }

    pub fn set(&mut self, i: usize, k: Option<Hotkey>) {
        match i {
            0 => self.record = k,
            1 => self.pause = k,
            2 => self.shot = k,
            _ => self.snip = k,
        }
    }

    /// 第 i 個的名稱（停用時是空字串）
    pub fn label(&self, i: usize) -> String {
        self.all()[i].map(|k| k.label()).unwrap_or_default()
    }

    /// 同一組按鍵用在兩個功能上：回傳另一個的位置
    pub fn conflict(&self, i: usize, k: &Hotkey) -> Option<usize> {
        self.all().iter().enumerate().find(|(j, o)| *j != i && o.as_ref() == Some(k)).map(|(j, _)| j)
    }
}

pub const HOTKEY_NAMES: [&str; 4] = ["開始 / 停止錄影", "暫停 / 繼續", "截圖（固定範圍）", "框選截圖"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderPreference {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordConfig {
    pub source: SourceConfig,
    pub fps: f64,
    pub scale: f64,
    pub draw_mouse: bool,
    /// 0 = 不限
    pub max_minutes: f64,
    #[serde(default)]
    pub method: MethodPreference,
    pub output_dir: String,
    #[serde(default)]
    pub audio: AudioConfig,
    /// 未指定視為 auto
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<EncoderPreference>,
    /// 開始前倒數秒數（0 = 立即開始）；未指定視為 3
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown_sec: Option<f64>,
    /// 開始擷取時縮小操作視窗；未指定視為 true
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_ui: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecorderState {
    #[default]
    Idle,
    Countdown,
    Recording,
    Paused,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub frames: u64,
    pub video_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub message: String,
    /// 合併失敗時保留的分段資料夾
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub t: u64,
    pub level: LogLevel,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStatus {
    pub state: RecorderState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<CaptureMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiles: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    pub segments: u32,
    pub frames: u64,
    pub video_sec: f64,
    pub bytes: u64,
    pub recorded_ms: u64,
    pub max_ms: u64,
    pub fps: f64,
    pub out_width: i32,
    pub out_height: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_fps: Option<f64>,
    pub slow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrying: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub countdown_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub countdown_covers_ui: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_free_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RecordingResult>,
    pub log: Vec<LogEntry>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    /// 修改時間（毫秒）
    pub mtime: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_audio: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Mp4,
    Gif,
}

impl ExportFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::Gif => "gif",
        }
    }
}

/// 加速版 / GIF：MediaInfo 加上倍率與格式
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportInfo {
    #[serde(flatten)]
    pub media: MediaInfo,
    pub speed: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ExportFormat>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry {
    #[serde(flatten)]
    pub media: MediaInfo,
    pub exports: Vec<ExportInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryFilter {
    /// 全部錄影（不含截圖）
    #[default]
    All,
    Original,
    Cut,
    Speed,
    Audio,
    /// 截圖（PNG）
    Shot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibrarySort {
    #[default]
    New,
    Old,
    Size,
    Duration,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibraryQuery {
    pub q: Option<String>,
    pub filter: LibraryFilter,
    pub sort: LibrarySort,
    pub page: Option<f64>,
    pub page_size: Option<f64>,
    /// 依高度分頁：表格可用的高度（px）；有指定時忽略 page_size
    pub fit_px: Option<f64>,
}

/// 全部錄影表格的列高（含 1px 分隔線）
pub const LIBRARY_ROW_MAIN_PX: f64 = 41.0;
pub const LIBRARY_ROW_SUB_PX: f64 = 33.0;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPage {
    pub dir: String,
    pub total: usize,
    pub page: usize,
    pub pages: usize,
    pub page_size: usize,
    pub items: Vec<LibraryEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportState {
    Running,
    Done,
    Error,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportKind {
    Speed,
    Gif,
    Cut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStatus {
    pub id: u64,
    pub kind: ExportKind,
    pub state: ExportState,
    pub source: String,
    pub output: String,
    pub speed: f64,
    /// 0～1
    pub progress: f64,
    pub expected_sec: f64,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadPhase {
    #[default]
    Idle,
    Downloading,
    Verifying,
    Extracting,
    Done,
    Error,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub phase: DownloadPhase,
    pub received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// bytes / 秒
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegInfo {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub has_ddagrab: bool,
    pub has_gdigrab: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    /// 實測可用的硬體編碼器；None = 測試中
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hw_encoders: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_gpu: Option<bool>,
    /// ddagrab 實測：None = 尚未測試
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ddagrab_works: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ddagrab_error: Option<String>,
    pub searched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioEnv {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<String>,
    pub captures: Vec<AudioDevice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvInfo {
    pub app_dir: String,
    pub app_version: String,
    pub default_output_dir: String,
    pub ffmpeg: FfmpegInfo,
    pub monitors: Vec<MonitorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_error: Option<String>,
    pub desktop: Rect,
    pub audio: AudioEnv,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotkeys: Option<HotkeyStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateInfo>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    /// 新版 exe 的下載網址（Release 有附 ScreenRecorder.exe 且有 SHA-256 時才有，可在程式內更新）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}
