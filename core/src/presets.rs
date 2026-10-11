//! 錄影設定組合：把「錄什麼、怎麼錄」存成有名字的組合（例如「教學影片」「線上會議」），
//! 在主畫面或系統匣一次切換。
//!
//! 存在 settings.json 的 presets：名稱、操作視窗設定（ui）裡與錄影有關的欄位、
//! 對應的錄影設定（config，系統匣「開始錄影」直接使用）。

use crate::types::RecordConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// ui 裡屬於設定組合的欄位：範圍、畫面、聲音、儲存位置、倒數、錄影時的效果、攝影機
pub const KEYS: [&str; 25] = [
    "sourceType",
    "monitorId",
    "monitorRegion",
    "region",
    "fps",
    "scale",
    "drawMouse",
    "maxMinutes",
    "method",
    "outputDir",
    "audioSystem",
    "audioMic",
    "micId",
    "encoder",
    "countdownSec",
    "hideUi",
    "showClicks",
    "showKeys",
    "cursorHalo",
    "hideIcons",
    "camera",
    "cameraCorner",
    "cameraSize",
    "cameraCircle",
    "cameraPos",
];

/// 最多幾組
pub const MAX: usize = 20;
/// 名稱最長幾個字
pub const NAME_MAX: usize = 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    /// ui 裡 KEYS 的值；沒有的欄位代表「沒有設定」（例如沒有框選螢幕的一部分），套用時也移除
    #[serde(default)]
    pub ui: Map<String, Value>,
    /// 錄影設定（RecordConfig）
    #[serde(default)]
    pub config: Value,
}

impl Preset {
    /// 把目前的設定存成組合
    pub fn capture(name: &str, ui: &Value, config: &RecordConfig) -> Preset {
        let ui = KEYS.iter().filter_map(|k| ui.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        Preset { name: name.to_string(), ui, config: serde_json::to_value(config).unwrap_or(Value::Null) }
    }

    /// 目前的設定就是這個組合
    pub fn matches(&self, ui: &Value) -> bool {
        KEYS.iter().all(|k| self.ui.get(*k) == ui.get(*k))
    }

    /// 套用到 ui（其他欄位不變）
    pub fn apply_to(&self, ui: &mut Value) {
        if !ui.is_object() {
            *ui = Value::Object(Map::new());
        }
        let Value::Object(m) = ui else { return };
        for k in KEYS {
            match self.ui.get(k) {
                Some(v) => {
                    m.insert(k.to_string(), v.clone());
                }
                None => {
                    m.remove(k);
                }
            }
        }
    }

    /// 錄影設定（格式不對時為 None）
    pub fn record_config(&self) -> Option<RecordConfig> {
        serde_json::from_value(self.config.clone()).ok()
    }
}

/// 目前的設定是哪一組（第一個符合的）
pub fn active(list: &[Preset], ui: &Value) -> Option<usize> {
    list.iter().position(|p| p.matches(ui))
}

/// 整理名稱：去掉頭尾空白與換行，太長的截掉；空的不行
pub fn clean_name(s: &str) -> Option<String> {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = s.chars().take(NAME_MAX).collect();
    (!s.is_empty()).then_some(s)
}

/// 同名（不分大小寫）的位置
pub fn find(list: &[Preset], name: &str) -> Option<usize> {
    list.iter().position(|p| p.name.to_lowercase() == name.to_lowercase())
}

/// 存入：同名的取代（位置不變），否則加在最後；超過上限時回傳 false
pub fn upsert(list: &mut Vec<Preset>, p: Preset) -> bool {
    match find(list, &p.name) {
        Some(i) => list[i] = p,
        None if list.len() >= MAX => return false,
        None => list.push(p),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioConfig, MethodPreference, SourceConfig};
    use serde_json::json;

    fn config(fps: f64) -> RecordConfig {
        RecordConfig {
            source: SourceConfig::Monitor { monitor_id: "0:0".into() },
            fps,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: MethodPreference::Auto,
            output_dir: "D:\\教學".into(),
            audio: AudioConfig { system: true, mic: true, mic_id: String::new() },
            encoder: None,
            countdown_sec: Some(3.0),
            hide_ui: None,
            show_clicks: true,
            show_keys: true,
            cursor_halo: false,
            hide_icons: false,
            follow_window: None,
            camera: None,
            audio_only: false,
        }
    }

    #[test]
    fn capture_match_and_apply() {
        let teach = json!({ "sourceType": "monitor", "monitorId": "0:0", "monitorRegion": { "x": 0, "y": 0, "width": 800, "height": 600 }, "fps": 30.0, "showClicks": true, "outputDir": "D:\\教學", "language": "en", "gifWidth": 480 });
        let p = Preset::capture("教學影片", &teach, &config(30.0));
        // 只存與錄影有關的欄位
        assert!(p.ui.contains_key("monitorRegion") && !p.ui.contains_key("language") && !p.ui.contains_key("gifWidth"));
        assert_eq!(p.record_config(), Some(config(30.0)));
        assert!(p.matches(&teach));
        // 介面語言、製作 GIF 的寬度不算
        let mut other = teach.clone();
        other["language"] = json!("zh-TW");
        other["gifWidth"] = json!(640);
        assert!(p.matches(&other));
        // 錄影設定不同就不是這一組
        let mut meeting = json!({ "sourceType": "region", "region": { "x": 10, "y": 10, "width": 1280, "height": 720 }, "fps": 15.0, "showClicks": false, "language": "en" });
        assert!(!p.matches(&meeting));
        assert_eq!(active(std::slice::from_ref(&p), &meeting), None);
        // 套用：組合裡的欄位蓋過去，組合裡沒有的（monitorRegion 以外沒存的 region）移除，其他欄位不動
        p.apply_to(&mut meeting);
        assert!(p.matches(&meeting));
        assert_eq!(meeting["language"], json!("en"));
        assert_eq!(meeting.get("region"), None);
        assert_eq!(meeting["monitorRegion"]["width"], json!(800));
        assert_eq!(active(std::slice::from_ref(&p), &meeting), Some(0));
        // 存檔再讀回來一樣
        let back: Preset = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        let mut empty = Value::Null;
        p.apply_to(&mut empty);
        assert!(p.matches(&empty));
    }

    #[test]
    fn names_and_list() {
        assert_eq!(clean_name("  線上\n會議  ").as_deref(), Some("線上 會議"));
        assert_eq!(clean_name(" \t "), None);
        assert_eq!(clean_name(&"長".repeat(50)).unwrap().chars().count(), NAME_MAX);
        let ui = json!({ "fps": 30.0 });
        let mut list = vec![];
        assert!(upsert(&mut list, Preset::capture("Demo", &ui, &config(30.0))));
        assert!(upsert(&mut list, Preset::capture("會議", &ui, &config(15.0))));
        // 同名（不分大小寫）取代、位置不變
        assert!(upsert(&mut list, Preset::capture("demo", &json!({ "fps": 60.0 }), &config(60.0))));
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].name.as_str(), list[0].ui["fps"].clone()), ("demo", json!(60.0)));
        assert_eq!(find(&list, "會議"), Some(1));
        for i in 0..MAX {
            upsert(&mut list, Preset::capture(&format!("p{i}"), &ui, &config(30.0)));
        }
        assert_eq!(list.len(), MAX);
        assert!(!upsert(&mut list, Preset::capture("再一個", &ui, &config(30.0))));
    }
}
