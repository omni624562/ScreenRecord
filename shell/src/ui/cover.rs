//! 做封面：從影片挑一格、打上標題，存成 1280×720 的封面圖（也可以寫進影片當縮圖）。見 core::cover。

use super::dialogs;
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, ColorImage, Id, Layout, RichText, TextureHandle, TextureOptions};
use screenrecorder_core::cover::{self, Cover, CoverStyle};
use screenrecorder_core::format::video_clock;
use screenrecorder_core::types::LibraryEntry;
use screenrecorder_core::{tr, trf};
use std::sync::Arc;
use tiny_skia::Pixmap;

/// 預覽的寬度（16:9）
const PREVIEW_W: f32 = 560.0;

pub struct CoverDialog {
    path: String,
    name: String,
    duration: f64,
    /// 選的時間點（滑桿）
    t: f64,
    /// 讀好的畫面與它的時間
    frame: Option<(f64, Arc<Pixmap>)>,
    /// 正在讀的時間點
    loading: Option<f64>,
    cover: Cover,
    /// 畫好的封面與預覽（內容變了才重畫）
    rendered: Option<(Cover, f64, Arc<Pixmap>)>,
    tex: Option<TextureHandle>,
    embed: bool,
    saving: bool,
    /// 讀不到畫面的原因
    error: Option<String>,
}

/// t：要從哪一格開始（檢視器裡目前的位置）；沒有時用影片的 1/3 處（通常已經進入正題）
pub fn open(app: &mut UiApp, entry: LibraryEntry, t: Option<f64>) {
    let duration = entry.media.duration_sec.unwrap_or(0.0).max(0.0);
    let path = entry.media.path.clone();
    let name = dialogs::file_name(&path);
    // 自己取過名字的錄影：標題先填檔名（「Rec_日期」這種就不填）
    let stem = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = if stem.starts_with("Rec_") || stem.starts_with("Replay_") { String::new() } else { stem.replace('_', " ") };
    let mp4 = path.to_ascii_lowercase().ends_with(".mp4");
    let t = t.unwrap_or(duration / 3.0).clamp(0.0, (duration - 0.1).max(0.0));
    app.cover = Some(CoverDialog {
        path,
        name,
        duration,
        t,
        frame: None,
        loading: None,
        cover: Cover { title, subtitle: String::new(), style: CoverStyle::Band },
        rendered: None,
        tex: None,
        embed: mp4,
        saving: false,
        error: None,
    });
    load(app, t);
}

/// 讀 t 秒的那一格（讀好時如果滑桿已經移到別的地方，再讀新的位置）
fn load(app: &mut UiApp, t: f64) {
    let Some(d) = &mut app.cover else { return };
    if d.loading.is_some() {
        return;
    }
    let Some(ff) = app.core.ffmpeg_path() else {
        d.error = Some(tr!("找不到 FFmpeg", "FFmpeg not found").into());
        return;
    };
    d.loading = Some(t);
    let path = d.path.clone();
    app.spawn(async move { cover::frame_at(&ff, &path, t).await }, move |app, r| {
        let Some(d) = &mut app.cover else { return };
        d.loading = None;
        match r {
            Ok(pm) => {
                d.frame = Some((t, Arc::new(pm)));
                d.error = None;
            }
            Err(e) => d.error = Some(e.message().to_string()),
        }
        let want = d.t;
        if (want - t).abs() > 0.05 {
            load(app, want);
        }
    });
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.cover.take() else { return };
    // 畫面或文字變了：重畫封面
    if let Some((ft, frame)) = &d.frame {
        let stale = d.rendered.as_ref().is_none_or(|(c, t, _)| *c != d.cover || *t != *ft);
        if stale {
            if let Some(pm) = cover::render(frame, &d.cover) {
                let img = ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                match &mut d.tex {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => d.tex = Some(ctx.load_texture("cover-preview", img, TextureOptions::LINEAR)),
                }
                d.rendered = Some((d.cover.clone(), *ft, Arc::new(pm)));
            }
        }
    }
    let mut close = false;
    let mut save = false;
    let mut reload = None;
    let modal = egui::Modal::new(Id::new("cover")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(PREVIEW_W);
        let pal = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("做封面", "Make a cover")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&d.name).color(pal.muted)).truncate());
        ui.add_space(8.0);
        // 預覽（讀畫面時蓋一個轉圈）
        let size = vec2(PREVIEW_W, PREVIEW_W * 9.0 / 16.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(rect, 6, egui::Color32::BLACK);
        if let Some(t) = &d.tex {
            egui::Image::new((t.id(), size)).corner_radius(6).paint_at(ui, rect);
        }
        if d.loading.is_some() || (d.frame.is_none() && d.error.is_none()) {
            ui.put(egui::Rect::from_center_size(rect.center(), vec2(32.0, 32.0)), egui::Spinner::new().size(28.0));
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(pal.warn).font(theme::font(12.5)));
        }
        ui.add_space(8.0);
        // 選哪一格
        ui.horizontal(|ui| {
            ui.label(tr!("畫面", "Frame"));
            ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0);
            let r = ui.add_enabled(d.duration > 0.0, egui::Slider::new(&mut d.t, 0.0..=d.duration.max(0.1)).show_value(false).trailing_fill(true));
            ui.label(RichText::new(video_clock(d.t)).font(theme::mono(12.5)));
            // 拖曳時就讀（一次只讀一張，讀好再讀最新的位置）
            if r.changed() || r.drag_stopped() {
                reload = Some(d.t);
            }
        });
        ui.add_space(4.0);
        egui::Grid::new("cover-form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(tr!("標題", "Title"));
            ui.add(egui::TextEdit::singleline(&mut d.cover.title).desired_width(ui.available_width()).hint_text(tr!("例如：報價單怎麼填", "e.g. How to fill in a quote")));
            ui.end_row();
            ui.label(tr!("副標題", "Subtitle"));
            ui.add(egui::TextEdit::singleline(&mut d.cover.subtitle).desired_width(ui.available_width()).hint_text(tr!("（可以不填）", "(optional)")));
            ui.end_row();
            ui.label(tr!("樣式", "Style"));
            let items: Vec<(CoverStyle, &str)> = CoverStyle::ALL.iter().map(|s| (*s, s.label())).collect();
            theme::segmented(ui, &mut d.cover.style, &items, true);
            ui.end_row();
        });
        ui.add_space(6.0);
        let mp4 = d.path.to_ascii_lowercase().ends_with(".mp4");
        ui.add_enabled_ui(mp4, |ui| {
            theme::check(ui, &mut d.embed, tr!("也寫進影片（檔案總管裡看到的縮圖就是封面）", "Also write it into the video (shown as the thumbnail in File Explorer)"));
        });
        ui.label(
            RichText::new(trf!(
                "封面圖存在影片旁邊（{}），上傳 YouTube 時在「縮圖」選它。",
                "The cover image is saved next to the video ({}). Choose it under “Thumbnail” when uploading to YouTube.",
                dialogs::file_name(&cover::cover_path(std::path::Path::new(&d.path)).display().to_string())
            ))
            .font(theme::font(12.0))
            .color(pal.muted),
        );
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ready = d.rendered.is_some() && !d.saving;
            if Btn::new(if d.saving { tr!("正在儲存…", "Saving…") } else { tr!("儲存封面", "Save cover") }).primary().enabled(ready).show(ui).clicked() {
                save = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
    });
    if (close || modal.should_close()) && !d.saving {
        return;
    }
    if save {
        if let Some((_, _, pm)) = d.rendered.clone() {
            d.saving = true;
            let (path, embed) = (d.path.clone(), d.embed);
            // 寫進影片時要換掉原檔：檢視器正在播這支就先關掉（播放中的檔案換不掉）
            if embed && app.viewer.as_ref().is_some_and(|v| v.cur().media.path == path) {
                app.viewer = None;
            }
            let ff = app.core.ffmpeg_path();
            let video = path.clone();
            app.spawn(async move { cover::save(ff.as_deref(), &path, &pm, embed).await }, move |app, r| {
                app.cover = None;
                match r {
                    Ok(s) => {
                        let png = s.png.display().to_string();
                        match s.embed_error {
                            Some(e) => app.toast(e, true),
                            None if s.embedded => {
                                app.thumbs.forget(&video);
                                app.toast_file(tr!("已存好封面，也寫進影片了", "Cover saved and written into the video"), &png);
                                app.sync_edited(&video);
                            }
                            None => app.toast_file(trf!("已存好封面 {}", "Cover saved as {}", dialogs::file_name(&png)), &png),
                        }
                        app.shots_changed();
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                }
            });
        }
    }
    app.cover = Some(d);
    if let Some(t) = reload {
        load(app, t);
    }
}
