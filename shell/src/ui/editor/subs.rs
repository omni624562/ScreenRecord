//! 剪輯視窗的字幕：自動產生（語音辨識）或匯入 SRT，變成影片下方一段一段的文字標註，可以修改後燒進影片。

use super::shot::Act;
use super::Editor;
use crate::ui::dialogs::{file_name, Ask};
use crate::ui::theme::{self, segmented, Btn};
use crate::ui::UiApp;
use eframe::egui::{self, vec2, RichText};
use screenrecorder_core::annotate::{self, Ann, AnnKind};
use screenrecorder_core::subtitles::{self, Cue, LANGUAGES, MODELS};
use screenrecorder_core::{tr, trf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

/// 一次最多加幾段（太多時輸出很慢）
const MAX_CUES: usize = 400;

/// 背景工作的進度
pub struct SubsJob {
    /// 0 檢查、1 下載語音模型、2 辨識語音
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

/// 「標註」分頁的字幕區塊（影片才有）
pub fn panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    ui.add_space(2.0);
    ui.label(RichText::new(tr!("字幕", "Subtitles")).font(theme::font_bold(13.5)));
    if let Some(job) = &ed.subs {
        let pct = job.progress.load(Ordering::Relaxed) as f32 / 10.0;
        let text = match job.phase.load(Ordering::Relaxed) {
            0 => tr!("正在確認 FFmpeg 支不支援語音辨識…", "Checking whether FFmpeg supports speech recognition…").to_string(),
            1 => trf!("正在下載語音模型… {pct:.0}%", "Downloading the speech model… {pct:.0}%"),
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
            let note = if subtitles::model_ready(m) {
                tr!("語音模型已下載", "Speech model downloaded").to_string()
            } else {
                trf!("第一次使用要下載約 {} MB 的語音模型", "The first use downloads a speech model of about {} MB", m.mb)
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
    ui.label(
        RichText::new(tr!(
            "字幕會放在影片下方，點一下就能改字或調整時間；不滿意可以按 Ctrl+Z 復原。",
            "Subtitles are placed at the bottom of the video; click one to change its text or timing. Press Ctrl+Z to undo."
        ))
        .font(theme::font(12.0))
        .color(p.muted),
    );
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
            app.spawn(async move { subtitles::has_whisper(&ffmpeg).await }, move |app, ok| {
                let Some(ed) = app.editor.as_mut().filter(|e| is_current(e, &mine)) else { return };
                if !ok {
                    ed.subs = None;
                    app.ask = Some(Ask::confirm(
                        tr!("這個 FFmpeg 不能辨識語音", "This FFmpeg can't recognize speech"),
                        tr!(
                            "自動字幕要用 FFmpeg 8 以上、而且有 whisper（語音辨識）的版本，目前用的 FFmpeg 沒有。\n\n可以改用 BtbN 的 FFmpeg（github.com/BtbN/FFmpeg-Builds，下載 win64-gpl 版），把裡面 bin 的 ffmpeg.exe 放到本程式的資料夾，重新開啟程式即可。\n\n也可以先用其他工具（例如 YouTube 的自動字幕）產生 SRT 字幕檔，再按「匯入字幕檔」。",
                            "Automatic subtitles need FFmpeg 8 or later built with whisper (speech recognition), and the FFmpeg in use doesn't have it.\n\nYou can switch to BtbN's FFmpeg (github.com/BtbN/FFmpeg-Builds, download the win64-gpl build): put the ffmpeg.exe from its bin folder into this program's folder and restart the program.\n\nYou can also create an SRT subtitle file with another tool (such as YouTube's automatic captions) and then press “Import subtitle file”."
                        ),
                        tr!("知道了", "Got it"),
                        |app, _| app.ask = None,
                    ));
                    return;
                }
                let m = MODELS[ed.subs_model.min(MODELS.len() - 1)];
                if subtitles::model_ready(&m) {
                    return start(app);
                }
                // 先問要不要下載（檔案不小）
                ed.subs = None;
                let msg = trf!(
                    "第一次使用要下載約 {} MB 的語音模型（{}），存在「{}」，之後就不用再下載。\n\n辨識在這台電腦上進行，影片不會上傳。",
                    "The first use downloads a speech model of about {} MB ({}) and saves it in “{}”, so you won't need to download it again.\n\nRecognition runs on this computer; the video isn't uploaded.",
                    m.mb,
                    m.label(),
                    subtitles::model_dir().display()
                );
                app.ask = Some(Ask::confirm(tr!("下載語音模型", "Download speech model"), msg, tr!("下載並開始", "Download and start"), |app, _| {
                    app.ask = None;
                    if let Some(ed) = &mut app.editor {
                        ed.subs = Some(SubsJob { phase: Arc::new(AtomicU8::new(1)), progress: Arc::new(AtomicU32::new(0)), cancel: Arc::new(AtomicBool::new(false)) });
                    }
                    start(app);
                }));
            });
        }
        _ => {}
    }
}

/// 下載模型（需要時）並辨識語音；完成後加上字幕、在影片旁邊存一份 SRT
fn start(app: &mut UiApp) {
    let Some(ed) = &mut app.editor else { return };
    let Some(job) = &ed.subs else { return };
    let (phase, progress, cancel) = (job.phase.clone(), job.progress.clone(), job.cancel.clone());
    let mine = job.cancel.clone();
    let m = MODELS[ed.subs_model.min(MODELS.len() - 1)];
    let lang = LANGUAGES[ed.subs_lang.min(LANGUAGES.len() - 1)].0;
    let (ffmpeg, video, duration) = (ed.ffmpeg.clone(), ed.entry.media.path.clone(), ed.duration);
    app.spawn(
        async move {
            if !subtitles::model_ready(&m) {
                phase.store(1, Ordering::Relaxed);
                let (p, c) = (progress.clone(), cancel.clone());
                tokio::task::spawn_blocking(move || subtitles::download_model(&m, &p, &c)).await.map_err(|e| e.to_string())??;
            }
            phase.store(2, Ordering::Relaxed);
            progress.store(0, Ordering::Relaxed);
            let cues = subtitles::transcribe(&ffmpeg, &video, duration, &m, lang, progress, cancel).await?;
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
            self.anns.push(a);
            n += 1;
        }
        if n > 0 {
            self.ann_sel = None;
            self.tool = None;
            self.tab = super::Tab::Ann;
        }
        n
    }
}
