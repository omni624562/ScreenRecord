//! 剪輯視窗的字幕（「字幕」分頁）：自動產生（語音辨識）或匯入 SRT，變成影片下方一段一段的文字標註；
//! 在清單裡直接改字，可以燒進影片，也可以依剪輯後的時間匯出成 SRT。

use super::shot::Act;
use super::{Editor, VAct};
use crate::ui::dialogs::{file_name, Ask};
use crate::ui::theme::{self, segmented, Btn};
use crate::ui::UiApp;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Id, RichText, Sense, Stroke};
use screenrecorder_core::annotate::{self, Ann, AnnKind};
use screenrecorder_core::format::video_clock;
use screenrecorder_core::subtitles::{self, Cue, LANGUAGES, MODELS};
use screenrecorder_core::{tr, trf};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

/// 一次最多加幾段（太多時輸出很慢）
const MAX_CUES: usize = 400;

/// 背景工作的進度
pub struct SubsJob {
    /// 0 檢查、1 下載語音模型、2 辨識語音、3 下載語音辨識用的 FFmpeg
    pub phase: Arc<AtomicU8>,
    /// 千分比
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
}

/// 關掉剪輯視窗（或換一個工作）時停止：下載與辨識語音不會在背景一直跑
impl Drop for SubsJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 剪輯視窗裡進行中的還是不是這個工作（關掉又開了別支影片時，結果不能加到別支影片上）
fn is_current(ed: &Editor, cancel: &Arc<AtomicBool>) -> bool {
    ed.subs.as_ref().is_some_and(|j| Arc::ptr_eq(&j.cancel, cancel))
}

/// 「字幕」分頁：產生或匯入字幕、選取的字幕的樣式與時間、逐段修改的清單
pub fn tab(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    if ed.vw <= 0.0 {
        return hint(ui, tr!("無法讀取影片尺寸，不能加上字幕。", "Couldn't read the video size, so subtitles can't be added."));
    }
    panel(ed, ui);
    let n = ed.sub_ids.len();
    if n == 0 {
        return;
    }
    ui.horizontal(|ui| {
        if Btn::new(tr!("匯出 SRT（依剪輯後的時間）", "Export SRT (edited timing)"))
            .small()
            .tooltip(tr!(
                "把字幕存成 SRT 字幕檔，時間換成剪輯後的影片的時間（刪掉的片段裡的字幕不算），可以上傳到 YouTube",
                "Save the subtitles as an SRT file with timing matched to the edited video (subtitles in removed sections are left out), e.g. for YouTube"
            ))
            .show(ui)
            .clicked()
        {
            ed.vpending = Some(VAct::ExportSrt);
        }
        if Btn::new(tr!("全部刪除", "Delete all")).ghost().small().tooltip(tr!("刪掉所有字幕（可以按 Ctrl+Z 復原）", "Delete all subtitles (press Ctrl+Z to undo)")).show(ui).clicked()
        {
            let ids = std::mem::take(&mut ed.sub_ids);
            ed.anns.retain(|a| !ids.contains(&a.id));
            if ed.ann_sel.is_some_and(|id| ids.contains(&id)) {
                ed.ann_sel = None;
            }
        }
    });
    // 選取的字幕：顏色、字級、深色底（可以套用到全部字幕）、出現時間
    if ed.ann_sel.is_some_and(|id| ed.is_sub(id)) {
        super::side::props(ed, ui, ctx);
    } else {
        hint(ui, tr!("點清單裡的時間可以跳到那一段並調整樣式；直接在文字上修改。", "Click a time in the list to jump there and change its style; edit the text right in the list."));
    }
    list(ed, ui);
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(theme::muted(ui, text).font(theme::font(12.0)));
}

/// 每一段字幕一行：時間（點一下跳過去並選取）、可以直接修改的文字、刪除
fn list(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let keep = ed.keep();
    let mut rows: Vec<(u64, f64, f64, String, bool)> =
        ed.anns.iter().filter(|a| ed.is_sub(a.id)).map(|a| (a.id, a.start, a.end, a.text.clone().unwrap_or_default().replace('\n', " "), !Editor::in_output(a, &keep))).collect();
    rows.sort_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)));
    let now = ed.now();
    let (mut select, mut delete, mut edit, mut seek) = (None, None, None, None);
    let w = ui.available_width();
    ui.spacing_mut().item_spacing.y = 3.0;
    for (id, start, end, mut text, gone) in rows {
        let (r, _) = ui.allocate_exact_size(vec2(w, 30.0), Sense::hover());
        // 看不到的那幾行不畫（字幕可能有幾百段）
        if !ui.is_rect_visible(r) {
            continue;
        }
        let on = ed.ann_sel == Some(id);
        let playing_here = now >= start && now < end;
        let painter = ui.painter();
        painter.rect(r, CornerRadius::same(6), if on { p.accent.gamma_multiply(0.12) } else { Color32::TRANSPARENT }, Stroke::new(1.0, if on { p.accent } else { p.border }), egui::StrokeKind::Inside);
        if playing_here {
            painter.rect_filled(egui::Rect::from_min_size(r.min + vec2(1.0, 5.0), vec2(3.0, r.height() - 10.0)), CornerRadius::same(2), p.accent);
        }
        let tr_rect = egui::Rect::from_min_size(r.min, vec2(62.0, r.height()));
        let t = ui.interact(tr_rect, Id::new(("sub-time", id)), Sense::click());
        let tcolor = if gone {
            p.muted
        } else if t.hovered() {
            p.accent
        } else {
            p.text
        };
        ui.painter().text(pos2(tr_rect.left() + 8.0, r.center().y), Align2::LEFT_CENTER, video_clock(start), theme::mono(11.5), tcolor);
        let gone_note = if gone { tr!("（在刪除的片段中，不會出現）", " (in a removed section; won't appear)") } else { "" };
        if t.on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(trf!("{} – {}{}・點一下跳到這一段並選取", "{} – {}{} · click to jump here and select it", video_clock(start), video_clock(end), gone_note))
            .clicked()
        {
            select = Some((id, start));
        }
        let er = egui::Rect::from_min_max(pos2(r.left() + 64.0, r.top() + 3.0), pos2(r.right() - 30.0, r.bottom() - 3.0));
        let resp = ui.put(er, egui::TextEdit::singleline(&mut text).id(Id::new(("sub-text", id))).font(theme::font(13.0)).margin(vec2(4.0, 3.0)));
        if resp.changed() {
            edit = Some((id, text));
        }
        // 開始改字：影片跳到這一段（不選取，上面不會多出設定、清單不會跳動）
        if resp.gained_focus() && (now < start || now > end) {
            seek = Some(start);
        }
        let x_rect = egui::Rect::from_center_size(pos2(r.right() - 15.0, r.center().y), vec2(22.0, 22.0));
        let xr = ui.interact(x_rect, Id::new(("sub-del", id)), Sense::click()).on_hover_text(tr!("刪除這段字幕", "Delete this subtitle")).on_hover_cursor(CursorIcon::PointingHand);
        if xr.hovered() {
            ui.painter().circle_filled(x_rect.center(), 11.0, p.rec_soft);
        }
        ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, "×", theme::font(14.0), if xr.hovered() { p.rec } else { p.muted });
        if xr.clicked() {
            delete = Some(id);
        }
    }
    if let Some((id, text)) = edit {
        ed.set_sub_text(id, &text);
    }
    if let Some(t) = seek {
        ed.seek(t);
    }
    if let Some(id) = delete {
        ed.delete_ann(id);
    }
    if let Some((id, start)) = select {
        if ed.ann_sel != Some(id) {
            let now = ed.now();
            if ed.anns.iter().find(|a| a.id == id).is_none_or(|a| now < a.start || now > a.end) {
                ed.seek(start);
            }
            ed.select_ann(Some(id));
        }
    }
}

/// 產生或匯入字幕的按鈕（「字幕」分頁最上面）
pub fn panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    ui.add_space(2.0);
    ui.label(RichText::new(tr!("字幕", "Subtitles")).font(theme::font_bold(13.5)));
    if let Some(job) = &ed.subs {
        let pct = job.progress.load(Ordering::Relaxed) as f32 / 10.0;
        let text = match job.phase.load(Ordering::Relaxed) {
            0 => tr!("正在確認 FFmpeg 支不支援語音辨識…", "Checking whether FFmpeg supports speech recognition…").to_string(),
            1 => trf!("正在下載語音模型… {pct:.0}%", "Downloading the speech model… {pct:.0}%"),
            3 => trf!("正在下載語音辨識用的 FFmpeg… {pct:.0}%", "Downloading FFmpeg for speech recognition… {pct:.0}%"),
            _ => trf!("正在辨識語音… {pct:.0}%", "Recognizing speech… {pct:.0}%"),
        };
        ui.horizontal(|ui| {
            ui.add(egui::ProgressBar::new(pct / 100.0).desired_width(ui.available_width() - 70.0).text(RichText::new(text).font(theme::font(12.0))));
            if Btn::new(tr!("取消", "Cancel")).ghost().small().show(ui).clicked() {
                ed.pending = Some(Act::CancelSubs);
            }
        });
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    ui.horizontal(|ui| {
        let auto = Btn::new(tr!("自動產生字幕", "Generate subtitles"))
            .small()
            .tooltip(tr!(
                "在這台電腦上辨識影片裡說的話（不用上傳），產生一段一段的字幕，可以修改後燒進影片；也會在影片旁邊存一份 SRT",
                "Recognize the speech in the video on this computer (nothing is uploaded) and turn it into subtitles you can edit and burn into the video; an SRT copy is also saved next to the video"
            ))
            .show(ui);
        egui::Popup::menu(&auto).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(260.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(theme::muted(ui, tr!("說的語言", "Spoken language")).font(theme::font(12.0)));
            let items: Vec<(usize, &str)> = LANGUAGES.iter().enumerate().map(|(i, _)| (i, subtitles::language_name(i))).collect();
            segmented(ui, &mut ed.subs_lang, &items, true);
            ui.label(theme::muted(ui, tr!("辨識的品質", "Recognition quality")).font(theme::font(12.0)));
            let items: Vec<(usize, &str)> =
                MODELS.iter().enumerate().map(|(i, m)| (i, if m.id == "base" { tr!("標準（較快）", "Standard (faster)") } else { tr!("精準（較慢）", "Accurate (slower)") })).collect();
            segmented(ui, &mut ed.subs_model, &items, true);
            let m = &MODELS[ed.subs_model.min(MODELS.len() - 1)];
            let need = subtitles::pending_downloads(!ed.ffmpeg_whisper && !subtitles::speech_ffmpeg_downloaded(), m);
            let note = if need.is_empty() {
                tr!("需要的檔案都已下載", "Everything needed is downloaded").to_string()
            } else {
                trf!("第一次使用要下載{}", "The first use downloads {}", subtitles::describe_downloads(&need))
            };
            ui.label(theme::muted(ui, note).font(theme::font(12.0)));
            if Btn::new(tr!("開始", "Start")).primary().small().show(ui).clicked() {
                ed.pending = Some(Act::AutoSubs);
                ui.close();
            }
        });
        if Btn::new(tr!("匯入字幕檔…", "Import subtitle file…"))
            .small()
            .tooltip(tr!("匯入 SRT 字幕檔（例如 YouTube 或其他工具產生的），變成可以修改的字幕", "Import an SRT subtitle file (e.g. from YouTube or another tool) as subtitles you can edit"))
            .show(ui)
            .clicked()
        {
            ed.pending = Some(Act::ImportSrt);
        }
    });
    if ed.sub_ids.is_empty() {
        ui.label(
            RichText::new(tr!(
                "字幕會放在影片下方，之後在這裡的清單逐段修改；不滿意可以按 Ctrl+Z 復原。",
                "Subtitles are placed at the bottom of the video; edit them one by one in the list here. Press Ctrl+Z to undo."
            ))
            .font(theme::font(12.0))
            .color(p.muted),
        );
    }
}

pub fn run(app: &mut UiApp, ed: &mut Editor, act: Act) {
    match act {
        Act::CancelSubs => {
            if let Some(j) = &ed.subs {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        Act::ImportSrt => {
            let video = ed.entry.media.path.clone();
            app.spawn(
                async move {
                    let path = tokio::task::spawn_blocking(subtitles::pick_file).await.unwrap_or(Ok(None))?;
                    let Some(path) = path else { return Ok(None) };
                    subtitles::read_file(&path).map(|c| Some((c, file_name(&path.to_string_lossy()))))
                },
                move |app, r: Result<Option<(Vec<Cue>, String)>, String>| match r {
                    Ok(Some((cues, name))) => {
                        // 選檔案時關掉或換了影片：不加
                        let Some(ed) = app.editor.as_mut().filter(|e| e.entry.media.path == video) else { return };
                        let n = ed.add_subtitles(&cues);
                        let more = if cues.len() > n { trf!("（太多了，只加前 {MAX_CUES} 段）", " (too many; only the first {MAX_CUES} were added)") } else { String::new() };
                        let msg = if screenrecorder_core::i18n::is_en() {
                            format!("Added {n} subtitle{} from “{name}”{more}", if n == 1 { "" } else { "s" })
                        } else {
                            format!("已從「{name}」加上 {n} 段字幕{more}")
                        };
                        app.toast(msg, false);
                    }
                    Ok(None) => {}
                    Err(e) => app.toast(e, true),
                },
            );
        }
        Act::AutoSubs => {
            if ed.subs.is_some() {
                return;
            }
            let job = SubsJob { phase: Arc::new(AtomicU8::new(0)), progress: Arc::new(AtomicU32::new(0)), cancel: Arc::new(AtomicBool::new(false)) };
            let mine = job.cancel.clone();
            let ffmpeg = ed.ffmpeg.clone();
            ed.subs = Some(job);
            // 目前用的 FFmpeg 沒有 whisper 時，用（或下載）語音辨識用的 FFmpeg
            app.spawn(async move { subtitles::find_speech_ffmpeg(&ffmpeg).await }, move |app, speech| {
                let Some(ed) = app.editor.as_mut().filter(|e| is_current(e, &mine)) else { return };
                let m = MODELS[ed.subs_model.min(MODELS.len() - 1)];
                let need = subtitles::pending_downloads(speech.is_none(), &m);
                if need.is_empty() {
                    return start(app, speech);
                }
                // 先問要不要下載（檔案不小）
                ed.subs = None;
                let msg = trf!(
                    "第一次使用要下載{}，存在「{}」，之後就不用再下載。\n\n辨識在這台電腦上進行，影片不會上傳。",
                    "The first use downloads {} to “{}”. This only happens once.\n\nRecognition runs on this computer; the video isn't uploaded.",
                    subtitles::describe_downloads(&need),
                    subtitles::model_dir().display()
                );
                let first = if speech.is_none() { 3 } else { 1 };
                app.ask = Some(Ask::confirm(tr!("下載語音辨識需要的檔案", "Download speech recognition files"), msg, tr!("下載並開始", "Download and start"), move |app, _| {
                    app.ask = None;
                    if let Some(ed) = &mut app.editor {
                        ed.subs = Some(SubsJob { phase: Arc::new(AtomicU8::new(first)), progress: Arc::new(AtomicU32::new(0)), cancel: Arc::new(AtomicBool::new(false)) });
                    }
                    start(app, speech);
                }));
            });
        }
        _ => {}
    }
}

/// 下載語音辨識用的 FFmpeg（speech 是 None 時）與語音模型（需要時），再辨識語音；完成後加上字幕、在影片旁邊存一份 SRT
fn start(app: &mut UiApp, speech: Option<PathBuf>) {
    let Some(ed) = &mut app.editor else { return };
    let Some(job) = &ed.subs else { return };
    let (phase, progress, cancel) = (job.phase.clone(), job.progress.clone(), job.cancel.clone());
    let mine = job.cancel.clone();
    let m = MODELS[ed.subs_model.min(MODELS.len() - 1)];
    let lang = LANGUAGES[ed.subs_lang.min(LANGUAGES.len() - 1)].0;
    let (video, duration) = (ed.entry.media.path.clone(), ed.duration);
    app.spawn(
        async move {
            let speech = match speech {
                Some(p) => p,
                None => {
                    phase.store(3, Ordering::Relaxed);
                    progress.store(0, Ordering::Relaxed);
                    subtitles::download_speech_ffmpeg(progress.clone(), cancel.clone()).await?
                }
            };
            if !subtitles::model_ready(&m) {
                phase.store(1, Ordering::Relaxed);
                progress.store(0, Ordering::Relaxed);
                let (p, c) = (progress.clone(), cancel.clone());
                tokio::task::spawn_blocking(move || subtitles::download_model(&m, &p, &c)).await.map_err(|e| e.to_string())??;
            }
            phase.store(2, Ordering::Relaxed);
            progress.store(0, Ordering::Relaxed);
            let cues = subtitles::transcribe(&speech, &video, duration, &m, lang, progress, cancel).await?;
            let srt = if cues.is_empty() { None } else { subtitles::save_next_to(&video, &cues).ok() };
            Ok::<_, String>((cues, srt))
        },
        move |app, r| {
            let Some(ed) = app.editor.as_mut().filter(|e| is_current(e, &mine)) else { return };
            // 按了取消（在 ed.subs 清掉之前讀：SubsJob 結束時也會設成取消）；core 的錯誤訊息可能是英文，不只比對文字
            let canceled = mine.load(Ordering::Relaxed);
            ed.subs = None;
            match r {
                Ok((cues, _)) if cues.is_empty() => app.toast(tr!("沒有辨識出說話的內容", "No speech was recognized"), false),
                Ok((cues, srt)) => {
                    let n = ed.add_subtitles(&cues);
                    let saved = srt.map(|p| trf!("，也存了一份「{}」", " and saved a copy as “{}”", file_name(&p.to_string_lossy()))).unwrap_or_default();
                    let msg = if screenrecorder_core::i18n::is_en() {
                        format!("Added {n} subtitle{}{saved}. Check for typos; click a subtitle to edit it", if n == 1 { "" } else { "s" })
                    } else {
                        format!("加上 {n} 段字幕{saved}；請檢查錯字，點一下字幕就能修改")
                    };
                    app.toast(msg, false);
                }
                Err(e) if canceled || e.contains("取消") => app.toast(tr!("已取消產生字幕", "Subtitle generation canceled"), false),
                Err(e) => app.toast(e, true),
            }
        },
    );
}

impl Editor {
    /// 改一段字幕的文字（太長自動斷成兩行）：寬度變了以下緣中間為準，位置不跑掉
    pub fn set_sub_text(&mut self, id: u64, text: &str) {
        let Some(a) = self.ann_mut(id) else { return };
        let ascii = text.chars().filter(|ch| ch.is_ascii()).count() * 2 > text.chars().count();
        let (cx, bottom) = (a.x + a.w / 2.0, a.y + a.h);
        a.text = Some(subtitles::wrap(text, if ascii { 42 } else { 22 }));
        annotate::measure(a);
        a.x = cx - a.w / 2.0;
        a.y = bottom - a.h;
    }

    /// 字幕換成剪輯後的時間（刪掉的片段裡的不算；跨過刪掉的片段時從第一段保留的開始到最後一段保留的結束）
    pub fn cut_cues(&self) -> Vec<Cue> {
        let parts = self.parts();
        let mut subs: Vec<&Ann> = self.anns.iter().filter(|a| self.is_sub(a.id)).collect();
        subs.sort_by(|x, y| x.start.total_cmp(&y.start));
        subs.into_iter()
            .filter_map(|a| {
                let over: Vec<&(f64, f64, u32)> = parts.iter().filter(|p| p.0 < a.end && p.1 > a.start).collect();
                let (first, last) = (over.first()?, over.last()?);
                let start = screenrecorder_core::zoom::map_time(&parts, a.start.max(first.0))?;
                let end = screenrecorder_core::zoom::map_time(&parts, (a.end.min(last.1) - 0.001).max(first.0))? + 0.001;
                (end - start >= 0.05).then(|| Cue { start, end, text: a.text.clone().unwrap_or_default() })
            })
            .collect()
    }

    /// 字幕變成文字標註：白字深色底，放在影片下方中間；回傳加了幾段
    pub fn add_subtitles(&mut self, cues: &[Cue]) -> usize {
        if self.vw <= 0.0 {
            return 0;
        }
        let k = self.vh / 1080.0;
        let mut n = 0;
        for c in cues.iter().take(MAX_CUES) {
            let (start, end) = (c.start.clamp(0.0, self.duration), c.end.clamp(0.0, self.duration));
            if end - start < 0.05 {
                continue;
            }
            // 中文一行約 22 字、英文約 42 個字母
            let ascii = c.text.chars().filter(|ch| ch.is_ascii()).count() * 2 > c.text.chars().count();
            let text = subtitles::wrap(&c.text, if ascii { 42 } else { 22 });
            let mut a = Ann {
                id: self.next_id,
                kind: AnnKind::Text,
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                start,
                end,
                color: "#ffffff".into(),
                size: (40.0 * k).round().max(14.0),
                text: Some(text),
                bg: true,
                n: None,
                shape: None,
                invert: false,
                rot: 0.0,
                pts: vec![],
            };
            annotate::measure(&mut a);
            a.x = ((self.vw - a.w) / 2.0).round().max(0.0);
            a.y = (self.vh - a.h - self.vh * 0.06).round().max(0.0);
            self.next_id += 1;
            self.sub_ids.insert(a.id);
            self.anns.push(a);
            n += 1;
        }
        if n > 0 {
            self.ann_sel = None;
            self.tool = None;
            self.tab = super::Tab::Subs;
        }
        n
    }
}
