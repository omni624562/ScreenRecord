//! 製作加速版 / GIF：MP4 或 GIF；加速方式可「指定倍率」或「指定長度」（自動換算倍率），
//! 送出後在右側「工作」區顯示進度。

use super::dialogs::{base_name, is_default_name, short_date};
use super::settings::ExportMode;
use super::theme::{self, segmented, switch, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, CornerRadius, Id, Layout, RichText, Stroke};
use screenrecorder_core::actions::{self, ExportRequest};
use screenrecorder_core::format::{
    estimate_bytes, export_file_name, format_bytes, human_duration, parse_clock, scaled_size, speed_for_target, speed_label, video_clock, EstimateInput, MP4_WIDTHS, SPEED_MAX, SPEED_MIN,
};
use screenrecorder_core::types::{ExportFormat, ExportInfo, LibraryEntry};

const SPEED_PRESETS: [f64; 5] = [1.5, 2.0, 4.0, 8.0, 16.0];
const TARGET_PRESETS: [&str; 4] = ["0:30", "1:00", "3:00", "5:00"];
/// 加速後短於這個秒數時提醒可能太快
const TOO_SHORT_SEC: f64 = 3.0;

pub struct ExportDialog {
    pub entry: LibraryEntry,
    speed_text: String,
    speed_focus: bool,
    busy: bool,
}

impl ExportDialog {
    pub fn new(entry: LibraryEntry) -> Self {
        Self { entry, speed_text: String::new(), speed_focus: false, busy: false }
    }
}

/// 這支錄影已經有同格式、同倍率的成品
fn existing(e: &LibraryEntry, speed: f64, format: ExportFormat) -> Option<&ExportInfo> {
    e.exports.iter().find(|x| x.format.unwrap_or_default() == format && (x.speed - speed).abs() < 0.005)
}

/// 實際會存成的檔名：同名已存在時會加上 _2、_3
fn output_name(e: &LibraryEntry, speed: f64, format: ExportFormat) -> String {
    let name = export_file_name(&e.media.name, speed, format);
    let taken: Vec<String> = e.exports.iter().map(|x| x.media.name.to_lowercase()).collect();
    if !taken.contains(&name.to_lowercase()) {
        return name;
    }
    let dot = name.rfind('.').unwrap_or(name.len());
    (2..).map(|i| format!("{}_{i}{}", &name[..dot], &name[dot..])).find(|n| !taken.contains(&n.to_lowercase())).unwrap_or(name)
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut dlg) = app.export_dlg.take() else {
        return;
    };
    let mut close = false;
    let e = dlg.entry.clone();
    let dur = e.media.duration_sec.unwrap_or(0.0);
    let modal = egui::Modal::new(Id::new("export")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(560.0);
        let p = theme::pal(ui);
        let s = &mut app.s;
        let gif = s.export_format == ExportFormat::Gif;
        // 目前設定實際會用的倍率；指定長度但輸入無效時為 None
        let speed = match s.export_mode {
            ExportMode::Speed => Some(if gif { s.speed.max(1.0) } else { s.speed.max(SPEED_MIN) }),
            ExportMode::Target => parse_clock(&s.export_target).filter(|_| dur > 0.0).map(|t| speed_for_target(dur, t, gif)),
        };

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(if gif { "製作 GIF" } else { "製作加速版" }).font(theme::font_bold(17.0)));
                let name = if is_default_name(&e.media.name) { format!("{} 的錄影", short_date(&e.media.name, e.media.mtime, false)) } else { base_name(&e.media.name) };
                ui.label(theme::muted(ui, name)).on_hover_text(&e.media.name);
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::new("關閉").ghost().small().show(ui).clicked() {
                    close = true;
                }
            });
        });
        let info: Vec<String> = [e.media.fps.map(|f| format!("{f} fps")), Some(if e.media.has_audio == Some(true) { "有聲音".into() } else { "無聲音".into() })].into_iter().flatten().collect();
        ui.label(theme::muted(ui, info.join("・")));
        ui.add_space(8.0);

        let mut changed = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::muted(ui, "格式"));
                let mut f = s.export_format;
                if segmented(ui, &mut f, &[(ExportFormat::Mp4, "MP4 影片"), (ExportFormat::Gif, "GIF 動畫")], true) {
                    // MP4 不能原速（那就是原檔）
                    if f == ExportFormat::Mp4 && s.speed < SPEED_MIN {
                        s.speed = 4.0;
                    }
                    s.export_format = f;
                    changed = true;
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(theme::muted(ui, "加速方式"));
                let mut m = s.export_mode;
                if segmented(ui, &mut m, &[(ExportMode::Speed, "指定倍率"), (ExportMode::Target, "指定長度")], true) {
                    s.export_mode = m;
                    changed = true;
                }
            });
        });
        ui.add_space(6.0);
        match s.export_mode {
            ExportMode::Speed => {
                ui.label(theme::muted(ui, "倍率"));
                ui.horizontal(|ui| {
                    let mut presets: Vec<(f64, String)> = SPEED_PRESETS.iter().map(|v| (*v, format!("{}×", speed_label(*v)))).collect();
                    if gif {
                        presets.insert(0, (1.0, "原速".into()));
                    }
                    for (v, label) in &presets {
                        if Btn::new(label).small().selected((s.speed - v).abs() < 1e-9).show(ui).clicked() {
                            s.speed = *v;
                            changed = true;
                        }
                    }
                    if !dlg.speed_focus {
                        dlg.speed_text = speed_label(s.speed);
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut dlg.speed_text).desired_width(60.0));
                    dlg.speed_focus = r.has_focus();
                    ui.label("×");
                    if r.changed() {
                        if let Ok(v) = dlg.speed_text.trim().parse::<f64>() {
                            let min = if gif { 1.0 } else { SPEED_MIN };
                            if v.is_finite() && v >= min && v <= SPEED_MAX {
                                s.speed = (v * 100.0).round() / 100.0;
                                changed = true;
                            }
                        }
                    }
                });
            }
            ExportMode::Target => {
                ui.label(theme::muted(ui, "加速後的長度（分:秒，例如 1:00）"));
                ui.horizontal(|ui| {
                    for t in TARGET_PRESETS {
                        if Btn::new(t).small().selected(s.export_target == t).show(ui).clicked() {
                            s.export_target = t.into();
                            changed = true;
                        }
                    }
                    if ui.add(egui::TextEdit::singleline(&mut s.export_target).desired_width(80.0).hint_text("1:00")).changed() {
                        changed = true;
                    }
                });
                let target = parse_clock(&s.export_target);
                let hint = match (target, speed) {
                    (None, _) => "請輸入長度，例如 1:00、90（秒）或 1:02:03".to_string(),
                    _ if dur <= 0.0 => "無法讀取原片長度".into(),
                    (Some(t), Some(sp)) if dur / sp > t + 0.5 => format!("最多只能加速到 {}×，實際約 {}", speed_label(SPEED_MAX), video_clock(dur / sp)),
                    (Some(t), Some(sp)) if dur / sp < t - 0.5 => {
                        format!("原片只有 {}，{}", video_clock(dur), if gif { "以原速製作".to_string() } else { format!("以最低倍率 {}× 製作", speed_label(sp)) })
                    }
                    (_, Some(sp)) => format!("需要加速 {}×", speed_label(sp)),
                    _ => String::new(),
                };
                ui.label(RichText::new(hint).font(theme::font(12.5)).color(if target.is_none() { p.rec } else { p.muted }));
            }
        }
        // GIF / MP4 尺寸
        let (sw, sh) = (e.media.width.unwrap_or(0) as i32, e.media.height.unwrap_or(0) as i32);
        if gif {
            ui.horizontal(|ui| {
                let widths: Vec<(u32, String)> = [320, 480, 640, 960, 1280].iter().map(|w| (*w, format!("{w} px"))).collect();
                ui.label(theme::muted(ui, "GIF 寬度"));
                egui::ComboBox::from_id_salt("gifw").selected_text(format!("{} px", s.gif_width)).show_ui(ui, |ui| {
                    for (w, t) in &widths {
                        if ui.selectable_label(s.gif_width == *w, t).clicked() {
                            s.gif_width = *w;
                            changed = true;
                        }
                    }
                });
                ui.add_space(12.0);
                ui.label(theme::muted(ui, "GIF 每秒張數"));
                egui::ComboBox::from_id_salt("giffps").selected_text(s.gif_fps.to_string()).show_ui(ui, |ui| {
                    for f in [5, 10, 15, 20] {
                        if ui.selectable_label(s.gif_fps == f, f.to_string()).clicked() {
                            s.gif_fps = f;
                            changed = true;
                        }
                    }
                });
            });
        } else if sw > 0 && sh > 0 {
            // 只列出比原片小的選項
            let opts: Vec<u32> = MP4_WIDTHS.iter().copied().filter(|w| *w == 0 || (*w as i32) < sw).collect();
            if !opts.contains(&s.mp4_width) {
                s.mp4_width = 0;
            }
            let label = |w: u32| {
                let (a, b) = scaled_size(sw, sh, w as i32);
                if w == 0 {
                    format!("原尺寸（{a}×{b}）")
                } else {
                    format!("寬 {w}（{a}×{b}）")
                }
            };
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, "尺寸"));
                egui::ComboBox::from_id_salt("mp4w").selected_text(label(s.mp4_width)).width(200.0).show_ui(ui, |ui| {
                    for w in &opts {
                        if ui.selectable_label(s.mp4_width == *w, label(*w)).clicked() {
                            s.mp4_width = *w;
                            changed = true;
                        }
                    }
                });
            });
        }
        ui.add_space(8.0);
        // 原片 → 加速後
        let dims = (sw > 0 && sh > 0).then(|| scaled_size(sw, sh, if gif { s.gif_width as i32 } else { s.mp4_width as i32 }));
        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(12.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let w = (ui.available_width() - 40.0) / 2.0;
            ui.horizontal_top(|ui| {
                ui.allocate_ui(vec2(w, 70.0), |ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::muted(ui, "原片"));
                        ui.label(RichText::new(if dur > 0.0 { video_clock(dur) } else { "—".into() }).font(theme::font_bold(18.0)));
                        let from: Vec<String> = [(sw > 0).then(|| format!("{sw}×{sh}")), Some(format_bytes(e.media.bytes))].into_iter().flatten().collect();
                        ui.label(theme::muted(ui, from.join("・")));
                    });
                });
                ui.allocate_ui(vec2(28.0, 70.0), |ui| {
                    ui.add_space(18.0);
                    ui.label(RichText::new("→").font(theme::font(22.0)).color(p.muted));
                });
                ui.allocate_ui(vec2(w, 70.0), |ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::muted(ui, "加速後"));
                        ui.label(
                            RichText::new(match speed {
                                Some(sp) if dur > 0.0 => video_clock(dur / sp),
                                _ => "—".into(),
                            })
                            .font(theme::font_bold(18.0)),
                        );
                        if let (Some((w, h)), Some(sp)) = (dims, speed.filter(|_| dur > 0.0)) {
                            let (lo, hi) = estimate_bytes(&EstimateInput {
                                format: s.export_format,
                                src_bytes: e.media.bytes as f64,
                                src_sec: dur,
                                src_width: sw as f64,
                                src_height: sh as f64,
                                speed: sp,
                                width: w as f64,
                                height: h as f64,
                                gif_fps: Some(s.gif_fps as f64),
                            });
                            ui.label(theme::muted(ui, format!("{w}×{h}・約 {}～{}", format_bytes(lo as u64), format_bytes(hi as u64))))
                                .on_hover_text("依原片的位元率與畫面大小粗估，實際大小視畫面變化而定");
                        }
                    });
                });
            });
        });
        // 提醒：已有同倍率的成品、加速後太短
        let ex = speed.and_then(|sp| existing(&e, sp, s.export_format)).cloned();
        let what = if gif {
            format!("GIF{}", speed.filter(|v| *v > 1.0).map(|v| format!(" {}×", speed_label(v))).unwrap_or_else(|| "（原速）".into()))
        } else {
            format!("{}× 加速版", speed_label(speed.unwrap_or(0.0)))
        };
        if let Some(x) = &ex {
            ui.add_space(6.0);
            egui::Frame::new().fill(p.accent_soft).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(format!(
                            "已經有 {what}（{}{}）。再製作會另存一份。",
                            x.media.duration_sec.map(|d| format!("{}・", video_clock(d))).unwrap_or_default(),
                            format_bytes(x.media.bytes)
                        ))
                        .wrap(),
                    );
                    if Btn::new("播放現有的").icon(Icon::Play).small().show(ui).clicked() {
                        let _ = actions::open(actions::OpenAction::Play, &x.media.path);
                    }
                });
            });
        }
        if let Some(sp) = speed.filter(|sp| dur > 0.0 && dur / sp < TOO_SHORT_SEC && *sp > 1.0) {
            ui.add_space(6.0);
            egui::Frame::new().fill(p.warn_soft).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let extra = if s.export_mode == ExportMode::Speed { "可以改用「指定長度」。" } else { "" };
                    ui.add(egui::Label::new(RichText::new(format!("加速後只有 {:.1} 秒，可能太快看不清楚。{extra}", dur / sp)).color(p.warn)).wrap());
                    if s.export_mode == ExportMode::Speed && Btn::new("改用指定長度").small().show(ui).clicked() {
                        // 預設 1:00，原片不到 1 分鐘時用原片長度的一半（至少 5 秒）
                        let half = (dur / 2.0).round().max(5.0) as u32;
                        s.export_target = if dur < 60.0 { format!("0:{half:02}") } else { "1:00".into() };
                        s.export_mode = ExportMode::Target;
                        changed = true;
                    }
                });
            });
        }
        if let Some(sp) = speed {
            ui.add_space(6.0);
            let mut hint = String::new();
            if sp > 1.0 {
                hint += &format!("{}× 時，1 小時的錄影 ≈ {}{}；", speed_label(sp), human_duration(3600.0 / sp), if sp >= 8.0 { "，適合做成縮時影片" } else { "" });
            }
            hint += &format!("存成 {}", output_name(&e, sp, s.export_format));
            if gif {
                hint += "；GIF 沒有聲音，檔案較大，建議 1 分鐘以內";
            }
            ui.add(egui::Label::new(theme::muted(ui, hint)).wrap());
        }
        if !gif && e.media.has_audio == Some(true) {
            ui.add_space(4.0);
            let mut keep = s.keep_audio;
            if switch(ui, &mut keep, "保留聲音（變速不變調）", true).changed() {
                s.keep_audio = keep;
                changed = true;
            }
        }
        if changed {
            app.save_settings();
        }
        ui.add_space(12.0);
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().min.y - 6.0, Stroke::new(1.0, p.border));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let s = &app.s;
            let label = match speed {
                None => "製作".to_string(),
                Some(sp) if gif => format!("製作 GIF{}", if sp > 1.0 { format!("（{}×）", speed_label(sp)) } else { String::new() }),
                Some(sp) => format!("製作 {}× 加速版", speed_label(sp)),
            };
            let label = if ex.is_some() { label.replacen("製作", "再製作一份", 1) } else { label };
            if Btn::new(label).primary().enabled(speed.is_some() && !dlg.busy).min_width(160.0).show(ui).clicked() {
                if let Some(sp) = speed {
                    dlg.busy = true;
                    let req = ExportRequest {
                        source: e.media.path.clone(),
                        speed: sp,
                        keep_audio: s.keep_audio,
                        format: s.export_format,
                        gif_width: s.gif_width as f64,
                        gif_fps: s.gif_fps as f64,
                        mp4_width: s.mp4_width as f64,
                    };
                    let core = app.core.clone();
                    app.spawn(async move { actions::export_start(&core, &req).await }, |app, r| match r {
                        Ok(()) => {
                            app.export_dlg = None;
                            app.dismissed_job = None;
                            app.toast("已開始製作，進度顯示在右側", false);
                        }
                        Err(e) => {
                            if let Some(d) = &mut app.export_dlg {
                                d.busy = false;
                            }
                            app.toast(e.message().to_string(), true);
                        }
                    });
                }
            }
            let _ = vec2(0.0, 0.0);
        });
    });
    if modal.should_close() {
        close = true;
    }
    if !close && app.export_dlg.is_none() {
        app.export_dlg = Some(dlg);
    }
}
