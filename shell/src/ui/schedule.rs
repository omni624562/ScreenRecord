//! 排程錄影：錄影面板右上角「排程」開啟的對話框，與排程中的提示列（還有多久、取消）。
//! 排程本身在 core（App::run_schedule、schedule::run_saved）：到時間用排程時的錄影設定開始，不倒數；等待中的排程存在設定裡，重新開啟程式後繼續等。

use super::theme::{self, segmented, Btn};
use super::UiApp;
use chrono::TimeZone;
use chrono::{Duration, Local, Timelike};
use eframe::egui::{self, Align, CornerRadius, Id, Layout, RichText};
use screenrecorder_core::schedule::{describe, next_at, when_text, Schedule};
use screenrecorder_core::{tr, trf};

/// 錄多久的選項（分鐘；0 = 不限）
const LENGTHS: [u32; 7] = [0, 5, 15, 30, 60, 90, 120];
/// 幾分鐘後的選項
const AFTERS: [u32; 5] = [5, 10, 15, 30, 60];

pub struct ScheduleDlg {
    /// true = 幾分鐘後；false = 指定時間
    after_mode: bool,
    after: u32,
    /// 哪一天：0 今天、1 明天、2 選日期（day_pick 天後）
    day: u8,
    day_pick: u32,
    hour: u32,
    minute: u32,
    length: u32,
    /// 打開時的內容：改過了按 Esc 或點外面不會直接關掉
    initial: (bool, u32, u8, u32, u32, u32, u32),
    /// 改過了還按 Esc / 點外面：提示要按「取消」才放棄
    keep_hint: bool,
}

/// 「選日期」最多可以選到幾天後
const MAX_DAYS: u32 = 30;

impl ScheduleDlg {
    fn values(&self) -> (bool, u32, u8, u32, u32, u32, u32) {
        (self.after_mode, self.after, self.day, self.day_pick, self.hour, self.minute, self.length)
    }

    /// 選的那一天是幾天後
    fn day_offset(&self) -> u32 {
        match self.day {
            0 => 0,
            1 => 1,
            _ => self.day_pick,
        }
    }
}

pub fn open(app: &mut UiApp) {
    // 預設：10 分鐘後的整 5 分
    let t = Local::now() + Duration::minutes(10);
    let minute = t.minute().div_ceil(5) * 5;
    let (hour, minute) = if minute >= 60 { ((t.hour() + 1) % 24, 0) } else { (t.hour(), minute) };
    // 過了午夜就是明天
    let day = if t.date_naive() > Local::now().date_naive() || (minute == 0 && hour == 0) { 1 } else { 0 };
    let mut d = ScheduleDlg { after_mode: false, after: 10, day, day_pick: 2, hour, minute, length: 0, initial: Default::default(), keep_hint: false };
    d.initial = d.values();
    app.schedule_dlg = Some(d);
}

/// 開始的時間；指定的時間已經過了是 None
fn start_time(d: &ScheduleDlg) -> Option<chrono::DateTime<Local>> {
    let now = Local::now();
    if d.after_mode {
        return Some(now + Duration::minutes(d.after as i64));
    }
    let date = now.date_naive() + Duration::days(d.day_offset() as i64);
    let t = chrono::NaiveTime::from_hms_opt(d.hour.min(23), d.minute.min(59), 0)?;
    // 日光節約時間跳過的時刻不存在：用 next_at 的規則往後找
    let at = Local.from_local_datetime(&date.and_time(t)).earliest().unwrap_or_else(|| next_at(now, d.hour, d.minute));
    (at > now).then_some(at)
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.schedule_dlg.take() else { return };
    let mut close = false;
    let mut submit = false;
    let rec = super::settings_dialog::record_summary(app).join(tr!("・", " · "));
    let source = app.s.source_label(&app.env);
    let modal = egui::Modal::new(Id::new("schedule")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(460.0);
        ui.label(RichText::new(tr!("排程錄影", "Schedule a recording")).font(theme::font_bold(18.0)));
        ui.add_space(10.0);
        ui.label(RichText::new(tr!("什麼時候開始", "Start")).font(theme::font_bold(13.5)));
        ui.add_space(2.0);
        segmented(ui, &mut d.after_mode, &[(false, tr!("指定時間", "At a time")), (true, tr!("幾分鐘後", "After a delay"))], true);
        ui.add_space(4.0);
        let two = |v: f64| format!("{:02}", v as u32);
        let parse = |t: &str| t.trim().parse::<f64>().ok().filter(|v| v.is_finite());
        if d.after_mode {
            ui.horizontal_wrapped(|ui| {
                for m in AFTERS {
                    if Btn::new(trf!("{m} 分鐘", "{m} min")).small().selected(d.after == m).show(ui).clicked() {
                        d.after = m;
                    }
                }
                let mut v = d.after as f64;
                if theme::stepper(ui, Id::new("schedule-after"), &mut v, 5.0, 1.0..=1440.0, 0, tr!("分鐘", "min")) {
                    d.after = v.round() as u32;
                }
            });
        } else {
            // 哪一天：今天、明天、選日期（之後 30 天內）
            ui.horizontal(|ui| {
                let mut day = d.day;
                if segmented(ui, &mut day, &[(0, tr!("今天", "Today")), (1, tr!("明天", "Tomorrow")), (2, tr!("選日期", "Pick a date"))], true) {
                    d.day = day;
                }
                if d.day == 2 {
                    let today = Local::now().date_naive();
                    let label = |n: u32| super::dialogs::day_label(today + Duration::days(n as i64), today);
                    theme::field_style(ui);
                    egui::ComboBox::from_id_salt("schedule-day").selected_text(label(d.day_pick)).width(130.0).show_ui(ui, |ui| {
                        for n in 2..=MAX_DAYS {
                            if ui.selectable_label(d.day_pick == n, label(n)).clicked() {
                                d.day_pick = n;
                            }
                        }
                    });
                }
            });
            ui.add_space(4.0);
            // 幾點幾分：− ＋ 或直接輸入
            ui.horizontal(|ui| {
                let mut h = d.hour as f64;
                if theme::stepper_with(ui, Id::new("schedule-hour"), &mut h, 1.0, 0.0..=23.0, two, parse) {
                    d.hour = h.round() as u32;
                }
                ui.label(RichText::new(tr!("點", ":")).font(theme::font(14.0)));
                let mut m = d.minute as f64;
                if theme::stepper_with(ui, Id::new("schedule-minute"), &mut m, 5.0, 0.0..=59.0, two, parse) {
                    d.minute = m.round() as u32;
                }
                ui.label(RichText::new(tr!("分", "")).font(theme::font(14.0)));
            });
        }
        let at = start_time(&d);
        match at {
            Some(at) => ui.label(theme::muted(ui, trf!("開始時間：{}", "Starts {}", when_text(at, Local::now())))),
            None => ui.label(RichText::new(tr!("這個時間已經過了，請改時間或選「明天」", "That time has already passed. Change the time or choose “Tomorrow”")).font(theme::font(12.5)).color(p.rec)),
        };
        ui.add_space(10.0);
        ui.label(RichText::new(tr!("錄多久", "Length")).font(theme::font_bold(13.5)));
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for m in LENGTHS {
                let label = if m == 0 { tr!("不限", "No limit").to_string() } else { trf!("{m} 分鐘", "{m} min") };
                if Btn::new(label).small().selected(d.length == m).show(ui).clicked() {
                    d.length = m;
                }
            }
            // 自訂長度（選了「不限」以外的才顯示）
            if d.length > 0 {
                let mut v = d.length as f64;
                if theme::stepper(ui, Id::new("schedule-length"), &mut v, 5.0, 1.0..=1440.0, 0, tr!("分鐘", "min")) {
                    d.length = v.round() as u32;
                }
            }
        });
        if d.length == 0 {
            ui.label(theme::muted(ui, tr!("不限：自己按停止（或照設定的「最長錄影時間」）", "No limit: stop it yourself (or at the “Max length” in settings)")));
        }
        ui.add_space(10.0);
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(trf!(
                        "用目前的錄影設定：{source}・{rec}。到時間不倒數直接開始。程式要開著（收到系統匣也可以；重新開啟程式也會繼續等），等待期間電腦不會因為閒置而睡眠。",
                        "Uses the current settings: {source} · {rec}. It starts right away at that time (no countdown). Keep the app running (the tray is fine, and it keeps waiting if you reopen the app); the PC won't go to sleep while waiting.",
                    ))
                    .font(theme::font(12.5))
                    .color(p.muted),
                )
                .wrap(),
            );
        });
        if d.keep_hint {
            ui.add_space(6.0);
            ui.label(RichText::new(tr!("還沒排程：要放棄這次的設定請按「取消」", "Not scheduled yet: click “Cancel” to discard these settings")).font(theme::font(12.5)).color(p.warn));
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new(tr!("排程", "Schedule")).primary().enabled(at.is_some()).show(ui).clicked() {
                submit = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
    });
    if let (true, Some(at)) = (submit, start_time(&d)) {
        let s = Schedule { at, minutes: d.length, config: app.s.record_config(&app.env) };
        app.toast(trf!("已排程：{}", "Scheduled: {}", describe(&s, Local::now())), false);
        let core = app.core.clone();
        app.spawn(async move { screenrecorder_core::schedule::run_saved(core, s).await }, |app, r| {
            if let Err(e) = r {
                app.toast(trf!("排程錄影沒有開始：{}", "The scheduled recording didn't start: {}", e.message()), true);
            }
        });
        return;
    }
    if close {
        return;
    }
    // Esc、點外面：沒改過就關掉；改過了不直接丟掉，提示按「取消」
    if modal.should_close() {
        if d.values() == d.initial {
            return;
        }
        d.keep_hint = true;
    }
    app.schedule_dlg = Some(d);
}
