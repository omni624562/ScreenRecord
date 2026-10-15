//! 螢幕配置改變（接上、拔掉外接螢幕、改解析度、改排列）時，錄影要怎麼處理。
//! 比對的是「同一台實體螢幕」（MonitorInfo::same_screen），不是 DXGI 的順序：拔掉一台後其他螢幕的編號可能會變。

use crate::types::{MonitorInfo, RecordConfig, SourceConfig};

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutChange {
    /// 錄的螢幕都還在、位置大小都沒變（例如只是接上另一台）：照常錄
    Same,
    /// 錄的螢幕都還在、大小沒變，只是編號或位置變了：用這個設定重新對應後繼續錄
    Remap(Box<RecordConfig>),
    /// 錄的螢幕不見了（拔掉、關掉）：哪一台
    Gone(String),
    /// 錄的螢幕解析度變了：哪一台
    Resized(String),
}

/// plan_monitors：開始錄影時範圍涵蓋到的螢幕；now：現在的螢幕
pub fn check(config: &RecordConfig, plan_monitors: &[MonitorInfo], now: &[MonitorInfo]) -> LayoutChange {
    if config.audio_only || plan_monitors.is_empty() {
        return LayoutChange::Same;
    }
    let mut moved = false;
    let mut shift: Option<(i32, i32)> = None;
    for pm in plan_monitors {
        let Some(nm) = now.iter().find(|n| n.same_screen(pm)) else {
            return LayoutChange::Gone(pm.label());
        };
        if (nm.width, nm.height) != (pm.width, pm.height) {
            return LayoutChange::Resized(nm.label());
        }
        if nm.id != pm.id || nm.x != pm.x || nm.y != pm.y {
            moved = true;
        }
        shift.get_or_insert((nm.x - pm.x, nm.y - pm.y));
    }
    if !moved {
        return LayoutChange::Same;
    }
    let (dx, dy) = shift.unwrap_or((0, 0));
    let source = match &config.source {
        SourceConfig::Monitor { .. } => {
            let id = now.iter().find(|n| n.same_screen(&plan_monitors[0])).map(|n| n.id.clone()).unwrap_or_default();
            SourceConfig::Monitor { monitor_id: id }
        }
        SourceConfig::All => SourceConfig::All,
        // 自訂範圍跟著螢幕走（錄的還是同一個畫面）
        SourceConfig::Region { x, y, width, height } => SourceConfig::Region { x: x + dx as f64, y: y + dy as f64, width: *width, height: *height },
    };
    LayoutChange::Remap(Box::new(RecordConfig { source, ..config.clone() }))
}

/// 單一螢幕：依 monitor_key 找回那台螢幕現在的 monitorId；那台沒接上時回傳說明（不默默改錄別台）
pub fn rebind(config: &mut RecordConfig, monitors: &[MonitorInfo]) -> Result<(), String> {
    let SourceConfig::Monitor { monitor_id } = &mut config.source else { return Ok(()) };
    let Some(key) = config.monitor_key.as_deref().filter(|k| !k.is_empty()) else { return Ok(()) };
    match monitors.iter().find(|m| m.key.eq_ignore_ascii_case(key)) {
        Some(m) => {
            *monitor_id = m.id.clone();
            Ok(())
        }
        // 這次列舉拿不到識別（例如舊版 Windows）：照原本的編號
        None if monitors.iter().all(|m| m.key.is_empty()) => Ok(()),
        None => Err(crate::tr!("選的螢幕目前沒有接上，請在主畫面重新選擇要錄的螢幕", "The selected screen isn't connected. Choose a screen to record in the main window").to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: &str, key: &str, x: i32, w: i32) -> MonitorInfo {
        MonitorInfo { id: id.into(), key: key.into(), device_name: format!("\\\\.\\DISPLAY{key}"), display_number: 1, x, y: 0, width: w, height: 1080, ..Default::default() }
    }

    fn cfg(source: SourceConfig) -> RecordConfig {
        RecordConfig {
            source,
            fps: 30.0,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: Default::default(),
            output_dir: "C:\\out".into(),
            shot_dir: String::new(),
            audio: Default::default(),
            encoder: None,
            countdown_sec: None,
            hide_ui: None,
            show_clicks: false,
            show_keys: false,
            cursor_halo: false,
            hide_icons: false,
            follow_window: None,
            camera: None,
            audio_only: false,
            window_capture: false,
            separate_audio: false,
            replay_sec: 0,
            monitor_key: None,
        }
    }

    #[test]
    fn unplugging_or_resizing_the_recorded_screen_stops() {
        let laptop = mon("0:0", "A", 0, 1920);
        let ext = mon("0:1", "B", 1920, 2560);
        let c = cfg(SourceConfig::Monitor { monitor_id: "0:1".into() });
        assert!(matches!(check(&c, std::slice::from_ref(&ext), std::slice::from_ref(&laptop)), LayoutChange::Gone(_)));
        let smaller = MonitorInfo { width: 1920, ..ext.clone() };
        assert!(matches!(check(&c, std::slice::from_ref(&ext), &[laptop.clone(), smaller]), LayoutChange::Resized(_)));
        // 接上另一台、錄的那台沒變：照常
        assert_eq!(check(&c, std::slice::from_ref(&ext), &[laptop, ext.clone(), mon("1:0", "C", 4480, 1920)]), LayoutChange::Same);
    }

    #[test]
    fn renumbered_or_moved_screens_are_remapped() {
        let ext = mon("0:1", "B", 1920, 2560);
        // 拔掉筆電螢幕：外接螢幕變成 0:0、移到 (0, 0)
        let now = [mon("0:0", "B", 0, 2560)];
        let c = cfg(SourceConfig::Monitor { monitor_id: "0:1".into() });
        match check(&c, std::slice::from_ref(&ext), &now) {
            LayoutChange::Remap(n) => assert_eq!(n.source, SourceConfig::Monitor { monitor_id: "0:0".into() }),
            other => panic!("{other:?}"),
        }
        // 自訂範圍跟著螢幕移動
        let r = cfg(SourceConfig::Region { x: 2000.0, y: 100.0, width: 800.0, height: 600.0 });
        match check(&r, std::slice::from_ref(&ext), &now) {
            LayoutChange::Remap(n) => assert_eq!(n.source, SourceConfig::Region { x: 80.0, y: 100.0, width: 800.0, height: 600.0 }),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rebind_finds_the_same_physical_screen() {
        let now = [mon("0:0", "B", 0, 2560), mon("0:1", "A", 2560, 1920)];
        let mut c = RecordConfig { monitor_key: Some("A".into()), ..cfg(SourceConfig::Monitor { monitor_id: "0:0".into() }) };
        rebind(&mut c, &now).unwrap();
        assert_eq!(c.source, SourceConfig::Monitor { monitor_id: "0:1".into() });
        // 那台沒接上：說明，不默默改錄別台
        let mut gone = RecordConfig { monitor_key: Some("Z".into()), ..c.clone() };
        assert!(rebind(&mut gone, &now).is_err());
        // 沒有記住哪一台：照原本的編號
        let mut old = RecordConfig { monitor_key: None, ..c.clone() };
        rebind(&mut old, &now).unwrap();
        assert_eq!(old.source, SourceConfig::Monitor { monitor_id: "0:1".into() });
    }

    #[test]
    fn audio_only_ignores_screens() {
        let c = RecordConfig { audio_only: true, ..cfg(SourceConfig::All) };
        assert_eq!(check(&c, &[], &[]), LayoutChange::Same);
    }
}
