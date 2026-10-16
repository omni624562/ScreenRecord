//! 整理步驟教學（步驟截圖、或錄影記下的操作步驟）：步驟列成清單（小圖、說明、時間），可以刪掉、調整順序、改文件標題，
//! 再做成教學文件（一個 HTML 檔，圖片包在裡面）並用瀏覽器開啟。

use super::dialogs::Ask;
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, ColorImage, CornerRadius, Id, Layout, RichText, Sense, TextureHandle, TextureOptions};
use screenrecorder_core::steps::{self, Draft, Step};
use screenrecorder_core::{tr, trf};

/// 小圖的長邊（像素）
const THUMB: u32 = 360;
/// 清單裡小圖的大小
const THUMB_W: f32 = 168.0;
const THUMB_H: f32 = 96.0;

pub struct Review {
    draft: Draft,
    title: String,
    items: Vec<Item>,
    /// 正在做成文件
    busy: bool,
    /// 做成哪一種文件
    format: screenrecorder_core::steps::DocFormat,
    /// 做成影片：每一步至少幾秒、唸出說明
    secs: f64,
    speak: bool,
}

struct Item {
    step: Step,
    tex: Option<TextureHandle>,
    /// 小圖讀不到
    failed: bool,
}

enum Act {
    Up(usize),
    Down(usize),
    Delete(usize),
    Build,
    /// 只要文字：說明整理成編號清單複製（不附圖，不會關閉）
    CopyText,
    Close,
}

/// 開啟整理視窗（App::steps_draft 有步驟時）
pub fn open(app: &mut UiApp) {
    let Some(draft) = app.core.steps_draft() else { return };
    if app.steps_review.as_ref().is_some_and(|r| r.draft == draft) {
        return;
    }
    let items = draft.steps.iter().map(|s| Item { step: s.clone(), tex: None, failed: false }).collect();
    app.steps_review = Some(Review { title: draft.title(), draft, items, busy: false, format: Default::default(), secs: 3.0, speak: cfg!(windows) });
    load_thumb(app, 0);
}

/// 一張一張在背景讀小圖（讀好一張就顯示一張）
fn load_thumb(app: &mut UiApp, i: usize) {
    let Some(r) = &app.steps_review else { return };
    let Some(step) = r.draft.steps.get(i) else { return };
    let (path, image) = (r.draft.image_path(step), step.image.clone());
    app.spawn(async move { tokio::task::spawn_blocking(move || steps::thumb(&path, THUMB)).await.ok().flatten() }, move |app, t| {
        let Some(r) = &mut app.steps_review else { return };
        let Some(it) = r.items.iter_mut().find(|it| it.step.image == image) else { return load_thumb(app, i + 1) };
        match t {
            Some((w, h, rgba)) => {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                it.tex = Some(app.ctx.load_texture(format!("step-{image}"), img, TextureOptions::LINEAR));
            }
            None => it.failed = true,
        }
        load_thumb(app, i + 1);
    });
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut r) = app.steps_review.take() else { return };
    let mut act = None;
    let modal = egui::Modal::new(Id::new("steps-review")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(600.0);
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("整理步驟", "Organize steps")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().enabled(!r.busy).tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    act = Some(Act::Close);
                }
            });
        });
        ui.add_space(2.0);
        let n = r.items.len();
        let sub = if screenrecorder_core::i18n::is_en() {
            format!("{n} step{}. Delete the ones you don't need or change the order, then make the guide.", if n == 1 { "" } else { "s" })
        } else {
            format!("共 {n} 步。不要的步驟可以刪掉、順序可以調整，好了再做成教學文件。")
        };
        ui.label(RichText::new(sub).color(p.muted));
        ui.add_space(10.0);
        ui.label(RichText::new(tr!("文件標題", "Title")).font(theme::font_bold(13.0)));
        ui.add_enabled(!r.busy, egui::TextEdit::singleline(&mut r.title).desired_width(f32::INFINITY).font(theme::font(14.0)).margin(vec2(8.0, 6.0)));
        ui.add_space(10.0);
        let max_h = (ctx.content_rect().height() - 330.0).clamp(160.0, 520.0);
        egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            if r.items.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new(tr!("所有步驟都刪掉了。", "All steps have been deleted.")).color(p.muted));
                ui.add_space(12.0);
            }
            for (i, it) in r.items.iter().enumerate() {
                egui::Frame::new().stroke(egui::Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(theme::RADIUS_SM)).inner_margin(8.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (rect, resp) = ui.allocate_exact_size(vec2(THUMB_W, THUMB_H), Sense::hover());
                        ui.painter().rect_filled(rect, CornerRadius::same(4), p.surface2);
                        match &it.tex {
                            Some(t) => {
                                let s = t.size_vec2();
                                let k = (THUMB_W / s.x).min(THUMB_H / s.y);
                                let img = egui::Rect::from_center_size(rect.center(), s * k);
                                ui.painter().image(t.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                                // 移上去看大一點
                                resp.on_hover_ui(|ui| {
                                    ui.add(egui::Image::new((t.id(), s * (THUMB as f32 / s.x.max(s.y)).max(1.0))));
                                });
                            }
                            None if it.failed => {
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tr!("讀不到圖", "Can't read image"), theme::font(12.0), p.muted);
                            }
                            None => {
                                ui.put(egui::Rect::from_center_size(rect.center(), vec2(18.0, 18.0)), egui::Spinner::new());
                            }
                        }
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.add_space(4.0);
                            ui.label(RichText::new(trf!("步驟 {}", "Step {}", i + 1)).font(theme::font_bold(14.0)));
                            ui.add(egui::Label::new(it.step.caption()).truncate());
                            ui.label(RichText::new(&it.step.time).font(theme::font(12.0)).color(p.muted));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if Btn::icon_only(Icon::Trash).ghost().small().enabled(!r.busy).tooltip(tr!("刪除這一步", "Delete this step")).show(ui).clicked() {
                                act = Some(Act::Delete(i));
                            }
                            if Btn::icon_only(Icon::ChevD).ghost().small().enabled(!r.busy && i + 1 < n).tooltip(tr!("往下移", "Move down")).show(ui).clicked() {
                                act = Some(Act::Down(i));
                            }
                            if Btn::icon_only(Icon::ChevU).ghost().small().enabled(!r.busy && i > 0).tooltip(tr!("往上移", "Move up")).show(ui).clicked() {
                                act = Some(Act::Up(i));
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if r.busy {
                ui.spinner();
                ui.label(if r.format == screenrecorder_core::steps::DocFormat::Video {
                    tr!("正在做成教學影片…", "Making the video…")
                } else {
                    tr!("正在做成教學文件…", "Making the guide…")
                });
            } else {
                use screenrecorder_core::steps::DocFormat;
                let items = [(DocFormat::Html, tr!("網頁", "Web page")), (DocFormat::Word, "Word"), (DocFormat::PowerPoint, "PowerPoint"), (DocFormat::Video, tr!("影片", "Video"))];
                theme::segmented(ui, &mut r.format, &items, true);
                if r.format == DocFormat::Video {
                    let secs: Vec<(u32, String)> = steps::VIDEO_SECS.iter().map(|s| (*s as u32, trf!("每步 {} 秒", "{} s each", s))).collect();
                    let refs: Vec<(u32, &str)> = secs.iter().map(|(v, t)| (*v, t.as_str())).collect();
                    let mut v = r.secs as u32;
                    if theme::segmented(ui, &mut v, &refs, true) {
                        r.secs = v as f64;
                    }
                    ui.add_enabled_ui(cfg!(windows), |ui| {
                        theme::check(ui, &mut r.speak, RichText::new(tr!("唸出說明", "Read captions aloud")).font(theme::font(12.5))).on_hover_text(tr!(
                            "用 Windows 內建的語音唸出每一步的說明（不用網路）；唸比較久的步驟會停久一點",
                            "Read each step's caption with the voice built into Windows (no internet); longer captions stay on screen longer"
                        ));
                    });
                } else {
                    let note = match r.format {
                        DocFormat::Html => tr!("一個 HTML 檔（圖片包在裡面），用瀏覽器開啟、可以直接改字", "One HTML file (pictures included); opens in your browser, where you can edit the text"),
                        DocFormat::Word => tr!("Word 文件（.docx），每一步一段說明和一張圖", "A Word document (.docx) with a caption and picture for each step"),
                        _ => tr!("PowerPoint 簡報（.pptx），一步一張投影片", "A PowerPoint deck (.pptx) with one slide per step"),
                    };
                    ui.label(RichText::new(note).font(theme::font(12.0)).color(p.muted));
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let make = if r.format == screenrecorder_core::steps::DocFormat::Video { tr!("產生教學影片", "Make video") } else { tr!("產生教學文件", "Make guide") };
                if Btn::new(make).primary().enabled(!r.busy && n > 0).show(ui).clicked() {
                    act = Some(Act::Build);
                }
                if Btn::new(tr!("複製成文字", "Copy as text"))
                    .ghost()
                    .enabled(!r.busy && n > 0)
                    .tooltip(tr!("不要圖、只要文字：每一步的說明整理成編號清單，貼到文件或信件裡", "Text only, no pictures: copy the captions as a numbered list to paste into a document or email"))
                    .show(ui)
                    .clicked()
                {
                    act = Some(Act::CopyText);
                }
                if Btn::new(tr!("放棄", "Discard")).ghost().enabled(!r.busy).tooltip(tr!("刪掉這次截的步驟，不做成文件", "Delete these steps without making a guide")).show(ui).clicked()
                {
                    act = Some(Act::Close);
                }
            });
        });
    });
    // 另一個對話框（確定放棄）開著時不處理 Esc、點外面
    if act.is_none() && modal.should_close() && app.ask.is_none() && !r.busy {
        act = Some(Act::Close);
    }
    match act {
        Some(Act::Up(i)) => r.items.swap(i, i - 1),
        Some(Act::Down(i)) => r.items.swap(i, i + 1),
        Some(Act::Delete(i)) => {
            r.items.remove(i);
        }
        Some(Act::CopyText) => {
            let title = if r.title.trim().is_empty() { r.draft.title() } else { r.title.trim().to_string() };
            let lines: Vec<String> = r.items.iter().enumerate().map(|(i, it)| format!("{}. {}", i + 1, it.step.caption())).collect();
            ctx.copy_text(format!("{title}\n\n{}", lines.join("\n")));
            app.toast(trf!("已複製 {} 個步驟（文字）", "Copied {} steps as text", lines.len()), false);
        }
        Some(Act::Build) => {
            r.busy = true;
            let (title, list, format) = (r.title.clone(), r.items.iter().map(|it| it.step.clone()).collect::<Vec<_>>(), r.format);
            let (secs, speak) = (r.secs, r.speak && cfg!(windows));
            let core = app.core.clone();
            let job = async move {
                if format == screenrecorder_core::steps::DocFormat::Video {
                    core.steps_build_video(title, list, secs, speak).await
                } else {
                    core.steps_build_as(title, list, format).await
                }
            };
            app.spawn(job, move |app, res| match res {
                Ok(path) => {
                    app.steps_review = None;
                    screenrecorder_core::desktop::open_with_explorer(&path, false);
                    let msg = match format {
                        screenrecorder_core::steps::DocFormat::Html => tr!("教學文件做好了，已用瀏覽器開啟", "The step-by-step guide is ready and open in your browser"),
                        screenrecorder_core::steps::DocFormat::Video => tr!("教學影片做好了，已經打開", "The step-by-step video is ready and open"),
                        _ => tr!("教學文件做好了，已經打開", "The step-by-step guide is ready and open"),
                    };
                    app.toast_file(msg, &path);
                }
                Err(e) => {
                    if let Some(r) = &mut app.steps_review {
                        r.busy = false;
                    }
                    app.toast(e.message().to_string(), true);
                }
            });
        }
        Some(Act::Close) if r.items.is_empty() => {
            // 全部刪掉了：直接放棄
            app.core.steps_discard();
            return;
        }
        Some(Act::Close) => {
            let n = r.items.len();
            let msg = if screenrecorder_core::i18n::is_en() {
                format!("The {n} step{} will be deleted and no guide will be made.", if n == 1 { "" } else { "s" })
            } else {
                format!("這 {n} 步會刪掉，不會做成文件。")
            };
            let mut ask = Ask::confirm(tr!("放棄這些步驟？", "Discard these steps?"), msg, tr!("放棄", "Discard"), |app, _| {
                app.steps_review = None;
                app.core.steps_discard();
            })
            .with_cancel(tr!("返回", "Back"));
            ask.danger = true;
            app.ask = Some(ask);
        }
        None => {}
    }
    app.steps_review = Some(r);
}
