//! 確認 / 輸入對話框、提示訊息、更新說明，以及檔名顯示用的小工具。

use super::theme::{self, Btn};
use super::UiApp;
use eframe::egui::{self, vec2, Align, Align2, Color32, CornerRadius, Id, Layout, RichText};
use screenrecorder_core::version::{APP_VERSION, CHANGELOG};
use std::time::Instant;

/// 送出時呼叫：回傳 None 關閉、Some(錯誤) 顯示在對話框裡；需要等待時自己設定 ask.busy 並在完成後處理
pub type Submit = Box<dyn FnOnce(&mut UiApp, String) + Send>;

pub struct Ask {
    pub title: String,
    pub message: Option<String>,
    pub list: Vec<String>,
    pub ok: String,
    pub danger: bool,
    /// 有值 = 輸入對話框
    pub input: Option<String>,
    pub input_label: String,
    pub error: Option<String>,
    /// 送出中（等待伺服器）：按鈕停用、不能關閉
    pub busy: bool,
    pub on_ok: Option<Submit>,
    focused: bool,
}

impl Ask {
    pub fn confirm(title: impl Into<String>, message: impl Into<String>, ok: impl Into<String>, on_ok: impl FnOnce(&mut UiApp, String) + Send + 'static) -> Ask {
        let m: String = message.into();
        Ask {
            title: title.into(),
            message: (!m.is_empty()).then_some(m),
            list: vec![],
            ok: ok.into(),
            danger: false,
            input: None,
            input_label: String::new(),
            error: None,
            busy: false,
            on_ok: Some(Box::new(on_ok)),
            focused: false,
        }
    }

    pub fn input(title: impl Into<String>, label: impl Into<String>, value: impl Into<String>, ok: impl Into<String>, on_ok: impl FnOnce(&mut UiApp, String) + Send + 'static) -> Ask {
        let mut a = Ask::confirm(title, "", ok, on_ok);
        a.input = Some(value.into());
        a.input_label = label.into();
        a
    }
}

pub fn show_ask(app: &mut UiApp, ctx: &egui::Context) {
    let Some(ask) = app.ask.as_mut() else { return };
    let mut submit = false;
    let mut cancel = false;
    let modal = egui::Modal::new(Id::new("ask")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(420.0);
        let p = theme::pal(ui);
        ui.label(RichText::new(&ask.title).font(theme::font_bold(16.0)));
        if let Some(m) = &ask.message {
            ui.add_space(4.0);
            ui.label(RichText::new(m).color(p.muted));
        }
        if !ask.list.is_empty() {
            ui.add_space(4.0);
            egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(6)).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for name in ask.list.iter().take(12) {
                    ui.label(RichText::new(name).font(theme::mono(12.5)));
                }
                if ask.list.len() > 12 {
                    ui.label(theme::muted(ui, format!("…以及另外 {} 個", ask.list.len() - 12)));
                }
            });
        }
        if let Some(v) = &mut ask.input {
            ui.add_space(8.0);
            ui.label(theme::muted(ui, &ask.input_label));
            let r = ui.add_enabled(!ask.busy, egui::TextEdit::singleline(v).desired_width(f32::INFINITY));
            if !ask.focused {
                r.request_focus();
                ask.focused = true;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
        }
        if let Some(e) = &ask.error {
            ui.add_space(4.0);
            ui.label(RichText::new(e).color(p.rec));
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ok = Btn::new(&ask.ok).kind(if ask.danger { theme::Kind::Danger } else { theme::Kind::Primary }).enabled(!ask.busy).show(ui);
            if ok.clicked() {
                submit = true;
            }
            if Btn::new("取消").ghost().enabled(!ask.busy).show(ui).clicked() {
                cancel = true;
            }
        });
        if ask.input.is_none() && !ask.danger && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
    });
    if (modal.should_close() || cancel) && !ask.busy && !submit {
        app.ask = None;
        return;
    }
    if submit && !ask.busy {
        let value = ask.input.clone().unwrap_or_default();
        if let Some(f) = ask.on_ok.take() {
            ask.error = None;
            // 送出的處理可以讓對話框留著（設 busy 等待結果，或設 error 顯示錯誤），否則自動關閉
            f(app, value);
            if app.ask.as_ref().is_some_and(|a| !a.busy && a.error.is_none() && a.on_ok.is_none()) {
                app.ask = None;
            }
        } else {
            app.ask = None;
        }
    }
}

pub fn show_toast(app: &mut UiApp, ctx: &egui::Context) {
    let Some(t) = &app.toast else { return };
    if Instant::now() >= t.until {
        app.toast = None;
        return;
    }
    ctx.request_repaint_after(t.until - Instant::now());
    let p = theme::pal_ctx(ctx);
    let (fill, fg) = if t.error { (p.rec, Color32::WHITE) } else { (p.text, p.surface) };
    egui::Area::new(Id::new("toast")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0)).order(egui::Order::Tooltip).interactable(false).show(ctx, |ui| {
        egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
            ui.set_max_width(560.0);
            ui.label(RichText::new(&t.text).color(fg));
        });
    });
}

/// 更新說明（CHANGELOG.md 的簡易轉換：## / ### 標題、- 清單、`程式碼`、[文字](網址)）
pub fn changelog(app: &mut UiApp, ctx: &egui::Context) {
    let mut open = true;
    let modal = egui::Modal::new(Id::new("changelog")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width((ctx.content_rect().width() - 80.0).min(720.0));
        ui.horizontal(|ui| {
            ui.label(RichText::new("更新說明").font(theme::font_bold(17.0)));
            ui.label(theme::muted(ui, format!("目前版本 {APP_VERSION}")));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new("關閉").ghost().small().show(ui).clicked() {
                    open = false;
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 160.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for line in CHANGELOG.lines() {
                if line.starts_with("# ") {
                    continue;
                }
                if let Some(h) = line.strip_prefix("## ") {
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(plain(h)).font(theme::font_bold(16.0)));
                        if h.starts_with(APP_VERSION) {
                            theme::chip(ui, "目前版本", theme::Tone::Accent, false);
                        }
                    });
                } else if let Some(h) = line.strip_prefix("### ") {
                    ui.add_space(4.0);
                    ui.label(RichText::new(plain(h)).font(theme::font_bold(13.0)).color(p.muted));
                } else if let Some(item) = line.strip_prefix("- ") {
                    bullet(ui, item, 0.0);
                } else if let Some(item) = line.strip_prefix("  - ") {
                    bullet(ui, item, 16.0);
                } else if !line.trim().is_empty() {
                    ui.label(plain(line));
                }
            }
        });
    });
    if modal.should_close() || !open {
        app.changelog_open = false;
    }
}

fn bullet(ui: &mut egui::Ui, text: &str, indent: f32) {
    ui.horizontal_top(|ui| {
        ui.add_space(indent + 4.0);
        ui.label("•");
        ui.add(egui::Label::new(plain(text)).wrap());
    });
}

/// 去掉 Markdown 記號：`code` → code、[文字](網址) → 文字
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match (after.find("]("), after.find(')')) {
            (Some(a), Some(b)) if a < b => {
                out.push_str(&after[..a]);
                rest = &after[b + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace('`', "")
}

// ───────────── 檔名 ─────────────

/// 程式產生的預設檔名（Rec_日期時間，可能帶 _2、_cut）：只看日期就夠
pub fn is_default_name(name: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^Rec_\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}(_\d+)?(_cut(_\d+)?)?\.mp4$").unwrap());
    RE.is_match(name)
}

pub fn is_cut_name(name: &str) -> bool {
    screenrecorder_core::library::is_cut_name(name)
}

/// 顯示用名稱：去掉 .mp4
pub fn base_name(name: &str) -> String {
    screenrecorder_core::format::strip_mp4(name)
}

pub fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// 由檔名 Rec_2026-10-06_08-17-18 取出「10/06 08:17」；不符合格式時用修改時間
pub fn short_date(name: &str, mtime: f64, with_sec: bool) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})(?:-(\d{2}))?").unwrap());
    if let Some(m) = RE.captures(name) {
        let sec = if with_sec { m.get(6).map(|s| format!(":{}", s.as_str())).unwrap_or_default() } else { String::new() };
        return format!("{}/{} {}:{}{sec}", &m[2], &m[3], &m[4], &m[5]);
    }
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(mtime as i64).single() {
        Some(d) => d.format(if with_sec { "%m/%d %H:%M:%S" } else { "%m/%d %H:%M" }).to_string(),
        None => String::new(),
    }
}

/// 一組錄影的日期標籤：同一分鐘的有好幾支時加上秒數，才分得出來
pub fn date_labels<'a>(items: impl Iterator<Item = (&'a str, f64)> + Clone) -> std::collections::HashMap<String, String> {
    let mut count = std::collections::HashMap::<String, usize>::new();
    for (n, m) in items.clone() {
        *count.entry(short_date(n, m, false)).or_default() += 1;
    }
    items
        .map(|(n, m)| {
            let k = short_date(n, m, false);
            let label = if count.get(&k).copied().unwrap_or(0) > 1 { short_date(n, m, true) } else { k };
            (n.to_string(), label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_default_name("Rec_2026-10-06_08-17-18.mp4"));
        assert!(is_default_name("Rec_2026-10-06_08-17-18_2_cut.mp4"));
        assert!(!is_default_name("操作示範.mp4"));
        assert_eq!(short_date("Rec_2026-10-06_08-17-18.mp4", 0.0, false), "10/06 08:17");
        assert_eq!(short_date("Rec_2026-10-06_08-17-18.mp4", 0.0, true), "10/06 08:17:18");
        let labels = date_labels([("Rec_2026-10-06_08-17-18.mp4", 0.0), ("Rec_2026-10-06_08-17-40.mp4", 0.0), ("Rec_2026-10-06_09-00-00.mp4", 0.0)].into_iter());
        assert_eq!(labels["Rec_2026-10-06_08-17-18.mp4"], "10/06 08:17:18");
        assert_eq!(labels["Rec_2026-10-06_09-00-00.mp4"], "10/06 09:00");
        assert_eq!(file_name("C:\\Videos\\a.mp4"), "a.mp4");
        assert_eq!(plain("見 [說明](https://x.y/z) 與 `code`"), "見 說明 與 code");
    }
}
