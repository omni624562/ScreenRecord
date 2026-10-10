//! 製作加速版 / GIF：MP4 或 GIF；加速方式可「指定倍率」或「指定長度」（自動換算倍率），
//! 送出後在右側「工作」區顯示進度。

use super::dialogs::{base_name, is_default_name, short_date};
use super::settings::ExportMode;
use super::theme::{self, segmented, switch, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, Id, Layout, RichText, Sense, Stroke};
use screenrecorder_core::actions::{self, ExportRequest};
use screenrecorder_core::format::{
    estimate_bytes, export_file_name, format_bytes, human_duration, parse_clock, scaled_size, speed_for_target, speed_label, video_clock, EstimateInput, MP4_WIDTHS, SPEED_MAX, SPEED_MIN,
};
use screenrecorder_core::types::{ExportFormat, ExportInfo, LibraryEntry};
use screenrecorder_core::{tr, trf};

const SPEED_PRESETS: [f64; 5] = [1.5, 2.0, 4.0, 8.0, 16.0];
const TARGET_PRESETS: [&str; 4] = ["0:30", "1:00", "3:00", "5:00"];
/// 加速後短於這個秒數時提醒可能太快
const TOO_SHORT_SEC: f64 = 3.0;

pub struct ExportDialog {
    pub entry: LibraryEntry,
    speed_text: String,
    speed_focus: bool,
    /// 內容最高到過多高：切換格式、加速方式時視窗不會忽大忽小（只會變高一次，不會縮回去）
    body_h: f32,
    busy: bool,
}

impl ExportDialog {
    pub fn new(entry: LibraryEntry) -> Self {
        Self { entry, speed_text: String::new(), speed_focus: false, body_h: 0.0, busy: false }
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

/// 左邊標籤欄的寬度
const LABEL_W: f32 = 64.0;
/// 每一列控制項的高度（標籤垂直置中）
const ROW_H: f32 = 34.0;
const PAD: i8 = 24;

/// 表單的一列：左邊標籤、右邊控制項（說明文字可放在控制項下方）
fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = theme::pal(ui);
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, ROW_H), Sense::hover());
        ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, label, theme::font(13.5), p.muted);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.set_min_height(ROW_H);
            add(ui);
        });
    });
}

/// 說明文字（控制項下方，固定一行高）
fn helper(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text.into()).font(theme::font(12.5)).color(color)).truncate());
}

/// 下拉選單（與其他控制項同高）
fn combo<T: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, width: f32, value: &mut T, items: &[(T, String)]) -> bool {
    let mut changed = false;
    let text = items.iter().find(|(v, _)| v == value).map(|(_, t)| t.clone()).unwrap_or_default();
    ui.scope(|ui| {
        theme::field_style(ui);
        egui::ComboBox::from_id_salt(id).selected_text(RichText::new(text).font(theme::font(13.5))).width(width).show_ui(ui, |ui| {
            for (v, t) in items {
                if ui.selectable_label(value == v, RichText::new(t).font(theme::font(13.5))).clicked() && value != v {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut dlg) = app.export_dlg.take() else {
        return;
    };
    let mut close = false;
    let e = dlg.entry.clone();
    let dur = e.media.duration_sec.unwrap_or(0.0);
    let has_audio = e.media.has_audio == Some(true);
    let (sw, sh) = (e.media.width.unwrap_or(0) as i32, e.media.height.unwrap_or(0) as i32);
    let modal = egui::Modal::new(Id::new("export")).frame(theme::modal_frame(ctx).inner_margin(0)).show(ctx, |ui| {
        ui.set_width(600.0);
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        let compact = ctx.content_rect().height() < 700.0;
        let p = theme::pal(ui);
        let s = &mut app.s;
        let gif = s.export_format == ExportFormat::Gif;
        // 有大小上限（壓縮）
        let small = !gif && s.mp4_max_mb > 0;
        // 目前設定實際會用的倍率；指定長度但輸入無效時為 None
        let speed = match s.export_mode {
            // 有大小上限時可以原速（只壓縮）
            ExportMode::Speed => Some(if gif || small { s.speed.max(1.0) } else { s.speed.max(SPEED_MIN) }),
            ExportMode::Target => parse_clock(&s.export_target).filter(|_| dur > 0.0).map(|t| speed_for_target(dur, t, gif || small)),
        };
        let mut changed = false;

        // ── 標題 ──
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: 16, top: 20, bottom: 14 }).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(RichText::new(if gif { tr!("製作 GIF", "Make GIF") } else { tr!("製作加速版", "Make sped-up video") }).font(theme::font_bold(18.0)));
                    let name = if is_default_name(&e.media.name) { trf!("{} 的錄影", "Recording from {}", short_date(&e.media.name, e.media.mtime, false)) } else { base_name(&e.media.name) };
                    let info: Vec<String> = [
                        Some(name),
                        (dur > 0.0).then(|| video_clock(dur)),
                        (sw > 0).then(|| format!("{sw}×{sh}")),
                        e.media.fps.map(|f| format!("{f} fps")),
                        Some(if has_audio { tr!("有聲音", "Has audio").into() } else { tr!("無聲音", "No audio").into() }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    ui.label(theme::muted(ui, info.join("・"))).on_hover_text(&e.media.name);
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                        close = true;
                    }
                });
            });
        });
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top() - 8.0, Stroke::new(1.0, p.border));

        // ── 設定 ──
        let body_top = ui.cursor().min.y;
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: PAD, top: 8, bottom: 4 }).show(ui, |ui| {
            // 視窗矮（1024×640 等）時列距縮小，整個對話框才放得下
            ui.spacing_mut().item_spacing.y = if compact { 4.0 } else { 10.0 };
            row(ui, tr!("格式", "Format"), |ui| {
                let mut f = s.export_format;
                if segmented(ui, &mut f, &[(ExportFormat::Mp4, tr!("MP4 影片", "MP4 video")), (ExportFormat::Gif, tr!("GIF 動畫", "Animated GIF"))], true) {
                    // MP4 不能原速（那就是原檔）
                    if f == ExportFormat::Mp4 && s.speed < SPEED_MIN {
                        s.speed = 4.0;
                    }
                    s.export_format = f;
                    changed = true;
                }
            });
            row(ui, tr!("加速", "Speed up"), |ui| {
                let mut m = s.export_mode;
                if segmented(ui, &mut m, &[(ExportMode::Speed, tr!("指定倍率", "By speed")), (ExportMode::Target, tr!("指定長度", "By length"))], true) {
                    s.export_mode = m;
                    changed = true;
                }
            });
            match s.export_mode {
                ExportMode::Speed => row(ui, tr!("倍率", "Speed"), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let mut presets: Vec<(f64, String)> = SPEED_PRESETS.iter().map(|v| (*v, format!("{}×", speed_label(*v)))).collect();
                        if gif || small {
                            presets.insert(0, (1.0, tr!("原速", "Original speed").into()));
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
                        ui.add_space(4.0);
                        let r = ui.add(egui::TextEdit::singleline(&mut dlg.speed_text).desired_width(52.0).font(theme::font(13.5)).margin(vec2(8.0, 5.0)).horizontal_align(Align::Center));
                        dlg.speed_focus = r.has_focus();
                        ui.label(RichText::new("×").font(theme::font(14.0)).color(p.muted));
                        if r.changed() {
                            if let Ok(v) = dlg.speed_text.trim().parse::<f64>() {
                                let min = if gif || small { 1.0 } else { SPEED_MIN };
                                if v.is_finite() && v >= min && v <= SPEED_MAX {
                                    s.speed = (v * 100.0).round() / 100.0;
                                    changed = true;
                                }
                            }
                        }
                    });
                    let range = if gif || small {
                        trf!("可輸入 1–{}×，1× 為原速", "Enter 1–{}×; 1× keeps the original speed", speed_label(SPEED_MAX))
                    } else {
                        trf!("可輸入 {}–{}×", "Enter {}–{}×", speed_label(SPEED_MIN), speed_label(SPEED_MAX))
                    };
                    helper(ui, range, p.muted);
                }),
                ExportMode::Target => row(ui, tr!("長度", "Length"), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for t in TARGET_PRESETS {
                            if Btn::new(t).small().selected(s.export_target == t).show(ui).clicked() {
                                s.export_target = t.into();
                                changed = true;
                            }
                        }
                        ui.add_space(4.0);
                        let r = ui
                            .add(egui::TextEdit::singleline(&mut s.export_target).desired_width(64.0).font(theme::font(13.5)).margin(vec2(8.0, 5.0)).horizontal_align(Align::Center).hint_text("1:00"));
                        if r.changed() {
                            changed = true;
                        }
                        ui.label(RichText::new(tr!("分:秒", "min:sec")).font(theme::font(12.5)).color(p.muted));
                    });
                    let target = parse_clock(&s.export_target);
                    let hint = match (target, speed) {
                        (None, _) => tr!("請輸入長度，例如 1:00、90（秒）或 1:02:03", "Enter a length, e.g. 1:00, 90 (seconds) or 1:02:03").to_string(),
                        _ if dur <= 0.0 => tr!("無法讀取原片長度", "Can't read the original's duration").into(),
                        (Some(t), Some(sp)) if dur / sp > t + 0.5 => {
                            trf!("最多只能加速到 {}×，實際約 {}", "The maximum speed is {}×, so it will be about {}", speed_label(SPEED_MAX), video_clock(dur / sp))
                        }
                        (Some(t), Some(sp)) if dur / sp < t - 0.5 => {
                            let how = if gif {
                                tr!("以原速製作", "it will be made at original speed").to_string()
                            } else {
                                trf!("以最低倍率 {}× 製作", "it will be made at the minimum {}×", speed_label(sp))
                            };
                            trf!("原片只有 {}，{how}", "The original is only {}, so {how}", video_clock(dur))
                        }
                        (_, Some(sp)) => trf!("需要加速 {}×", "Needs {}× speed", speed_label(sp)),
                        _ => String::new(),
                    };
                    helper(ui, hint, if target.is_none() { p.rec } else { p.muted });
                }),
            }
            row(ui, tr!("尺寸", "Size"), |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(ROW_H);
                    if gif {
                        let widths: Vec<(u32, String)> = [320, 480, 640, 960, 1280].iter().map(|w| (*w, trf!("寬 {w} px", "Width {w} px"))).collect();
                        changed |= combo(ui, "gifw", 130.0, &mut s.gif_width, &widths);
                        ui.add_space(8.0);
                        ui.label(RichText::new(tr!("每秒", "Frame rate")).font(theme::font(13.5)).color(p.muted));
                        let fps: Vec<(u32, String)> = [5, 10, 15, 20].iter().map(|f| (*f, trf!("{f} 張", "{f} fps"))).collect();
                        changed |= combo(ui, "giffps", 90.0, &mut s.gif_fps, &fps);
                    } else if sw > 0 && sh > 0 {
                        // 只列出比原片小的選項
                        let opts: Vec<(u32, String)> = MP4_WIDTHS
                            .iter()
                            .copied()
                            .filter(|w| *w == 0 || (*w as i32) < sw)
                            .map(|w| {
                                let (a, b) = scaled_size(sw, sh, w as i32);
                                (w, if w == 0 { trf!("原尺寸（{a}×{b}）", "Original ({a}×{b})") } else { trf!("寬 {w}（{a}×{b}）", "Width {w} ({a}×{b})") })
                            })
                            .collect();
                        if !opts.iter().any(|(w, _)| *w == s.mp4_width) {
                            s.mp4_width = 0;
                        }
                        changed |= combo(ui, "mp4w", 230.0, &mut s.mp4_width, &opts);
                    } else {
                        ui.label(theme::muted(ui, tr!("原尺寸", "Original size")));
                    }
                });
            });
            if !gif {
                row(ui, tr!("大小上限", "Size limit"), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        let opts: Vec<(u32, String)> =
                            crate::ui::settings::MAX_MB.iter().map(|m| (*m, if *m == 0 { tr!("不限", "No limit").to_string() } else { trf!("{m} MB 以內", "Up to {m} MB") })).collect();
                        if combo(ui, "mp4max", 130.0, &mut s.mp4_max_mb, &opts) {
                            // 改回不限：不能原速
                            if s.mp4_max_mb == 0 && s.speed < SPEED_MIN {
                                s.speed = 4.0;
                            }
                            changed = true;
                        }
                        ui.add_space(8.0);
                        let note = if s.mp4_max_mb > 0 {
                            tr!("降低畫質讓檔案小於上限，方便用 Email、LINE 傳送；可選「原速」只壓縮", "Lowers quality to fit the limit for email or LINE; “Original speed” only compresses")
                        } else {
                            ""
                        };
                        ui.label(RichText::new(note).font(theme::font(12.5)).color(p.muted));
                    });
                });
            }
            row(ui, tr!("聲音", "Audio"), |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(ROW_H);
                    if gif {
                        ui.label(theme::muted(ui, tr!("GIF 不含聲音；檔案較大，建議 1 分鐘以內", "GIFs have no audio and are large; keep them under 1 min")));
                    } else if !has_audio {
                        ui.label(theme::muted(ui, tr!("原片沒有聲音", "The original has no audio")));
                    } else {
                        let mut keep = s.keep_audio;
                        if switch(ui, &mut keep, tr!("保留聲音（變速不變調）", "Keep audio (pitch preserved)"), true).changed() {
                            s.keep_audio = keep;
                            changed = true;
                        }
                    }
                });
            });
        });

        // ── 結果：原片 → 輸出 ──
        let ex = speed.and_then(|sp| existing(&e, sp, s.export_format)).cloned();
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: PAD, top: 6, bottom: 0 }).show(ui, |ui| {
            let dims = (sw > 0 && sh > 0).then(|| scaled_size(sw, sh, if gif { s.gif_width as i32 } else { s.mp4_width as i32 }));
            egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(theme::RADIUS)).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let col_w = (ui.available_width() - 56.0) / 2.0;
                let side = |ui: &mut egui::Ui, title: &str, time: String, sub: String| {
                    ui.allocate_ui_with_layout(vec2(col_w, 64.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(title).font(theme::font(12.5)).color(p.muted));
                        ui.label(RichText::new(time).font(theme::font_bold(24.0)).color(p.text));
                        ui.add(egui::Label::new(RichText::new(sub).font(theme::font(12.5)).color(p.muted)).truncate());
                    });
                };
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let from: Vec<String> = [(sw > 0).then(|| format!("{sw}×{sh}")), Some(format_bytes(e.media.bytes))].into_iter().flatten().collect();
                    side(ui, tr!("原片", "Original"), if dur > 0.0 { video_clock(dur) } else { "—".into() }, from.join("・"));
                    // 箭頭
                    let (r, _) = ui.allocate_exact_size(vec2(56.0, 64.0), Sense::hover());
                    let (a, b) = (pos2(r.left() + 12.0, r.top() + 34.0), pos2(r.right() - 16.0, r.top() + 34.0));
                    let st = Stroke::new(2.0, p.muted);
                    ui.painter().line_segment([a, b], st);
                    ui.painter().line_segment([b, b + vec2(-6.0, -6.0)], st);
                    ui.painter().line_segment([b, b + vec2(-6.0, 6.0)], st);
                    let out_time = match speed {
                        Some(sp) if dur > 0.0 => video_clock(dur / sp),
                        _ => "—".into(),
                    };
                    let out_sub = match (dims, speed.filter(|_| dur > 0.0)) {
                        (Some((w, h)), Some(sp)) => {
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
                            let cap = (s.mp4_max_mb > 0 && !gif).then_some(s.mp4_max_mb as f64 * 1024.0 * 1024.0);
                            match cap {
                                Some(c) if hi > c => trf!("{w}×{h}・{} 以內", "{w}×{h}・up to {}", format_bytes(c as u64)),
                                _ => trf!("{w}×{h}・約 {}～{}", "{w}×{h}・about {}–{}", format_bytes(lo as u64), format_bytes(hi as u64)),
                            }
                        }
                        _ => String::new(),
                    };
                    let title = if gif {
                        "GIF"
                    } else if speed.is_some_and(|sp| sp <= 1.0) {
                        tr!("壓縮版", "Compressed")
                    } else {
                        tr!("加速版", "Sped-up")
                    };
                    side(ui, title, out_time, out_sub);
                });
                // 存成的檔名、1 小時的錄影會變成多長
                let r = ui.cursor();
                ui.painter().hline(r.x_range(), r.top() + 2.0, Stroke::new(1.0, p.border));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let per_hour = speed.filter(|sp| *sp > 1.0).map(|sp| {
                        let fit = if sp >= 8.0 { tr!("，適合縮時影片", ", good for time-lapse") } else { "" };
                        trf!("{}×：1 小時的錄影 ≈ {}{fit}", "{}×: a 1-hour recording ≈ {}{fit}", speed_label(sp), human_duration(3600.0 / sp))
                    });
                    let name = speed.map(|sp| output_name(&e, sp, s.export_format)).unwrap_or_default();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(t) = per_hour {
                            ui.label(RichText::new(t).font(theme::font(12.5)).color(p.muted));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if !name.is_empty() {
                                ui.label(RichText::new(tr!("存成", "Saved as")).font(theme::font(12.5)).color(p.muted));
                                ui.add(egui::Label::new(RichText::new(&name).font(theme::mono(12.0)).color(p.text)).truncate()).on_hover_text(&name);
                            }
                        });
                    });
                });
            });

            // 提醒：已有同倍率的成品、加速後太短
            let what = if gif {
                format!("GIF{}", speed.filter(|v| *v > 1.0).map(|v| format!(" {}×", speed_label(v))).unwrap_or_else(|| tr!("（原速）", " (original speed)").into()))
            } else {
                trf!("{}× 加速版", "{}× sped-up video", speed_label(speed.unwrap_or(0.0)))
            };
            if let Some(x) = &ex {
                ui.add_space(2.0);
                egui::Frame::new().fill(p.accent_soft).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if Btn::new(tr!("播放現有的", "Play existing")).icon(Icon::Play).small().show(ui).clicked() {
                                let _ = actions::open(actions::OpenAction::Play, &x.media.path);
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let info = format!("{}{}", x.media.duration_sec.map(|d| format!("{}・", video_clock(d))).unwrap_or_default(), format_bytes(x.media.bytes));
                                let text = trf!("已經有 {what}（{info}），再製作會另存一份", "A {what} already exists ({info}). Making it again saves another copy");
                                ui.add(egui::Label::new(RichText::new(text).font(theme::font(13.0))).wrap());
                            });
                        });
                    });
                });
            }
            if let Some(sp) = speed.filter(|sp| dur > 0.0 && dur / sp < TOO_SHORT_SEC && *sp > 1.0) {
                ui.add_space(2.0);
                egui::Frame::new().fill(p.warn_soft).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if s.export_mode == ExportMode::Speed && Btn::new(tr!("改用指定長度", "Set length instead")).small().show(ui).clicked() {
                                // 預設 1:00，原片不到 1 分鐘時用原片長度的一半（至少 5 秒）
                                let half = (dur / 2.0).round().max(5.0) as u32;
                                s.export_target = if dur < 60.0 { format!("0:{half:02}") } else { "1:00".into() };
                                s.export_mode = ExportMode::Target;
                                changed = true;
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let text = trf!("加速後只有 {:.1} 秒，可能太快看不清楚", "Only {:.1} s after speeding up; it may be too fast to follow", dur / sp);
                                ui.add(egui::Label::new(RichText::new(text).font(theme::font(13.0)).color(p.warn)).wrap());
                            });
                        });
                    });
                });
            }
        });

        // 出現提醒而變高後就不再縮回（切換設定時按鈕不會跳動）
        let used = ui.cursor().min.y - body_top;
        if used < dlg.body_h {
            ui.add_space(dlg.body_h - used);
        }
        dlg.body_h = dlg.body_h.max(used);
        if changed {
            app.save_settings();
        }

        // ── 按鈕 ──
        ui.add_space(8.0);
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: PAD, top: 14, bottom: 18 }).show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let s = &app.s;
                // 已有同倍率的成品：「再製作一份…」
                let make = if ex.is_some() { tr!("再製作一份", "Make another") } else { tr!("製作", "Make") };
                let cap = if s.mp4_max_mb > 0 { trf!("（{} MB 以內）", " (up to {} MB)", s.mp4_max_mb) } else { String::new() };
                let label = match speed {
                    None => make.to_string(),
                    Some(sp) if gif => format!("{make} GIF{}", if sp > 1.0 { trf!("（{}×）", " ({}×)", speed_label(sp)) } else { String::new() }),
                    Some(sp) if sp <= 1.0 => trf!("{make}壓縮版{cap}", "{make} compressed video{cap}"),
                    Some(sp) => trf!("{make} {}× 加速版{cap}", "{make} {}× sped-up video{cap}", speed_label(sp)),
                };
                if Btn::new(label).primary().enabled(speed.is_some() && !dlg.busy).min_width(170.0).show(ui).clicked() {
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
                            mp4_max_mb: if gif { 0.0 } else { s.mp4_max_mb as f64 },
                        };
                        let core = app.core.clone();
                        app.spawn(async move { actions::export_start(&core, &req).await }, |app, r| match r {
                            Ok(()) => {
                                app.export_dlg = None;
                                app.dismissed_job = None;
                                app.toast(tr!("已開始製作，進度顯示在右側", "Started. Progress is shown on the right"), false);
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
                if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                    close = true;
                }
            });
        });
    });
    if modal.should_close() {
        close = true;
    }
    if !close && app.export_dlg.is_none() {
        app.export_dlg = Some(dlg);
    }
}
