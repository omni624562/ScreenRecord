//! 操作視窗的設定：存在 settings.json 的 ui 欄位（與 2.x 版的網頁介面相同的格式，升級後沿用）。
//! 同時把錄影設定存到 config 欄位，系統匣的「開始錄影」直接使用。

use screenrecorder_core::format::MP4_WIDTHS;
use screenrecorder_core::types::{AudioConfig, CameraConfig, EncoderPreference, EnvInfo, ExportFormat, MethodPreference, MonitorInfo, RecBarMode, RecordConfig, Rect, SourceConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Monitor,
    All,
    Region,
    /// 只錄聲音（畫面不錄）
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportMode {
    /// 指定倍率
    Speed,
    /// 指定長度
    Target,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSettings {
    pub source_type: SourceType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_id: Option<String>,
    /// 選的那台實體螢幕（MonitorInfo::key）：拔掉、接上其他螢幕後編號變了也找得回來
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub monitor_key: String,
    /// 單一螢幕模式下只錄其中一部分（桌面座標，在選的螢幕內）；None = 整個螢幕
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_region: Option<Rect>,
    pub region: Rect,
    pub fps: f64,
    pub scale: u32,
    pub draw_mouse: bool,
    pub max_minutes: f64,
    pub method: MethodPreference,
    pub output_dir: String,
    pub speed: f64,
    pub audio_system: bool,
    pub audio_mic: bool,
    /// 空字串 = 預設麥克風
    pub mic_id: String,
    /// 系統聲音只錄這個程式（執行檔名稱）；空字串 = 全部
    pub audio_app: String,
    pub keep_audio: bool,
    pub live_preview: bool,
    pub encoder: EncoderPreference,
    pub countdown_sec: u32,
    pub hide_ui: bool,
    pub export_format: ExportFormat,
    pub export_mode: ExportMode,
    pub export_target: String,
    pub gif_width: u32,
    pub gif_fps: u32,
    /// 加速版縮小後的寬度（0 = 原尺寸）
    pub mp4_width: u32,
    /// 截圖後直接開啟編輯
    pub edit_after_shot: bool,
    /// 加速版壓縮到這個大小以內（MB）；0 = 不限
    pub mp4_max_mb: u32,
    /// 錄影時顯示滑鼠點擊
    pub show_clicks: bool,
    /// 錄影時顯示按下的快速鍵
    pub show_keys: bool,
    /// 自訂範圍跟著這個視窗（「只錄這個視窗」）；視窗關掉、重新開機後就失效
    #[serde(skip)]
    pub follow_window: Option<FollowWindow>,
    /// 攝影機子母畫面：攝影機名稱（空字串 = 不使用）、位置、大小、圓形
    pub camera: String,
    pub camera_corner: u8,
    pub camera_size: u32,
    pub camera_circle: bool,
    /// 攝影機小視窗拖曳後記住的位置（擷取範圍內的相對位置，萬分比）；None = 放在 camera_corner
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_pos: Option<[u16; 2]>,
    /// 介面大小（%）
    pub ui_scale: u32,
    /// 介面語言："auto"（跟著 Windows）、"zh-TW"、"en"
    pub language: String,
    /// 錄影時游標光暈
    pub cursor_halo: bool,
    /// 錄影時隱藏桌面圖示
    pub hide_icons: bool,
    /// 錄影時打開 Windows 的勿擾
    pub rec_quiet: bool,
    /// 開會時（Teams、Zoom… 用麥克風）提醒錄影（core 的 tray 讀這個欄位）
    pub meeting_remind: bool,
    /// 自動匯入的資料夾（core::watch 讀這個欄位）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub watch_dirs: Vec<String>,
    /// 自動匯入後接著做錄完自動處理
    pub watch_after: bool,
    /// 下載外部元件時的 Proxy：auto（跟著系統）、off、custom（core::http 讀這兩個欄位）
    pub proxy_mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy: String,
    /// 錄影時打上馬賽克的固定範圍（桌面座標）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rec_masks: Vec<Rect>,
    /// 錄影時麥克風降噪
    pub mic_denoise: bool,
    /// 錄影時螢幕上的控制列：完整、精簡、不顯示（core 的 rec_bar_prefs 讀這個欄位）
    pub rec_bar: RecBarMode,
    /// 錄整個螢幕時控制列拖到的位置（控制列上拖曳後由 core 存進來；這裡只是原樣保留）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rec_bar_pos: Option<[i32; 2]>,
    /// 截圖後在右下角顯示小縮圖
    pub shot_preview: bool,
    /// 框選截圖放開滑鼠就直接存檔（不留下可以調整的範圍與工具列；core 的框選畫面讀這個欄位）
    pub snip_instant: bool,
    /// 截圖「另存新檔」上次存到的資料夾
    pub shot_save_dir: String,
    /// 看過哪一版的新功能介紹（例如 "3.1"）
    pub seen_version: String,
    /// 上次加到截圖或影片上的圖片（Logo）
    pub last_picture: String,
    /// 標註的「我的樣式」
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ann_styles: Vec<screenrecorder_core::annotate::AnnStyle>,
    /// 講稿小視窗：提詞的字級、捲動速度（1～10）、不透明度（%）、開始錄影時自動捲動、位置與大小（點）
    pub notes_font: u32,
    pub notes_speed: u32,
    pub notes_opacity: u32,
    pub notes_auto: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_rect: Option<[i32; 4]>,
    /// 錄完自動處理（core 的 after_record.rs 讀這些欄位）：剪掉沒動靜的片段、產生字幕（模型、語言）、壓縮到幾 MB 以內
    pub after_idle_cut: bool,
    pub after_subs: bool,
    pub after_subs_model: u32,
    pub after_subs_lang: u32,
    pub after_compress_mb: u32,
    /// 錄完依內容自動命名（只改預設的 Rec_時間 名稱）
    pub after_auto_name: bool,
    /// 錄完產生字幕時也翻成英文（同名.en.srt）
    pub after_subs_en: bool,
    /// 錄完處理後複製到這個資料夾（core::after_record 讀這個欄位）
    #[serde(skip_serializing_if = "String::is_empty")]
    pub after_copy_to: String,
    /// 只錄某個視窗時只錄視窗的內容（Windows Graphics Capture）
    pub window_capture: bool,
    /// 系統聲音與麥克風分開存（分軌）
    pub separate_audio: bool,
    /// 回溯錄影保留最近幾秒（core 的 replay.rs 也讀這個欄位：快速鍵、系統匣開始時用）
    pub replay_sec: u32,
    /// 主畫面展開事件紀錄（預設收起來）
    pub show_log: bool,
    /// 之前用過的儲存位置（全部錄影的資料夾選單；最近用的在前，最多 RECENT_DIRS 個）
    pub recent_dirs: Vec<String>,
    /// 截圖的儲存位置（空白 = 預設的截圖資料夾，見 EnvInfo::default_shot_dir）
    pub shot_dir: String,
    /// 選了「留在原處」的舊截圖所在的資料夾：全部截圖也列出來
    pub shot_extra_dirs: Vec<String>,
    /// 已經問過要不要把錄影資料夾裡的截圖搬到截圖資料夾（截圖改成分開存之後，第一次啟動時問一次）
    pub shot_move_asked: bool,
    /// 全部截圖的縮圖大小：0 小、1 中、2 大
    pub shot_grid_size: u8,
    /// 只錄聲音時另存一份 M4A（core 的 after_record.rs 讀這個欄位）
    pub audio_m4a: bool,
    /// 外觀："system"（跟著 Windows）、"light"、"dark"
    pub theme: String,
    /// 已經說明過「關掉視窗後仍在系統匣執行」（只在第一次關掉時通知）
    pub hide_notice_shown: bool,
    /// 清單（最近錄影、全部錄影、全部截圖）也列出子資料夾裡的（core 的 actions::list_subdirs 讀這個欄位）
    pub list_subdirs: bool,
    /// 第一次錄影的教學看到第幾步（1 起算；0 = 沒有看到一半的教學）
    pub tour_step: u32,
    /// 看完或跳過的教學："first"（第一次錄影）、"editor"、"shot"、"library"
    pub tours_done: Vec<String>,
    /// 剪輯視窗、截圖編輯、全部截圖第一次打開時顯示短提示（新安裝或按了「重新看教學」才開）
    pub tips: bool,
    /// 上次切換或儲存的設定組合（目前的設定和它不一樣時顯示「已修改」）
    pub preset_name: String,
    /// 等待中的排程錄影（core 的 schedule.rs 存進來；這裡只是原樣保留，並跟著 core 的狀態更新）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Value>,
}

/// 記住幾個用過的儲存位置
pub const RECENT_DIRS: usize = 5;

/// 講稿提詞的字級範圍、不透明度下限
pub const NOTES_FONT: (u32, u32) = (16, 72);
pub const NOTES_OPACITY_MIN: u32 = 40;

/// 介面大小可選的百分比
pub const UI_SCALES: [u32; 4] = [100, 110, 125, 150];
/// 主畫面至少要這麼大（點）才放得下：螢幕放大後比這個小的介面大小不能選
pub const UI_MIN_SIZE: (f32, f32) = (1000.0, 600.0);

/// 自訂範圍跟著的視窗
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowWindow {
    pub id: i64,
    pub title: String,
}

pub const FPS_CHOICES: [f64; 8] = [10.0, 15.0, 20.0, 24.0, 25.0, 30.0, 50.0, 60.0];
pub const MAX_PRESETS: [f64; 4] = [0.0, 30.0, 60.0, 120.0];
/// 加速版的大小上限（MB）；0 = 不限
pub const MAX_MB: [u32; 5] = [0, 10, 25, 50, 100];

fn primary(env: &EnvInfo) -> Option<&MonitorInfo> {
    env.monitors.iter().find(|m| m.primary).or_else(|| env.monitors.first())
}

impl UiSettings {
    /// 從存檔讀取（欄位缺少或不合理時用預設值）
    pub fn from_saved(v: Option<&Value>, env: &EnvInfo) -> UiSettings {
        Self::from_saved_noted(v, env).0
    }

    /// 同上，也回傳因為螢幕改變而調整的說明（啟動時提示使用者）
    pub fn from_saved_noted(v: Option<&Value>, env: &EnvInfo) -> (UiSettings, Vec<String>) {
        let empty = serde_json::Map::new();
        let o = v.and_then(Value::as_object).unwrap_or(&empty);
        let num = |k: &str| o.get(k).and_then(Value::as_f64);
        let boolean = |k: &str| o.get(k).and_then(Value::as_bool);
        let string = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
        let p = primary(env);
        let region = o
            .get("region")
            .and_then(|r| serde_json::from_value::<Rect>(r.clone()).ok())
            .unwrap_or_else(|| p.map(|m| Rect { x: m.x, y: m.y, width: m.width / 2, height: m.height / 2 }).unwrap_or(Rect { x: 0, y: 0, width: 1280, height: 720 }));
        let mut s = UiSettings {
            source_type: match o.get("sourceType").and_then(Value::as_str) {
                Some("region") => SourceType::Region,
                Some("all") => SourceType::All,
                Some("audio") => SourceType::Audio,
                _ => SourceType::Monitor,
            },
            monitor_id: string("monitorId"),
            monitor_key: string("monitorKey").unwrap_or_default(),
            monitor_region: o.get("monitorRegion").and_then(|r| serde_json::from_value::<Rect>(r.clone()).ok()),
            region,
            fps: num("fps").filter(|f| f.fract() == 0.0 && *f >= 1.0 && *f <= 60.0).unwrap_or(30.0),
            scale: num("scale").map(|v| v as u32).filter(|v| [100, 75, 50, 25].contains(v)).unwrap_or(100),
            draw_mouse: boolean("drawMouse").unwrap_or(true),
            max_minutes: num("maxMinutes").filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0),
            method: o.get("method").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
            output_dir: string("outputDir").map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).unwrap_or_else(|| env.default_output_dir.clone()),
            speed: num("speed").filter(|v| *v >= 1.0).unwrap_or(4.0),
            // 預設錄系統聲音與麥克風（之前改過的設定照舊）
            audio_system: boolean("audioSystem").unwrap_or(true),
            audio_mic: boolean("audioMic").unwrap_or(true),
            mic_id: string("micId").unwrap_or_default(),
            audio_app: string("audioApp").unwrap_or_default(),
            keep_audio: boolean("keepAudio").unwrap_or(true),
            live_preview: boolean("livePreview").unwrap_or(true),
            encoder: o.get("encoder").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
            countdown_sec: num("countdownSec").map(|v| v as u32).filter(|v| [0, 3, 5, 10].contains(v)).unwrap_or(3),
            hide_ui: boolean("hideUi").unwrap_or(true),
            export_format: match o.get("exportFormat").and_then(Value::as_str) {
                Some("gif") => ExportFormat::Gif,
                Some("webp") => ExportFormat::Webp,
                _ => ExportFormat::Mp4,
            },
            export_mode: if o.get("exportMode").and_then(Value::as_str) == Some("target") { ExportMode::Target } else { ExportMode::Speed },
            export_target: string("exportTarget").unwrap_or_else(|| "1:00".into()),
            gif_width: num("gifWidth").map(|v| v as u32).filter(|v| [320, 480, 640, 960, 1280].contains(v)).unwrap_or(640),
            gif_fps: num("gifFps").map(|v| v as u32).filter(|v| [5, 10, 15, 20].contains(v)).unwrap_or(10),
            mp4_width: num("mp4Width").map(|v| v as u32).filter(|v| MP4_WIDTHS.contains(v)).unwrap_or(0),
            edit_after_shot: boolean("editAfterShot").unwrap_or(false),
            mp4_max_mb: num("mp4MaxMb").map(|v| v as u32).filter(|v| MAX_MB.contains(v)).unwrap_or(0),
            show_clicks: boolean("showClicks").unwrap_or(false),
            show_keys: boolean("showKeys").unwrap_or(false),
            follow_window: None,
            camera: string("camera").unwrap_or_default(),
            camera_corner: num("cameraCorner").map(|v| v as u8).filter(|v| *v <= 3).unwrap_or(0),
            // 小 / 中 / 大，或在小視窗上拖曳、滾輪調出來的大小
            camera_size: num("cameraSize").map(|v| v as u32).filter(|v| (screenrecorder_core::camera_bubble::SIZE_MIN..=screenrecorder_core::camera_bubble::SIZE_MAX).contains(v)).unwrap_or(20),
            camera_circle: boolean("cameraCircle").unwrap_or(true),
            camera_pos: o.get("cameraPos").and_then(|v| serde_json::from_value::<[u16; 2]>(v.clone()).ok()).filter(|p| p.iter().all(|c| *c <= 10000)),
            cursor_halo: boolean("cursorHalo").unwrap_or(false),
            hide_icons: boolean("hideIcons").unwrap_or(false),
            rec_quiet: boolean("recQuiet").unwrap_or(false),
            meeting_remind: boolean("meetingRemind").unwrap_or(true),
            watch_dirs: o
                .get("watchDirs")
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .take(10)
                .collect(),
            watch_after: boolean("watchAfter").unwrap_or(false),
            proxy_mode: o.get("proxyMode").and_then(|v| v.as_str()).filter(|m| ["auto", "off", "custom"].contains(m)).unwrap_or("auto").to_string(),
            proxy: o.get("proxy").and_then(|v| v.as_str()).map(|v| v.trim().chars().take(200).collect()).unwrap_or_default(),
            rec_masks: o.get("recMasks").and_then(|v| serde_json::from_value::<Vec<Rect>>(v.clone()).ok()).unwrap_or_default().into_iter().filter(|r| r.width >= 4 && r.height >= 4).take(20).collect(),
            mic_denoise: boolean("micDenoise").unwrap_or(false),
            rec_bar: o.get("recBar").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
            rec_bar_pos: o.get("recBarPos").and_then(|v| serde_json::from_value::<[i32; 2]>(v.clone()).ok()),
            shot_preview: boolean("shotPreview").unwrap_or(true),
            snip_instant: boolean("snipInstant").unwrap_or(false),
            shot_save_dir: string("shotSaveDir").unwrap_or_default(),
            seen_version: string("seenVersion").unwrap_or_default(),
            last_picture: string("lastPicture").unwrap_or_default(),
            ann_styles: o
                .get("annStyles")
                .and_then(|v| serde_json::from_value::<Vec<screenrecorder_core::annotate::AnnStyle>>(v.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.size.is_finite() && s.size >= 1.0 && s.color.starts_with('#'))
                .take(screenrecorder_core::annotate::MAX_STYLES)
                .collect(),
            ui_scale: num("uiScale").map(|v| v as u32).filter(|v| UI_SCALES.contains(v)).unwrap_or(100),
            language: string("language").filter(|v| ["zh-TW", "en"].contains(&v.as_str())).unwrap_or_else(|| "auto".into()),
            notes_font: num("notesFont").map(|v| v as u32).filter(|v| (NOTES_FONT.0..=NOTES_FONT.1).contains(v)).unwrap_or(30),
            notes_speed: num("notesSpeed").map(|v| v as u32).filter(|v| (1..=10).contains(v)).unwrap_or(3),
            notes_opacity: num("notesOpacity").map(|v| v as u32).filter(|v| (NOTES_OPACITY_MIN..=100).contains(v)).unwrap_or(90),
            notes_auto: boolean("notesAuto").unwrap_or(true),
            notes_rect: o.get("notesRect").and_then(|v| serde_json::from_value::<[i32; 4]>(v.clone()).ok()).filter(|r| r[2] >= 200 && r[3] >= 120),
            after_idle_cut: boolean("afterIdleCut").unwrap_or(false),
            after_subs: boolean("afterSubs").unwrap_or(false),
            after_subs_model: num("afterSubsModel").map(|v| v as u32).filter(|v| (*v as usize) < screenrecorder_core::subtitles::MODELS.len()).unwrap_or(0),
            after_subs_lang: num("afterSubsLang").map(|v| v as u32).filter(|v| (*v as usize) < screenrecorder_core::subtitles::LANGUAGES.len()).unwrap_or(0),
            after_auto_name: boolean("afterAutoName").unwrap_or(false),
            after_subs_en: boolean("afterSubsEn").unwrap_or(false),
            after_copy_to: o.get(screenrecorder_core::after_record::COPY_KEY).and_then(Value::as_str).unwrap_or("").trim().to_string(),
            after_compress_mb: num("afterCompressMb").map(|v| v as u32).filter(|v| MAX_MB.contains(v)).unwrap_or(0),
            window_capture: boolean("windowCapture").unwrap_or(false),
            separate_audio: boolean("separateAudio").unwrap_or(false),
            replay_sec: screenrecorder_core::replay::sec_from_ui(v),
            show_log: boolean("showLog").unwrap_or(false),
            recent_dirs: o
                .get("recentDirs")
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .take(RECENT_DIRS)
                .collect(),
            shot_dir: string("shotDir").map(|d| d.trim().to_string()).unwrap_or_default(),
            shot_extra_dirs: o
                .get("shotExtraDirs")
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .collect(),
            shot_move_asked: boolean("shotMoveAsked").unwrap_or(false),
            shot_grid_size: num("shotGridSize").map(|v| v as u8).filter(|v| *v <= 2).unwrap_or(1),
            audio_m4a: screenrecorder_core::after_record::audio_m4a(v),
            theme: string("theme").filter(|v| ["light", "dark"].contains(&v.as_str())).unwrap_or_else(|| "system".into()),
            hide_notice_shown: boolean("hideNoticeShown").unwrap_or(false),
            list_subdirs: boolean("listSubdirs").unwrap_or(true),
            tour_step: num("tourStep").map(|v| v as u32).filter(|v| *v <= 50).unwrap_or(0),
            tours_done: o.get("toursDone").and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok()).unwrap_or_default(),
            tips: boolean("tips").unwrap_or(false),
            preset_name: string("presetName").unwrap_or_default(),
            schedule: o.get(screenrecorder_core::schedule::UI_KEY).cloned(),
        };
        let notes = s.fix_monitor(env);
        (s, notes)
    }

    /// 螢幕改變後（接上、拔掉外接螢幕、改解析度、改排列）調整設定，回傳給使用者看的說明：
    /// - 依實體螢幕找回選的那台（編號變了也一樣）；那台沒接上時改用主螢幕
    /// - 單一螢幕裡框選的範圍不在螢幕內（解析度變了）：改錄整個螢幕
    /// - 自訂範圍不在任何螢幕上：移到主螢幕；有一部分超出桌面：移回桌面內
    ///
    /// 還沒偵測到螢幕時不動（啟動時偵測完成前）
    pub fn fix_monitor(&mut self, env: &EnvInfo) -> Vec<String> {
        use screenrecorder_core::{tr, trf};
        let mut notes = vec![];
        let Some(p) = primary(env) else { return notes };
        // 1. 選的螢幕：先依實體螢幕找
        let by_key = (!self.monitor_key.is_empty()).then(|| env.monitors.iter().find(|m| m.key.eq_ignore_ascii_case(&self.monitor_key))).flatten();
        match by_key {
            Some(m) => self.monitor_id = Some(m.id.clone()),
            // 記得的那台沒接上（這次拿得到識別才能確定）
            None if !self.monitor_key.is_empty() && env.monitors.iter().any(|m| !m.key.is_empty()) => {
                if self.source_type == SourceType::Monitor {
                    notes.push(trf!("選的螢幕沒有接上，改錄{}", "The selected screen isn't connected; recording {} instead", p.label()));
                }
                self.monitor_id = Some(p.id.clone());
                self.monitor_region = None;
            }
            None if !env.monitors.iter().any(|m| Some(&m.id) == self.monitor_id.as_ref()) => {
                if self.monitor_id.is_some() && self.source_type == SourceType::Monitor {
                    notes.push(trf!("選的螢幕不見了，改錄{}", "The selected screen is gone; recording {} instead", p.label()));
                }
                self.monitor_id = Some(p.id.clone());
            }
            None => {}
        }
        if let Some(m) = self.selected_monitor(env) {
            self.monitor_key = m.key.clone();
        }
        // 2. 單一螢幕裡框選的範圍
        if self.monitor_region.is_some() && self.region_in_monitor(env).is_none() {
            if self.source_type == SourceType::Monitor {
                notes.push(tr!("框選的範圍已經不在螢幕上，改錄整個螢幕", "The selected area is no longer on the screen; recording the whole screen").into());
            }
            self.monitor_region = None;
        }
        // 3. 自訂範圍
        let r = self.region;
        let on = |m: &MonitorInfo| r.x < m.x + m.width && r.x + r.width > m.x && r.y < m.y + m.height && r.y + r.height > m.y;
        let dk = env.desktop;
        if !env.monitors.iter().any(on) {
            let (w, h) = (r.width.min(p.width), r.height.min(p.height));
            self.region = Rect { x: p.x + (p.width - w) / 2, y: p.y + (p.height - h) / 2, width: w, height: h };
            if self.source_type == SourceType::Region {
                notes.push(trf!("自訂範圍不在任何螢幕上，已移到{}", "The custom area wasn't on any screen; moved it to {}", p.label()));
            }
        } else if dk.width > 0 && (r.x < dk.x || r.y < dk.y || r.x + r.width > dk.x + dk.width || r.y + r.height > dk.y + dk.height) {
            let (w, h) = (r.width.min(dk.width), r.height.min(dk.height));
            let x = r.x.clamp(dk.x, dk.x + dk.width - w);
            let y = r.y.clamp(dk.y, dk.y + dk.height - h);
            self.region = Rect { x, y, width: w, height: h };
            if self.source_type == SourceType::Region {
                notes.push(tr!("自訂範圍有一部分超出螢幕，已移回螢幕內", "Part of the custom area was off screen; moved it back").into());
            }
        }
        notes
    }

    /// 單一螢幕模式的範圍（要完全在選的螢幕內）
    pub fn region_in_monitor(&self, env: &EnvInfo) -> Option<Rect> {
        let (r, m) = (self.monitor_region?, self.selected_monitor(env)?);
        (r.width >= 16 && r.height >= 16 && r.x >= m.x && r.y >= m.y && r.x + r.width <= m.x + m.width && r.y + r.height <= m.y + m.height).then_some(r)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// 截圖存到哪裡
    pub fn shots_dir(&self, env: &EnvInfo) -> String {
        match self.shot_dir.trim() {
            "" => env.default_shot_dir.clone(),
            d => d.to_string(),
        }
    }

    /// 全部截圖除了截圖資料夾，也要列出的資料夾（留在原處的舊截圖）
    pub fn shot_extra(&self, env: &EnvInfo) -> Vec<String> {
        let main = self.shots_dir(env);
        self.shot_extra_dirs.iter().filter(|d| !d.eq_ignore_ascii_case(&main)).cloned().collect()
    }

    pub fn out_dir(&self, env: &EnvInfo) -> String {
        let d = self.output_dir.trim();
        if d.is_empty() {
            env.default_output_dir.clone()
        } else {
            d.to_string()
        }
    }

    /// 錄影範圍的簡短說明：「螢幕 1」「所有螢幕」「範圍 1280×720」「只錄聲音」
    pub fn source_label(&self, env: &EnvInfo) -> String {
        use screenrecorder_core::{tr, trf};
        match self.source_type {
            SourceType::Monitor => match self.region_in_monitor(env) {
                Some(r) => trf!("範圍 {}×{}", "Area {}×{}", r.width, r.height),
                None => trf!("螢幕 {}", "Screen {}", self.selected_monitor(env).map(|m| m.display_number).unwrap_or(1)),
            },
            SourceType::All => tr!("所有螢幕", "All screens").to_string(),
            SourceType::Region => trf!("範圍 {}×{}", "Area {}×{}", self.region.width, self.region.height),
            SourceType::Audio => tr!("只錄聲音", "Audio only").to_string(),
        }
    }

    pub fn selected_monitor<'a>(&self, env: &'a EnvInfo) -> Option<&'a MonitorInfo> {
        env.monitors.iter().find(|m| Some(&m.id) == self.monitor_id.as_ref())
    }

    /// 錄影範圍（桌面座標）
    pub fn source_rect(&self, env: &EnvInfo) -> Option<Rect> {
        match self.source_type {
            SourceType::Monitor => self.region_in_monitor(env).or_else(|| self.selected_monitor(env).map(|m| Rect { x: m.x, y: m.y, width: m.width, height: m.height })),
            SourceType::All => (env.desktop.width > 0).then_some(env.desktop),
            SourceType::Region => Some(self.region),
            SourceType::Audio => None,
        }
    }

    pub fn record_config(&self, env: &EnvInfo) -> RecordConfig {
        RecordConfig {
            source: match self.source_type {
                SourceType::Monitor => match self.region_in_monitor(env) {
                    Some(r) => SourceConfig::Region { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 },
                    None => SourceConfig::Monitor { monitor_id: self.monitor_id.clone().unwrap_or_default() },
                },
                // 只錄聲音時，截圖仍截整個桌面
                SourceType::All | SourceType::Audio => SourceConfig::All,
                SourceType::Region => SourceConfig::Region { x: self.region.x as f64, y: self.region.y as f64, width: self.region.width as f64, height: self.region.height as f64 },
            },
            fps: self.fps,
            scale: self.scale as f64,
            draw_mouse: self.draw_mouse,
            max_minutes: self.max_minutes,
            method: self.method,
            output_dir: self.out_dir(env),
            shot_dir: self.shot_dir.trim().to_string(),
            audio: AudioConfig { system: self.audio_system, mic: self.audio_mic, mic_id: self.mic_id.clone(), app: self.audio_app.clone() },
            encoder: Some(self.encoder),
            countdown_sec: Some(self.countdown_sec as f64),
            hide_ui: Some(self.hide_ui),
            show_clicks: self.show_clicks,
            show_keys: self.show_keys,
            cursor_halo: self.cursor_halo,
            hide_icons: self.hide_icons,
            follow_window: if self.source_type == SourceType::Region { self.follow_window.as_ref().map(|w| w.id) } else { None },
            camera: (!self.camera.is_empty()).then(|| CameraConfig {
                device: self.camera.clone(),
                corner: self.camera_corner,
                size: self.camera_size,
                circle: self.camera_circle,
                pos: self.camera_pos,
            }),
            audio_only: self.source_type == SourceType::Audio,
            window_capture: self.window_capture && self.source_type == SourceType::Region && self.follow_window.is_some(),
            separate_audio: self.separate_audio,
            mic_denoise: self.mic_denoise,
            quiet: self.rec_quiet,
            masks: self.rec_masks.clone(),
            replay_sec: 0,
            monitor_key: (self.source_type == SourceType::Monitor && self.region_in_monitor(env).is_none() && !self.monitor_key.is_empty()).then(|| self.monitor_key.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> EnvInfo {
        EnvInfo {
            default_output_dir: "C:\\Videos".into(),
            monitors: vec![MonitorInfo {
                id: "0:0".into(),
                adapter: 0,
                output: 0,
                adapter_name: "GPU".into(),
                device_name: "\\\\.\\DISPLAY1".into(),
                display_number: 1,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                primary: true,
                rotation: 1,
                key: "K1".into(),
                name: String::new(),
            }],
            desktop: Rect { x: 0, y: 0, width: 1920, height: 1080 },
            ..Default::default()
        }
    }

    #[test]
    fn reads_settings_saved_by_version_2() {
        let saved = serde_json::json!({
            "sourceType": "region", "monitorId": "9:9", "region": { "x": 10, "y": 20, "width": 300, "height": 200 },
            "fps": 24, "scale": 50, "drawMouse": false, "maxMinutes": 30, "method": "gdigrab", "outputDir": " D:\\Rec ",
            "speed": 8, "audioSystem": false, "audioMic": true, "micId": "mic-1", "keepAudio": false, "livePreview": false,
            "encoder": "gpu", "countdownSec": 5, "hideUi": false, "exportFormat": "gif", "exportMode": "target", "exportTarget": "0:30",
            "gifWidth": 480, "gifFps": 15, "mp4Width": 1280, "unknown": 1
        });
        let s = UiSettings::from_saved(Some(&saved), &env());
        assert_eq!(s.source_type, SourceType::Region);
        assert_eq!(s.monitor_id.as_deref(), Some("0:0")); // 不存在的螢幕改用主螢幕
        assert_eq!(s.region, Rect { x: 10, y: 20, width: 300, height: 200 });
        assert_eq!((s.fps, s.scale, s.draw_mouse, s.max_minutes), (24.0, 50, false, 30.0));
        assert_eq!(s.method, MethodPreference::Gdigrab);
        assert_eq!(s.output_dir, "D:\\Rec");
        assert_eq!((s.audio_system, s.audio_mic, s.mic_id.as_str()), (false, true, "mic-1"));
        assert_eq!(s.encoder, EncoderPreference::Gpu);
        assert_eq!((s.countdown_sec, s.hide_ui), (5, false));
        assert_eq!((s.export_format, s.export_mode, s.export_target.as_str()), (ExportFormat::Gif, ExportMode::Target, "0:30"));
        assert_eq!((s.gif_width, s.gif_fps, s.mp4_width), (480, 15, 1280));
        // 存回去再讀：相同
        assert_eq!(UiSettings::from_saved(Some(&s.to_value()), &env()), s);
    }

    #[test]
    fn after_record_settings_are_read_by_core() {
        let mut s = UiSettings::from_saved(None, &env());
        assert!(!screenrecorder_core::after_record::AfterRecord::from_ui(Some(&s.to_value())).any());
        (s.after_idle_cut, s.after_subs, s.after_subs_lang, s.after_compress_mb, s.after_auto_name) = (true, true, 1, 25, true);
        s.after_copy_to = "D:\\Sync".into();
        let a = screenrecorder_core::after_record::AfterRecord::from_ui(Some(&s.to_value()));
        assert_eq!((a.idle_cut, a.subtitles, a.subs_lang, a.compress_mb, a.auto_name), (true, true, 1, 25, true));
        assert_eq!(a.copy_to, "D:\\Sync");
        // 設定組合也存這些欄位
        for k in screenrecorder_core::after_record::UI_KEYS {
            assert!(screenrecorder_core::presets::KEYS.contains(&k), "{k}");
            assert!(s.to_value().get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn defaults_and_config() {
        let s = UiSettings::from_saved(None, &env());
        assert_eq!(s.source_type, SourceType::Monitor);
        assert!(s.audio_system && s.audio_mic && s.live_preview && s.hide_ui);
        assert_eq!(s.region, Rect { x: 0, y: 0, width: 960, height: 540 });
        assert_eq!(s.output_dir, "C:\\Videos");
        let c = s.record_config(&env());
        assert_eq!(c.source, SourceConfig::Monitor { monitor_id: "0:0".into() });
        assert_eq!(c.countdown_sec, Some(3.0));
        // 系統匣讀得懂
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(serde_json::from_value::<RecordConfig>(v).unwrap(), c);
        // 不合理的值用預設
        let bad = serde_json::json!({ "fps": 7.5, "scale": 33, "countdownSec": 4, "gifWidth": 1 });
        let s = UiSettings::from_saved(Some(&bad), &env());
        assert_eq!((s.fps, s.scale, s.countdown_sec, s.gif_width), (30.0, 100, 3, 640));
    }

    #[test]
    fn system_audio_from_one_app() {
        let mut s = UiSettings::from_saved(None, &env());
        assert_eq!(s.record_config(&env()).audio.app, "");
        // 沒有指定程式時，錄影設定裡不寫這個欄位（舊版讀得懂）
        assert!(serde_json::to_value(s.record_config(&env())).unwrap()["audio"].get("app").is_none());
        s.audio_app = "chrome.exe".into();
        let c = s.record_config(&env());
        assert_eq!(c.audio.app, "chrome.exe");
        assert_eq!(serde_json::from_value::<RecordConfig>(serde_json::to_value(&c).unwrap()).unwrap(), c);
        // 存回去再讀：保留；設定組合也存
        assert_eq!(UiSettings::from_saved(Some(&s.to_value()), &env()).audio_app, "chrome.exe");
        assert!(screenrecorder_core::presets::KEYS.contains(&"audioApp"));
    }

    #[test]
    fn part_of_a_single_monitor() {
        let mut s = UiSettings::from_saved(None, &env());
        s.monitor_region = Some(Rect { x: 100, y: 50, width: 800, height: 600 });
        assert_eq!(s.record_config(&env()).source, SourceConfig::Region { x: 100.0, y: 50.0, width: 800.0, height: 600.0 });
        assert_eq!(s.source_rect(&env()), Some(Rect { x: 100, y: 50, width: 800, height: 600 }));
        // 存回去再讀：保留
        assert_eq!(UiSettings::from_saved(Some(&s.to_value()), &env()).monitor_region, s.monitor_region);
        // 超出螢幕（解析度變小了）：錄整個螢幕
        s.monitor_region = Some(Rect { x: 1500, y: 0, width: 800, height: 600 });
        assert_eq!(s.record_config(&env()).source, SourceConfig::Monitor { monitor_id: "0:0".into() });
        assert_eq!(UiSettings::from_saved(Some(&s.to_value()), &env()).monitor_region, None);
    }

    /// 筆電（K1）＋外接螢幕（K2）；swap：拔掉筆電螢幕後外接螢幕變成 0:0
    fn two(swap: bool) -> EnvInfo {
        let m = |id: &str, key: &str, x: i32, primary: bool| MonitorInfo { id: id.into(), key: key.into(), x, width: 1920, height: 1080, primary, display_number: 1, ..Default::default() };
        let monitors = if swap { vec![m("0:0", "K2", 0, true)] } else { vec![m("0:0", "K1", 0, true), m("0:1", "K2", 1920, false)] };
        let w = monitors.len() as i32 * 1920;
        EnvInfo { default_output_dir: "C:\\Videos".into(), monitors, desktop: Rect { x: 0, y: 0, width: w, height: 1080 }, ..Default::default() }
    }

    #[test]
    fn remembers_the_physical_screen() {
        let mut s = UiSettings::from_saved(None, &two(false));
        s.source_type = SourceType::Monitor;
        // 舊版的設定只有編號：依編號記住是哪一台
        s.monitor_id = Some("0:1".into());
        s.monitor_key.clear();
        assert!(s.fix_monitor(&two(false)).is_empty());
        assert_eq!(s.monitor_key, "K2");
        assert_eq!(s.record_config(&two(false)).monitor_key.as_deref(), Some("K2"));
        // 編號變了（拔掉筆電螢幕）：還是同一台，不用說明
        assert!(s.fix_monitor(&two(true)).is_empty());
        assert_eq!(s.monitor_id.as_deref(), Some("0:0"));
        // 外接螢幕拔掉：改用主螢幕並說明
        let mut s = UiSettings { monitor_id: Some("0:1".into()), monitor_key: "K2".into(), ..s };
        let only_laptop = EnvInfo { monitors: vec![two(false).monitors[0].clone()], desktop: Rect { x: 0, y: 0, width: 1920, height: 1080 }, ..two(false) };
        assert_eq!(s.fix_monitor(&only_laptop).len(), 1);
        assert_eq!((s.monitor_id.as_deref(), s.monitor_key.as_str()), (Some("0:0"), "K1"));
        // 還沒偵測到螢幕：不動
        let mut s2 = UiSettings { monitor_id: Some("0:1".into()), monitor_key: "K2".into(), ..s.clone() };
        assert!(s2.fix_monitor(&EnvInfo::default()).is_empty());
        assert_eq!(s2.monitor_id.as_deref(), Some("0:1"));
    }

    #[test]
    fn custom_area_moves_back_on_screen() {
        let mut s = UiSettings::from_saved(None, &two(false));
        s.source_type = SourceType::Region;
        // 在拔掉的外接螢幕上：移到主螢幕中間
        s.region = Rect { x: 2400, y: 100, width: 800, height: 600 };
        let only_laptop = EnvInfo { monitors: vec![two(false).monitors[0].clone()], desktop: Rect { x: 0, y: 0, width: 1920, height: 1080 }, ..two(false) };
        assert_eq!(s.fix_monitor(&only_laptop).len(), 1);
        assert_eq!(s.region, Rect { x: 560, y: 240, width: 800, height: 600 });
        // 有一部分超出桌面：移回桌面內
        s.region = Rect { x: 1500, y: 100, width: 800, height: 600 };
        assert_eq!(s.fix_monitor(&only_laptop).len(), 1);
        assert_eq!(s.region, Rect { x: 1120, y: 100, width: 800, height: 600 });
        // 在螢幕內：不動
        assert!(s.fix_monitor(&only_laptop).is_empty());
    }
}
