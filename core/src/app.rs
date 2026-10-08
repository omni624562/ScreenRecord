//! 全域狀態：FFmpeg 偵測結果、螢幕與音訊裝置清單、錄影器、轉檔、下載、檢查新版本、預覽。

use crate::args::{desktop_rect, encoder_spec, preview_args, EncoderSpec};
use crate::downloader::{Deps as DownloadDeps, Downloader};
use crate::exporter::{ExportCtx, Exporter};
use crate::ffmpeg::{probe_ffmpeg, test_ddagrab, test_hw_encoders};
use crate::library::MediaCache;
use crate::paths::{data_dir, default_output_dir, now_ms};
use crate::process::command;
use crate::recorder::{BoxFut, Encoders, Recorder, RecorderDeps};
use crate::settings::{SettingsPatch, SettingsStore};
use crate::thumbs::Thumbnails;
use crate::types::{AudioEnv, EnvInfo, FfmpegInfo, HotkeyStatus, MonitorInfo, Rect, UpdateInfo};
use crate::updater::{check_for_update, UpdateError};
use crate::version::APP_VERSION;
use crate::{info, warn};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

pub type Notifier = Arc<dyn Fn(&str, &str, bool) + Send + Sync>;
pub type UiOpener = Arc<dyn Fn(String) + Send + Sync>;
pub type Exiter = Arc<dyn Fn(i32) + Send + Sync>;

#[derive(Default)]
struct Ffmpeg {
    info: FfmpegInfo,
    encoder: Option<EncoderSpec>,
    hw_listed: Vec<String>,
}

#[derive(Default)]
struct State {
    ffmpeg: Ffmpeg,
    monitors: Vec<MonitorInfo>,
    monitor_error: Option<String>,
    audio: AudioEnv,
    /// 全域快捷鍵登記結果（系統匣啟動後才知道；被其他程式占用時為 false）
    hotkeys: Option<HotkeyStatus>,
    /// GitHub 上較新的版本（檢查後才有）
    update: Option<UpdateInfo>,
    update_checked_at: Option<u64>,
    /// 最近一次檢查失敗的原因（介面顯示用）
    update_error: Option<String>,
    /// 操作介面網址（伺服器啟動後設定）
    url: String,
}

pub struct App {
    pub settings: SettingsStore,
    pub recorder: Recorder,
    pub exporter: Exporter,
    pub downloader: Downloader,
    pub thumbs: Thumbnails,
    pub cache: Arc<MediaCache>,
    pub default_output_dir: String,
    st: Mutex<State>,
    /// 硬體編碼器測試完成（false = 測試中）
    hw_ready: watch::Sender<bool>,
    update_gen: AtomicU64,
    /// 最後一次有頁面來查狀態的時間（判斷視窗是否都關了）
    last_seen: AtomicU64,
    quitting: AtomicBool,
    /// 即時預覽：同時只保留一條
    live: Mutex<Option<Arc<Mutex<Option<tokio::process::Child>>>>>,
    notifier: Mutex<Option<Notifier>>,
    ui_opener: Mutex<Option<UiOpener>>,
    exiter: Mutex<Option<Exiter>>,
    /// 結束前要收拾的系統匣
    tray_dispose: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// 錄影器需要的環境（以 Weak 參照 App，避免循環參照）
struct RecDeps(Weak<App>);

impl RecorderDeps for RecDeps {
    fn ffmpeg_path(&self) -> Option<PathBuf> {
        self.0.upgrade()?.ffmpeg_path()
    }
    fn encoders(&self) -> BoxFut<'_, Encoders> {
        let app = self.0.upgrade();
        Box::pin(async move {
            match app {
                Some(a) => a.encoders().await,
                None => Encoders::default(),
            }
        })
    }
    fn prefer_gpu(&self) -> bool {
        self.0.upgrade().is_some_and(|a| a.settings.load().prefer_gpu == Some(true))
    }
    fn learn_gpu(&self) {
        if let Some(a) = self.0.upgrade() {
            a.settings.save(SettingsPatch { prefer_gpu: Some(true), ..Default::default() });
            a.lock().ffmpeg.info.prefer_gpu = Some(true);
        }
    }
    fn monitors(&self) -> Vec<MonitorInfo> {
        self.0.upgrade().map(|a| a.lock().monitors.clone()).unwrap_or_default()
    }
    fn ddagrab_usable(&self) -> BoxFut<'_, bool> {
        let app = self.0.upgrade();
        Box::pin(async move {
            match app {
                Some(a) => a.ddagrab_ready().await,
                None => false,
            }
        })
    }
    // 只縮小擋到擷取範圍的視窗（例如在螢幕 2 操作、錄螢幕 1 時不縮小）；
    // 縮小動畫約 0.25 秒，等它結束再開始擷取，第一張畫面才不會拍到縮到一半的視窗
    fn before_capture(&self, area: Rect) -> BoxFut<'_, ()> {
        Box::pin(async move {
            let minimized = tokio::task::spawn_blocking(move || crate::winui::minimize_ui(Some(&area))).await.unwrap_or(false);
            if minimized {
                tokio::time::sleep(Duration::from_millis(350)).await;
            }
        })
    }
    fn ui_in_area(&self, area: &Rect) -> bool {
        crate::winui::ui_in_area(area)
    }
    fn after_stop(&self) {
        crate::winui::restore_ui();
    }
    fn notify(&self, title: &str, text: &str, warn: bool) {
        if let Some(a) = self.0.upgrade() {
            a.notify(title, text, warn);
        }
    }
}

impl App {
    pub fn new() -> Arc<App> {
        Self::with_data_dir(data_dir())
    }

    /// 設定與縮圖放在 dir（測試用暫存資料夾）
    pub fn with_data_dir(dir: PathBuf) -> Arc<App> {
        Arc::new_cyclic(|weak: &Weak<App>| App {
            settings: SettingsStore::new(dir.join("settings.json")),
            recorder: Recorder::new(Arc::new(RecDeps(weak.clone()))),
            exporter: Exporter::new(),
            downloader: Downloader::new(DownloadDeps::default()),
            thumbs: Thumbnails::new(dir.join("thumbs")),
            cache: Arc::default(),
            default_output_dir: default_output_dir().display().to_string(),
            st: Mutex::default(),
            hw_ready: watch::channel(true).0,
            update_gen: AtomicU64::new(0),
            last_seen: AtomicU64::new(now_ms()),
            quitting: AtomicBool::new(false),
            live: Mutex::default(),
            notifier: Mutex::default(),
            ui_opener: Mutex::default(),
            exiter: Mutex::default(),
            tray_dispose: Mutex::default(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ───────────── 與外部（系統匣、視窗）的連接 ─────────────

    pub fn set_notifier(&self, n: Notifier) {
        *self.notifier.lock().unwrap() = Some(n);
    }

    pub fn notify(&self, title: &str, text: &str, warn: bool) {
        let n = self.notifier.lock().unwrap().clone();
        if let Some(n) = n {
            n(title, text, warn);
        }
    }

    pub fn set_ui_opener(&self, o: UiOpener) {
        *self.ui_opener.lock().unwrap() = Some(o);
    }

    /// 開啟操作視窗（沒有設定時改用瀏覽器）
    pub fn open_ui(&self, url: String) {
        let o = self.ui_opener.lock().unwrap().clone();
        match o {
            Some(o) => o(url),
            None => crate::desktop::open_in_browser(&url),
        }
    }

    pub fn set_exiter(&self, e: Exiter) {
        *self.exiter.lock().unwrap() = Some(e);
    }

    pub fn set_tray_dispose(&self, f: Box<dyn FnOnce() + Send>) {
        *self.tray_dispose.lock().unwrap() = Some(f);
    }

    pub fn set_url(&self, url: String) {
        self.lock().url = url;
    }

    pub fn url(&self) -> String {
        self.lock().url.clone()
    }

    pub fn set_hotkeys(&self, h: HotkeyStatus) {
        self.lock().hotkeys = Some(h);
    }

    pub fn touch(&self) {
        self.last_seen.store(now_ms(), Ordering::Relaxed);
    }

    pub fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    pub fn update(&self) -> Option<UpdateInfo> {
        self.lock().update.clone()
    }

    pub fn update_checked_at(&self) -> Option<u64> {
        self.lock().update_checked_at
    }

    pub fn update_error(&self) -> Option<String> {
        self.lock().update_error.clone()
    }

    // ───────────── FFmpeg ─────────────

    pub fn ffmpeg_path(&self) -> Option<PathBuf> {
        let st = self.lock();
        if st.ffmpeg.info.found {
            st.ffmpeg.info.path.clone().map(PathBuf::from)
        } else {
            None
        }
    }

    pub fn export_ctx(&self) -> ExportCtx {
        ExportCtx { ffmpeg: self.ffmpeg_path(), encoder: self.lock().ffmpeg.encoder, cache: self.cache.clone() }
    }

    /// 軟體編碼器與實測可用的硬體編碼器（硬體測試還沒做完就先等它）
    async fn encoders(&self) -> Encoders {
        let mut rx = self.hw_ready.subscribe();
        let _ = rx.wait_for(|v| *v).await;
        let st = self.lock();
        Encoders { cpu: st.ffmpeg.encoder, gpu: st.ffmpeg.info.hw_encoders.clone().unwrap_or_default().iter().filter_map(|n| encoder_spec(n)).collect() }
    }

    /// 清除「自動改用 GPU」的紀錄（例如換了更快的電腦，或當時只是暫時忙碌）
    pub fn reset_learned_gpu(&self) {
        self.settings.save(SettingsPatch { prefer_gpu: Some(false), ..Default::default() });
        self.lock().ffmpeg.info.prefer_gpu = Some(false);
    }

    fn ddagrab_usable(&self) -> bool {
        let st = self.lock();
        st.ffmpeg.info.has_ddagrab && st.ffmpeg.info.ddagrab_works != Some(false)
    }

    /// 錄影前確認 ddagrab：啟動時的實測可能剛好遇到鎖定畫面或 UAC 安全桌面而失敗
    /// （例如開機自動啟動時），上次失敗就重測一次，不要整段期間都只能用 gdigrab。
    async fn ddagrab_ready(&self) -> bool {
        let (has, works) = {
            let st = self.lock();
            (st.ffmpeg.info.has_ddagrab, st.ffmpeg.info.ddagrab_works)
        };
        if !has {
            return false;
        }
        if works == Some(false) {
            self.test_ddagrab().await;
        }
        self.lock().ffmpeg.info.ddagrab_works != Some(false)
    }

    pub fn env(&self) -> EnvInfo {
        let st = self.lock();
        EnvInfo {
            app_dir: crate::paths::app_dir().display().to_string(),
            app_version: APP_VERSION.to_string(),
            default_output_dir: self.default_output_dir.clone(),
            ffmpeg: st.ffmpeg.info.clone(),
            monitors: st.monitors.clone(),
            monitor_error: st.monitor_error.clone(),
            desktop: desktop_rect(&st.monitors),
            audio: st.audio.clone(),
            hotkeys: st.hotkeys,
            update: st.update.clone(),
        }
    }

    pub async fn refresh_devices(&self) {
        let (monitors, audio) = tokio::task::spawn_blocking(|| (crate::monitors::enumerate_monitors(), crate::audio::list_audio_devices())).await.unwrap_or_else(|e| (Err(e.to_string()), Err(e.to_string())));
        let mut st = self.lock();
        match monitors {
            Ok(m) => {
                st.monitor_error = m.is_empty().then(|| "找不到任何螢幕".to_string());
                st.monitors = m;
            }
            Err(e) => {
                st.monitors = Vec::new();
                st.monitor_error = Some(format!("無法列舉螢幕：{e}"));
            }
        }
        st.audio = match audio {
            Ok(a) => AudioEnv { render: a.render, captures: a.captures, error: None },
            Err(e) => AudioEnv { render: None, captures: Vec::new(), error: Some(format!("無法列舉音訊裝置：{e}")) },
        };
    }

    /// 重新偵測 FFmpeg、螢幕與音訊裝置；ddagrab 與硬體編碼器實測在背景進行。
    pub async fn refresh(self: &Arc<Self>, wait_ddagrab: bool) -> EnvInfo {
        self.refresh_devices().await;
        let mut probe = probe_ffmpeg().await;
        let prefer_gpu = self.settings.load().prefer_gpu == Some(true);
        let (need_hw, need_dda) = {
            let mut st = self.lock();
            let old = &st.ffmpeg.info;
            // 保留上一次成功的 ddagrab 實測結果（同一個執行檔），避免每次重新整理都要等；失敗則重測
            if probe.info.path == old.path && old.ddagrab_works == Some(true) {
                probe.info.ddagrab_works = old.ddagrab_works;
                probe.info.ddagrab_error = old.ddagrab_error.clone();
            }
            // 硬體編碼器實測結果同樣保留（換了 ffmpeg.exe 才重測）
            if probe.info.path == old.path && old.hw_encoders.is_some() {
                probe.info.hw_encoders = old.hw_encoders.clone();
            }
            probe.info.prefer_gpu = Some(prefer_gpu);
            let info = &probe.info;
            let need_hw = info.found && info.hw_encoders.is_none();
            let need_dda = info.found && info.has_ddagrab && info.ddagrab_works.is_none();
            st.ffmpeg = Ffmpeg { info: probe.info, encoder: probe.encoder, hw_listed: probe.hw_listed };
            (need_hw, need_dda)
        };
        if need_hw {
            self.hw_ready.send_replace(false);
            let app = self.clone();
            tokio::spawn(async move {
                let (path, listed) = {
                    let st = app.lock();
                    (st.ffmpeg.info.path.clone().unwrap_or_default(), st.ffmpeg.hw_listed.clone())
                };
                // 測試出錯時視為「沒有可用的 GPU」，不能讓之後每次錄影都因此失敗
                let list = test_hw_encoders(&PathBuf::from(&path), &listed).await;
                {
                    let mut st = app.lock();
                    if st.ffmpeg.info.path.as_deref() == Some(path.as_str()) {
                        if list.is_empty() {
                            info!("沒有可用的 GPU 編碼器，將使用 CPU 編碼");
                        } else {
                            info!("可用的 GPU 編碼器：{}", list.join("、"));
                        }
                        st.ffmpeg.info.hw_encoders = Some(list);
                    }
                }
                app.hw_ready.send_replace(true);
            });
        }
        if need_dda {
            let app = self.clone();
            let test = tokio::spawn(async move { app.test_ddagrab().await });
            if wait_ddagrab {
                let _ = test.await;
            }
        }
        self.env()
    }

    async fn test_ddagrab(&self) {
        let (path, primary) = {
            let st = self.lock();
            let primary = st.monitors.iter().find(|m| m.primary).or(st.monitors.first()).cloned();
            (st.ffmpeg.info.path.clone().unwrap_or_default(), primary)
        };
        let r = test_ddagrab(&PathBuf::from(&path), primary.as_ref()).await;
        let mut st = self.lock();
        if st.ffmpeg.info.path.as_deref() != Some(path.as_str()) {
            return;
        }
        match r {
            Ok(()) => {
                info!("ddagrab 測試成功，將優先使用 GPU 擷取");
                st.ffmpeg.info.ddagrab_works = Some(true);
                st.ffmpeg.info.ddagrab_error = None;
            }
            Err(e) => {
                info!("ddagrab 測試失敗，將改用 gdigrab：{e}");
                st.ffmpeg.info.ddagrab_works = Some(false);
                st.ffmpeg.info.ddagrab_error = Some(e);
            }
        }
    }

    // ───────────── 檢查新版本 ─────────────

    pub fn check_updates_enabled(&self) -> bool {
        self.settings.load().check_updates != Some(false)
    }

    /// 啟動 1 分鐘後檢查一次，之後每 12 小時；可在介面上關閉
    pub fn schedule_update_checks(self: &Arc<Self>) {
        let gen = self.update_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if !self.check_updates_enabled() {
            return;
        }
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                let next = if app.update_checked_at().is_some() { Duration::from_secs(12 * 3600) } else { Duration::from_secs(60) };
                tokio::time::sleep(next).await;
                if app.update_gen.load(Ordering::SeqCst) != gen {
                    return;
                }
                if let Err(e) = app.check_update().await {
                    info!("檢查新版本失敗：{e}");
                    if e == UpdateError::NotPublic {
                        return; // 私人儲存庫：再查也是 404，不再排下一次
                    }
                }
            }
        });
    }

    pub fn set_check_updates(self: &Arc<Self>, on: bool) {
        self.settings.save(SettingsPatch { check_updates: Some(on), ..Default::default() });
        if !on {
            self.lock().update = None;
        }
        self.schedule_update_checks();
    }

    /// 立即檢查；找到新版本時以系統匣通知（同一版只通知一次）
    pub async fn check_update(&self) -> Result<Option<UpdateInfo>, UpdateError> {
        let r = tokio::task::spawn_blocking(|| check_for_update(APP_VERSION)).await.unwrap_or_else(|e| Err(UpdateError::Other(e.to_string())));
        {
            let mut st = self.lock();
            st.update_checked_at = Some(now_ms());
            match &r {
                Ok(u) => {
                    st.update = u.clone();
                    st.update_error = None;
                }
                Err(e) => st.update_error = Some(e.to_string()),
            }
        }
        let u = r?;
        if let Some(u) = &u {
            if self.settings.load().update_notified.as_deref() != Some(u.version.as_str()) {
                self.settings.save(SettingsPatch { update_notified: Some(u.version.clone()), ..Default::default() });
                info!("有新版本 v{}：{}", u.version, u.url);
                self.notify("有新版本", &format!("v{} 已發佈，可從系統匣選單下載", u.version), false);
            }
        }
        Ok(u)
    }

    // ───────────── 預覽 ─────────────

    /// 預覽範圍：指定螢幕時只有那一台，否則整個桌面
    fn preview_monitors(&self, monitor_id: Option<&str>) -> Vec<MonitorInfo> {
        let st = self.lock();
        match monitor_id.and_then(|id| st.monitors.iter().find(|m| m.id == id)) {
            Some(m) => vec![m.clone()],
            None => st.monitors.clone(),
        }
    }

    /// 預覽 JPEG（整個桌面或單一螢幕）；ddagrab 失敗時退回 gdigrab
    pub async fn preview(&self, monitor_id: Option<&str>) -> Option<Vec<u8>> {
        let ffmpeg = self.ffmpeg_path()?;
        let mons = self.preview_monitors(monitor_id);
        let dda = self.ddagrab_usable() && !self.lock().monitors.is_empty();
        let grab = |dda: bool| {
            let args = preview_args(&mons, dda, 1600, None);
            let ffmpeg = ffmpeg.clone();
            async move {
                let mut cmd = command(&ffmpeg);
                cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
                let mut child = cmd.spawn().ok()?;
                let mut out = child.stdout.take()?;
                let mut img = Vec::new();
                let done = tokio::time::timeout(Duration::from_secs(10), async {
                    let _ = out.read_to_end(&mut img).await;
                    child.wait().await
                })
                .await;
                match done {
                    Ok(Ok(s)) if s.success() && !img.is_empty() => Some(img),
                    _ => None, // 逾時：child 被丟棄時結束行程
                }
            }
        };
        if dda {
            if let Some(img) = grab(true).await {
                return Some(img);
            }
        }
        grab(false).await
    }

    /// 即時預覽串流（multipart JPEG）。同時只保留一條：新的連線會結束舊的；
    /// 回傳的子行程被丟棄（瀏覽器斷線）時 FFmpeg 也跟著結束。
    pub fn live_preview(&self, fps: f64, monitor_id: Option<&str>) -> Option<(tokio::process::ChildStdout, LiveGuard)> {
        let ffmpeg = self.ffmpeg_path()?;
        self.kill_live();
        let dda = self.ddagrab_usable() && !self.lock().monitors.is_empty();
        let args = preview_args(&self.preview_monitors(monitor_id), dda, 1280, Some(fps));
        let mut cmd = command(&ffmpeg);
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().ok()?;
        let out = child.stdout.take()?;
        let slot = Arc::new(Mutex::new(Some(child)));
        *self.live.lock().unwrap() = Some(slot.clone());
        Some((out, LiveGuard(slot)))
    }

    fn kill_live(&self) {
        if let Some(slot) = self.live.lock().unwrap().take() {
            if let Some(mut c) = slot.lock().unwrap().take() {
                let _ = c.start_kill();
            }
        }
    }

    // ───────────── 結束 ─────────────

    /// 正常收尾後結束程式（錄影中會先停止並合併）。
    pub async fn quit(self: &Arc<Self>, code: i32) {
        if self.quitting.swap(true, Ordering::SeqCst) {
            return;
        }
        info!("正在結束程式…");
        self.kill_live();
        self.downloader.cancel();
        tokio::join!(self.recorder.shutdown(), self.exporter.shutdown());
        let dispose = self.tray_dispose.lock().unwrap().take();
        if let Some(d) = dispose {
            let _ = tokio::task::spawn_blocking(d).await;
        }
        let exiter = self.exiter.lock().unwrap().clone();
        match exiter {
            Some(e) => e(code),
            None => std::process::exit(code),
        }
    }

    /// 下載 FFmpeg；完成後重新偵測，介面輪詢時就會看到 FFmpeg 已就緒
    pub fn start_download(self: &Arc<Self>) {
        let app = self.clone();
        let rt = tokio::runtime::Handle::current();
        self.downloader.start(move || {
            rt.block_on(app.refresh(false));
            Ok(())
        });
    }

    pub fn log_startup(&self) {
        let env = self.env();
        let ff = &env.ffmpeg;
        if !ff.found {
            warn!("找不到 ffmpeg.exe！請在介面上按「下載 FFmpeg」，或把 ffmpeg.exe 放到 {}", env.app_dir);
        } else {
            info!("FFmpeg {}：{}", ff.version.clone().unwrap_or_default(), ff.path.clone().unwrap_or_default());
            info!("  ddagrab：{}　編碼器：{}", if ff.has_ddagrab { "有（測試中…）" } else { "無，將使用 gdigrab" }, ff.encoder.clone().unwrap_or_else(|| "無".into()));
            if let Some(e) = &ff.error {
                warn!("  {e}");
            }
        }
        for m in &env.monitors {
            info!("  螢幕 {}{}：{}×{} @ ({}, {})，{}", m.display_number, if m.primary { "（主螢幕）" } else { "" }, m.width, m.height, m.x, m.y, m.adapter_name);
        }
        if let Some(e) = &env.monitor_error {
            warn!("  {e}");
        }
    }
}

/// 即時預覽的 FFmpeg：丟棄時結束（瀏覽器斷線、換張數）
pub struct LiveGuard(Arc<Mutex<Option<tokio::process::Child>>>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.lock().unwrap().take() {
            let _ = c.start_kill();
        }
    }
}
