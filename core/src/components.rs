//! 外部元件（設定 → 外部元件）：FFmpeg、mpv 播放元件、語音辨識用的 FFmpeg、語音模型，下載、取消、移除都在這裡。
//! 其他功能（剪輯視窗的自動字幕、錄完自動處理）缺少元件時不自己下載，請使用者到設定下載。
//!
//! 無法自動下載時（公司網路擋住、沒有網路……）：
//! - 設定 Proxy（http.rs）
//! - 用瀏覽器（或另一台電腦）下載，再「選擇已下載的檔案」安裝：FFmpeg 可以選 ffmpeg.exe 或 zip，語音模型選 .bin

use crate::app::App;
use crate::subtitles::{self, Model, MODELS};
use crate::{tr, trf};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// 哪一個元件
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// 錄影、轉檔用的 FFmpeg（必要）
    Ffmpeg,
    /// mpv 播放元件（libmpv-2.dll）：檢視器播放更順（沒有時用 FFmpeg 播放）
    Mpv,
    /// 語音辨識用的 FFmpeg（目前的 FFmpeg 沒有 whisper 時才需要）
    SpeechFfmpeg,
    /// 語音模型（MODELS 的第幾個）
    Model(usize),
}

impl Kind {
    pub fn all() -> Vec<Kind> {
        let mut v = vec![Kind::Ffmpeg, Kind::Mpv, Kind::SpeechFfmpeg];
        v.extend((0..MODELS.len()).map(Kind::Model));
        v
    }

    pub fn model(self) -> Option<&'static Model> {
        match self {
            Kind::Model(i) => MODELS.get(i),
            _ => None,
        }
    }

    /// 名稱（依介面語言）
    pub fn name(self) -> String {
        match self {
            Kind::Ffmpeg => "FFmpeg".into(),
            Kind::Mpv => tr!("mpv 播放元件", "mpv playback component").into(),
            Kind::SpeechFfmpeg => tr!("語音辨識用的 FFmpeg", "FFmpeg for speech recognition").into(),
            Kind::Model(_) => trf!("語音模型：{}", "Speech model: {}", self.model().map(|m| m.label()).unwrap_or_default()),
        }
    }

    /// 大約幾 MB（下載的大小）
    pub fn mb(self) -> u32 {
        match self {
            Kind::Ffmpeg => 100,
            Kind::Mpv => crate::mpv_download::MPV_MB,
            Kind::SpeechFfmpeg => crate::downloader::SPEECH_FFMPEG_MB,
            Kind::Model(_) => self.model().map(|m| m.mb).unwrap_or(0),
        }
    }

    /// 用瀏覽器下載的網址（自動下載失敗時）
    pub fn url(self) -> String {
        match self {
            Kind::Ffmpeg => crate::downloader::ffmpeg_urls().into_iter().next().unwrap_or_default(),
            Kind::Mpv => crate::mpv_download::mpv_urls().into_iter().next().unwrap_or_default(),
            Kind::SpeechFfmpeg => crate::downloader::speech_ffmpeg_urls().into_iter().next().unwrap_or_default(),
            Kind::Model(_) => self.model().map(|m| m.url.to_string()).unwrap_or_default(),
        }
    }

    /// 選擇已下載的檔案時，檔案類型篩選
    pub fn file_filter(self) -> &'static str {
        match self {
            Kind::Ffmpeg | Kind::SpeechFfmpeg => "ffmpeg.exe;*.zip",
            Kind::Mpv => "libmpv-2.dll;*.7z",
            Kind::Model(_) => "*.bin",
        }
    }

    /// 安裝的位置（FFmpeg 是自動下載會放的位置）
    pub fn path(self) -> Option<PathBuf> {
        match self {
            Kind::Ffmpeg => crate::downloader::download_target().ok(),
            Kind::Mpv => Some(crate::mpv_download::installed_path().unwrap_or_else(crate::mpv_download::install_path)),
            Kind::SpeechFfmpeg => Some(subtitles::speech_ffmpeg_path()),
            Kind::Model(_) => self.model().map(subtitles::model_path),
        }
    }
}

/// 一個元件的下載狀態
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobStatus {
    pub running: bool,
    /// 0～1
    pub progress: f32,
    /// 上次失敗的原因（取消不算）
    pub error: Option<String>,
}

struct Job {
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    running: bool,
    error: Option<String>,
}

/// 語音辨識用的 FFmpeg、語音模型的下載工作（FFmpeg 用 App::downloader）
#[derive(Default)]
pub struct Components {
    jobs: Mutex<HashMap<Kind, Job>>,
}

impl Components {
    fn status(&self, k: Kind) -> JobStatus {
        let g = self.jobs.lock().unwrap();
        g.get(&k).map(|j| JobStatus { running: j.running, progress: j.progress.load(Ordering::Relaxed) as f32 / 1000.0, error: j.error.clone() }).unwrap_or_default()
    }

    /// 開始：回傳 (progress, cancel)；已經在下載就回傳 None
    fn begin(&self, k: Kind) -> Option<(Arc<AtomicU32>, Arc<AtomicBool>)> {
        let mut g = self.jobs.lock().unwrap();
        if g.get(&k).is_some_and(|j| j.running) {
            return None;
        }
        let (p, c) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        g.insert(k, Job { progress: p.clone(), cancel: c.clone(), running: true, error: None });
        Some((p, c))
    }

    fn end(&self, k: Kind, r: Result<(), String>) {
        let mut g = self.jobs.lock().unwrap();
        if let Some(j) = g.get_mut(&k) {
            j.running = false;
            let canceled = j.cancel.load(Ordering::Relaxed);
            j.error = r.err().filter(|_| !canceled);
        }
    }

    fn cancel(&self, k: Kind) {
        if let Some(j) = self.jobs.lock().unwrap().get(&k) {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn set_error(&self, k: Kind, e: Option<String>) {
        let mut g = self.jobs.lock().unwrap();
        let j = g.entry(k).or_insert_with(|| Job { progress: Arc::new(AtomicU32::new(0)), cancel: Arc::new(AtomicBool::new(false)), running: false, error: None });
        if !j.running {
            j.error = e;
        }
    }
}

/// 外部元件以程式所在的資料夾為主（整個資料夾帶著走也能用）：以前因為那裡不能寫入等原因放到
/// %LOCALAPPDATA%\ScreenRecorder 的 FFmpeg、mpv 播放元件、語音辨識用的 FFmpeg 與語音模型，程式資料夾能寫入時搬過去。
/// 啟動時、還沒開始用這些檔案之前呼叫（另一個執行中的程式可能正在用：要在確定只有自己在執行之後）
pub fn move_to_app_dir() {
    for line in move_components(&crate::paths::data_dir(), &crate::paths::app_dir()) {
        crate::info!("[外部元件] {line}");
    }
}

/// 可能放在資料夾裡的外部元件（相對位置）
fn component_files() -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let mut v = vec![PathBuf::from(exe), PathBuf::from(crate::mpv::LIB_NAME), Path::new("whisper").join(exe)];
    v.extend(MODELS.iter().map(|m| Path::new("whisper").join(m.file)));
    v
}

/// 把 from 裡的外部元件搬到 to（to 不能寫入時不動）；to 已經有同樣大小的檔案時，from 的那份用不到了，刪掉。回傳做了什麼（寫進記錄檔）
fn move_components(from: &Path, to: &Path) -> Vec<String> {
    let mut log = vec![];
    if crate::paths::inside(from, to) && crate::paths::inside(to, from) {
        return log;
    }
    let files: Vec<PathBuf> = component_files().into_iter().filter(|f| from.join(f).is_file()).collect();
    if files.is_empty() {
        return log;
    }
    if !crate::selfupdate::dir_writable(to) {
        log.push(format!("程式資料夾 {} 不能寫入，元件留在 {}", to.display(), from.display()));
        return log;
    }
    for f in files {
        let (src, dst) = (from.join(&f), to.join(&f));
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
        if dst.exists() {
            if size(&src) == size(&dst) {
                match std::fs::remove_file(&src) {
                    Ok(()) => log.push(format!("程式資料夾已經有 {}，刪掉 {} 裡用不到的那份", f.display(), from.display())),
                    Err(e) => log.push(format!("無法刪掉用不到的 {}：{e}", src.display())),
                }
            } else {
                log.push(format!("{} 和 {} 都有 {}（大小不同），保留不動", from.display(), to.display(), f.display()));
            }
            continue;
        }
        if let Some(dir) = dst.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // 同一顆硬碟直接搬；不同顆時複製好再刪掉原本的（複製到一半不會留下不完整的元件）
        let moved = std::fs::rename(&src, &dst).is_ok() || {
            let part = dst.with_extension("part");
            let ok = crate::paths::copy_keep_time(&src, &part).is_ok() && std::fs::rename(&part, &dst).is_ok();
            let _ = std::fs::remove_file(&part);
            ok && std::fs::remove_file(&src).is_ok()
        };
        log.push(if moved { format!("已把 {} 搬到 {}", src.display(), dst.display()) } else { format!("無法把 {} 搬到 {}，留在原處", src.display(), dst.display()) });
    }
    // 搬空了的 whisper 資料夾（remove_dir 只刪空的）
    let _ = std::fs::remove_dir(from.join("whisper"));
    log
}

/// 自動字幕還缺哪些元件（main_has_whisper：目前用的 FFmpeg 能辨識語音）
pub fn missing_for_subtitles(main_has_whisper: bool, model: usize) -> Vec<Kind> {
    let mut v = vec![];
    if !main_has_whisper && !subtitles::speech_ffmpeg_downloaded() {
        v.push(Kind::SpeechFfmpeg);
    }
    let m = model.min(MODELS.len() - 1);
    if !subtitles::model_ready(&MODELS[m]) {
        v.push(Kind::Model(m));
    }
    v
}

/// 翻成英文還缺的模型：選的模型不能翻譯、也沒有其他能翻譯的模型時，要下載「精準」
pub fn missing_for_translation(model: usize) -> Vec<Kind> {
    let m = &MODELS[model.min(MODELS.len() - 1)];
    if subtitles::translation_model(m).is_some() {
        return vec![];
    }
    vec![Kind::Model(MODELS.iter().rposition(|o| o.translates).unwrap_or(0))]
}

/// 「語音辨識用的 FFmpeg（約 258 MB）、語音模型：標準（約 150 MB）」
pub fn describe(list: &[Kind]) -> String {
    list.iter()
        .map(|k| match k {
            Kind::Model(_) => k.name(),
            _ => trf!("{}（約 {} MB）", "{} (about {} MB)", k.name(), k.mb()),
        })
        .collect::<Vec<_>>()
        .join(tr!("、", ", "))
}

/// 安裝使用者選的語音模型檔：確認是 whisper.cpp 的模型（開頭 "lmgg"）、不是另一個模型，再放到定位
pub fn install_model_file(m: &Model, src: &Path) -> Result<(), String> {
    let name = src.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if let Some(other) = MODELS.iter().find(|o| o.id != m.id && name == o.file.to_lowercase()) {
        return Err(trf!("這是「{}」的模型檔，請選 {}", "This is the model file for “{}”. Choose {}", other.label(), m.file));
    }
    if !subtitles::is_model_file(src) {
        return Err(trf!("這不是語音模型檔（請選 {}）", "This isn't a speech model file (choose {})", m.file));
    }
    if !subtitles::model_size_ok(m, src) {
        return Err(trf!("檔案不完整（應該約 {} MB），請重新下載", "The file is incomplete (it should be about {} MB). Please download it again", m.mb));
    }
    let target = subtitles::model_install_path(m);
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let staging = target.with_extension("part");
    std::fs::copy(src, &staging).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
    crate::info!("[外部元件] 已安裝選擇的語音模型 {}", m.file);
    Ok(())
}

impl App {
    /// 元件已經可以用了嗎
    pub fn component_installed(&self, k: Kind) -> bool {
        match k {
            Kind::Ffmpeg => self.env().ffmpeg.found,
            // 下載的，或系統裝的（Linux：apt install libmpv2）
            Kind::Mpv => crate::mpv_download::installed_path().is_some() || crate::mpv::available(),
            Kind::SpeechFfmpeg => self.env().ffmpeg.has_whisper || subtitles::speech_ffmpeg_downloaded(),
            Kind::Model(_) => k.model().is_some_and(subtitles::model_ready),
        }
    }

    pub fn component_status(&self, k: Kind) -> JobStatus {
        if k != Kind::Ffmpeg {
            return self.components.status(k);
        }
        use crate::types::DownloadPhase as P;
        let d = self.downloader.status();
        let running = matches!(d.phase, P::Downloading | P::Verifying | P::Extracting);
        let progress = match d.phase {
            P::Downloading => d.total.map(|t| d.received as f32 / t.max(1) as f32).unwrap_or(0.0),
            P::Verifying | P::Extracting => 0.999,
            _ => 0.0,
        };
        let error = if d.phase == P::Error { d.message } else { self.components.status(k).error };
        JobStatus { running, progress, error }
    }

    /// 開始下載（設定 → 外部元件）
    pub fn component_download(self: &Arc<Self>, k: Kind) {
        if k == Kind::Ffmpeg {
            self.components.set_error(k, None);
            self.start_download();
            return;
        }
        let Some((progress, cancel)) = self.components.begin(k) else { return };
        let app = self.clone();
        tokio::spawn(async move {
            let r = match k {
                Kind::SpeechFfmpeg => subtitles::download_speech_ffmpeg(progress, cancel).await.map(|_| ()),
                Kind::Mpv => tokio::task::spawn_blocking(move || crate::mpv_download::download(&progress, &cancel)).await.map_err(|e| e.to_string()).and_then(|r| r.map(|_| ())),
                Kind::Model(_) => match k.model().copied() {
                    Some(m) => tokio::task::spawn_blocking(move || subtitles::download_model(&m, &progress, &cancel)).await.map_err(|e| e.to_string()).and_then(|r| r),
                    None => Ok(()),
                },
                Kind::Ffmpeg => Ok(()),
            };
            // 透過 Proxy 連不上時，說明用的是哪一個（多半是 Proxy 設定的問題）
            let r = r.map_err(|e| match crate::http::current_proxy() {
                Some(px) => trf!("{e}（目前透過 Proxy：{px}）", "{e} (using proxy: {px})"),
                _ => e,
            });
            if let Err(e) = &r {
                crate::error!("[外部元件] {} 下載失敗：{e}", k.name());
            }
            app.components.end(k, r);
        });
    }

    pub fn component_cancel(&self, k: Kind) {
        match k {
            Kind::Ffmpeg => self.downloader.cancel(),
            _ => self.components.cancel(k),
        }
    }

    /// 移除下載的語音辨識檔案（FFmpeg 不在這裡移除：錄影要用）
    pub fn component_remove(&self, k: Kind) -> Result<(), String> {
        let path = match k {
            Kind::Ffmpeg => return Err(tr!("FFmpeg 是錄影必要的元件，不能移除", "FFmpeg is required for recording and can't be removed").into()),
            Kind::Mpv => crate::mpv_download::installed_path()
                .ok_or_else(|| tr!("這是系統安裝的 libmpv，請用系統的套件管理程式移除", "This libmpv is installed by the system. Remove it with your package manager").to_string())?,
            _ => k.path().ok_or_else(|| tr!("找不到檔案", "File not found").to_string())?,
        };
        if self.after_status().is_some() {
            return Err(tr!("錄完自動處理進行中，完成後再移除", "Processing after recording is running. Remove it when it's done").into());
        }
        std::fs::remove_file(&path).map_err(|e| {
            // 播放過之後 DLL 已經載入，Windows 上刪不掉
            if k == Kind::Mpv {
                tr!("播放元件使用中，請重新開啟程式後再移除", "The playback component is in use. Restart the app and then remove it").to_string()
            } else {
                e.to_string()
            }
        })?;
        crate::info!("[外部元件] 已移除 {}", path.display());
        self.components.set_error(k, None);
        Ok(())
    }

    /// 安裝使用者自己下載的檔案（無法自動下載時）：FFmpeg 可以是 ffmpeg.exe 或 zip，語音模型是 .bin
    pub async fn component_install_file(self: &Arc<Self>, k: Kind, src: PathBuf) -> Result<(), String> {
        if self.components.status(k).running || self.component_status(k).running {
            return Err(tr!("正在下載，請先取消或等下載完成", "A download is in progress. Cancel it or wait for it to finish").into());
        }
        let r = match k {
            Kind::Ffmpeg => {
                let target = crate::downloader::download_target()?;
                let s = src.clone();
                let r = tokio::task::spawn_blocking(move || crate::downloader::install_local(&s, &target)).await.map_err(|e| e.to_string()).and_then(|r| r);
                if r.is_ok() {
                    self.refresh(false).await;
                    if !self.env().ffmpeg.found {
                        return Err(tr!("已放好 ffmpeg.exe，但偵測不到，請按「重新偵測」", "ffmpeg.exe was copied but couldn't be detected. Press Detect again").into());
                    }
                }
                r.map(|_| ())
            }
            Kind::Mpv => {
                let (s, t) = (src.clone(), crate::mpv_download::install_path());
                tokio::task::spawn_blocking(move || crate::mpv_download::install_file(&s, &t)).await.map_err(|e| e.to_string()).and_then(|r| r)
            }
            Kind::SpeechFfmpeg => {
                let target = subtitles::speech_ffmpeg_install_path();
                let (s, t) = (src.clone(), target.clone());
                let r = tokio::task::spawn_blocking(move || crate::downloader::install_local(&s, &t)).await.map_err(|e| e.to_string()).and_then(|r| r);
                match r {
                    Ok(_) if !subtitles::has_whisper(&target).await => {
                        let _ = std::fs::remove_file(&target);
                        Err(tr!(
                            "這個 FFmpeg 不能辨識語音（沒有 whisper）。請選 FFmpeg 8 以上的 full 版本，例如 ffmpeg-…-full_build.zip",
                            "This FFmpeg can't recognize speech (no whisper). Choose a full build of FFmpeg 8 or later, e.g. ffmpeg-…-full_build.zip"
                        )
                        .into())
                    }
                    r => r.map(|_| ()),
                }
            }
            Kind::Model(_) => match k.model().copied() {
                Some(m) => {
                    let s = src.clone();
                    tokio::task::spawn_blocking(move || install_model_file(&m, &s)).await.map_err(|e| e.to_string()).and_then(|r| r)
                }
                None => Ok(()),
            },
        };
        match &r {
            Ok(()) => {
                crate::info!("[外部元件] 已安裝選擇的檔案：{}", k.name());
                self.components.set_error(k, None);
            }
            Err(e) => crate::error!("[外部元件] 安裝選擇的檔案失敗（{}）：{e}", k.name()),
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_file_checks() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("ggml-base.bin");
        std::fs::write(&bad, b"not a model").unwrap();
        assert!(install_model_file(&MODELS[0], &bad).is_err(), "不是模型檔");
        let other = dir.path().join("ggml-small.bin");
        std::fs::write(&other, b"lmgg....").unwrap();
        assert!(install_model_file(&MODELS[0], &other).unwrap_err().contains("ggml-base.bin"), "選錯模型");
        let short = dir.path().join("my-base.bin");
        std::fs::write(&short, b"lmgg....").unwrap();
        assert!(install_model_file(&MODELS[0], &short).unwrap_err().contains("148"), "太小：下載不完整");
    }

    #[test]
    fn components_move_next_to_the_program() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (f, t) = (from.path(), to.path());
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        std::fs::create_dir_all(f.join("whisper")).unwrap();
        for (name, body) in [(exe.to_string(), "ff"), (crate::mpv::LIB_NAME.to_string(), "mpv"), (format!("whisper/{exe}"), "speech"), (format!("whisper/{}", MODELS[0].file), "model")] {
            std::fs::write(f.join(&name), body).unwrap();
        }
        // 不是元件的檔案（設定、記錄檔）不動
        std::fs::write(f.join("settings.json"), "{}").unwrap();
        let log = move_components(f, t);
        assert_eq!(log.len(), 4, "{log:?}");
        assert_eq!(std::fs::read_to_string(t.join(crate::mpv::LIB_NAME)).unwrap(), "mpv");
        assert_eq!(std::fs::read_to_string(t.join("whisper").join(MODELS[0].file)).unwrap(), "model");
        assert_eq!(std::fs::read_to_string(t.join("whisper").join(exe)).unwrap(), "speech");
        assert!(!f.join(exe).exists() && !f.join("whisper").exists(), "搬空的 whisper 資料夾也刪掉");
        assert!(f.join("settings.json").exists() && !t.join("settings.json").exists());
        // 再搬一次：沒有東西要做
        assert!(move_components(f, t).is_empty());
    }

    #[test]
    fn components_already_next_to_the_program() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (f, t) = (from.path(), to.path());
        let lib = crate::mpv::LIB_NAME;
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        std::fs::write(f.join(lib), "same").unwrap();
        std::fs::write(t.join(lib), "same").unwrap();
        std::fs::write(f.join(exe), "old build").unwrap();
        std::fs::write(t.join(exe), "new").unwrap();
        move_components(f, t);
        assert!(!f.join(lib).exists(), "一樣的那份用不到了，刪掉");
        assert_eq!(std::fs::read_to_string(f.join(exe)).unwrap(), "old build", "大小不同：兩份都留著");
        assert_eq!(std::fs::read_to_string(t.join(exe)).unwrap(), "new");
        // 同一個資料夾：什麼都不做
        assert!(move_components(t, t).is_empty());
        // 程式資料夾不能寫入（這裡用一個檔案代替資料夾）：留在原處
        let file = t.join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let log = move_components(f, &file);
        assert!(f.join(exe).exists() && log.len() == 1, "{log:?}");
    }

    #[test]
    fn jobs_track_running_and_errors() {
        let c = Components::default();
        let k = Kind::Model(0);
        let (_, cancel) = c.begin(k).unwrap();
        assert!(c.begin(k).is_none(), "同一個元件不重複下載");
        assert!(c.status(k).running);
        c.end(k, Err("連線逾時".into()));
        assert_eq!(c.status(k).error.as_deref(), Some("連線逾時"));
        let (_, cancel2) = c.begin(k).unwrap();
        assert!(c.status(k).error.is_none(), "重新下載時清掉上次的錯誤");
        c.cancel(k);
        assert!(cancel2.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed));
        c.end(k, Err("已取消".into()));
        assert!(c.status(k).error.is_none(), "取消不算失敗");
    }
}
