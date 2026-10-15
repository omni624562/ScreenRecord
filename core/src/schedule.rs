//! 排程錄影：到了指定的時間自動開始錄影（用排程時的錄影設定，不倒數），可以設定錄幾分鐘後自動停止。
//! 程式要開著（收到系統匣也可以）；等待期間電腦不會因為閒置而睡眠（power.rs）。
//! 等待中的排程也存在設定（ui.schedule），程式重新開啟後繼續等；錯過的排程開啟時通知。

use crate::app::App;
use crate::types::RecordConfig;
use crate::{info, tr, trf};
use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone};
use serde_json::Value;
use std::sync::Arc;

/// 操作視窗設定（ui）裡存等待中排程的欄位
pub const UI_KEY: &str = "schedule";

#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    /// 開始時間
    pub at: DateTime<Local>,
    /// 錄幾分鐘後自動停止；0 = 照錄影設定的「最長錄影時間」
    pub minutes: u32,
    pub config: RecordConfig,
}

impl Schedule {
    /// 到時間要用的錄影設定：不倒數；有指定長度時改成那個長度
    pub fn record_config(&self) -> RecordConfig {
        let mut c = self.config.clone();
        c.countdown_sec = Some(0.0);
        if self.minutes > 0 {
            c.max_minutes = self.minutes as f64;
        }
        c
    }
}

/// 下一次的 hour:minute（今天已經過了就是明天）
pub fn next_at(now: DateTime<Local>, hour: u32, minute: u32) -> DateTime<Local> {
    let t = NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0).unwrap_or_default();
    let mut day = now.date_naive();
    loop {
        // 日光節約時間跳過的時刻不存在：往後找下一天
        if let Some(at) = Local.from_local_datetime(&day.and_time(t)).earliest() {
            if at > now {
                return at;
            }
        }
        day = day.succ_opt().unwrap_or(day);
        if day > now.date_naive() + Duration::days(2) {
            return now + Duration::days(1);
        }
    }
}

/// 「今天 10:30」「明天 08:00」「10/12 08:00」
pub fn when_text(at: DateTime<Local>, now: DateTime<Local>) -> String {
    let hm = at.format("%H:%M").to_string();
    let days = (at.date_naive() - now.date_naive()).num_days();
    match days {
        0 => trf!("今天 {hm}", "today at {hm}"),
        1 => trf!("明天 {hm}", "tomorrow at {hm}"),
        _ => at.format("%m/%d %H:%M").to_string(),
    }
}

/// 「今天 10:30 開始，錄 30 分鐘」
pub fn describe(s: &Schedule, now: DateTime<Local>) -> String {
    let when = when_text(s.at, now);
    if s.minutes > 0 {
        trf!("{when} 開始，錄 {} 分鐘", "Starts {when}, records for {} min", s.minutes)
    } else {
        trf!("{when} 開始", "Starts {when}")
    }
}

/// 還有多久開始：「12:34」「1:02:03」
pub fn left_text(at: DateTime<Local>, now: DateTime<Local>) -> String {
    let secs = (at - now).num_seconds().max(0);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 排程錄影的標題（通知用）
pub fn title() -> &'static str {
    tr!("排程錄影", "Scheduled recording")
}

/// 存進設定的格式：開始時間（毫秒）、錄幾分鐘、錄影設定
pub fn to_value(s: &Schedule) -> Value {
    serde_json::json!({ "at": s.at.timestamp_millis(), "minutes": s.minutes, "config": serde_json::to_value(&s.config).unwrap_or(Value::Null) })
}

pub fn from_value(v: &Value) -> Option<Schedule> {
    let at = Local.timestamp_millis_opt(v.get("at")?.as_i64()?).single()?;
    let minutes = v.get("minutes").and_then(Value::as_u64).unwrap_or(0) as u32;
    let config = serde_json::from_value(v.get("config")?.clone()).ok()?;
    Some(Schedule { at, minutes, config })
}

/// 把等待中的排程存進設定（None = 清掉）
fn persist(app: &App, s: Option<&Schedule>) {
    let mut ui = app.settings.load().ui.filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({}));
    let Some(o) = ui.as_object_mut() else { return };
    match s {
        Some(s) => {
            o.insert(UI_KEY.into(), to_value(s));
        }
        None => {
            if o.remove(UI_KEY).is_none() {
                return;
            }
        }
    }
    app.settings.save(crate::settings::SettingsPatch { ui: Some(ui), ..Default::default() });
}

/// 設定裡等待中的排程
pub fn saved(app: &App) -> Option<Schedule> {
    app.settings.load().ui.as_ref().and_then(|u| u.get(UI_KEY)).and_then(from_value)
}

/// 排程錄影並存進設定（程式重新開啟後繼續等）；結束（開始錄影、取消、被新的排程取代）後從設定清掉
pub async fn run_saved(app: Arc<App>, s: Schedule) -> crate::Result<bool> {
    persist(&app, Some(&s));
    let r = app.run_schedule(s.clone()).await;
    // 被新的排程取代時，設定裡已經是新的：不要清掉
    if saved(&app).is_none_or(|cur| cur.at == s.at) {
        persist(&app, None);
    }
    r
}

/// 程式啟動時：設定裡有還沒到時間的排程就繼續等；已經過了（程式當時沒開）就清掉並通知
pub fn resume_saved(app: &Arc<App>) {
    let Some(s) = saved(app) else { return };
    let now = Local::now();
    if s.at <= now {
        info!("[排程] 錯過了：{}", describe(&s, now));
        persist(app, None);
        app.notify(title(), &trf!("排程錄影沒有執行：當時程式沒有開著（{}）", "The scheduled recording didn't run because the app wasn't open ({})", s.at.format("%m/%d %H:%M")), true);
        return;
    }
    info!("[排程] 繼續等待：{}", describe(&s, now));
    let app = app.clone();
    tokio::spawn(async move {
        let _ = run_saved(app, s).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioConfig, MethodPreference, SourceConfig};

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap()
    }

    #[test]
    fn next_time_today_or_tomorrow() {
        let now = local(2026, 10, 11, 9, 30);
        assert_eq!(next_at(now, 10, 15), local(2026, 10, 11, 10, 15));
        // 已經過了（或剛好現在）：明天
        assert_eq!(next_at(now, 9, 30), local(2026, 10, 12, 9, 30));
        assert_eq!(next_at(now, 8, 0), local(2026, 10, 12, 8, 0));
        // 跨月
        assert_eq!(next_at(local(2026, 10, 31, 23, 0), 0, 5), local(2026, 11, 1, 0, 5));
        assert_eq!(when_text(local(2026, 10, 11, 10, 15), now), "今天 10:15");
        assert_eq!(when_text(local(2026, 10, 12, 8, 0), now), "明天 08:00");
        assert_eq!(when_text(local(2026, 10, 14, 8, 0), now), "10/14 08:00");
        assert_eq!(left_text(local(2026, 10, 11, 9, 42), now), "12:00");
        assert_eq!(left_text(local(2026, 10, 11, 11, 0), now), "1:30:00");
        assert_eq!(left_text(local(2026, 10, 11, 9, 0), now), "0:00");
    }

    #[tokio::test]
    async fn saved_schedule_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_data_dir(dir.path().to_path_buf());
        let config = app.settings.load().record_config();
        assert!(config.is_none() && saved(&app).is_none());
        let s = Schedule { at: Local::now() + Duration::hours(2), minutes: 30, config: test_config() };
        persist(&app, Some(&s));
        // 毫秒以下的部分不存
        let back = saved(&app).unwrap();
        assert_eq!((back.at.timestamp_millis(), back.minutes, back.config.clone()), (s.at.timestamp_millis(), 30, s.config.clone()));
        // 重新開啟：還沒到時間就繼續等
        resume_saved(&app);
        for _ in 0..50 {
            if app.scheduled().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(app.scheduled().map(|x| x.minutes), Some(30));
        // 取消：設定裡也清掉
        app.cancel_schedule();
        for _ in 0..200 {
            if saved(&app).is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(saved(&app).is_none());
        // 已經過了的排程：清掉，不會開始
        persist(&app, Some(&Schedule { at: Local::now() - Duration::minutes(5), ..s }));
        resume_saved(&app);
        assert!(saved(&app).is_none() && app.scheduled().is_none());
    }

    fn test_config() -> RecordConfig {
        RecordConfig {
            source: SourceConfig::All,
            fps: 30.0,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: MethodPreference::Auto,
            output_dir: "C:\\v".into(),
            shot_dir: String::new(),
            audio: AudioConfig::default(),
            encoder: None,
            countdown_sec: Some(5.0),
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
            mic_denoise: false,
            quiet: false,
            masks: vec![],
            replay_sec: 0,
            monitor_key: None,
        }
    }

    #[test]
    fn config_for_the_scheduled_start() {
        let config = RecordConfig {
            source: SourceConfig::All,
            fps: 30.0,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: MethodPreference::Auto,
            output_dir: "C:\\v".into(),
            shot_dir: String::new(),
            audio: AudioConfig::default(),
            encoder: None,
            countdown_sec: Some(5.0),
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
            mic_denoise: false,
            quiet: false,
            masks: vec![],
            replay_sec: 0,
            monitor_key: None,
        };
        let now = local(2026, 10, 11, 9, 30);
        let s = Schedule { at: local(2026, 10, 11, 10, 0), minutes: 45, config: config.clone() };
        let c = s.record_config();
        assert_eq!((c.countdown_sec, c.max_minutes), (Some(0.0), 45.0));
        assert_eq!(describe(&s, now), "今天 10:00 開始，錄 45 分鐘");
        // 不限長度：照原本的最長錄影時間
        let s = Schedule { minutes: 0, config: RecordConfig { max_minutes: 60.0, ..config }, ..s };
        assert_eq!(s.record_config().max_minutes, 60.0);
        assert_eq!(describe(&s, now), "今天 10:00 開始");
    }
}
