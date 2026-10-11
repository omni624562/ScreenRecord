//! 排程錄影：到了指定的時間自動開始錄影（用排程時的錄影設定，不倒數），可以設定錄幾分鐘後自動停止。
//! 只記在記憶體：程式要開著（收到系統匣也可以）；等待期間電腦不會因為閒置而睡眠（power.rs）。

use crate::types::RecordConfig;
use crate::{tr, trf};
use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone};

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
