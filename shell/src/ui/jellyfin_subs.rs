//! 幫 Jellyfin 上的影片產生字幕：直接讀 Jellyfin 上的影片（不用先下載到這台電腦），用這台電腦的語音辨識
//! （和錄完自動處理同一套、同一個模型與語言），做好的繁體中文字幕傳回 Jellyfin（電視、手機上看也有字幕）。
//! 一次一支，Jellyfin 分頁下面顯示進度、可以取消。見 core::subtitles::transcribe、core::jellyfin::Jellyfin::put_subtitle

use super::dialogs::Ask;
use super::{settings_dialog, UiApp};
use screenrecorder_core::jellyfin::{self, JfItem};
use screenrecorder_core::subtitles::{self, LANGUAGES, MODELS};
use screenrecorder_core::{components, tr, trf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// （影片名稱, 放在哪裡或沒做成的原因）
type Outcome = Vec<(String, String)>;

/// 背景正在產生字幕的
pub struct SubsRun {
    /// （第幾支, 全部幾支, 這一支的名稱）
    pub at: Arc<Mutex<(usize, usize, String)>>,
    /// 這一支辨識到哪裡（千分比）
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
}

impl SubsRun {
    /// Jellyfin 分頁下面畫進度用：（第幾支, 全部幾支, 名稱, 百分比）
    pub fn status(&self) -> (usize, usize, String, u32) {
        let (i, n, name) = self.at.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (i, n, name, self.progress.load(Ordering::Relaxed) / 10)
    }
}

/// 開始幫這些影片產生字幕（語音辨識的檔案還沒下載時請使用者先到「設定 → 外部元件」）
pub fn start(app: &mut UiApp, items: Vec<JfItem>) {
    let items: Vec<JfItem> = items.into_iter().filter(|i| i.kind == jellyfin::JfKind::Video && i.duration > 0.0).collect();
    if items.is_empty() || app.jf_subs.is_some() {
        return;
    }
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        return app.toast(tr!("要先下載錄影元件（FFmpeg）", "Download the recording component (FFmpeg) first"), true);
    };
    let model = (app.s.after_subs_model as usize).min(MODELS.len() - 1);
    app.spawn(async move { subtitles::find_speech_ffmpeg(&ffmpeg).await }, move |app, speech| {
        let need = components::missing_for_subtitles(speech.is_some(), model);
        match speech {
            Some(speech) if need.is_empty() => run(app, speech, items),
            _ => {
                let msg = trf!(
                    "產生字幕要先下載{}。\n\n請到「設定 → 外部元件」下載。辨識在這台電腦上進行，影片不會傳到別的地方。",
                    "Subtitles need {} first.\n\nDownload it in Settings → Components. Recognition runs on this computer; the video isn't sent anywhere else.",
                    components::describe(&need)
                );
                app.ask = Some(Ask::confirm(tr!("需要先下載語音辨識的檔案", "Speech recognition files needed"), msg, tr!("前往外部元件", "Go to Components"), |app, _| {
                    app.ask = None;
                    settings_dialog::open(app, settings_dialog::Page::Components);
                }));
            }
        }
    });
}

fn run(app: &mut UiApp, speech: std::path::PathBuf, items: Vec<JfItem>) {
    let Some(jf) = app.jellyfin() else { return };
    let m = MODELS[(app.s.after_subs_model as usize).min(MODELS.len() - 1)];
    let lang = LANGUAGES[(app.s.after_subs_lang as usize).min(LANGUAGES.len() - 1)].0;
    let run = SubsRun { at: Arc::new(Mutex::new((0, items.len(), String::new()))), progress: Arc::new(AtomicU32::new(0)), cancel: Arc::new(AtomicBool::new(false)) };
    let (at, progress, cancel) = (run.at.clone(), run.progress.clone(), run.cancel.clone());
    app.jf_subs = Some(run);
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            // （做好的名稱與放在哪裡, 沒做成的名稱與原因）
            let (mut done, mut failed): (Outcome, Outcome) = (vec![], vec![]);
            let n = items.len();
            for (i, it) in items.into_iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                *at.lock().unwrap_or_else(|e| e.into_inner()) = (i + 1, n, it.name.clone());
                progress.store(0, Ordering::Relaxed);
                ctx.request_repaint();
                let url = jf.stream_url(&it.id);
                let cues = subtitles::transcribe(&speech, &url, it.duration, &m, lang, progress.clone(), cancel.clone()).await;
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                match cues {
                    Ok(c) if c.is_empty() => failed.push((it.name.clone(), tr!("沒有辨識出說話的內容", "no speech recognized").into())),
                    Ok(c) => {
                        let (jf2, srt, it2) = (jf.clone(), subtitles::to_srt(&c), it.clone());
                        let put = tokio::task::spawn_blocking(move || jf2.put_subtitle(&it2, jellyfin::subtitle_lang(lang), &srt)).await.unwrap_or_else(|e| Err(e.to_string()));
                        match put {
                            Ok(how) => done.push((it.name.clone(), how)),
                            Err(e) => failed.push((it.name.clone(), e)),
                        }
                    }
                    Err(e) => failed.push((it.name.clone(), jellyfin::redact(&e))),
                }
            }
            (done, failed, cancel.load(Ordering::Relaxed))
        },
        |app, (done, failed, cancelled): (Outcome, Outcome, bool)| {
            app.jf_subs = None;
            if cancelled {
                app.toast(trf!("已取消產生字幕（做好 {} 支）", "Subtitles cancelled ({} done)", done.len()), false);
            } else if failed.is_empty() {
                let msg = match done.as_slice() {
                    [(name, how)] => trf!("✓ 「{name}」的字幕{how}了，電視、手機上看也有字幕", "✓ Subtitles for “{name}” {how}; TVs and phones show them too"),
                    _ => trf!("✓ {} 支影片的字幕做好了，電視、手機上看也有字幕", "✓ Subtitles done for {} videos; TVs and phones show them too", done.len()),
                };
                app.toast(msg, false);
            } else {
                let (name, why) = &failed[0];
                app.toast(trf!("做好 {} 支，{} 支沒做成（{name}：{why}）", "{} done, {} failed ({name}: {why})", done.len(), failed.len()), true);
            }
            if !done.is_empty() {
                super::jellyfin_browse::reload(app);
            }
        },
    );
}
