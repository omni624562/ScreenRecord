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
    /// 開啟操作視窗並編輯最近的截圖（App::last_shot）
    EditShot,
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
    /// 步驟截圖進行中：已經截了幾步
    pub steps: Option<u32>,
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
    /// 步驟截圖進行中：(工作, 文件的資料夾, 文件名稱, 開始的時間)
    #[cfg(windows)]
    steps: Option<(crate::steps_win::Session, PathBuf, String, String)>,
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
    /// 錄影中打的點（每支錄影一份，存在 markers/）
    pub markers: Arc<crate::projects::ProjectStore>,
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
        crate::winui::set_desktop_icons(true);
    }
    fn save_markers(&self, output: &str, marks: &crate::recorder::Marks) {
        if let Some(a) = self.0.upgrade() {
            if let Err(e) = a.markers.save(output, output, serde_json::to_value(marks).unwrap_or_default()) {
                crate::warn!("無法儲存打的點：{e}");
            }
        }
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
            markers: Arc::new(crate::projects::ProjectStore::new(dir.join("markers"))),
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
        let names: Vec<String> =
            (0..crate::types::HOTKEY_COUNT).map(|i| format!("{} {}", crate::types::HOTKEY_NAMES[i], if k.label(i).is_empty() { "停用".to_string() } else { k.label(i) })).collect();
        crate::info!("快捷鍵改為：{}", names.join("、"));
        st
    }

    /// 介面的即時狀態
    pub fn status(&self) -> Status {
        // 先取出、放開鎖再組合：結構裡的暫時鎖會留到整個運算式結束，
        // 而 steps_count 在 Windows 上也要鎖同一把，寫在一起會卡死（主視窗打不開）
        let shot = self.lock().shot.clone();
        Status {
            recorder: self.recorder.status(),
            export: self.exporter.status(),
            download: self.downloader.status(),
            settings_rev: self.settings.load().rev,
            update: self.update().map(|u| u.version),
            install: Some(self.installer.status()).filter(|s| s.phase != crate::selfupdate::InstallPhase::Idle),
            shot,
            steps: self.steps_count(),
        }
    }

    // ───────────── 步驟截圖 ─────────────

    /// 步驟截圖進行中：已經截了幾步
    pub fn steps_count(&self) -> Option<u32> {
        #[cfg(windows)]
        return self.lock().steps.as_ref().map(|s| s.0.count());
        #[cfg(not(windows))]
        None
    }

    /// 開始步驟截圖：之後每點一下滑鼠就截一張（圖放在儲存資料夾的「教學_日期_時間」資料夾）
    pub fn steps_start(&self, output_dir: &str) -> crate::Result<()> {
        #[cfg(not(windows))]
        {
            let _ = output_dir;
            Err(crate::Error::config("步驟截圖只支援 Windows"))
        }
        #[cfg(windows)]
        {
            let mut st = self.lock();
            if st.steps.is_some() {
                return Err(crate::Error::config("步驟截圖已經在進行中"));
            }
            let stamp = crate::paths::timestamp();
            let name = format!("教學_{stamp}");
            let dir = PathBuf::from(output_dir);
            let session = crate::steps_win::Session::start(dir.clone(), name.clone()).map_err(|e| crate::Error::config(format!("無法建立資料夾：{e}")))?;
            st.steps = Some((session, dir, name, chrono::Local::now().format("%Y/%m/%d %H:%M").to_string()));
            crate::info!("[截圖] 開始步驟截圖");
            Ok(())
        }
    }

    /// 完成步驟截圖：做成教學文件（HTML），回傳文件路徑；一步都沒有時刪掉資料夾、回傳 None
    pub async fn steps_finish(&self) -> crate::Result<Option<String>> {
        #[cfg(not(windows))]
        return Err(crate::Error::config("步驟截圖只支援 Windows"));
        #[cfg(windows)]
        {
            let Some((session, dir, name, date)) = self.lock().steps.take() else {
                return Err(crate::Error::config("沒有在步驟截圖"));
            };
            let steps = tokio::task::spawn_blocking(move || session.finish()).await.unwrap_or_default();
            if steps.is_empty() {
                let _ = std::fs::remove_dir(dir.join(&name));
                crate::info!("[截圖] 步驟截圖：沒有任何步驟");
                return Ok(None);
            }
            let path = dir.join(format!("{name}.html"));
            let doc = crate::steps::html(&format!("操作步驟（{date}）"), &date, &steps);
            tokio::fs::write(&path, doc).await.map_err(|e| crate::Error::other(format!("無法儲存教學文件：{e}")))?;
            crate::info!("[截圖] 步驟截圖完成：{} 步，{}", steps.len(), path.display());
            Ok(Some(path.display().to_string()))
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
        self.shot_preview(&shot.path);
        crate::info!("[截圖] {}（{}×{}{}）", shot.path, shot.width, shot.height, if shot.copied { "，已複製到剪貼簿" } else { "" });
        Ok(shot)
    }

    /// 截圖後在右下角顯示小縮圖（設定可關掉；只有 Windows）
    pub fn shot_preview_enabled(&self) -> bool {
        cfg!(windows) && self.settings.load().ui.and_then(|u| u.get("shotPreview").and_then(serde_json::Value::as_bool)).unwrap_or(true)
    }

    /// 顯示截圖後的小縮圖：編輯、複製、釘選、刪除
    fn shot_preview(self: &Arc<Self>, path: &str) {
        if !self.shot_preview_enabled() {
            return;
        }
        #[cfg(windows)]
        {
            use crate::shot_toast_win::Cmd;
            let me = Arc::downgrade(self);
            crate::shot_toast_win::show(path.to_string(), move |cmd, p| {
                let Some(app) = me.upgrade() else { return };
                match cmd {
                    Cmd::Edit => app.open_ui(UiPage::EditShot),
                    Cmd::Copy => {
                        crate::clipboard::copy_png(Path::new(p));
                    }
                    Cmd::Pin => {
                        if let Some(pm) = std::fs::read(p).ok().and_then(|b| tiny_skia::Pixmap::decode_png(&b).ok()) {
                            crate::pin_win::show(crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height());
                        }
                    }
                    Cmd::Delete => match crate::recycle::move_to_recycle_bin(&[p.to_string()]) {
                        Ok(()) => crate::info!("[截圖] 從小縮圖刪除 {p}"),
                        Err(e) => app.notify("刪除截圖", e.message(), true),
                    },
                }
            });
        }
        #[cfg(not(windows))]
        let _ = path;
    }

    async fn take_screenshot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<crate::types::ShotInfo> {
        let monitors = self.lock().monitors.clone();
        // 選了「只錄聲音」時，截圖仍截螢幕
        let plan = crate::args::resolve_plan(&RecordConfig { audio_only: false, ..config.clone() }, &monitors)?;
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

    /// 從影片擷取 t 秒的那一格，存成截圖（原尺寸 PNG，和影片放在同一個資料夾）並複製到剪貼簿
    pub async fn grab_frame(self: &Arc<Self>, video: &str, t: f64) -> crate::Result<crate::types::ShotInfo> {
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| crate::Error::config("找不到 FFmpeg"))?;
        let dir = Path::new(video).parent().map(|p| p.display().to_string()).unwrap_or_default();
        let out = new_shot_path(&dir).await?;
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-ss", &format!("{t:.3}"), "-i", video, "-frames:v", "1", "-update", "1", "-y"]
            .iter()
            .map(|s| s.to_string())
            .chain(std::iter::once(out.display().to_string()))
            .collect();
        let r = crate::process::run(&ffmpeg, &args, Duration::from_secs(30)).await;
        if r.code != 0 || !tokio::fs::metadata(&out).await.map(|m| m.len() > 0).unwrap_or(false) {
            let _ = tokio::fs::remove_file(&out).await;
            crate::info!("[截圖] 擷取影片畫面失敗：{}", r.stderr.trim());
            return Err(crate::Error::other("無法擷取這一格，詳見記錄檔"));
        }
        let shot = self.finish_shot(&out, 0, 0).await;
        self.lock().shot = Some(shot.clone());
        crate::info!("[截圖] 從 {video} 的 {t:.2} 秒擷取 {}（{}×{}）", shot.path, shot.width, shot.height);
        Ok(shot)
    }

    /// 「只錄這個視窗」：視窗移動後（停下來 0.4 秒）錄影範圍跟著移過去；大小改變時維持原本的大小
    pub fn spawn_window_follower(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut seen: Option<(i64, Rect)> = None;
            let mut warned: Option<i64> = None;
            loop {
                tokio::time::sleep(Duration::from_millis(400)).await;
                let Some(app) = me.upgrade() else { return };
                let Some((id, area)) = app.recorder.follow_info() else {
                    seen = None;
                    continue;
                };
                let Some(b) = crate::winui::window_bounds(id) else { continue };
                if (b.x, b.y) == (area.x, area.y) {
                    seen = None;
                    continue;
                }
                // 拖曳中先等它停下來
                if seen != Some((id, b)) {
                    seen = Some((id, b));
                    continue;
                }
                seen = None;
                if ((b.width - area.width).abs() > 4 || (b.height - area.height).abs() > 4) && warned != Some(id) {
                    warned = Some(id);
                    crate::info!("[錄影] 視窗大小改變（{}×{}），錄影維持原本的大小 {}×{}", b.width, b.height, area.width, area.height);
                }
                if let Err(e) = app.recorder.move_region(b.x, b.y).await {
                    crate::info!("[錄影] 無法跟著視窗移動：{}", e.message());
                }
            }
        });
    }

    /// 多張截圖拼成一張（依檔案時間由舊到新；左右或上下排），存成新的截圖並複製到剪貼簿
    pub async fn combine_shots(self: &Arc<Self>, mut paths: Vec<String>, vertical: bool) -> crate::Result<crate::types::ShotInfo> {
        if paths.len() < 2 {
            return Err(crate::Error::config("請選兩張以上的截圖"));
        }
        if paths.len() > 20 {
            return Err(crate::Error::config("一次最多拼 20 張"));
        }
        let mtime = |p: &String| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        paths.sort_by_key(mtime);
        let mut images = vec![];
        for p in &paths {
            let (rgba, w, h) = crate::actions::load_image(p).await?;
            let mut pm = tiny_skia::Pixmap::new(w, h).ok_or_else(|| crate::Error::config("圖片太大"))?;
            for (d, s) in pm.pixels_mut().iter_mut().zip(rgba.as_chunks::<4>().0) {
                *d = tiny_skia::ColorU8::from_rgba(s[0], s[1], s[2], s[3]).premultiply();
            }
            images.push(pm);
        }
        let dir = Path::new(&paths[0]).parent().map(|p| p.display().to_string()).unwrap_or_default();
        let out = new_shot_path(&dir).await?;
        let o = out.clone();
        let (w, h) = tokio::task::spawn_blocking(move || {
            let gap = images.iter().map(|i| i.width().min(i.height())).min().unwrap_or(0) / 40;
            let pm = crate::shot_edit::combine(&images, vertical, gap.clamp(8, 24)).ok_or_else(|| crate::Error::config("拼起來的圖太大"))?;
            pm.save_png(&o).map_err(|e| crate::Error::other(format!("無法儲存圖片：{e}")))?;
            Ok::<_, crate::Error>((pm.width(), pm.height()))
        })
        .await
        .map_err(|e| crate::Error::other(e.to_string()))??;
        let shot = self.finish_shot(&out, w, h).await;
        self.lock().shot = Some(shot.clone());
        crate::info!("[截圖] {} 張{}拼成 {}（{w}×{h}）", paths.len(), if vertical { "上下" } else { "左右" }, shot.path);
        Ok(shot)
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

    /// 在凍結的畫面上框選（Windows 原生視窗）；取消時 None
    #[cfg(windows)]
    async fn snip_pick(&self, src: &SnipSource, record: crate::snip_win::Mode) -> Option<crate::types::Rect> {
        self.lock().snipping = true;
        let (path, desk, windows) = (src.path.clone(), src.desktop, src.windows.clone());
        tokio::task::spawn_blocking(move || {
            let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).ok()?).ok()?;
            // 以實際截到的大小為準
            let desk = crate::types::Rect { width: pm.width() as i32, height: pm.height() as i32, ..desk };
            crate::snip_win::select(pm.data(), desk, windows, record)
        })
        .await
        .ok()
        .flatten()
    }

    #[cfg(windows)]
    async fn snip_native(self: &Arc<Self>, src: SnipSource) -> crate::Result<Option<crate::types::ShotInfo>> {
        let sel = self.snip_pick(&src, crate::snip_win::Mode::Shot).await;
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

    /// 取色器（tool = false）或尺規（tool = true）：凍結畫面後顯示放大鏡。
    /// 取色器點一下把色碼（#1D4ED8）複製到剪貼簿並回傳；尺規只是量，回傳 None。只有 Windows
    pub async fn screen_tool(self: &Arc<Self>, config: &RecordConfig, ruler: bool) -> crate::Result<Option<String>> {
        #[cfg(not(windows))]
        {
            let _ = (config, ruler);
            Err(crate::Error::config("取色器與尺規只支援 Windows"))
        }
        #[cfg(windows)]
        {
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
            self.lock().snipping = true;
            let (path, desk) = (src.path.clone(), src.desktop);
            let color = tokio::task::spawn_blocking(move || {
                let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).ok()?).ok()?;
                let desk = crate::types::Rect { width: pm.width() as i32, height: pm.height() as i32, ..desk };
                if ruler {
                    crate::snip_win::ruler(pm.data(), desk);
                    None
                } else {
                    crate::snip_win::pick_color(pm.data(), desk)
                }
            })
            .await
            .ok()
            .flatten();
            self.snip_end(Some(&src));
            let hex = color.map(crate::snip_tools::hex);
            if let Some(h) = &hex {
                crate::clipboard::copy_text(h);
                crate::info!("[取色器] {h}（已複製到剪貼簿）");
            }
            Ok(hex)
        }
    }

    /// 讀取畫面上的 QR 碼：框選範圍（或點一下選視窗）後解出內容；取消時 None。只有 Windows
    pub async fn qr_snip(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<Option<Vec<String>>> {
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(crate::Error::config("讀取畫面上的 QR 碼只支援 Windows；可以在檢視器開啟截圖後按「讀取 QR 碼」"))
        }
        #[cfg(windows)]
        {
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
            let sel = self.snip_pick(&src, crate::snip_win::Mode::Qr).await;
            let path = src.path.clone();
            let desk = src.desktop;
            let res = match sel {
                None => Ok(None),
                Some(r) => tokio::task::spawn_blocking(move || -> crate::Result<Vec<String>> {
                    let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).map_err(|e| crate::Error::other(e.to_string()))?).map_err(|e| crate::Error::other(e.to_string()))?;
                    let (x, y) = ((r.x - desk.x).max(0) as u32, (r.y - desk.y).max(0) as u32);
                    let (w, h) = ((r.width as u32).min(pm.width().saturating_sub(x)), (r.height as u32).min(pm.height().saturating_sub(y)));
                    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                    let data = crate::shot_edit::straight_rgba(&pm);
                    for row in y..y + h {
                        let i = ((row * pm.width() + x) * 4) as usize;
                        rgba.extend_from_slice(&data[i..i + (w * 4) as usize]);
                    }
                    Ok(crate::qr::decode(&rgba, w, h))
                })
                .await
                .map_err(|e| crate::Error::other(e.to_string()))?
                .map(Some),
            };
            self.snip_end(Some(&src));
            res
        }
    }

    /// 長截圖：框選範圍（或點一下選視窗）後，自動一邊往下捲一邊截取，接成一張長圖，
    /// 存成新的截圖並複製到剪貼簿。捲到底、畫面對不起來、太長或按 Esc 時停止。只有 Windows
    pub async fn long_shot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<Option<crate::types::ShotInfo>> {
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(crate::Error::config("長截圖只支援 Windows"))
        }
        #[cfg(windows)]
        {
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
            let Some(area) = self.snip_pick(&src, crate::snip_win::Mode::Scroll).await else {
                crate::info!("[截圖] 取消長截圖");
                self.snip_end(Some(&src));
                return Ok(None);
            };
            // 框選畫面關掉後等一下，讓下面的視窗重畫
            tokio::time::sleep(Duration::from_millis(250)).await;
            let res = tokio::task::spawn_blocking(move || scroll_capture(area)).await.map_err(|e| crate::Error::other(e.to_string()))?;
            self.snip_end(Some(&src));
            let (rgba, w, h, steps) = res?;
            let out = new_shot_path(&src.output_dir).await?;
            let o = out.clone();
            tokio::task::spawn_blocking(move || {
                let mut pm = tiny_skia::Pixmap::new(w, h).ok_or_else(|| crate::Error::other("圖片太大"))?;
                pm.data_mut().copy_from_slice(&rgba);
                pm.save_png(&o).map_err(|e| crate::Error::other(format!("無法儲存圖片：{e}")))
            })
            .await
            .map_err(|e| crate::Error::other(e.to_string()))??;
            let shot = self.finish_shot(&out, w, h).await;
            self.lock().shot = Some(shot.clone());
            self.shot_preview(&shot.path);
            crate::info!("[截圖] 長截圖 {}（{w}×{h}，捲動 {steps} 次）", shot.path);
            Ok(Some(shot))
        }
    }

    /// 框選要錄影的範圍：與框選截圖相同的畫面（拖曳框選或點一下選視窗），回傳範圍（取消時 None），
    /// 也記成「上次框選」。只有 Windows 有原生的框選畫面
    pub async fn select_record_region(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<Option<crate::types::Rect>> {
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
        let sel = self.snip_pick(&src, crate::snip_win::Mode::Record).await;
        #[cfg(not(windows))]
        let sel = None;
        match sel {
            Some(r) => {
                self.lock().last_snip = Some(r);
                crate::info!("[錄影] 框選範圍 ({}, {}) {}×{}", r.x, r.y, r.width, r.height);
            }
            None => crate::info!("[錄影] 取消框選"),
        }
        self.snip_end(Some(&src));
        Ok(sel)
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
        let all = RecordConfig { source: crate::types::SourceConfig::All, audio_only: false, ..config.clone() };
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

    /// 錄影中拖曳移動了範圍：主畫面的自訂範圍（或單一螢幕裡框選的部分）、系統匣「開始錄影」用的設定、
    /// 「重複上次框選」原本是 old 的，都改成 new，下次錄影直接用新位置（操作視窗看到設定變了會重新讀取）
    pub fn region_moved(&self, old: Rect, new: Rect) {
        let monitors = {
            let mut st = self.lock();
            if st.last_snip == Some(old) {
                st.last_snip = Some(new);
            }
            st.monitors.clone()
        };
        let saved = self.settings.load();
        let mut ui = saved.ui.clone().filter(|v| v.is_object());
        let ui_changed = ui.as_mut().is_some_and(|u| move_ui_region(u, old, new, &monitors));
        let mut cfg = saved.record_config();
        let cfg_changed = match cfg.as_mut().map(|c| &mut c.source) {
            Some(crate::types::SourceConfig::Region { x, y, width, height }) if (*x, *y, *width, *height) == (old.x as f64, old.y as f64, old.width as f64, old.height as f64) => {
                (*x, *y) = (new.x as f64, new.y as f64);
                true
            }
            _ => false,
        };
        if ui_changed || cfg_changed {
            self.settings.save(SettingsPatch { ui: ui.filter(|_| ui_changed), config: cfg.filter(|_| cfg_changed).and_then(|c| serde_json::to_value(c).ok()), ..Default::default() });
        }
    }

    /// 最近的截圖
    pub fn last_shot(&self) -> Option<crate::types::ShotInfo> {
        self.lock().shot.clone()
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
        self.shot_preview(&shot.path);
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
/// 一邊往下捲一邊截取範圍、接成長圖；回傳 (RGBA, 寬, 高, 捲了幾次)
#[cfg(windows)]
fn scroll_capture(area: crate::types::Rect) -> crate::Result<(Vec<u8>, u32, u32, usize)> {
    use crate::longshot::{Step, Stitcher};
    let first = crate::scroll_win::grab(area).ok_or_else(|| crate::Error::other("無法截取畫面"))?;
    let (w, h) = (area.width as u32, area.height as u32);
    let mut st = Stitcher::new(first, w, h);
    // 範圍小時一次捲少一點，前後兩張才有足夠的重疊
    let notches = if area.height < 360 {
        1
    } else if area.height < 700 {
        2
    } else {
        3
    };
    let _ = crate::scroll_win::esc_pressed();
    let (mut steps, mut same) = (0, 0);
    while steps < 120 {
        crate::scroll_win::scroll_down(area, notches);
        std::thread::sleep(Duration::from_millis(350));
        if crate::scroll_win::esc_pressed() {
            crate::info!("[截圖] 長截圖：按了 Esc，停止");
            break;
        }
        let Some(img) = crate::scroll_win::grab(area) else { break };
        steps += 1;
        match st.add(img) {
            Step::Added(_) => same = 0,
            // 有些網頁捲動有動畫：再等一下看看
            Step::Same => {
                same += 1;
                if same >= 2 {
                    break;
                }
            }
            Step::Lost => {
                crate::info!("[截圖] 長截圖：畫面對不起來，停止");
                break;
            }
            Step::Full => {
                crate::info!("[截圖] 長截圖：已達最高 {} 像素，停止", crate::longshot::MAX_HEIGHT);
                break;
            }
        }
    }
    let (rgba, w, h) = st.finish();
    Ok((rgba, w, h, steps))
}

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

/// 操作視窗的設定（settings.json 的 ui 欄位）裡，原本是 old 的範圍改成 new。
/// 單一螢幕裡框選的部分：new 完全在某個螢幕內就改選那個螢幕，否則改成「自訂範圍」
fn move_ui_region(ui: &mut serde_json::Value, old: Rect, new: Rect, monitors: &[MonitorInfo]) -> bool {
    let rect_of = |v: &serde_json::Value| serde_json::from_value::<Rect>(v.clone()).ok();
    let Ok(new_v) = serde_json::to_value(new) else { return false };
    match ui["sourceType"].as_str() {
        Some("region") if rect_of(&ui["region"]) == Some(old) => {
            ui["region"] = new_v;
            true
        }
        Some("monitor") if rect_of(&ui["monitorRegion"]) == Some(old) => {
            let inside = |m: &&MonitorInfo| new.x >= m.x && new.y >= m.y && new.x + new.width <= m.x + m.width && new.y + new.height <= m.y + m.height;
            match monitors.iter().find(inside) {
                Some(m) => {
                    ui["monitorId"] = serde_json::Value::String(m.id.clone());
                    ui["monitorRegion"] = new_v;
                }
                None => {
                    ui["sourceType"] = serde_json::Value::String("region".into());
                    ui["region"] = new_v;
                }
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn moved_region_updates_ui_settings() {
        let mon = |id: &str, x: i32| MonitorInfo { id: id.into(), x, y: 0, width: 1920, height: 1080, ..Default::default() };
        let mons = [mon("0:0", 0), mon("0:1", 1920)];
        let r = |x, y| Rect { x, y, width: 640, height: 480 };
        // 自訂範圍
        let mut ui = serde_json::json!({ "sourceType": "region", "region": r(10, 20) });
        assert!(move_ui_region(&mut ui, r(10, 20), r(300, 200), &mons));
        assert_eq!(ui["region"], serde_json::to_value(r(300, 200)).unwrap());
        // 不是同一個範圍（例如從系統匣框選錄的）：不改
        assert!(!move_ui_region(&mut ui, r(1, 1), r(2, 2), &mons));
        // 單一螢幕裡的部分：移到另一個螢幕內就改選那個螢幕
        let mut ui = serde_json::json!({ "sourceType": "monitor", "monitorId": "0:0", "monitorRegion": r(10, 20), "region": r(0, 0) });
        assert!(move_ui_region(&mut ui, r(10, 20), r(2000, 100), &mons));
        assert_eq!(ui["monitorId"], "0:1");
        assert_eq!(ui["monitorRegion"], serde_json::to_value(r(2000, 100)).unwrap());
        // 跨兩個螢幕：改成自訂範圍
        assert!(move_ui_region(&mut ui, r(2000, 100), r(1700, 100), &mons));
        assert_eq!(ui["sourceType"], "region");
        assert_eq!(ui["region"], serde_json::to_value(r(1700, 100)).unwrap());
    }

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

    /// 介面一開始就讀狀態：任何一個欄位都不能在持有鎖時再鎖一次（Windows 的步驟截圖曾經這樣卡死）
    #[test]
    fn status_does_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_data_dir(dir.path().to_path_buf());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let st = app.status();
            let _ = tx.send(st.steps);
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).expect("status() 卡住了"), None);
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
