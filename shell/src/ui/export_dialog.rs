//! 製作加速版 / 壓縮 / GIF、WebP 動圖：最上面選要做哪一種；加速方式可「指定倍率」或「指定長度」（自動換算倍率），
//! 送出後在右側「工作」區顯示進度。這次的選擇按「製作」才存成下次的預設（按取消不會改到）。

use super::dialogs::{base_name, file_name, is_default_name, short_date};
use super::settings::{ExportMode, UiSettings};
use super::theme::{self, segmented, switch, Btn, Icon};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, Id, Layout, RichText, Sense, Stroke};
use screenrecorder_core::actions::{self, ExportRequest};
use screenrecorder_core::format::{
    compressed_file_name, estimate_bytes, export_file_name, format_bytes, human_duration, parse_clock, scaled_size, short_clock, speed_for_target, speed_label, EstimateInput, MP4_WIDTHS, SPEED_MAX,
    SPEED_MIN,
};
use screenrecorder_core::types::{ExportFormat, ExportInfo, LibraryEntry};
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicU32, Ordering};

const SPEED_PRESETS: [f64; 5] = [1.5, 2.0, 4.0, 8.0, 16.0];
const TARGET_PRESETS: [&str; 4] = ["0:30", "1:00", "3:00", "5:00"];
/// 加速後短於這個秒數時提醒可能太快
const TOO_SHORT_SEC: f64 = 3.0;
/// 壓縮可選的大小上限（MB）
const COMPRESS_MB: [u32; 4] = [10, 25, 50, 100];
/// 上次壓縮選的大小上限（這次開啟程式期間記住；不影響加速版的大小上限）
static LAST_COMPRESS_MB: AtomicU32 = AtomicU32::new(25);

/// 要做哪一種
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 加速版（MP4）
    Speed,
    /// 原速、只壓縮到某個大小以內（傳 LINE / Email）
    Compress,
    /// GIF / WebP 動圖
    Anim,
}

/// 這次的選擇（開啟時從設定讀，按「製作」才寫回設定）
#[derive(Clone)]
struct Opts {
    /// 動圖的格式（GIF 或 WebP）
    anim: ExportFormat,
    export_mode: ExportMode,
    export_target: String,
    speed: f64,
    gif_width: u32,
    gif_fps: u32,
    mp4_width: u32,
    /// 加速版的大小上限（0 = 不限）
    mp4_max_mb: u32,
    keep_audio: bool,
    /// 動圖來回播放（不存成預設）
    bounce: bool,
}

impl Opts {
    fn from(s: &UiSettings) -> Opts {
        Opts {
            anim: if s.export_format.is_animation() { s.export_format } else { ExportFormat::Gif },
            export_mode: s.export_mode,
            export_target: s.export_target.clone(),
            speed: s.speed,
            gif_width: s.gif_width,
            gif_fps: s.gif_fps,
            mp4_width: s.mp4_width,
            mp4_max_mb: s.mp4_max_mb,
            keep_audio: s.keep_audio,
            bounce: false,
        }
    }
}

pub struct ExportDialog {
    pub entry: LibraryEntry,
    mode: Mode,
    o: Opts,
    /// 壓縮的大小上限（MB）
    compress_mb: u32,
    speed_text: String,
    speed_focus: bool,
    /// 內容最高到過多高：切換格式、加速方式時視窗不會忽大忽小（只會變高一次，不會縮回去）
    body_h: f32,
    /// 對話框上緣的位置：開啟時置中後固定，內容變高時往下長（超出視窗才往上移），按鈕不會跟著上下跳
    top: Option<f32>,
    /// 對話框最高到過多高、顯示了幾格（前幾格還在量大小）
    max_h: f32,
    frames: u32,
    /// 另存新檔選的位置；None = 和原片同一個資料夾、自動取名
    output: Option<String>,
    busy: bool,
}

impl ExportDialog {
    /// compress：從「壓縮（傳 LINE / Email）」開啟
    pub fn new(entry: LibraryEntry, s: &UiSettings, compress: bool) -> Self {
        let mut o = Opts::from(s);
        let mode = if compress {
            Mode::Compress
        } else if s.export_format.is_animation() {
            Mode::Anim
        } else {
            Mode::Speed
        };
        // 加速版不能原速（那就是原檔；只壓縮改用「壓縮」）
        if mode == Mode::Speed && o.speed < SPEED_MIN {
            o.speed = 4.0;
        }
        let compress_mb = if compress && COMPRESS_MB.contains(&s.mp4_max_mb) { s.mp4_max_mb } else { LAST_COMPRESS_MB.load(Ordering::Relaxed) };
        Self { entry, mode, o, compress_mb, speed_text: String::new(), speed_focus: false, body_h: 0.0, top: None, max_h: 0.0, frames: 0, output: None, busy: false }
    }

    fn format(&self) -> ExportFormat {
        if self.mode == Mode::Anim {
            self.o.anim
        } else {
            ExportFormat::Mp4
        }
    }

    fn max_mb(&self) -> u32 {
        match self.mode {
            Mode::Speed => self.o.mp4_max_mb,
            Mode::Compress => self.compress_mb,
            Mode::Anim => 0,
        }
    }

    /// 按了「製作」：這次的選擇存成下次的預設
    fn save(&self, s: &mut UiSettings) {
        let o = &self.o;
        s.export_format = self.format();
        s.keep_audio = o.keep_audio;
        s.mp4_width = o.mp4_width;
        if self.mode == Mode::Compress {
            LAST_COMPRESS_MB.store(self.compress_mb, Ordering::Relaxed);
            return;
        }
        s.export_mode = o.export_mode;
        s.export_target = o.export_target.clone();
        s.speed = o.speed;
        if self.mode == Mode::Anim {
            (s.gif_width, s.gif_fps) = (o.gif_width, o.gif_fps);
        } else {
            s.mp4_max_mb = o.mp4_max_mb;
        }
    }
}

/// 這支錄影已經有同格式、同倍率的成品（壓縮：原速的 MP4）
fn existing(e: &LibraryEntry, speed: f64, format: ExportFormat) -> Option<&ExportInfo> {
    e.exports.iter().find(|x| x.format.unwrap_or_default() == format && (x.speed - speed).abs() < 0.005)
}

/// 實際會存成的檔名：同名已存在時會加上 _2、_3
fn output_name(e: &LibraryEntry, speed: f64, format: ExportFormat, max_mb: u32) -> String {
    let name = if format == ExportFormat::Mp4 && max_mb > 0 && speed <= 1.0 { compressed_file_name(&e.media.name, max_mb as f64) } else { export_file_name(&e.media.name, speed, format) };
    let taken: Vec<String> = e.exports.iter().map(|x| x.media.name.to_lowercase()).collect();
    if !taken.contains(&name.to_lowercase()) {
        return name;
    }
    let dot = name.rfind('.').unwrap_or(name.len());
    (2..).map(|i| format!("{}_{i}{}", &name[..dot], &name[dot..])).find(|n| !taken.contains(&n.to_lowercase())).unwrap_or(name)
}

/// 左邊標籤欄的寬度
const LABEL_W: f32 = 72.0;
/// 每一列控制項的高度（標籤垂直置中）
const ROW_H: f32 = 34.0;
const PAD: i8 = 24;
const WIDTH: f32 = 620.0;

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

/// 說明文字（控制項下方）；太長時換行，不會被截掉
fn helper(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text.into()).font(theme::font(12.5)).color(color)).wrap());
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

/// 有底色的提醒（右邊可放一個按鈕）；回傳按鈕是否按下
fn notice(ui: &mut egui::Ui, fill: Color32, text: &str, color: Color32, button: Option<(&str, Icon)>) -> bool {
    let mut clicked = false;
    ui.add_space(2.0);
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some((label, icon)) = button {
                    clicked = Btn::new(label).icon(icon).small().show(ui).clicked();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(text).font(theme::font(13.0)).color(color)).wrap());
                });
            });
        });
    });
    clicked
}

/// 每秒幾張
fn fps_text(f: f64) -> String {
    trf!("每秒 {} 張", "{} fps", screenrecorder_core::format::num((f * 100.0).round() / 100.0))
}

/// 用 Windows 的「另存新檔」選存檔位置（不用打字）
fn choose_output(app: &mut UiApp, dir: String, name: String, format: ExportFormat) {
    let filter: (&'static str, &'static str) = match format {
        ExportFormat::Gif => (tr!("GIF 動畫", "Animated GIF"), "*.gif"),
        ExportFormat::Webp => (tr!("WebP 動圖", "Animated WebP"), "*.webp"),
        ExportFormat::Mp4 => (tr!("MP4 影片", "MP4 video"), "*.mp4"),
    };
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || screenrecorder_core::filepick::save_file(tr!("存成", "Save as"), Some(std::path::PathBuf::from(dir)), &name, &[filter])).await.unwrap_or(Ok(None))
        },
        move |app, r| match r {
            Ok(Some((path, _))) => {
                if let Some(d) = &mut app.export_dlg {
                    d.output = Some(path.display().to_string());
                }
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut dlg) = app.export_dlg.take() else {
        return;
    };
    let mut close = false;
    let mut make = false;
    let mut play_existing: Option<LibraryEntry> = None;
    let mut change_output: Option<(String, String)> = None;
    let e = dlg.entry.clone();
    let dur = e.media.duration_sec.unwrap_or(0.0);
    let has_audio = e.media.has_audio == Some(true);
    let (sw, sh) = (e.media.width.unwrap_or(0) as i32, e.media.height.unwrap_or(0) as i32);
    let screen = ctx.content_rect();
    // 上緣固定（第一次顯示時置中，之後內容變高只往下長）
    let mut area = egui::Modal::default_area(Id::new("export"));
    if let Some(top) = dlg.top {
        area = area.anchor(Align2::CENTER_TOP, vec2(0.0, top));
    }
    let modal = egui::Modal::new(Id::new("export")).area(area).frame(theme::modal_frame(ctx).inner_margin(0)).show(ctx, |ui| {
        ui.set_width(WIDTH);
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        let compact = screen.height() < 700.0;
        let p = theme::pal(ui);
        let format = dlg.format();
        let gif = dlg.mode == Mode::Anim;
        let anim = if format == ExportFormat::Webp { "WebP" } else { "GIF" };
        let max_mb = dlg.max_mb();
        // 目前設定實際會用的倍率；指定長度但輸入無效時為 None
        let speed = match (dlg.mode, dlg.o.export_mode) {
            (Mode::Compress, _) => Some(1.0),
            (_, ExportMode::Speed) => Some(if gif { dlg.o.speed.max(1.0) } else { dlg.o.speed.max(SPEED_MIN) }),
            (_, ExportMode::Target) => parse_clock(&dlg.o.export_target).filter(|_| dur > 0.0).map(|t| speed_for_target(dur, t, gif)),
        };
        // 壓縮：原片已經比上限小
        let small_enough = dlg.mode == Mode::Compress && e.media.bytes > 0 && e.media.bytes <= max_mb as u64 * 1024 * 1024;

        // ── 標題 ──
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: 16, top: 20, bottom: 14 }).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let title = match dlg.mode {
                        Mode::Speed => tr!("製作加速版", "Make sped-up video").to_string(),
                        Mode::Compress => tr!("壓縮影片", "Compress video").to_string(),
                        Mode::Anim => trf!("製作 {anim}", "Make {anim}"),
                    };
                    ui.label(RichText::new(title).font(theme::font_bold(18.0)));
                    let name = if is_default_name(&e.media.name) { trf!("{} 的錄影", "Recording from {}", short_date(&e.media.name, e.media.time_ms(), false)) } else { base_name(&e.media.name) };
                    let info: Vec<String> = [
                        Some(name),
                        (dur > 0.0).then(|| short_clock(dur)),
                        (sw > 0).then(|| format!("{sw}×{sh}")),
                        e.media.fps.map(fps_text),
                        Some(if has_audio { tr!("有聲音", "Has audio").into() } else { tr!("無聲音", "No audio").into() }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    ui.set_max_width(WIDTH - 80.0);
                    ui.add(egui::Label::new(theme::muted(ui, info.join("・"))).truncate()).on_hover_text(&e.media.name);
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
            let o = &mut dlg.o;
            row(ui, tr!("製作", "Make"), |ui| {
                let mut m = dlg.mode;
                if segmented(ui, &mut m, &[(Mode::Speed, tr!("加速版", "Sped-up video")), (Mode::Compress, tr!("壓縮", "Compress")), (Mode::Anim, tr!("GIF / WebP 動圖", "GIF / WebP"))], true) {
                    dlg.mode = m;
                    if m == Mode::Speed && o.speed < SPEED_MIN {
                        o.speed = 4.0;
                    }
                }
            });
            if dlg.mode == Mode::Anim {
                row(ui, tr!("格式", "Format"), |ui| {
                    segmented(ui, &mut o.anim, &[(ExportFormat::Gif, tr!("GIF 動畫", "Animated GIF")), (ExportFormat::Webp, tr!("WebP 動圖", "Animated WebP"))], true);
                });
            }
            if dlg.mode != Mode::Compress {
                row(ui, tr!("加速", "Speed up"), |ui| {
                    segmented(ui, &mut o.export_mode, &[(ExportMode::Speed, tr!("指定倍率", "By speed")), (ExportMode::Target, tr!("指定長度", "By length"))], true);
                });
                match o.export_mode {
                    ExportMode::Speed => row(ui, tr!("倍率", "Speed"), |ui| {
                        ui.horizontal(|ui| {
                            ui.set_height(ROW_H);
                            ui.spacing_mut().item_spacing.x = 6.0;
                            // 動圖可以原速；「原速」固定放在最前面，切換格式時其他按鈕不會移動
                            let mut presets: Vec<(f64, String)> = SPEED_PRESETS.iter().map(|v| (*v, format!("{}×", speed_label(*v)))).collect();
                            if gif {
                                presets.insert(0, (1.0, tr!("原速", "Original speed").into()));
                            }
                            for (v, label) in &presets {
                                if Btn::new(label).small().selected((o.speed - v).abs() < 1e-9).show(ui).clicked() {
                                    o.speed = *v;
                                }
                            }
                            if !dlg.speed_focus {
                                dlg.speed_text = speed_label(o.speed);
                            }
                            ui.add_space(4.0);
                            let r = ui.add(egui::TextEdit::singleline(&mut dlg.speed_text).desired_width(52.0).font(theme::font(13.5)).margin(vec2(8.0, 5.0)).horizontal_align(Align::Center));
                            dlg.speed_focus = r.has_focus();
                            ui.label(RichText::new("×").font(theme::font(14.0)).color(p.muted));
                            if r.changed() {
                                if let Ok(v) = dlg.speed_text.trim().parse::<f64>() {
                                    let min = if gif { 1.0 } else { SPEED_MIN };
                                    if v.is_finite() && v >= min && v <= SPEED_MAX {
                                        o.speed = (v * 100.0).round() / 100.0;
                                    }
                                }
                            }
                        });
                        let range = if gif {
                            trf!("可輸入 1–{}×，1× 為原速", "Enter 1–{}×; 1× keeps the original speed", speed_label(SPEED_MAX))
                        } else {
                            trf!("可輸入 {}–{}×", "Enter {}–{}×", speed_label(SPEED_MIN), speed_label(SPEED_MAX))
                        };
                        // 1 小時的錄影會變成多長
                        let per_hour = speed.filter(|sp| *sp > 1.0).map(|sp| {
                            let fit = if sp >= 8.0 { tr!("，適合縮時影片", ", good for time-lapse") } else { "" };
                            trf!("・{}×：1 小時的錄影 ≈ {}{fit}", " · {}×: a 1-hour recording ≈ {}{fit}", speed_label(sp), human_duration(3600.0 / sp))
                        });
                        helper(ui, format!("{range}{}", per_hour.unwrap_or_default()), p.muted);
                    }),
                    ExportMode::Target => row(ui, tr!("長度", "Length"), |ui| {
                        ui.horizontal(|ui| {
                            ui.set_height(ROW_H);
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for t in TARGET_PRESETS {
                                if Btn::new(t).small().selected(o.export_target == t).show(ui).clicked() {
                                    o.export_target = t.into();
                                }
                            }
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut o.export_target).desired_width(64.0).font(theme::font(13.5)).margin(vec2(8.0, 5.0)).horizontal_align(Align::Center).hint_text("1:00"),
                            );
                            ui.label(RichText::new(tr!("分:秒", "min:sec")).font(theme::font(12.5)).color(p.muted));
                        });
                        let target = parse_clock(&o.export_target);
                        let hint = match (target, speed) {
                            (None, _) => tr!("請輸入長度，例如 1:00、90（秒）或 1:02:03", "Enter a length, e.g. 1:00, 90 (seconds) or 1:02:03").to_string(),
                            _ if dur <= 0.0 => tr!("無法讀取原片長度", "Can't read the original's duration").into(),
                            (Some(t), Some(sp)) if dur / sp > t + 0.5 => {
                                trf!("最多只能加速到 {}×，實際約 {}", "The maximum speed is {}×, so it will be about {}", speed_label(SPEED_MAX), short_clock(dur / sp))
                            }
                            (Some(t), Some(sp)) if dur / sp < t - 0.5 => {
                                let how = if gif {
                                    tr!("以原速製作", "it will be made at original speed").to_string()
                                } else {
                                    trf!("以最低倍率 {}× 製作", "it will be made at the minimum {}×", speed_label(sp))
                                };
                                trf!("原片只有 {}，{how}", "The original is only {}, so {how}", short_clock(dur))
                            }
                            (_, Some(sp)) => trf!("需要加速 {}×", "Needs {}× speed", speed_label(sp)),
                            _ => String::new(),
                        };
                        helper(ui, hint, if target.is_none() { p.rec } else { p.muted });
                    }),
                }
            }
            // 壓縮：大小上限放在最前面（這就是要做的事）
            let limit_row = |ui: &mut egui::Ui, mode: Mode, value: &mut u32| {
                row(ui, tr!("大小上限", "Size limit"), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        let list: &[u32] = if mode == Mode::Compress { &COMPRESS_MB } else { &crate::ui::settings::MAX_MB };
                        let opts: Vec<(u32, String)> = list.iter().map(|m| (*m, if *m == 0 { tr!("不限", "No limit").to_string() } else { trf!("{m} MB 以內", "Up to {m} MB") })).collect();
                        combo(ui, "mp4max", 130.0, value, &opts);
                    });
                    // 說明放在下方自己一行（太長時換行），不會把下拉選單擠開
                    if mode == Mode::Compress {
                        helper(ui, tr!("降低畫質讓檔案小於上限，方便用 LINE、Email 傳送", "Lowers the quality so the file fits the limit, for sending via LINE or email"), p.muted);
                    } else if *value > 0 {
                        helper(
                            ui,
                            tr!("降低畫質讓加速版小於上限；只想壓縮、不加速請選上面的「壓縮」", "Lowers quality so the sped-up video fits the limit. To only compress, choose “Compress” above"),
                            p.muted,
                        );
                    }
                });
            };
            if dlg.mode == Mode::Compress {
                limit_row(ui, Mode::Compress, &mut dlg.compress_mb);
            }
            row(ui, tr!("尺寸", "Size"), |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(ROW_H);
                    if gif {
                        let widths: Vec<(u32, String)> = [320, 480, 640, 960, 1280].iter().map(|w| (*w, trf!("寬 {w} px", "Width {w} px"))).collect();
                        combo(ui, "gifw", 130.0, &mut o.gif_width, &widths);
                        ui.add_space(8.0);
                        let fps: Vec<(u32, String)> = [5, 10, 15, 20].iter().map(|f| (*f, fps_text(*f as f64))).collect();
                        combo(ui, "giffps", 110.0, &mut o.gif_fps, &fps);
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
                        if !opts.iter().any(|(w, _)| *w == o.mp4_width) {
                            o.mp4_width = 0;
                        }
                        combo(ui, "mp4w", 230.0, &mut o.mp4_width, &opts);
                    } else {
                        ui.label(theme::muted(ui, tr!("原尺寸", "Original size")));
                    }
                });
            });
            if dlg.mode == Mode::Speed {
                limit_row(ui, Mode::Speed, &mut o.mp4_max_mb);
            }
            if gif {
                row(ui, tr!("播放", "Playback"), |ui| {
                    let one_way = speed.filter(|_| dur > 0.0).map(|sp| dur / sp);
                    let fits = one_way.is_none_or(|t| t <= screenrecorder_core::args::BOUNCE_MAX_SEC);
                    if !fits {
                        o.bounce = false;
                    }
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        ui.add_enabled_ui(fits, |ui| {
                            theme::check(ui, &mut o.bounce, RichText::new(tr!("來回播放（播完倒著播回來）", "Back and forth (plays, then reverses)")).font(theme::font(13.0)));
                        });
                    });
                    let note = if fits {
                        tr!("循環播放時不會從結尾跳回開頭；長度變兩倍", "Loops smoothly without jumping back to the start; twice as long").to_string()
                    } else {
                        trf!(
                            "只能用在 {} 秒以內的動圖（可以加速或先剪短）",
                            "Only for animations up to {} s (speed it up or trim it first)",
                            screenrecorder_core::format::num(screenrecorder_core::args::BOUNCE_MAX_SEC)
                        )
                    };
                    helper(ui, note, p.muted);
                });
            }
            row(ui, tr!("聲音", "Audio"), |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(ROW_H);
                    if format == ExportFormat::Webp {
                        ui.add(
                            egui::Label::new(theme::muted(
                                ui,
                                tr!(
                                    "WebP 動圖不含聲音；檔案約為 GIF 的 1/6、顏色更好，瀏覽器、Slack、Discord 都能看",
                                    "WebP has no audio; about 1/6 the size of a GIF with better colors, and works in browsers, Slack and Discord"
                                ),
                            ))
                            .wrap(),
                        );
                    } else if gif {
                        ui.label(theme::muted(ui, tr!("GIF 不含聲音；檔案較大，建議 1 分鐘以內", "GIFs have no audio and are large; keep them under 1 min")));
                    } else if !has_audio {
                        ui.label(theme::muted(ui, tr!("原片沒有聲音", "The original has no audio")));
                    } else {
                        let label = if dlg.mode == Mode::Compress { tr!("保留聲音", "Keep audio") } else { tr!("保留聲音（變速不變調）", "Keep audio (pitch preserved)") };
                        switch(ui, &mut o.keep_audio, label, true);
                    }
                });
            });
        });
        // 存檔位置的副檔名跟格式不合（換了格式）：改回自動取名
        if dlg.output.as_deref().is_some_and(|o| !o.to_lowercase().ends_with(&format!(".{}", format.ext()))) {
            dlg.output = None;
        }

        // ── 結果：原片 → 輸出 ──
        let ex = speed.and_then(|sp| existing(&e, sp, format)).cloned();
        let o = dlg.o.clone();
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: PAD, top: 6, bottom: 0 }).show(ui, |ui| {
            let dims = (sw > 0 && sh > 0).then(|| scaled_size(sw, sh, if gif { o.gif_width as i32 } else { o.mp4_width as i32 }));
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
                    side(ui, tr!("原片", "Original"), if dur > 0.0 { short_clock(dur) } else { "—".into() }, from.join("・"));
                    // 箭頭
                    let (r, _) = ui.allocate_exact_size(vec2(56.0, 64.0), Sense::hover());
                    let (a, b) = (pos2(r.left() + 12.0, r.top() + 34.0), pos2(r.right() - 16.0, r.top() + 34.0));
                    let st = Stroke::new(2.0, p.muted);
                    ui.painter().line_segment([a, b], st);
                    ui.painter().line_segment([b, b + vec2(-6.0, -6.0)], st);
                    ui.painter().line_segment([b, b + vec2(-6.0, 6.0)], st);
                    let twice = if gif && o.bounce { 2.0 } else { 1.0 };
                    let out_time = match speed {
                        Some(sp) if dur > 0.0 => short_clock(dur / sp * twice),
                        _ => "—".into(),
                    };
                    let out_sub = match (dims, speed.filter(|_| dur > 0.0)) {
                        (Some((w, h)), Some(sp)) => {
                            let (lo, hi) = estimate_bytes(&EstimateInput {
                                format,
                                src_bytes: e.media.bytes as f64,
                                src_sec: dur,
                                src_width: sw as f64,
                                src_height: sh as f64,
                                speed: sp,
                                width: w as f64,
                                height: h as f64,
                                gif_fps: Some(o.gif_fps as f64),
                            });
                            let (lo, hi) = (lo * twice, hi * twice);
                            let cap = (max_mb > 0).then_some(max_mb as f64 * 1024.0 * 1024.0);
                            match cap {
                                Some(c) if hi > c || dlg.mode == Mode::Compress => trf!("{w}×{h}・{} 以內", "{w}×{h}・up to {}", format_bytes(c as u64)),
                                _ => trf!("{w}×{h}・約 {}～{}", "{w}×{h}・about {}–{}", format_bytes(lo as u64), format_bytes(hi as u64)),
                            }
                        }
                        _ => String::new(),
                    };
                    let title = match dlg.mode {
                        Mode::Anim => anim,
                        Mode::Compress => tr!("壓縮版", "Compressed"),
                        Mode::Speed => tr!("加速版", "Sped-up"),
                    };
                    side(ui, title, out_time, out_sub);
                });
                // 存成的檔名與位置：「存成 操作示範_4x.mp4（和原片同一個資料夾）」，可按「變更…」另存到別處
                let r = ui.cursor();
                ui.painter().hline(r.x_range(), r.top() + 2.0, Stroke::new(1.0, p.border));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let name = speed.map(|sp| output_name(&e, sp, format, max_mb)).unwrap_or_default();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let src_dir = std::path::Path::new(&e.media.path).parent().map(|d| d.display().to_string()).unwrap_or_default();
                        let tip = tr!("用「另存新檔」選別的資料夾或檔名", "Choose another folder or file name with Save As");
                        if Btn::new(tr!("變更…", "Change…")).small().enabled(!name.is_empty()).tooltip(tip).show(ui).clicked() {
                            let (dir, n) = match &dlg.output {
                                Some(o) => (std::path::Path::new(o).parent().map(|d| d.display().to_string()).unwrap_or(src_dir.clone()), file_name(o)),
                                None => (src_dir.clone(), name.clone()),
                            };
                            change_output = Some((dir, n));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if name.is_empty() {
                                return;
                            }
                            ui.label(RichText::new(tr!("存成", "Save as")).font(theme::font(12.5)).color(p.muted));
                            let (shown, where_, full) = match &dlg.output {
                                Some(o) => {
                                    let dir = std::path::Path::new(o).parent().map(|d| d.display().to_string()).unwrap_or_default();
                                    (file_name(o), trf!("（在 {dir}）", " (in {dir})"), o.clone())
                                }
                                None => (name.clone(), tr!("（和原片同一個資料夾）", " (same folder as the original)").to_string(), std::path::Path::new(&src_dir).join(&name).display().to_string()),
                            };
                            let mut job = egui::text::LayoutJob::default();
                            job.append(&shown, 0.0, egui::TextFormat::simple(theme::font_bold(12.5), p.text));
                            job.append(&where_, 0.0, egui::TextFormat::simple(theme::font(12.5), p.muted));
                            ui.add(egui::Label::new(job).truncate()).on_hover_text(full);
                        });
                    });
                });
            });

            // 提醒：已經夠小、已有同倍率的成品、加速後太短
            if small_enough {
                let text = trf!("原片只有 {}，已小於 {max_mb} MB，不用壓縮", "The original is only {}, already under {max_mb} MB. No need to compress", format_bytes(e.media.bytes));
                notice(ui, p.ok_soft, &text, p.ok, None);
            } else if let Some(x) = &ex {
                let what = match dlg.mode {
                    Mode::Anim => format!("{anim}{}", speed.filter(|v| *v > 1.0).map(|v| format!(" {}×", speed_label(v))).unwrap_or_else(|| tr!("（原速）", " (original speed)").into())),
                    Mode::Compress => tr!("壓縮版", "compressed version").to_string(),
                    Mode::Speed => trf!("{}× 加速版", "{}× sped-up video", speed_label(speed.unwrap_or(0.0))),
                };
                let info = format!("{}{}", x.media.duration_sec.map(|d| format!("{}・", short_clock(d))).unwrap_or_default(), format_bytes(x.media.bytes));
                let text = trf!("已經有 {what}（{info}），再製作會另存一份", "A {what} already exists ({info}). Making it again saves another copy");
                if notice(ui, p.accent_soft, &text, p.text, Some((tr!("播放現有的", "Play existing"), Icon::Play))) {
                    play_existing = Some(LibraryEntry { media: x.media.clone(), exports: vec![], hits: vec![] });
                }
            }
            if let Some(sp) = speed.filter(|sp| dlg.mode != Mode::Compress && dur > 0.0 && dur / sp < TOO_SHORT_SEC && *sp > 1.0) {
                let text = trf!("加速後只有 {:.1} 秒，可能太快看不清楚", "Only {:.1} s after speeding up; it may be too fast to follow", dur / sp);
                let button = (dlg.o.export_mode == ExportMode::Speed).then_some((tr!("改用指定長度", "Set length instead"), Icon::Clock));
                if notice(ui, p.warn_soft, &text, p.warn, button) {
                    // 預設 1:00，原片不到 1 分鐘時用原片長度的一半（至少 5 秒）
                    let half = (dur / 2.0).round().max(5.0) as u32;
                    dlg.o.export_target = if dur < 60.0 { format!("0:{half:02}") } else { "1:00".into() };
                    dlg.o.export_mode = ExportMode::Target;
                }
            }
        });

        // 出現提醒而變高後就不再縮回（切換設定時按鈕不會跳動）
        let used = ui.cursor().min.y - body_top;
        if used < dlg.body_h {
            ui.add_space(dlg.body_h - used);
        }
        dlg.body_h = dlg.body_h.max(used);

        // ── 按鈕 ──
        ui.add_space(8.0);
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
        egui::Frame::new().inner_margin(egui::Margin { left: PAD, right: PAD, top: 14, bottom: 18 }).show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 已有同倍率的成品：「再製作一份…」
                let again = ex.is_some();
                let make_word = if again { tr!("再製作一份", "Make another") } else { tr!("製作", "Make") };
                let cap = if max_mb > 0 { trf!("（{max_mb} MB 以內）", " (up to {max_mb} MB)") } else { String::new() };
                let label = match (dlg.mode, speed) {
                    (_, None) => make_word.to_string(),
                    (Mode::Compress, _) if again => trf!("再壓縮一份（{max_mb} MB 以內）", "Compress again (up to {max_mb} MB)"),
                    (Mode::Compress, _) => trf!("壓縮到 {max_mb} MB 以內", "Compress to under {max_mb} MB"),
                    (Mode::Anim, Some(sp)) => format!("{make_word} {anim}{}", if sp > 1.0 { trf!("（{}×）", " ({}×)", speed_label(sp)) } else { String::new() }),
                    (Mode::Speed, Some(sp)) => trf!("{make_word} {}× 加速版{cap}", "{make_word} {}× sped-up video{cap}", speed_label(sp)),
                };
                if Btn::new(label).primary().enabled(speed.is_some() && !dlg.busy && !small_enough).min_width(170.0).show(ui).clicked() {
                    make = true;
                }
                if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                    close = true;
                }
            });
        });
        speed
    });
    // 開啟時置中（前幾格還在量大小），之後上緣固定：內容變高只往下長，超出視窗才往上移
    dlg.max_h = dlg.max_h.max(modal.response.rect.height());
    dlg.frames += 1;
    let centered = ((screen.height() - dlg.max_h) / 2.0).max(8.0);
    dlg.top = Some(match dlg.top {
        Some(t) if dlg.frames > 3 => t.min((screen.height() - dlg.max_h - 8.0).max(8.0)),
        _ => centered,
    });
    if modal.should_close() {
        close = true;
    }
    if let Some((dir, name)) = change_output {
        let format = dlg.format();
        app.export_dlg = Some(dlg);
        choose_output(app, dir, name, format);
        return;
    }
    if let Some(x) = play_existing {
        app.export_dlg = Some(dlg);
        app.act(EntryAction::Play, x);
        return;
    }
    if make && !close {
        if let Some(sp) = modal.inner {
            start(app, &mut dlg, sp);
        }
    }
    if !close && app.export_dlg.is_none() {
        app.export_dlg = Some(dlg);
    }
}

/// 按了「製作」：這次的選擇存成下次的預設，開始轉檔
fn start(app: &mut UiApp, dlg: &mut ExportDialog, speed: f64) {
    dlg.busy = true;
    dlg.save(&mut app.s);
    app.save_settings();
    let o = &dlg.o;
    let format = dlg.format();
    let req = ExportRequest {
        source: dlg.entry.media.path.clone(),
        speed,
        keep_audio: o.keep_audio,
        format,
        gif_width: o.gif_width as f64,
        gif_fps: o.gif_fps as f64,
        bounce: o.bounce && format.is_animation(),
        mp4_width: o.mp4_width as f64,
        mp4_max_mb: dlg.max_mb() as f64,
        output: dlg.output.clone(),
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
