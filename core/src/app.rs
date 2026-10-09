//! 全域狀態：FFmpeg 偵測結果、螢幕與音訊裝置清單、錄影器、轉檔、下載、檢查新版本、預覽。

use crate::args::{desktop_rect, encoder_spec, preview_args, preview_size, EncoderSpec};
use crate::downloader::{Deps as DownloadDeps, Downloader};
use crate::exporter::{ExportCtx, Exporter};
use crate::ffmpeg::{probe_ffmpeg, test_ddagrab, test_hw_encoders};
use crate::library::MediaCache;
use crate::paths::{data_dir, default_output_dir, now_ms};
use crate::process::command;
use crate::recorder::{BoxFut, Encoders, Recorder, RecorderDeps};
use crate::settings::{SettingsPatch, SettingsStore};
use crate::thumbs::Thumbnails;
use crate::types::{AudioEnv, EnvInfo, FfmpegInfo, HotkeyStatus, MonitorInfo, RecordConfig, Rect, UpdateInfo};
use crate::updater::{check_for_update, UpdateError};
use crate::version::APP_VERSION;
use crate::{info, warn};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

pub type Notifier = Arc<dyn Fn(&str, &str, bool) + Send + Sync>;
pub type UiOpener = Arc<dyn Fn(UiPage) + Send + Sync>;
pub type HotkeyApplier = Arc<dyn Fn(&crate::types::Hotkeys) -> Option<HotkeyStatus> + Send + Sync>;

/// 開啟操作視窗時要顯示的畫面
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPage {
    Main,
    /// 更新說明（系統匣選單「更新說明」）
    Changelog,
    /// 在螢幕上框選截圖（不開啟操作視窗；凍結的畫面用 App::take_snip 取得）
    Snip,
}

/// 框選截圖：先截下整個桌面（凍結），使用者在畫面上框選後再從中裁切
#[derive(Debug, Clone)]
pub struct SnipSource {
    /// 整個桌面的 PNG（暫存檔）
    pub path: PathBuf,
    pub desktop: crate::types::Rect,
    pub monitors: Vec<MonitorInfo>,
    /// 截下時看得到的視窗（由上層到下層）：點一下截整個視窗
    pub windows: Vec<crate::types::Rect>,
    /// 存檔的資料夾
    pub output_dir: String,
}

/// 預覽畫面（RGBA，由上而下）
#[derive(Debug, Clone)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 介面需要的即時狀態（錄影、轉檔、下載 FFmpeg、程式內更新）
#[derive(Debug, Clone)]
pub struct Status {
    pub recorder: crate::types::RecorderStatus,
    pub export: Option<crate::types::ExportStatus>,
    pub download: crate::types::DownloadStatus,
    pub settings_rev: u64,
    /// 有新版本時的版本號
    pub update: Option<String>,
    /// 程式內更新的進度（沒有在更新時為 None）
    pub install: Option<crate::selfupdate::InstallStatus>,
    /// 最近一次的截圖
    pub shot: Option<crate::types::ShotInfo>,
}

/// 預覽畫面的最大寬度
pub const PREVIEW_MAX_WIDTH: u32 = 1600;
pub const LIVE_MAX_WIDTH: u32 = 1280;
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
    /// 最近一次的截圖
    shot: Option<crate::types::ShotInfo>,
    /// 截圖中（避免連按快捷鍵同時截好幾張）
    shooting: bool,
    /// 上次框選的範圍（「重複上次框選」）
    last_snip: Option<crate::types::Rect>,
    /// 等介面顯示框選畫面的凍結畫面
    snip: Option<SnipSource>,
    /// 框選畫面開著（還沒截好或取消）
    snipping: bool,
}

pub struct App {
    pub settings: SettingsStore,
    pub recorder: Recorder,
    pub exporter: Exporter,
    pub downloader: Downloader,
    /// 程式內更新
    pub installer: crate::selfupdate::Installer,
    pub thumbs: Thumbnails,
    pub cache: Arc<MediaCache>,
    /// 剪輯版的剪輯設定與標註（之後可以再修改）
    pub projects: Arc<crate::projects::ProjectStore>,
    pub default_output_dir: String,
    st: Mutex<State>,
    /// 硬體編碼器測試完成（false = 測試中）
    hw_ready: watch::Sender<bool>,
    /// 第一次偵測（FFmpeg、螢幕、音訊裝置）完成；啟動時先開視窗，偵測在背景進行
    ready: watch::Sender<bool>,
    update_gen: AtomicU64,
    quitting: AtomicBool,
    /// 即時預覽：同時只保留一條
    live: Mutex<Option<Arc<Mutex<Option<tokio::process::Child>>>>>,
    notifier: Mutex<Option<Notifier>>,
    ui_opener: Mutex<Option<UiOpener>>,
    exiter: Mutex<Option<Exiter>>,
    /// 結束前要收拾的系統匣
    tray_dispose: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// 重新登記全域快捷鍵（系統匣執行緒）
    hotkey_applier: Mutex<Option<HotkeyApplier>>,
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
            installer: crate::selfupdate::Installer::default(),
            thumbs: Thumbnails::new(dir.join("thumbs")),
            projects: Arc::new(crate::projects::ProjectStore::new(dir.join("edits"))),
            cache: Arc::default(),
            default_output_dir: default_output_dir().display().to_string(),
            st: Mutex::default(),
            hw_ready: watch::channel(true).0,
            ready: watch::channel(false).0,
            update_gen: AtomicU64::new(0),
            quitting: AtomicBool::new(false),
            live: Mutex::default(),
            notifier: Mutex::default(),
            ui_opener: Mutex::default(),
            exiter: Mutex::default(),
            tray_dispose: Mutex::default(),
            hotkey_applier: Mutex::default(),
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

    /// 開啟操作視窗（已開著時帶到前面）
    pub fn open_ui(&self, page: UiPage) {
        let o = self.ui_opener.lock().unwrap().clone();
        match o {
            Some(o) => o(page),
            None => warn!("操作視窗還沒準備好"),
        }
    }

    pub fn set_exiter(&self, e: Exiter) {
        *self.exiter.lock().unwrap() = Some(e);
    }

    pub fn set_tray_dispose(&self, f: Box<dyn FnOnce() + Send>) {
        *self.tray_dispose.lock().unwrap() = Some(f);
    }

    pub fn set_hotkeys(&self, h: HotkeyStatus) {
        self.lock().hotkeys = Some(h);
    }

    /// 目前設定的全域快捷鍵
    pub fn hotkeys(&self) -> crate::types::Hotkeys {
        self.settings.load().hotkeys.unwrap_or_default()
    }

    /// 全域快捷鍵目前的登記結果（沒有系統匣時為 None）
    pub fn hotkey_status(&self) -> Option<HotkeyStatus> {
        self.lock().hotkeys
    }

    pub fn set_hotkey_applier(&self, f: HotkeyApplier) {
        *self.hotkey_applier.lock().unwrap() = Some(f);
    }

    /// 重新登記全域快捷鍵（不存檔；設定時先暫停全部，才能按到原本的組合）。沒有系統匣時回傳 None
    pub fn apply_hotkeys(&self, k: &crate::types::Hotkeys) -> Option<HotkeyStatus> {
        let f = self.hotkey_applier.lock().unwrap().clone()?;
        let st = f(k)?;
        self.set_hotkeys(st);
        Some(st)
    }

    /// 儲存並套用自訂的全域快捷鍵
    pub fn save_hotkeys(&self, k: crate::types::Hotkeys) -> Option<HotkeyStatus> {
        self.settings.save(crate::settings::SettingsPatch { hotkeys: Some(k), ..Default::default() });
        let st = self.apply_hotkeys(&k);
        let names: Vec<String> = (0..4).map(|i| format!("{} {}", crate::types::HOTKEY_NAMES[i], if k.label(i).is_empty() { "停用".to_string() } else { k.label(i) })).collect();
        crate::info!("快捷鍵改為：{}", names.join("、"));
        st
    }

    /// 介面的即時狀態
    pub fn status(&self) -> Status {
        Status {
            recorder: self.recorder.status(),
            export: self.exporter.status(),
            download: self.downloader.status(),
            settings_rev: self.settings.load().rev,
            update: self.update().map(|u| u.version),
            install: Some(self.installer.status()).filter(|s| s.phase != crate::selfupdate::InstallPhase::Idle),
            shot: self.lock().shot.clone(),
        }
    }

    /// 截圖：與錄影相同的擷取範圍，存成原尺寸 PNG（儲存資料夾）並複製到剪貼簿。
    /// 操作視窗擋到範圍時先縮小、截完還原。
    pub async fn screenshot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<crate::types::ShotInfo> {
        {
            let mut st = self.lock();
            if st.shooting {
                return Err(crate::Error::config("正在截圖"));
            }
            st.shooting = true;
        }
        let r = self.take_screenshot(config).await;
        self.lock().shooting = false;
        let shot = r?;
        self.lock().shot = Some(shot.clone());
        crate::info!("[截圖] {}（{}×{}{}）", shot.path, shot.width, shot.height, if shot.copied { "，已複製到剪貼簿" } else { "" });
        Ok(shot)
    }

    async fn take_screenshot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<crate::types::ShotInfo> {
        let monitors = self.lock().monitors.clone();
        let plan = crate::args::resolve_plan(config, &monitors)?;
        let out = new_shot_path(&config.output_dir).await?;
        // 操作視窗擋到範圍：先縮小（等動畫結束）再截
        let hid = crate::winui::minimize_ui(Some(&plan.rect));
        if hid {
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        let r = self.capture_png(&plan, config.draw_mouse, &out).await;
        if hid {
            crate::winui::restore_ui();
        }
        r?;
        Ok(self.finish_shot(&out, plan.rect.width as u32, plan.rect.height as u32).await)
    }

    /// 擷取 plan 的範圍存成 PNG（ddagrab 失敗時改用 gdigrab）
    async fn capture_png(&self, plan: &crate::args::CapturePlan, draw_mouse: bool, out: &Path) -> crate::Result<()> {
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| crate::Error::config("找不到 FFmpeg，無法截圖"))?;
        let out_s = out.display().to_string();
        let use_dda = plan.dda.is_some() && self.ddagrab_ready().await;
        for dda in [true, false] {
            if dda && !use_dda {
                continue;
            }
            let Ok(args) = crate::args::screenshot_args(plan, dda, draw_mouse, &out_s) else { continue };
            let r = crate::process::run(&ffmpeg, &args, Duration::from_secs(10)).await;
            if r.code == 0 && tokio::fs::metadata(out).await.map(|m| m.len() > 0).unwrap_or(false) {
                return Ok(());
            }
            crate::info!("[截圖] {} 失敗：{}", if dda { "ddagrab" } else { "gdigrab" }, r.stderr.trim());
        }
        let _ = tokio::fs::remove_file(out).await;
        Err(crate::Error::other("截圖失敗，詳見記錄檔"))
    }

    /// 截好的 PNG：複製到剪貼簿（失敗不影響已存好的檔案），編上序號
    async fn finish_shot(&self, out: &Path, w: u32, h: u32) -> crate::types::ShotInfo {
        let png = out.to_path_buf();
        let (width, height, copied) = tokio::task::spawn_blocking(move || crate::clipboard::copy_png(&png)).await.unwrap_or((0, 0, false));
        let seq = self.lock().shot.as_ref().map(|s| s.seq + 1).unwrap_or(1);
        let (width, height) = if width > 0 { (width, height) } else { (w, h) };
        crate::types::ShotInfo { seq, path: out.display().to_string(), width, height, copied }
    }

    // ───────────── 框選截圖 ─────────────

    /// 框選截圖：截下整個桌面（不含游標）當作凍結的畫面，讓使用者框選後裁切存檔。
    /// Windows 上用原生的全螢幕視窗框選（不靠操作視窗）；其他平台交給介面（開發測試用）。
    /// 操作視窗先縮小，框選結束才還原。回傳截好的圖（取消時為 None）
    pub async fn snip_begin(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<Option<crate::types::ShotInfo>> {
        {
            let mut st = self.lock();
            if st.shooting || st.snipping {
                return Err(crate::Error::config("正在截圖"));
            }
            st.shooting = true;
        }
        let r = self.snip_capture(config).await;
        self.lock().shooting = false;
        let src = r?;
        #[cfg(windows)]
        return self.snip_native(src).await;
        #[cfg(not(windows))]
        {
            self.snip_offer(src);
            Ok(None)
        }
    }

    #[cfg(windows)]
    async fn snip_native(self: &Arc<Self>, src: SnipSource) -> crate::Result<Option<crate::types::ShotInfo>> {
        self.lock().snipping = true;
        let (path, desk, windows) = (src.path.clone(), src.desktop, src.windows.clone());
        let sel = tokio::task::spawn_blocking(move || {
            let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).ok()?).ok()?;
            // 以實際截到的大小為準
            let desk = crate::types::Rect { width: pm.width() as i32, height: pm.height() as i32, ..desk };
            crate::snip_win::select(pm.data(), desk, windows)
        })
        .await
        .ok()
        .flatten();
        let res = match sel {
            Some(r) => self.snip_save(&src, r).await.map(Some),
            None => {
                crate::info!("[截圖] 取消框選");
                Ok(None)
            }
        };
        self.snip_end(Some(&src));
        res
    }

    /// 交給介面顯示框選畫面
    pub fn snip_offer(&self, src: SnipSource) {
        {
            let mut st = self.lock();
            st.snip = Some(src);
            st.snipping = true;
        }
        self.open_ui(UiPage::Snip);
    }

    async fn snip_capture(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<SnipSource> {
        let monitors = self.lock().monitors.clone();
        let all = RecordConfig { source: crate::types::SourceConfig::All, ..config.clone() };
        let plan = crate::args::resolve_plan(&all, &monitors)?;
        let path = std::env::temp_dir().join(format!("ScreenRecorder-snip-{}.png", std::process::id()));
        if crate::winui::minimize_ui(Some(&plan.rect)) {
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        if let Err(e) = self.capture_png(&plan, false, &path).await {
            crate::winui::restore_ui();
            return Err(e);
        }
        let windows = crate::winui::visible_windows();
        Ok(SnipSource { path, desktop: plan.rect, monitors: plan.monitors.clone(), windows, output_dir: config.output_dir.clone() })
    }

    /// 上次框選的範圍
    pub fn last_snip(&self) -> Option<crate::types::Rect> {
        self.lock().last_snip
    }

    /// 介面取走凍結的畫面（只取一次）
    pub fn take_snip(&self) -> Option<SnipSource> {
        self.lock().snip.take()
    }

    /// 框選好了：從凍結的畫面裁切 rect（桌面座標），存成 PNG 並複製到剪貼簿
    pub async fn snip_save(self: &Arc<Self>, src: &SnipSource, rect: crate::types::Rect) -> crate::Result<crate::types::ShotInfo> {
        let out = new_shot_path(&src.output_dir).await?;
        let (from, to, desk) = (src.path.clone(), out.clone(), src.desktop);
        let r = tokio::task::spawn_blocking(move || crop_png(&from, desk, rect, &to)).await.map_err(|e| crate::Error::other(e.to_string()))?;
        let (w, h) = r?;
        let shot = self.finish_shot(&out, w, h).await;
        {
            let mut st = self.lock();
            st.shot = Some(shot.clone());
            st.last_snip = Some(rect);
        }
        crate::info!("[截圖] 框選 {}（{}×{}{}）", shot.path, shot.width, shot.height, if shot.copied { "，已複製到剪貼簿" } else { "" });
        Ok(shot)
    }

    /// 框選結束（截好或取消）：刪掉暫存的畫面、還原操作視窗
    pub fn snip_end(&self, src: Option<&SnipSource>) {
        if let Some(s) = src {
            let _ = std::fs::remove_file(&s.path);
        }
        self.lock().snipping = false;
        crate::winui::restore_ui();
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

    /// 標記第一次偵測已完成（refresh 結束時自動呼叫）
    pub fn mark_ready(&self) {
        self.ready.send_replace(true);
    }

    /// 等第一次偵測完成（介面要讀環境資訊、系統匣要開始錄影時）；偵測卡住時最多等 60 秒
    pub async fn wait_ready(&self) {
        let mut rx = self.ready.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(60), rx.wait_for(|v| *v)).await;
    }

    pub async fn refresh_devices(&self) {
        let (monitors, audio) =
            tokio::task::spawn_blocking(|| (crate::monitors::enumerate_monitors(), crate::audio::list_audio_devices())).await.unwrap_or_else(|e| (Err(e.to_string()), Err(e.to_string())));
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
        self.mark_ready();
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
                self.notify("有新版本", &format!("v{} 已發佈，開啟操作視窗即可更新", u.version), false);
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

    /// 預覽畫面的大小（整個桌面或單一螢幕；沒有螢幕資訊時為 None）
    pub fn preview_dims(&self, monitor_id: Option<&str>, max_width: u32) -> Option<(u32, u32)> {
        let mons = self.preview_monitors(monitor_id);
        let rect = desktop_rect(&mons);
        (rect.width > 0 && rect.height > 0).then(|| preview_size(&rect, max_width))
    }

    /// 預覽一張畫面（整個桌面或單一螢幕）；ddagrab 失敗時退回 gdigrab
    pub async fn preview(&self, monitor_id: Option<&str>) -> Option<PreviewImage> {
        let ffmpeg = self.ffmpeg_path()?;
        let mons = self.preview_monitors(monitor_id);
        let (width, height) = self.preview_dims(monitor_id, PREVIEW_MAX_WIDTH)?;
        let dda = self.ddagrab_usable() && !self.lock().monitors.is_empty();
        let want = (width * height * 4) as usize;
        let grab = |dda: bool| {
            let args = preview_args(&mons, dda, PREVIEW_MAX_WIDTH, None);
            let ffmpeg = ffmpeg.clone();
            async move {
                let mut cmd = command(&ffmpeg);
                cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
                let mut child = cmd.spawn().ok()?;
                let mut out = child.stdout.take()?;
                let mut img = Vec::with_capacity(want);
                let done = tokio::time::timeout(Duration::from_secs(10), async {
                    let _ = out.read_to_end(&mut img).await;
                    child.wait().await
                })
                .await;
                match done {
                    Ok(Ok(s)) if s.success() && img.len() >= want => {
                        img.truncate(want);
                        Some(PreviewImage { width, height, rgba: img })
                    }
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

    /// 即時預覽：FFmpeg 持續輸出固定大小的 RGBA 畫面（回傳寬高）。同時只保留一條：新的會結束舊的；
    /// 回傳的 LiveGuard 被丟棄時 FFmpeg 也跟著結束。
    pub fn live_preview(&self, fps: f64, monitor_id: Option<&str>) -> Option<(tokio::process::ChildStdout, LiveGuard, (u32, u32))> {
        let ffmpeg = self.ffmpeg_path()?;
        let dims = self.preview_dims(monitor_id, LIVE_MAX_WIDTH)?;
        self.kill_live();
        let dda = self.ddagrab_usable() && !self.lock().monitors.is_empty();
        let args = preview_args(&self.preview_monitors(monitor_id), dda, LIVE_MAX_WIDTH, Some(fps));
        let mut cmd = command(&ffmpeg);
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().ok()?;
        let out = child.stdout.take()?;
        let slot = Arc::new(Mutex::new(Some(child)));
        *self.live.lock().unwrap() = Some(slot.clone());
        Some((out, LiveGuard(slot), dims))
    }

    /// 停止即時預覽（視窗隱藏時）
    pub fn stop_live(&self) {
        self.kill_live();
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

    /// 程式內更新：下載新版、替換 exe，完成後啟動新版並正常結束自己
    pub fn start_self_update(self: &Arc<Self>) -> crate::Result<()> {
        let Some(info) = self.update() else { return Err(crate::Error::config("目前沒有新版本")) };
        if self.recorder.active() {
            return Err(crate::Error::config("錄影中無法更新，請先停止錄影"));
        }
        if self.exporter.running() {
            return Err(crate::Error::config("正在製作加速版 / GIF，請等完成再更新"));
        }
        if self.downloader.busy() {
            return Err(crate::Error::config("正在下載 FFmpeg，請等完成再更新"));
        }
        if cfg!(debug_assertions) {
            return Err(crate::Error::config("開發版無法自動更新"));
        }
        let exe = std::env::current_exe()?;
        if !exe.parent().is_some_and(crate::selfupdate::dir_writable) {
            return Err(crate::Error::config("程式所在的資料夾沒有寫入權限，請到下載頁面手動更新"));
        }
        let app = self.clone();
        let rt = tokio::runtime::Handle::current();
        self.installer
            .start(&info, exe, move |exe| {
                rt.spawn(async move { app.restart_into(exe).await });
            })
            .map(|_| ())
            .map_err(crate::Error::config)
    }

    /// 啟動新版（它會等這個程式結束才開始），然後正常結束
    async fn restart_into(self: &Arc<Self>, exe: PathBuf) {
        let args = vec![format!("--wait-pid={}", std::process::id())];
        if let Err(e) = crate::job::spawn_detached(&exe.to_string_lossy(), &args) {
            crate::error!("無法啟動新版：{e}");
            self.installer.fail(format!("已下載新版，但無法啟動：{e}。請手動重新開啟程式"));
            return;
        }
        self.quit(0).await;
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

/// 即時預覽的 FFmpeg：丟棄時結束（換範圍、換張數、視窗隱藏）
pub struct LiveGuard(Arc<Mutex<Option<tokio::process::Child>>>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.lock().unwrap().take() {
            let _ = c.start_kill();
        }
    }
}

/// 新截圖的檔名：Shot_日期_時間.png（同一秒有好幾張時加 _2、_3…）
async fn new_shot_path(dir: &str) -> crate::Result<PathBuf> {
    let dir = PathBuf::from(dir);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| crate::Error::config(format!("無法建立儲存資料夾：{e}")))?;
    let stamp = crate::paths::timestamp();
    Ok((1..).map(|i| dir.join(if i == 1 { format!("Shot_{stamp}.png") } else { format!("Shot_{stamp}_{i}.png") })).find(|p| !p.exists()).unwrap_or_else(|| dir.join(format!("Shot_{stamp}.png"))))
}

/// 從整個桌面的 PNG（左上角是 desk 的原點）裁切 rect，存成 PNG；回傳實際的寬高
fn crop_png(from: &Path, desk: crate::types::Rect, rect: crate::types::Rect, to: &Path) -> crate::Result<(u32, u32)> {
    let bytes = std::fs::read(from).map_err(|e| crate::Error::other(format!("讀不到截下的畫面：{e}")))?;
    let src = tiny_skia::Pixmap::decode_png(&bytes).map_err(|e| crate::Error::other(format!("讀不到截下的畫面：{e}")))?;
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let x0 = (rect.x - desk.x).clamp(0, sw);
    let y0 = (rect.y - desk.y).clamp(0, sh);
    let x1 = (rect.x + rect.width - desk.x).clamp(x0, sw);
    let y1 = (rect.y + rect.height - desk.y).clamp(y0, sh);
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut out = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or_else(|| crate::Error::config("範圍太小"))?;
    let row = w as usize * 4;
    for y in 0..h as usize {
        let s0 = ((y0 as usize + y) * sw as usize + x0 as usize) * 4;
        out.data_mut()[y * row..(y + 1) * row].copy_from_slice(&src.data()[s0..s0 + row]);
    }
    out.save_png(to).map_err(|e| crate::Error::other(format!("無法儲存截圖：{e}")))?;
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_ready_returns_after_first_detection() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_data_dir(dir.path().to_path_buf());
        let waiting = tokio::spawn({
            let app = app.clone();
            async move { app.wait_ready().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        app.mark_ready();
        tokio::time::timeout(Duration::from_secs(2), waiting).await.unwrap().unwrap();
        app.wait_ready().await; // 已完成：立即返回
    }

    #[test]
    fn snip_crops_the_frozen_desktop() {
        use crate::types::Rect;
        let dir = tempfile::tempdir().unwrap();
        // 3×2 的桌面，原點在 (-1, 0)（左邊有一台螢幕）；每個像素的紅色 = x*10 + y
        let mut pm = tiny_skia::Pixmap::new(3, 2).unwrap();
        for y in 0..2u8 {
            for x in 0..3u8 {
                let i = (y as usize * 3 + x as usize) * 4;
                pm.data_mut()[i..i + 4].copy_from_slice(&[x * 10 + y, 0, 0, 255]);
            }
        }
        let from = dir.path().join("desk.png");
        pm.save_png(&from).unwrap();
        let to = dir.path().join("out.png");
        let desk = Rect { x: -1, y: 0, width: 3, height: 2 };
        // 超出桌面的部分裁掉
        assert_eq!(crop_png(&from, desk, Rect { x: 0, y: 1, width: 5, height: 5 }, &to).unwrap(), (2, 1));
        let out = tiny_skia::Pixmap::load_png(&to).unwrap();
        assert_eq!((out.data()[0], out.data()[4]), (11, 21));
    }
}
