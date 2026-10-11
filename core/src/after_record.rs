//! 錄完自動處理：錄影存好後依設定依序做——
//! 1. 剪掉沒動靜的片段（畫面不動而且沒有聲音，見 idle.rs）：另存剪輯版，並存剪輯設定，之後在剪輯視窗可以再改
//! 2. 產生字幕：在影片（有剪輯版時用剪輯版）旁邊存一份 SRT（需要時先下載語音模型）
//! 3. 壓縮到指定大小以內：另存一支（原速）
//!
//! 設定存在操作視窗的設定（ui）裡（設定組合也會一起存）；系統匣、快速鍵、排程的錄影一樣會處理。
//! 一次處理一支：等手動的轉檔、新的錄影結束才開始下一步（錄影時做會搶 CPU、掉格）。

use crate::app::App;
use crate::edit::EditSpec;
use crate::format::video_clock;
use crate::types::ExportState;
use crate::{info, tr, trf, warn};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

/// 處理中的狀態（介面顯示、可以取消）
#[derive(Debug, Clone)]
pub struct AfterStatus {
    /// 錄影的檔名
    pub name: String,
    /// 正在做哪一步
    pub step: String,
    /// 這一步的進度（千分比；不知道時是 0）
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
}

impl App {
    /// 錄完自動處理中（介面顯示用）
    pub fn after_status(&self) -> Option<AfterStatus> {
        self.after.lock().unwrap().clone()
    }

    /// 取消錄完自動處理（正在轉檔時一併取消）
    pub fn cancel_after(&self) {
        let Some(s) = self.after_status() else { return };
        s.cancel.store(true, Ordering::Relaxed);
        if self.exporter.running() {
            let _ = self.exporter.cancel();
        }
    }

    fn after_step(&self, step: String) {
        if let Some(s) = self.after.lock().unwrap().as_mut() {
            s.step = step;
            s.progress.store(0, Ordering::Relaxed);
        }
    }

    /// 等到可以轉檔：沒有錄影、沒有其他轉檔工作；取消時回傳 false
    async fn after_wait_idle(&self, cancel: &AtomicBool) -> bool {
        loop {
            if cancel.load(Ordering::Relaxed) || self.is_quitting() {
                return false;
            }
            if !self.recorder.active() && !self.exporter.running() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// 錄影存好之後（由錄影器呼叫）：依設定處理，完成時用系統匣通知
    pub async fn after_record(self: Arc<Self>, video: String) {
        let after = AfterRecord::from_ui(self.settings.load().ui.as_ref());
        if !after.any() {
            return;
        }
        // 前一支還在處理：等它做完（一次一支）
        while self.after.lock().unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let name = Path::new(&video).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (progress, cancel) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        *self.after.lock().unwrap() = Some(AfterStatus { name: name.clone(), step: String::new(), progress: progress.clone(), cancel: cancel.clone() });
        info!("[錄完自動處理] 開始：{name}");
        let r = self.after_run(&video, &after, progress, cancel.clone()).await;
        self.after.lock().unwrap().take();
        let title = tr!("錄完自動處理", "Post-recording processing");
        match r {
            _ if cancel.load(Ordering::Relaxed) => info!("[錄完自動處理] 已取消：{name}"),
            Ok(done) if done.is_empty() => info!("[錄完自動處理] 沒有需要處理的：{name}"),
            Ok(done) => {
                let text = done.join(tr!("；", "; "));
                info!("[錄完自動處理] 完成：{name}：{text}");
                self.notify(title, &text, false);
            }
            Err(e) => {
                warn!("[錄完自動處理] {name}：{e}");
                self.notify(title, &e, true);
            }
        }
    }

    async fn after_run(&self, video: &str, after: &AfterRecord, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<Vec<String>, String> {
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| tr!("找不到 FFmpeg", "FFmpeg not found").to_string())?;
        let mut video = video.to_string();
        let mut done = vec![];
        // 1. 剪掉沒動靜的片段
        if after.idle_cut {
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            self.after_step(tr!("找出沒動靜的片段", "Finding idle parts").into());
            let info = self.cache.probe(&ffmpeg, &video).await.map_err(|e| e.message().to_string())?;
            let dur = info.duration_sec.unwrap_or(0.0);
            let ranges = crate::idle::find(&ffmpeg, &video, dur, info.has_audio == Some(true)).await.map_err(|e| e.message().to_string())?;
            let cut: f64 = ranges.iter().map(|(a, b)| b - a).sum();
            if cut >= 1.0 && self.after_wait_idle(&cancel).await {
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
                };
                crate::actions::cut_start(self, &video, &spec, None, serde_json::to_value(project).ok()).await.map_err(|e| e.message().to_string())?;
                let out = self.after_export_result().await?;
                if cancel.load(Ordering::Relaxed) {
                    return Ok(done);
                }
                done.push(trf!("剪掉 {} 沒動靜的片段", "cut {} of idle time", video_clock(cut)));
                video = out;
            } else if cut < 1.0 {
                done.push(tr!("沒有沒動靜的片段", "no idle parts").into());
            }
        }
        // 2. 字幕
        if after.subtitles {
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            if !crate::subtitles::has_whisper(&ffmpeg).await {
                done.push(tr!("沒有產生字幕（這個 FFmpeg 沒有語音辨識）", "no subtitles (this FFmpeg has no speech recognition)").into());
            } else {
                let m = crate::subtitles::MODELS[after.subs_model];
                if !crate::subtitles::model_ready(&m) {
                    self.after_step(tr!("下載語音模型", "Downloading the speech model").into());
                    let (p, c) = (progress.clone(), cancel.clone());
                    tokio::task::spawn_blocking(move || crate::subtitles::download_model(&m, &p, &c)).await.map_err(|e| e.to_string())??;
                }
                self.after_step(tr!("產生字幕", "Generating subtitles").into());
                let dur = self.cache.probe(&ffmpeg, &video).await.ok().and_then(|i| i.duration_sec).unwrap_or(0.0);
                let lang = crate::subtitles::LANGUAGES[after.subs_lang].0;
                let cues = crate::subtitles::transcribe(&ffmpeg, &video, dur, &m, lang, progress.clone(), cancel.clone()).await?;
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
        }
        // 3. 壓縮
        if after.compress_mb > 0 {
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            let bytes = std::fs::metadata(&video).map(|m| m.len()).unwrap_or(0);
            if bytes <= after.compress_mb as u64 * 1024 * 1024 {
                done.push(trf!("已經小於 {} MB，不用壓縮", "already under {} MB, not compressed", after.compress_mb));
            } else {
                self.after_step(trf!("壓縮到 {} MB 以內", "Compressing to under {} MB", after.compress_mb));
                self.exporter.start(&self.export_ctx(), &video, 1.0, true, 0.0, after.compress_mb as f64).await.map_err(|e| e.message().to_string())?;
                let out = self.after_export_result().await?;
                if !cancel.load(Ordering::Relaxed) {
                    let size = std::fs::metadata(&out).map(|m| crate::format::format_bytes(m.len())).unwrap_or_default();
                    done.push(trf!("壓縮版 {size}", "compressed copy {size}"));
                }
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
    fn reads_settings() {
        assert!(!AfterRecord::from_ui(None).any());
        let a = AfterRecord::from_ui(Some(&json!({ "afterIdleCut": true, "afterSubs": true, "afterSubsModel": 9, "afterSubsLang": 1, "afterCompressMb": 25 })));
        assert_eq!(a, AfterRecord { idle_cut: true, subtitles: true, subs_model: crate::subtitles::MODELS.len() - 1, subs_lang: 1, compress_mb: 25 });
        assert!(a.any());
        assert!(AfterRecord::from_ui(Some(&json!({ "afterCompressMb": 10 }))).any());
    }
}
