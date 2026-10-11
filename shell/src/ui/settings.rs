//! 操作視窗的設定：存在 settings.json 的 ui 欄位（與 2.x 版的網頁介面相同的格式，升級後沿用）。
//! 同時把錄影設定存到 config 欄位，系統匣的「開始錄影」直接使用。

use screenrecorder_core::format::MP4_WIDTHS;
use screenrecorder_core::types::{AudioConfig, CameraConfig, EncoderPreference, EnvInfo, ExportFormat, MethodPreference, MonitorInfo, RecordConfig, Rect, SourceConfig};
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
    /// 截圖後在右下角顯示小縮圖
    pub shot_preview: bool,
    /// 看過哪一版的新功能介紹（例如 "3.1"）
    pub seen_version: String,
    /// 上次加到截圖或影片上的圖片（Logo）
    pub last_picture: String,
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
}

/// 講稿提詞的字級範圍、不透明度下限
pub const NOTES_FONT: (u32, u32) = (16, 72);
pub const NOTES_OPACITY_MIN: u32 = 40;

/// 介面大小可選的百分比
pub const UI_SCALES: [u32; 4] = [100, 110, 125, 150];

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
            shot_preview: boolean("shotPreview").unwrap_or(true),
            seen_version: string("seenVersion").unwrap_or_default(),
            last_picture: string("lastPicture").unwrap_or_default(),
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
            after_compress_mb: num("afterCompressMb").map(|v| v as u32).filter(|v| MAX_MB.contains(v)).unwrap_or(0),
        };
        s.fix_monitor(env);
        s
    }

    /// 選的螢幕不存在（換了螢幕）：改用主螢幕；範圍不在螢幕內（解析度變了）就錄整個螢幕
    pub fn fix_monitor(&mut self, env: &EnvInfo) {
        if !env.monitors.iter().any(|m| Some(&m.id) == self.monitor_id.as_ref()) {
            self.monitor_id = primary(env).map(|m| m.id.clone());
        }
        if self.monitor_region.is_some() && self.region_in_monitor(env).is_none() {
            self.monitor_region = None;
        }
    }

    /// 單一螢幕模式的範圍（要完全在選的螢幕內）
    pub fn region_in_monitor(&self, env: &EnvInfo) -> Option<Rect> {
        let (r, m) = (self.monitor_region?, self.selected_monitor(env)?);
        (r.width >= 16 && r.height >= 16 && r.x >= m.x && r.y >= m.y && r.x + r.width <= m.x + m.width && r.y + r.height <= m.y + m.height).then_some(r)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
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
            audio: AudioConfig { system: self.audio_system, mic: self.audio_mic, mic_id: self.mic_id.clone() },
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
        (s.after_idle_cut, s.after_subs, s.after_subs_lang, s.after_compress_mb) = (true, true, 1, 25);
        let a = screenrecorder_core::after_record::AfterRecord::from_ui(Some(&s.to_value()));
        assert_eq!((a.idle_cut, a.subtitles, a.subs_lang, a.compress_mb), (true, true, 1, 25));
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
}
