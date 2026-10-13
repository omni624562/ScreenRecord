//! 錄完自動處理：錄影存好後依設定依序做——
//! 1. 剪掉沒動靜的片段（畫面不動而且沒有聲音，見 idle.rs）：另存剪輯版，並存剪輯設定，之後在剪輯視窗可以再改
//! 2. 產生字幕：在影片（有剪輯版時用剪輯版）旁邊存一份 SRT（需要時先下載語音辨識用的 FFmpeg 與語音模型）
//! 3. 壓縮到指定大小以內：另存一支（原速）
//!
//! 設定存在操作視窗的設定（ui）裡（設定組合也會一起存）；系統匣、快速鍵、排程的錄影一樣會處理。
//! 一次處理一支：等手動的轉檔、新的錄影結束才開始下一步（錄影時做會搶 CPU、掉格）。

use crate::app::App;
use crate::edit::EditSpec;
use crate::format::human_duration;
use crate::recorder::BoxFut;
use crate::types::ExportState;
use crate::{info, tr, trf, warn};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 錄完要做什麼（操作視窗設定的 afterIdleCut、afterSubs、afterSubsModel、afterSubsLang、afterCompressMb）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterRecord {
    pub idle_cut: bool,
    pub subtitles: bool,
    /// subtitles::MODELS、LANGUAGES 的第幾個
    pub subs_model: usize,
    pub subs_lang: usize,
    /// 壓縮到這個大小以內（MB）；0 = 不壓縮
    pub compress_mb: u32,
}

/// ui 設定裡的欄位名稱（設定組合也存這些）
pub const UI_KEYS: [&str; 5] = ["afterIdleCut", "afterSubs", "afterSubsModel", "afterSubsLang", "afterCompressMb"];

impl AfterRecord {
    pub fn from_ui(ui: Option<&Value>) -> AfterRecord {
        let Some(o) = ui.and_then(Value::as_object) else { return AfterRecord::default() };
        let b = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
        let n = |k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
        AfterRecord {
            idle_cut: b(UI_KEYS[0]),
            subtitles: b(UI_KEYS[1]),
            subs_model: (n(UI_KEYS[2]) as usize).min(crate::subtitles::MODELS.len() - 1),
            subs_lang: (n(UI_KEYS[3]) as usize).min(crate::subtitles::LANGUAGES.len() - 1),
            compress_mb: n(UI_KEYS[4]).min(10_000) as u32,
        }
    }

    pub fn any(&self) -> bool {
        self.idle_cut || self.subtitles || self.compress_mb > 0
    }
}

/// 只錄聲音存成 M4A 的設定（操作視窗設定的 audioM4a；沒有設定時是要）
pub const AUDIO_M4A_KEY: &str = "audioM4a";

pub fn audio_m4a(ui: Option<&Value>) -> bool {
    ui.and_then(|u| u.get(AUDIO_M4A_KEY)).and_then(Value::as_bool).unwrap_or(true)
}

/// 只錄聲音的錄影（錄影器用「錄音_」開頭的檔名）
pub fn is_audio_recording(path: &str) -> bool {
    Path::new(path).file_name().is_some_and(|n| n.to_string_lossy().starts_with("錄音_"))
}

/// 只錄聲音時另存的 M4A（和錄影同一個資料夾、同名）
pub fn m4a_path(video: &str) -> std::path::PathBuf {
    Path::new(video).with_extension("m4a")
}

/// 處理中的狀態（介面顯示、可以取消）
#[derive(Debug, Clone)]
pub struct AfterStatus {
    /// 錄影的檔名
    pub name: String,
    /// 正在做哪一步
    pub step: String,
    /// 第幾步、總共幾步（剪掉沒動靜的片段、字幕、壓縮）
    pub step_no: usize,
    pub steps: usize,
    /// 這一步的進度（千分比；不知道時是 0）
    pub progress: Arc<AtomicU32>,
    /// 使用者按了取消
    pub cancel: Arc<AtomicBool>,
    /// 停止目前這一步（取消，或讓錄影先做）
    stop_step: Arc<AtomicBool>,
    /// 開始錄影時讓出來：這一步錄完再重做
    preempted: Arc<AtomicBool>,
    /// 正在轉檔的工作是錄完自動處理開始的（介面不另外顯示轉檔卡）
    pub exporting: bool,
    /// 正在辨識字幕（很吃 CPU，開始錄影時也讓出來）
    transcribing: bool,
    /// 這次處理開始過的轉檔工作（完成後的轉檔卡也不另外顯示）
    pub export_ids: Vec<u64>,
    /// 等錄影或其他轉檔結束
    pub waiting: bool,
    /// 處理完了（介面顯示結果）
    pub done: Option<AfterDone>,
}

/// 處理完的結果（介面顯示「已自動處理：…」）
#[derive(Debug, Clone, PartialEq)]
pub struct AfterDone {
    /// 第幾次（介面記住關掉了哪一個）
    pub seq: u64,
    pub ok: bool,
    /// 做了什麼（或失敗的原因）
    pub text: String,
    /// 處理好的影片（剪輯版或壓縮版）；沒有另存影片時是 None
    pub output: Option<String>,
    /// 原本的錄影
    pub source: String,
}

static DONE_SEQ: AtomicU64 = AtomicU64::new(0);

impl App {
    /// 錄完自動處理中（介面顯示用；處理完就是 None）
    pub fn after_status(&self) -> Option<AfterStatus> {
        self.after.lock().unwrap().clone().filter(|s| s.done.is_none())
    }

    /// 最近一次錄完自動處理的結果（取消時沒有）
    pub fn after_done(&self) -> Option<AfterDone> {
        self.after.lock().unwrap().as_ref().and_then(|s| s.done.clone())
    }

    /// 這個轉檔工作是錄完自動處理開始的（介面不另外顯示它的轉檔卡、完成提示）
    pub fn after_owns_export(&self, id: u64) -> bool {
        self.after.lock().unwrap().as_ref().is_some_and(|s| s.export_ids.contains(&id))
    }

    /// 取消錄完自動處理（正在轉檔時一併取消）
    pub fn cancel_after(&self) {
        let Some(s) = self.after_status() else { return };
        s.cancel.store(true, Ordering::Relaxed);
        s.stop_step.store(true, Ordering::Relaxed);
        if s.exporting && self.exporter.running() {
            let _ = self.exporter.cancel();
        }
    }

    /// 要開始錄影了：錄完自動處理正在轉檔或辨識字幕時先停下來，錄完再重做這一步（錄影時做會搶 CPU、掉格）。
    /// 回傳有沒有停下來
    pub(crate) fn after_yield(&self) -> bool {
        let Some(s) = self.after_status() else { return false };
        if !s.exporting && !s.transcribing {
            return false;
        }
        s.preempted.store(true, Ordering::Relaxed);
        s.stop_step.store(true, Ordering::Relaxed);
        if s.exporting && self.exporter.running() {
            let _ = self.exporter.cancel();
        }
        info!("[錄完自動處理] 開始錄影，先暫停「{}」，錄完再繼續", s.step);
        true
    }

    fn after_set(&self, f: impl FnOnce(&mut AfterStatus)) {
        if let Some(s) = self.after.lock().unwrap().as_mut() {
            f(s);
        }
    }

    fn after_step(&self, step: String) {
        self.after_set(|s| {
            s.step = step;
            s.progress.store(0, Ordering::Relaxed);
        });
    }

    /// 開始做一次可以被錄影打斷的事（轉檔、辨識字幕）：清掉之前的記號
    fn after_arm(&self) {
        if let Some(s) = self.after_status() {
            s.preempted.store(false, Ordering::Relaxed);
            if !s.cancel.load(Ordering::Relaxed) {
                s.stop_step.store(false, Ordering::Relaxed);
            }
        }
    }

    /// 這一步被錄影打斷了（清掉記號，等錄完再做一次）
    fn after_take_preempted(&self) -> bool {
        let Some(s) = self.after_status() else { return false };
        let p = s.preempted.swap(false, Ordering::Relaxed);
        if p && !s.cancel.load(Ordering::Relaxed) {
            s.stop_step.store(false, Ordering::Relaxed);
        }
        p && !s.cancel.load(Ordering::Relaxed)
    }

    /// 等到可以轉檔：沒有錄影、沒有其他轉檔工作；取消時回傳 false
    async fn after_wait_idle(&self, cancel: &AtomicBool) -> bool {
        let mut waiting = false;
        let ok = loop {
            if cancel.load(Ordering::Relaxed) || self.is_quitting() {
                break false;
            }
            if !self.recorder.active() && !self.recorder.starting() && !self.exporter.running() {
                break true;
            }
            if !waiting {
                waiting = true;
                self.after_set(|s| s.waiting = true);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        self.after_set(|s| s.waiting = false);
        ok
    }

    /// 錄影存好之後（由錄影器呼叫）：依設定處理，完成時用系統匣通知
    pub async fn after_record(self: Arc<Self>, video: String) {
        let ui = self.settings.load().ui;
        // 只錄聲音、設定「存成 M4A」（預設）：另存一份只有聲音的 M4A（不重新壓縮，一下子就好）
        if is_audio_recording(&video) && audio_m4a(ui.as_ref()) {
            match crate::actions::save_audio(&self, &video).await {
                Ok(out) => info!("[錄音] 存成 M4A：{out}"),
                Err(e) => {
                    warn!("[錄音] 無法存成 M4A：{}", e.message());
                    self.notify(tr!("錄音", "Audio recording"), e.message(), true);
                }
            }
        }
        let after = AfterRecord::from_ui(ui.as_ref());
        if !after.any() {
            return;
        }
        // 前一支還在處理：等它做完（一次一支）
        while self.after_status().is_some() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let name = Path::new(&video).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (progress, cancel) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        let steps = [after.idle_cut, after.subtitles, after.compress_mb > 0].iter().filter(|b| **b).count();
        *self.after.lock().unwrap() = Some(AfterStatus {
            name: name.clone(),
            step: String::new(),
            step_no: 0,
            steps,
            progress: progress.clone(),
            cancel: cancel.clone(),
            stop_step: Arc::new(AtomicBool::new(false)),
            preempted: Arc::new(AtomicBool::new(false)),
            exporting: false,
            transcribing: false,
            export_ids: vec![],
            waiting: false,
            done: None,
        });
        info!("[錄完自動處理] 開始：{name}");
        let mut output = None;
        let r = self.after_run(&video, &after, progress, cancel.clone(), &mut output).await;
        let title = tr!("錄完自動處理", "Post-recording processing");
        let done = match r {
            _ if cancel.load(Ordering::Relaxed) => {
                info!("[錄完自動處理] 已取消：{name}");
                None
            }
            Ok(done) if done.is_empty() => {
                info!("[錄完自動處理] 沒有需要處理的：{name}");
                None
            }
            Ok(done) => {
                let text = done.join(tr!("；", "; "));
                info!("[錄完自動處理] 完成：{name}：{text}");
                self.notify(title, &text, false);
                Some(AfterDone { seq: DONE_SEQ.fetch_add(1, Ordering::Relaxed) + 1, ok: true, text, output, source: video.clone() })
            }
            Err(e) => {
                warn!("[錄完自動處理] {name}：{e}");
                self.notify(title, &e, true);
                Some(AfterDone { seq: DONE_SEQ.fetch_add(1, Ordering::Relaxed) + 1, ok: false, text: e, output: None, source: video.clone() })
            }
        };
        let mut st = self.after.lock().unwrap();
        match done {
            // 留著結果給介面顯示（處理中的狀態不算）
            Some(d) => {
                if let Some(s) = st.as_mut() {
                    s.done = Some(d);
                    s.exporting = false;
                }
            }
            None => *st = None,
        }
    }

    /// 開始一步：第幾步、做什麼
    fn after_begin(&self, step_no: usize, step: String) {
        self.after_set(|s| s.step_no = step_no);
        self.after_step(step);
    }

    /// 錄完自動處理的一個轉檔步驟（剪掉沒動靜的片段、壓縮）：開始錄影時讓出來，錄完再做一次；回傳輸出檔。
    /// cancel 時回傳 Ok(None)
    async fn after_export<'f>(&self, cancel: &AtomicBool, start: impl Fn() -> BoxFut<'f, crate::Result<()>>) -> Result<Option<String>, String> {
        loop {
            if !self.after_wait_idle(cancel).await {
                return Ok(None);
            }
            self.after_arm();
            self.after_set(|s| s.exporting = true);
            if let Err(e) = start().await {
                self.after_set(|s| s.exporting = false);
                // 剛好開始錄影：錄完再做
                if self.recorder.active() || self.recorder.starting() {
                    continue;
                }
                return Err(e.message().to_string());
            }
            if let Some(id) = self.exporter.status().map(|s| s.id) {
                self.after_set(|s| s.export_ids.push(id));
            }
            let r = self.after_export_result().await;
            self.after_set(|s| s.exporting = false);
            if self.after_take_preempted() {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            return r.map(Some);
        }
    }

    async fn after_run(&self, video: &str, after: &AfterRecord, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>, output: &mut Option<String>) -> Result<Vec<String>, String> {
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| tr!("找不到 FFmpeg", "FFmpeg not found").to_string())?;
        let stop_step = self.after.lock().unwrap().as_ref().map(|s| s.stop_step.clone()).unwrap_or_default();
        let mut video = video.to_string();
        let mut done = vec![];
        let mut step_no = 0;
        // 1. 剪掉沒動靜的片段
        if after.idle_cut {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            self.after_begin(step_no, tr!("找出沒動靜的片段", "Finding idle parts").into());
            let info = self.cache.probe(&ffmpeg, &video).await.map_err(|e| e.message().to_string())?;
            let dur = info.duration_sec.unwrap_or(0.0);
            let ranges = crate::idle::find(&ffmpeg, &video, dur, info.has_audio == Some(true)).await.map_err(|e| e.message().to_string())?;
            let cut: f64 = ranges.iter().map(|(a, b)| b - a).sum();
            if cut >= 1.0 {
                self.after_step(tr!("剪掉沒動靜的片段", "Cutting idle parts").into());
                let spec = EditSpec { start: 0.0, end: dur, removed: ranges.clone(), ..Default::default() };
                // 剪輯設定（與剪輯視窗相同的格式）：之後在剪輯視窗打開剪輯版可以再改
                let project = crate::annotate::ProjectData {
                    v: 1,
                    duration: dur,
                    spec: crate::annotate::ProjectSpec { start: 0.0, end: dur, removed: ranges, crop: None },
                    crop_on: false,
                    anns: vec![],
                    audio: Default::default(),
                    fast: vec![],
                    zoom: 0.0,
                    frame: None,
                    cursor: 0.0,
                    vertical: 0,
                };
                let data = serde_json::to_value(project).ok();
                let (me, src, spec, data) = (self, video.as_str(), &spec, &data);
                let Some(out) = self.after_export(&cancel, move || Box::pin(crate::actions::cut_start(me, src, spec, None, data.clone()))).await? else { return Ok(done) };
                done.push(trf!("剪掉 {} 沒動靜的片段", "cut {} of idle time", human_duration(cut)));
                video = out.clone();
                *output = Some(out);
            } else {
                done.push(tr!("沒有沒動靜的片段", "no idle parts").into());
            }
        }
        // 2. 字幕
        if after.subtitles {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            // 目前用的 FFmpeg 沒有 whisper：先下載語音辨識用的 FFmpeg（和語音模型一樣，只有第一次）
            let speech = match crate::subtitles::find_speech_ffmpeg(&ffmpeg).await {
                Some(p) => p,
                None => {
                    self.after_begin(step_no, tr!("下載語音辨識用的 FFmpeg", "Downloading FFmpeg for speech recognition").into());
                    crate::subtitles::download_speech_ffmpeg(progress.clone(), cancel.clone()).await?
                }
            };
            let m = crate::subtitles::MODELS[after.subs_model];
            if !crate::subtitles::model_ready(&m) {
                self.after_begin(step_no, tr!("下載語音模型", "Downloading the speech model").into());
                let (p, c) = (progress.clone(), cancel.clone());
                tokio::task::spawn_blocking(move || crate::subtitles::download_model(&m, &p, &c)).await.map_err(|e| e.to_string())??;
            }
            let dur = self.cache.probe(&ffmpeg, &video).await.ok().and_then(|i| i.duration_sec).unwrap_or(0.0);
            let lang = crate::subtitles::LANGUAGES[after.subs_lang].0;
            // 辨識很吃 CPU：開始錄影時停下來，錄完再重新辨識
            let cues = loop {
                if !self.after_wait_idle(&cancel).await {
                    return Ok(done);
                }
                self.after_begin(step_no, tr!("產生字幕", "Generating subtitles").into());
                self.after_arm();
                self.after_set(|s| s.transcribing = true);
                let r = crate::subtitles::transcribe(&speech, &video, dur, &m, lang, progress.clone(), stop_step.clone()).await;
                self.after_set(|s| s.transcribing = false);
                if self.after_take_preempted() {
                    continue;
                }
                break r?;
            };
            if cancel.load(Ordering::Relaxed) {
                return Ok(done);
            }
            if cues.is_empty() {
                done.push(tr!("沒有辨識出說話的內容", "no speech recognized").into());
            } else {
                let srt = crate::subtitles::save_next_to(&video, &cues)?;
                done.push(trf!("字幕存成「{}」", "subtitles saved as “{}”", srt.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
        }
        // 3. 壓縮
        if after.compress_mb > 0 {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            let bytes = std::fs::metadata(&video).map(|m| m.len()).unwrap_or(0);
            if bytes <= after.compress_mb as u64 * 1024 * 1024 {
                done.push(trf!("已經小於 {} MB，不用壓縮", "already under {} MB, not compressed", after.compress_mb));
            } else {
                self.after_begin(step_no, trf!("壓縮到 {} MB 以內", "Compressing to under {} MB", after.compress_mb));
                let (me, src, mb) = (self, video.as_str(), after.compress_mb as f64);
                let Some(out) = self.after_export(&cancel, move || Box::pin(async move { me.exporter.start(&me.export_ctx(), src, 1.0, true, 0.0, mb).await.map(|_| ()) })).await? else {
                    return Ok(done);
                };
                let size = std::fs::metadata(&out).map(|m| crate::format::format_bytes(m.len())).unwrap_or_default();
                done.push(trf!("壓縮版 {size}", "compressed copy {size}"));
                *output = Some(out);
            }
        }
        Ok(done)
    }

    /// 等轉檔工作結束：完成時回傳輸出檔，取消或失敗時回傳錯誤訊息
    async fn after_export_result(&self) -> Result<String, String> {
        self.exporter.wait().await;
        let s = self.exporter.status().ok_or_else(|| tr!("轉檔沒有結果", "The conversion didn't finish").to_string())?;
        match s.state {
            ExportState::Done => Ok(s.output),
            ExportState::Canceled => Err(tr!("已取消", "Canceled").into()),
            _ => Err(s.message.unwrap_or_else(|| tr!("轉檔失敗", "The conversion failed").into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn audio_recordings_are_also_saved_as_m4a() {
        assert!(is_audio_recording("C:\\v\\錄音_2026-10-13_10-00-00.mp4") && !is_audio_recording("C:\\v\\Rec_2026-10-13_10-00-00.mp4"));
        assert!(audio_m4a(None) && audio_m4a(Some(&json!({}))) && !audio_m4a(Some(&json!({ "audioM4a": false }))));
        assert_eq!(m4a_path("/v/錄音_1.mp4"), std::path::PathBuf::from("/v/錄音_1.m4a"));
    }

    #[test]
    fn reads_settings() {
        assert!(!AfterRecord::from_ui(None).any());
        let a = AfterRecord::from_ui(Some(&json!({ "afterIdleCut": true, "afterSubs": true, "afterSubsModel": 9, "afterSubsLang": 1, "afterCompressMb": 25 })));
        assert_eq!(a, AfterRecord { idle_cut: true, subtitles: true, subs_model: crate::subtitles::MODELS.len() - 1, subs_lang: 1, compress_mb: 25 });
        assert!(a.any());
        assert!(AfterRecord::from_ui(Some(&json!({ "afterCompressMb": 10 }))).any());
    }
}
