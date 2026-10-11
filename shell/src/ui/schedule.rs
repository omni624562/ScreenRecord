//! 排程錄影：錄影面板右上角「排程」開啟的對話框，與排程中的提示列（還有多久、取消）。
//! 排程本身在 core（App::run_schedule）：到時間用排程時的錄影設定開始，不倒數。

use super::theme::{self, segmented, Btn, Icon};
use super::UiApp;
use chrono::{Duration, Local, Timelike};
use eframe::egui::{self, Align, CornerRadius, Id, Layout, RichText};
use screenrecorder_core::schedule::{describe, left_text, next_at, when_text, Schedule};
use screenrecorder_core::{tr, trf};

/// 錄多久的選項（分鐘；0 = 不限）
const LENGTHS: [u32; 7] = [0, 5, 15, 30, 60, 90, 120];
/// 幾分鐘後的選項
const AFTERS: [u32; 5] = [5, 10, 15, 30, 60];

pub struct ScheduleDlg {
    /// true = 幾分鐘後；false = 指定時間
    after_mode: bool,
    after: u32,
    hour: u32,
    minute: u32,
    length: u32,
}

pub fn open(app: &mut UiApp) {
    // 預設：10 分鐘後的整 5 分
    let t = Local::now() + Duration::minutes(10);
    let minute = t.minute().div_ceil(5) * 5;
    let (hour, minute) = if minute >= 60 { ((t.hour() + 1) % 24, 0) } else { (t.hour(), minute) };
    app.schedule_dlg = Some(ScheduleDlg { after_mode: false, after: 10, hour, minute, length: 0 });
}

fn start_time(d: &ScheduleDlg) -> chrono::DateTime<Local> {
    let now = Local::now();
    if d.after_mode {
        now + Duration::minutes(d.after as i64)
    } else {
        next_at(now, d.hour, d.minute)
    }
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.schedule_dlg.take() else { return };
    let mut close = false;
    let mut submit = false;
    let rec = super::settings_dialog::record_summary(app).join(tr!("・", " · "));
    let source = app.s.source_label(&app.env);
    let modal = egui::Modal::new(Id::new("schedule")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(440.0);
        ui.label(RichText::new(tr!("排程錄影", "Schedule a recording")).font(theme::font_bold(18.0)));
        ui.add_space(10.0);
        ui.label(RichText::new(tr!("什麼時候開始", "Start")).font(theme::font_bold(13.5)));
        ui.add_space(2.0);
        segmented(ui, &mut d.after_mode, &[(false, tr!("指定時間", "At a time")), (true, tr!("幾分鐘後", "After a delay"))], true);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if d.after_mode {
                for m in AFTERS {
                    if Btn::new(trf!("{m} 分鐘", "{m} min")).small().selected(d.after == m).show(ui).clicked() {
                        d.after = m;
                    }
                }
                ui.add(egui::DragValue::new(&mut d.after).range(1..=1440).suffix(tr!(" 分鐘", " min")));
            } else {
                ui.add(egui::DragValue::new(&mut d.hour).range(0..=23).custom_formatter(|v, _| format!("{:02}", v as u32)));
                ui.label(":");
                ui.add(egui::DragValue::new(&mut d.minute).range(0..=59).custom_formatter(|v, _| format!("{:02}", v as u32)));
            }
            ui.label(theme::muted(ui, when_text(start_time(&d), Local::now())));
        });
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
                ui.add(egui::DragValue::new(&mut d.length).range(1..=1440).suffix(tr!(" 分鐘", " min")));
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
                        "用目前的錄影設定：{source}・{rec}。到時間不倒數直接開始。程式要開著（收到系統匣也可以），等待期間電腦不會因為閒置而睡眠。",
                        "Uses the current settings: {source} · {rec}. It starts right away at that time (no countdown). Keep the app running (the tray is fine); the PC won't go to sleep while waiting.",
                    ))
                    .font(theme::font(12.5))
                    .color(p.muted),
                )
                .wrap(),
            );
        });
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new(tr!("排程", "Schedule")).primary().show(ui).clicked() {
                submit = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
    });
    if submit {
        let s = Schedule { at: start_time(&d), minutes: d.length, config: app.s.record_config(&app.env) };
        app.toast(trf!("已排程：{}", "Scheduled: {}", describe(&s, Local::now())), false);
        let core = app.core.clone();
        app.spawn(async move { core.run_schedule(s).await }, |app, r| {
            if let Err(e) = r {
                app.toast(trf!("排程錄影沒有開始：{}", "The scheduled recording didn't start: {}", e.message()), true);
            }
        });
        return;
    }
    if close || modal.should_close() {
        return;
    }
    app.schedule_dlg = Some(d);
}

/// 排程中：錄影面板裡的提示列（什麼時候開始、還有多久、取消）
pub fn strip(app: &mut UiApp, ui: &mut egui::Ui) {
    let Some(s) = app.core.scheduled() else { return };
    let p = theme::pal(ui);
    let now = Local::now();
    ui.add_space(6.0);
    egui::Frame::new().fill(p.accent.gamma_multiply(0.1)).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(trf!("排程錄影：{}", "Scheduled: {}", describe(&s, now))).color(p.accent).font(theme::font_bold(13.0))).wrap());
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, trf!("還有 {}", "Starts in {}", left_text(s.at, now))));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(tr!("取消排程", "Cancel")).icon(Icon::Close).ghost().small().show(ui).clicked() {
                    app.core.cancel_schedule();
                    app.toast(tr!("已取消排程錄影", "Scheduled recording canceled"), false);
                }
            });
        });
    });
    // 倒數每秒更新
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
}
