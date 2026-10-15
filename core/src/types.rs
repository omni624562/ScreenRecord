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
    /// Windows Graphics Capture：直接擷取一個視窗（被擋住、移動都錄得到）
    Gfxcapture,
}

impl CaptureMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMethod::Ddagrab => "ddagrab",
            CaptureMethod::Gdigrab => "gdigrab",
            CaptureMethod::Gfxcapture => "gfxcapture",
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
    /// 系統聲音只錄這個程式（執行檔名稱，例如 chrome.exe）；空字串 = 全部系統聲音
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
}

/// 全域快速鍵是否登記成功（false = 已被其他程式占用；停用的視為成功）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HotkeyStatus {
    pub record: bool,
    pub pause: bool,
    #[serde(default)]
    pub shot: bool,
    #[serde(default)]
    pub snip: bool,
    #[serde(default)]
    pub mark: bool,
    #[serde(default)]
    pub pen: bool,
    #[serde(default)]
    pub replay: bool,
}

impl HotkeyStatus {
    /// 依 Hotkeys::all 的順序
    pub fn all(&self) -> [bool; HOTKEY_COUNT] {
        [self.record, self.pause, self.shot, self.snip, self.mark, self.pen, self.replay]
    }

    pub fn from_list(ok: &[bool]) -> HotkeyStatus {
        let g = |i: usize| ok.get(i).copied().unwrap_or(false);
        HotkeyStatus { record: g(0), pause: g(1), shot: g(2), snip: g(3), mark: g(4), pen: g(5), replay: g(6) }
    }
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

/// 一組全域快速鍵：修飾鍵＋按鍵（Windows 虛擬鍵碼）
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

/// 錄影時要顯示的按鍵：有 Ctrl / Alt / Win 的組合，或 Enter、Esc 等功能鍵；一般打字（含 Shift + 字母）不顯示，避免錄到密碼
pub fn shown_keys(vk: u32, ctrl: bool, alt: bool, shift: bool, win: bool) -> Option<String> {
    let name = match vk {
        // 修飾鍵本身不顯示（和其他鍵一起按時才顯示）
        0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5 => return None,
        0x0D => "Enter".to_string(),
        0x1B => "Esc".to_string(),
        0x09 => "Tab".to_string(),
        0x08 => "Backspace".to_string(),
        0x25 => "←".to_string(),
        0x26 => "↑".to_string(),
        0x27 => "→".to_string(),
        0x28 => "↓".to_string(),
        0x7C..=0x87 => format!("F{}", vk - 0x6F),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        _ => key_name(vk)?,
    };
    let special = !matches!(vk, 0x41..=0x5A | 0x30..=0x39 | 0x20 | 0xBA..=0xBF | 0xDB..=0xDD);
    if !(ctrl || alt || win || special) {
        return None;
    }
    let mut parts: Vec<String> = vec![];
    for (on, n) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (win, "Win")] {
        if on {
            parts.push(n.into());
        }
    }
    parts.push(name);
    Some(parts.join(" + "))
}

/// 全域快速鍵的數量
pub const HOTKEY_COUNT: usize = 7;

/// 錄影時螢幕上的控制列（設定的 recBar）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecBarMode {
    /// 時間、標記、暫停 / 繼續、停止
    #[default]
    Full,
    /// 只有狀態點與停止（不顯示時間）
    Compact,
    /// 不顯示（倒數時仍顯示，開始錄影後隱藏；用快速鍵或系統匣停止）
    Hidden,
}

/// 錄影控制列的設定：樣式（recBar）與錄整個螢幕時拖曳後的位置（recBarPos）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecBarPrefs {
    pub mode: RecBarMode,
    /// 控制列左上角（虛擬桌面的實體像素，可以在另一個螢幕上）；None = 錄的螢幕上方中間
    pub pos: Option<(i32, i32)>,
}

/// 全域快速鍵（None = 停用）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    pub record: Option<Hotkey>,
    pub pause: Option<Hotkey>,
    pub shot: Option<Hotkey>,
    pub snip: Option<Hotkey>,
    /// 錄影中加標記（3.1 新增：舊的設定沒有這個欄位時用預設）
    #[serde(default = "default_mark")]
    pub mark: Option<Hotkey>,
    /// 螢幕畫筆（3.1 新增）
    #[serde(default = "default_pen")]
    pub pen: Option<Hotkey>,
    /// 重播緩衝：沒在錄影時開始，重播緩衝中存下最近幾分鐘（3.1 新增）
    #[serde(default = "default_replay")]
    pub replay: Option<Hotkey>,
}

fn default_mark() -> Option<Hotkey> {
    Some(Hotkey::ctrl_alt(0x4D))
}

fn default_pen() -> Option<Hotkey> {
    Some(Hotkey::ctrl_alt(0x44))
}

fn default_replay() -> Option<Hotkey> {
    Some(Hotkey::ctrl_alt(0x42))
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys {
            record: Some(Hotkey::ctrl_alt(0x52)),
            pause: Some(Hotkey::ctrl_alt(0x50)),
            shot: Some(Hotkey::ctrl_alt(0x53)),
            snip: Some(Hotkey::ctrl_alt(0x41)),
            mark: default_mark(),
            pen: default_pen(),
            replay: default_replay(),
        }
    }
}

impl Hotkeys {
    /// 依序：開始 / 停止錄影、暫停 / 繼續、截圖、框選截圖、加標記、螢幕畫筆、重播緩衝
    pub fn all(&self) -> [Option<Hotkey>; HOTKEY_COUNT] {
        [self.record, self.pause, self.shot, self.snip, self.mark, self.pen, self.replay]
    }

    pub fn set(&mut self, i: usize, k: Option<Hotkey>) {
        match i {
            0 => self.record = k,
            1 => self.pause = k,
            2 => self.shot = k,
            3 => self.snip = k,
            4 => self.mark = k,
            5 => self.pen = k,
            _ => self.replay = k,
        }
    }

    /// 全部停用（設定新的快速鍵時暫停）
    pub fn none() -> Hotkeys {
        Hotkeys { record: None, pause: None, shot: None, snip: None, mark: None, pen: None, replay: None }
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

/// 每組快速鍵的名稱（依介面語言）
pub fn hotkey_names() -> [&'static str; HOTKEY_COUNT] {
    crate::tr!(
        ["開始 / 停止錄影", "暫停 / 繼續", "截圖（整個錄影範圍）", "框選截圖", "錄影中加標記", "螢幕畫筆（開 / 關）", "回溯錄影（開始 / 存下）"],
        [
            "Start / stop recording",
            "Pause / resume",
            "Screenshot (whole recording area)",
            "Select area screenshot",
            "Add marker while recording",
            "Screen pen (on / off)",
            "Replay buffer (start / save the last minutes)"
        ]
    )
}

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
    /// 截圖的儲存位置（空白 = 預設的截圖資料夾，見 paths::default_shot_dir）；錄影存在 output_dir
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shot_dir: String,
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
    /// 錄影時在畫面上顯示滑鼠點擊（會錄進影片）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub show_clicks: bool,
    /// 錄影時在畫面上顯示按下的快速鍵（有 Ctrl / Alt / Win 的組合與 Enter、Esc 等；一般打字不顯示）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub show_keys: bool,
    /// 錄影時游標周圍顯示一圈光暈（會錄進影片）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cursor_halo: bool,
    /// 錄影時隱藏桌面圖示（停止後還原）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide_icons: bool,
    /// 只錄這個視窗（自訂範圍跟著視窗移動）；視窗代碼見 winui::WindowInfo
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_window: Option<i64>,
    /// 攝影機子母畫面（把攝影機的畫面疊在角落）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraConfig>,
    /// 只錄聲音（畫面是一張「只錄聲音」的卡片，見 audio_card.rs）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audio_only: bool,
    /// 只錄視窗的內容（follow_window 的視窗，用 Windows Graphics Capture）：被其他視窗擋住、移動都錄得到；
    /// 畫面上的其他東西（攝影機小視窗、點擊效果、螢幕畫筆）不會錄進去。FFmpeg 不支援或失敗時改用跟著視窗的範圍
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub window_capture: bool,
    /// 系統聲音與麥克風分開存（多兩條音軌；第一條仍是混在一起的，一般播放器照常播放）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub separate_audio: bool,
    /// 重播緩衝：在背景一直錄、只保留最近這麼多秒，需要時再存下來（0 = 一般錄影）
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub replay_sec: u32,
}

impl RecordConfig {
    /// 這次錄影要分軌：有打開分軌，而且系統聲音、麥克風都有錄
    pub fn separate_tracks(&self) -> bool {
        self.separate_audio && self.audio.system && self.audio.mic
    }

    /// 截圖存到哪裡（沒有指定時是預設的截圖資料夾）
    pub fn shots_dir(&self) -> String {
        match self.shot_dir.trim() {
            "" => crate::paths::default_shot_dir().display().to_string(),
            d => d.to_string(),
        }
    }
}

/// 攝影機子母畫面
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraConfig {
    /// 攝影機（DirectShow 的裝置名稱）
    pub device: String,
    /// 位置：0 右下、1 左下、2 右上、3 左上
    #[serde(default)]
    pub corner: u8,
    /// 大小：畫面短邊（橫的畫面是高度）的百分比
    #[serde(default = "default_camera_size")]
    pub size: u32,
    /// 圓形（否則是方形）
    #[serde(default = "default_true")]
    pub circle: bool,
    /// 拖曳後記住的位置：小視窗中心在擷取範圍內的相對位置（萬分比，0～10000）；None = 放在 corner
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[u16; 2]>,
}

fn default_camera_size() -> u32 {
    20
}

fn default_true() -> bool {
    true
}

/// 攝影機大小可選的百分比
pub const CAMERA_SIZES: [u32; 3] = [15, 20, 28];

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
    /// 沒錄到的聲音（audiopipe::SILENT_* 全部是靜音、PARTLY_* 有一段是靜音）：結果卡說明
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub silent_audio: u8,
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
    /// 這次錄影加了幾個標記
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub markers: u32,
    /// 重播緩衝中（不是一般錄影）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplayStatus>,
    /// 以靜音代替的聲音來源（audiopipe::SILENT_SYSTEM、SILENT_MIC）：錄影面板標成紅色
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub silent_audio: u8,
}

/// 重播緩衝的狀態
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayStatus {
    /// 保留最近幾秒
    pub sec: u32,
    /// 緩衝在硬碟上的大小
    pub bytes: u64,
    /// 正在存
    pub saving: bool,
    /// 這次存了幾支
    pub saved: u32,
    /// 最近存下的檔案
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

impl MediaInfo {
    /// 錄影開始（截圖）的時間（毫秒），檔名沒有日期時間時用：建立時間與修改時間較早的那個。
    /// 修改時間是錄完的時間；改名不會改變建立時間。從別處複製來的檔案建立時間是複製的時候，這時修改時間比較早
    pub fn time_ms(&self) -> f64 {
        if self.created > 0.0 {
            self.created.min(self.mtime)
        } else {
            self.mtime
        }
    }
}

/// 影片章節（開始的秒數與名稱）
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    /// 修改時間（毫秒）
    pub mtime: f64,
    /// 建立時間（毫秒）；拿不到時 0
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub created: f64,
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
    /// 章節（錄影時加的標記）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter>,
    /// 幾條音軌（分軌錄影是 3：混音、系統聲音、麥克風）
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub audio_tracks: u32,
    /// 在儲存位置的哪個子資料夾（例如「教學\2026」；空的 = 儲存位置本身）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Mp4,
    Gif,
    /// WebP 動圖：比 GIF 小很多、顏色也比較好
    Webp,
}

impl ExportFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::Gif => "gif",
            ExportFormat::Webp => "webp",
        }
    }

    /// 動圖（GIF、WebP）：沒有聲音、選寬度與每秒張數
    pub fn is_animation(self) -> bool {
        matches!(self, ExportFormat::Gif | ExportFormat::Webp)
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
    /// 搜尋時在字幕（影片旁邊的 SRT）裡找到的地方
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<SpeechHit>,
}

/// 說過的話：字幕裡符合搜尋的一句
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SpeechHit {
    /// 影片的秒數
    pub start: f64,
    pub text: String,
}

/// 全部錄影的表格：每支錄影底下最多列幾句說過的話
pub const LIBRARY_HITS_SHOWN: usize = 3;

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
    /// 也列出只有聲音的檔案（存成 M4A 的）：全部錄影、檢視器要；主畫面的最近錄影不要
    pub audio: bool,
    /// 也列出這些資料夾裡的截圖（選了「留在原處」、沒有搬到截圖資料夾的舊截圖）
    pub extra_dirs: Vec<String>,
    /// 只列這些檔案（「只看已選取」）；原檔或底下的加速版在裡面就列出
    pub only: Option<Vec<String>>,
    /// 截圖格依日期分組分頁（每天從新的一列開始、上面有標題列）；有指定時忽略 page_size、fit_px
    pub grid: Option<GridFit>,
    /// 換縮圖大小時：回傳含有第幾筆（從 0 算）的那一頁，忽略 page
    pub anchor: Option<usize>,
    /// 截圖的瀏覽條件（瀏覽欄）；有指定時回傳每個選項的張數
    pub facet: Option<FacetQuery>,
    /// 也列出子資料夾裡的（最多 SUBDIR_DEPTH 層）
    pub subdirs: bool,
}

/// 瀏覽欄選的條件：同一組裡選多個是「其中一個」，常見詞是「全部都有」；不同組之間是「而且」
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FacetQuery {
    /// 程式（執行檔名稱，例如 chrome.exe）
    pub apps: Vec<String>,
    /// 內容類型（error、web、chat…）
    pub kinds: Vec<String>,
    /// 常見詞
    pub terms: Vec<String>,
    /// 日期範圍（today、yesterday、week、month、lastmonth、older）
    pub when: Vec<String>,
    /// 一天裡的時段（morning、afternoon、evening）
    pub hours: Vec<String>,
    /// 顏色（dark、light、red…）
    pub colors: Vec<String>,
    /// 其他（long、edited、starred、frame、text）
    pub other: Vec<String>,
    /// 某一天（2026-10-08，月曆點一天）
    pub day: Option<String>,
    /// 子資料夾（MediaInfo::folder；空的 = 儲存位置本身）
    #[serde(default)]
    pub folders: Vec<String>,
}

impl FacetQuery {
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// 選了幾個條件
    pub fn count(&self) -> usize {
        self.folders.len() + self.apps.len() + self.kinds.len() + self.terms.len() + self.when.len() + self.hours.len() + self.colors.len() + self.other.len() + usize::from(self.day.is_some())
    }
}

/// 瀏覽欄的一個選項：代號、顯示的名稱、符合的張數
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FacetOption {
    pub id: String,
    pub label: String,
    pub count: usize,
}

/// 瀏覽欄每一組的選項（張數是加上這個條件後會剩幾張）
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FacetCounts {
    /// 子資料夾（只有一個資料夾時是空的）
    pub folders: Vec<FacetOption>,
    pub apps: Vec<FacetOption>,
    pub kinds: Vec<FacetOption>,
    pub terms: Vec<FacetOption>,
    pub when: Vec<FacetOption>,
    pub hours: Vec<FacetOption>,
    pub colors: Vec<FacetOption>,
    pub other: Vec<FacetOption>,
    /// 還有幾張截圖的字還沒讀（瀏覽欄提示分類還不完整）
    pub pending_text: usize,
}

/// 截圖格的版面：每列幾格、可用高度、一列（含間距）的高度、日期標題列的高度（px）
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GridFit {
    pub cols: usize,
    pub height: f64,
    pub row_h: f64,
    pub head_h: f64,
}

/// 截圖格裡的一天（這一頁出現的）
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayGroup {
    /// 2026-07-27
    pub day: String,
    /// 這一天在這一頁的第一筆（items 的索引）
    pub first: usize,
    /// 這一天一共幾筆（每一頁）
    pub total: usize,
    /// 這一天全部的檔案（每一頁）：「選這一天」用
    pub paths: Vec<String>,
    /// 這一天從前一頁接過來
    pub continued: bool,
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
    /// 符合條件的全部檔案（每一頁，含加速版），依清單順序：「選取全部」用
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_paths: Vec<String>,
    /// 這一頁第一筆在全部裡是第幾筆（從 0 算）
    #[serde(default)]
    pub start: usize,
    /// 依日期分組時：這一頁出現的每一天
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<DayGroup>,
    /// 瀏覽欄的選項與張數（查詢有 facet 時）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facets: Option<FacetCounts>,
    /// 資料夾讀不到（不存在、隨身碟或網路磁碟沒接上）時的說明
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// WebP 動圖
    Webp,
    Cut,
    /// 多支錄影合併成一支
    Merge,
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
    /// 有 gfxcapture 濾鏡（Windows Graphics Capture；FFmpeg 8.1 以後）
    #[serde(default)]
    pub has_gfxcapture: bool,
    /// 有 whisper 濾鏡（語音辨識；沒有時自動字幕另外下載語音辨識用的 FFmpeg）
    #[serde(default)]
    pub has_whisper: bool,
    /// ddagrab 能在畫面沒變化時不送出（dup_frames，FFmpeg 7.0 起）：即時預覽用
    #[serde(default)]
    pub ddagrab_skip_static: bool,
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
    /// 錄影時在顯示卡上處理畫面的方式（實測通過時的說明）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_convert: Option<String>,
    /// 顯示卡處理測試中
    #[serde(default)]
    pub gpu_testing: bool,
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
    /// 預設的截圖資料夾
    #[serde(default)]
    pub default_shot_dir: String,
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

#[cfg(test)]
mod tests {
    use super::{shown_keys, Hotkey, Hotkeys};

    #[test]
    fn old_hotkey_settings_get_the_new_defaults() {
        // 3.0 存的設定沒有標記、畫筆
        let old = r#"{"record":{"ctrl":true,"alt":true,"shift":false,"win":false,"key":82},"pause":null,"shot":null,"snip":null}"#;
        let k: Hotkeys = serde_json::from_str(old).unwrap();
        assert_eq!(k.pen, Some(Hotkey::ctrl_alt(0x44)));
        assert_eq!(k.mark, Some(Hotkey::ctrl_alt(0x4D)));
        assert_eq!(k.pause, None);
        assert_eq!(k.label(5), "Ctrl+Alt+D");
        // 停用畫筆後存起來，讀回來還是停用
        let mut k = Hotkeys::default();
        k.set(5, None);
        let back: Hotkeys = serde_json::from_str(&serde_json::to_string(&k).unwrap()).unwrap();
        assert_eq!(back.pen, None);
        assert_eq!(back.mark, Hotkeys::default().mark);
        // 重播緩衝也是 3.1 新增
        assert_eq!(k.label(6), "Ctrl+Alt+B");
        assert_eq!(serde_json::from_str::<Hotkeys>(old).unwrap().replay, Some(Hotkey::ctrl_alt(0x42)));
    }

    #[test]
    fn only_shortcuts_and_function_keys_are_shown() {
        assert_eq!(shown_keys(0x43, true, false, false, false).as_deref(), Some("Ctrl + C"));
        assert_eq!(shown_keys(0x53, true, false, true, false).as_deref(), Some("Ctrl + Shift + S"));
        assert_eq!(shown_keys(0x44, false, false, false, true).as_deref(), Some("Win + D"));
        assert_eq!(shown_keys(0x0D, false, false, false, false).as_deref(), Some("Enter"));
        assert_eq!(shown_keys(0x74, false, false, false, false).as_deref(), Some("F5"));
        assert_eq!(shown_keys(0x09, false, true, false, false).as_deref(), Some("Alt + Tab"));
        // 一般打字（含 Shift）、單獨的修飾鍵不顯示
        assert_eq!(shown_keys(0x41, false, false, false, false), None);
        assert_eq!(shown_keys(0x41, false, false, true, false), None);
        assert_eq!(shown_keys(0x20, false, false, false, false), None);
        assert_eq!(shown_keys(0xA2, true, false, false, false), None);
        assert_eq!(shown_keys(0x10, false, false, true, false), None);
    }
}
