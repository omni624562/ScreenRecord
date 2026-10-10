//! 設定存檔：%LOCALAPPDATA%\ScreenRecorder\settings.json（與 1.x 版的檔案格式相同，升級後沿用原本的設定）
//! - ui：操作視窗的完整設定
//! - config：最後一次的錄影設定，系統匣選單「開始錄影」直接使用
//!
//! rev 每次變更 +1（不存檔），操作視窗看到 rev 變了（例如從系統匣切換錄音）就重新讀取。
//! ui 與 config 以原始 JSON 保存：不認得的欄位也會原樣保留。

use crate::types::RecordConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// 「自動」編碼曾偵測到 CPU 跟不上：之後的錄影改用 GPU
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_gpu: Option<bool>,
    /// 自動檢查新版本（未指定視為開啟）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
    /// 已用系統匣通知過的新版本（同一版只通知一次）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_notified: Option<String>,
    /// 自訂的全域快速鍵（未指定時用預設的 Ctrl+Alt+R / P / S / A）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkeys: Option<crate::types::Hotkeys>,
    #[serde(default)]
    pub rev: u64,
}

impl SavedSettings {
    /// 最後一次的錄影設定（格式不對時視為沒有）
    pub fn record_config(&self) -> Option<RecordConfig> {
        self.config.clone().and_then(|v| serde_json::from_value(v).ok())
    }
}

/// 要修改的欄位（None = 不變）
#[derive(Debug, Clone, Default)]
pub struct SettingsPatch {
    pub ui: Option<Value>,
    pub config: Option<Value>,
    pub prefer_gpu: Option<bool>,
    pub check_updates: Option<bool>,
    pub update_notified: Option<String>,
    pub hotkeys: Option<crate::types::Hotkeys>,
}

pub struct SettingsStore {
    file: PathBuf,
    cache: Mutex<Option<SavedSettings>>,
}

impl SettingsStore {
    pub fn new(file: PathBuf) -> Self {
        Self { file, cache: Mutex::new(None) }
    }

    pub fn load(&self) -> SavedSettings {
        let mut c = self.cache.lock().unwrap();
        if let Some(s) = c.as_ref() {
            return s.clone();
        }
        let mut s: SavedSettings = std::fs::read_to_string(&self.file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        s.rev = 1;
        *c = Some(s.clone());
        s
    }

    /// 設定的版本（每次儲存加一）：介面每 0.25 秒問一次，不用複製整份設定
    pub fn rev(&self) -> u64 {
        if let Some(s) = self.cache.lock().unwrap().as_ref() {
            return s.rev;
        }
        self.load().rev
    }

    pub fn save(&self, patch: SettingsPatch) -> SavedSettings {
        let cur = self.load();
        let mut next = SavedSettings {
            ui: patch.ui.or(cur.ui),
            config: patch.config.or(cur.config),
            prefer_gpu: patch.prefer_gpu.or(cur.prefer_gpu),
            check_updates: patch.check_updates.or(cur.check_updates),
            update_notified: patch.update_notified.or(cur.update_notified),
            hotkeys: patch.hotkeys.or(cur.hotkeys),
            rev: cur.rev + 1,
        };
        *self.cache.lock().unwrap() = Some(next.clone());
        let rev = next.rev;
        next.rev = 0;
        // 存檔不含 rev
        let mut v = serde_json::to_value(&next).unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            m.remove("rev");
        }
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.file, serde_json::to_string_pretty(&v).unwrap_or_default()) {
            crate::error!("無法儲存設定：{e}");
        }
        next.rev = rev;
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_keeps_unknown_fields_and_bumps_rev() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"ui":{"fps":30,"future":"x"},"preferGpu":true}"#).unwrap();
        let s = SettingsStore::new(file.clone());
        let a = s.load();
        assert_eq!(a.rev, 1);
        assert_eq!(a.prefer_gpu, Some(true));
        let b = s.save(SettingsPatch { check_updates: Some(false), ..Default::default() });
        assert_eq!(b.rev, 2);
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["ui"]["future"], json!("x"));
        assert_eq!(saved["checkUpdates"], json!(false));
        assert!(saved.get("rev").is_none());
        // 重新開啟：rev 從 1 開始
        assert_eq!(SettingsStore::new(file).load().rev, 1);
    }

    #[test]
    fn missing_or_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        assert_eq!(SettingsStore::new(file.clone()).load(), SavedSettings { rev: 1, ..Default::default() });
        std::fs::write(&file, "{broken").unwrap();
        assert_eq!(SettingsStore::new(file).load().rev, 1);
    }

    #[test]
    fn record_config_parses_ui_json() {
        let s = SavedSettings {
            config: Some(
                json!({"source":{"type":"monitor","monitorId":"0:0"},"fps":30,"scale":100,"drawMouse":true,"maxMinutes":0,"method":"auto","outputDir":"C:\\v","audio":{"system":true,"mic":false,"micId":""},"countdownSec":3,"hideUi":true}),
            ),
            ..Default::default()
        };
        let c = s.record_config().unwrap();
        assert!(c.audio.system);
        assert_eq!(c.countdown_sec, Some(3.0));
    }

    #[test]
    fn custom_hotkeys() {
        use crate::types::{Hotkey, Hotkeys};
        let d = Hotkeys::default();
        assert_eq!((0..5).map(|i| d.label(i)).collect::<Vec<_>>(), vec!["Ctrl+Alt+R", "Ctrl+Alt+P", "Ctrl+Alt+S", "Ctrl+Alt+A", "Ctrl+Alt+M"]);
        // 3.0 存的設定沒有「加標記」：用預設；停用的（null）維持停用
        let old: Hotkeys = serde_json::from_str(r#"{"record":null,"pause":null,"shot":null,"snip":null}"#).unwrap();
        assert_eq!(old.label(4), "Ctrl+Alt+M");
        let off: Hotkeys = serde_json::from_str(r#"{"record":null,"pause":null,"shot":null,"snip":null,"mark":null}"#).unwrap();
        assert_eq!(off.label(4), "");
        let f9 = Hotkey { ctrl: false, alt: false, shift: true, win: true, key: 0x78 };
        assert_eq!(f9.label(), "Shift+Win+F9");
        assert!(f9.valid());
        // 只有 Shift、不支援的按鍵：不能用
        assert!(!Hotkey { ctrl: false, alt: false, shift: true, win: false, key: 0x41 }.valid());
        assert!(!Hotkey::ctrl_alt(0xBA).valid());
        let mut k = d;
        k.set(3, None);
        k.set(1, Some(f9));
        assert_eq!(k.label(3), "");
        assert_eq!(k.conflict(2, &Hotkey::ctrl_alt(0x52)), Some(0));
        assert_eq!(k.conflict(0, &Hotkey::ctrl_alt(0x52)), None);
        // 存檔再讀：相同（停用的存成 null）
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        SettingsStore::new(file.clone()).save(SettingsPatch { hotkeys: Some(k), ..Default::default() });
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["hotkeys"]["snip"], Value::Null);
        assert_eq!(SettingsStore::new(file).load().hotkeys, Some(k));
    }
}
