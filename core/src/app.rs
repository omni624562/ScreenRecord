//! 全域狀態：FFmpeg 偵測結果、螢幕與音訊裝置清單、錄影器、轉檔、下載、檢查新版本、預覽。

use crate::args::{desktop_rect, encoder_spec, preview_args, preview_size, EncoderSpec, GpuConvert, GpuSupport};
use crate::downloader::{Deps as DownloadDeps, Downloader};
use crate::exporter::{ExportCtx, Exporter};
use crate::ffmpeg::{probe_ffmpeg, test_ddagrab, test_gpu_convert, test_hw_encoders};
use crate::library::MediaCache;
use crate::paths::{data_dir, default_output_dir, now_ms};
use crate::process::command;
use crate::recorder::{BoxFut, Encoders, Recorder, RecorderDeps};
use crate::settings::{SettingsPatch, SettingsStore};
use crate::thumbs::Thumbnails;
use crate::types::{AudioEnv, EnvInfo, FfmpegInfo, HotkeyStatus, MonitorInfo, RecordConfig, Rect, UpdateInfo};
use crate::updater::{check_for_update, UpdateError};
use crate::version::APP_VERSION;
use crate::{info, tr, trf, warn};
use std::collections::HashMap;
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
    /// 設定 → 關於（系統匣選單「關於」：版本、更新說明、檢查新版本）
    About,
    /// 在螢幕上框選截圖（不開啟操作視窗；凍結的畫面用 App::take_snip 取得）
    Snip,
    /// 開啟操作視窗並編輯最近的截圖（App::last_shot）
    EditShot,
    /// 開啟操作視窗整理剛截好的步驟（App::steps_draft），再做成教學文件
    StepsReview,
    /// 停止回溯錄影（系統匣、錄影控制列的「停止」）：開啟操作視窗先問要不要存下最近幾分鐘
    ReplayStop,
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
    /// 截圖存檔的資料夾
    pub shot_dir: String,
}

/// 預覽畫面（RGBA，由上而下）
#[derive(Debug, Clone)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 介面需要的即時狀態（錄影、轉檔、下載 FFmpeg、程式內更新）
#[derive(Debug, Clone, PartialEq)]
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
    /// 螢幕配置改變的次數（接上、拔掉外接螢幕、改解析度、改排列）
    pub monitors_rev: u64,
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
    /// 列得出濾鏡的顯示卡處理方式
    gpu_filters: Vec<GpuConvert>,
    /// 顯示卡處理已經測過（或正在測）
    gpu_tested: bool,
    /// 實測通過的顯示卡處理；錄影時失敗會清掉
    gpu: Option<GpuSupport>,
}

#[derive(Default)]
struct State {
    ffmpeg: Ffmpeg,
    monitors: Vec<MonitorInfo>,
    monitor_error: Option<String>,
    /// 螢幕配置改變的次數（介面看到變了就重新讀螢幕清單）
    monitors_rev: u64,
    audio: AudioEnv,
    /// 全域快速鍵登記結果（系統匣啟動後才知道；被其他程式占用時為 false）
    hotkeys: Option<HotkeyStatus>,
    /// GitHub 上較新的版本（檢查後才有）
    update: Option<UpdateInfo>,
    update_checked_at: Option<u64>,
    /// 最近一次檢查失敗的原因（介面顯示用）
    update_error: Option<String>,
    /// 最近一次的截圖
    shot: Option<crate::types::ShotInfo>,
    /// 截圖中（避免連按快速鍵同時截好幾張）
    shooting: bool,
    /// 這次截圖開始時在用的程式（存檔時記進截圖的來歷）
    shot_meta: Option<crate::shot_meta::ShotMeta>,
    /// 上次框選的範圍（「重複上次框選」）
    last_snip: Option<crate::types::Rect>,
    /// 等介面顯示框選畫面的凍結畫面
    snip: Option<SnipSource>,
    /// 框選畫面開著（還沒截好或取消）
    snipping: bool,
    /// 步驟截圖進行中：(工作, 文件的資料夾, 文件名稱, 開始的時間)
    #[cfg(windows)]
    steps: Option<(crate::steps_win::Session, PathBuf, String, String)>,
    /// 截好、等在操作視窗整理的步驟
    steps_draft: Option<crate::steps::Draft>,
}

pub struct App {
    pub settings: SettingsStore,
    pub recorder: Recorder,
    pub exporter: Exporter,
    pub downloader: Downloader,
    /// 語音辨識用的 FFmpeg、語音模型的下載（設定 → 外部元件）
    pub components: crate::components::Components,
    /// 程式內更新
    pub installer: crate::selfupdate::Installer,
    pub thumbs: Thumbnails,
    pub cache: Arc<MediaCache>,
    /// 剪輯版的剪輯設定與標註（之後可以再修改）
    pub projects: Arc<crate::projects::ProjectStore>,
    /// 錄影中加的標記（每支錄影一份，存在 markers/）
    pub markers: Arc<crate::projects::ProjectStore>,
    /// 釘選（我的最愛）的截圖、錄影
    pub pins: Arc<crate::pins::PinStore>,
    /// 截圖的文字索引（搜尋圖裡的字）
    pub ocr: Arc<crate::ocr_index::OcrIndex>,
    /// 截圖的來歷（截圖當下的程式、視窗標題）
    pub shot_meta: Arc<crate::shot_meta::ShotMetaStore>,
    /// 截圖的字整理出來的常見詞（快取）
    pub vocab: Arc<crate::facets::VocabCache>,
    /// 截圖的長相指紋（找相似的截圖）
    pub hashes: Arc<crate::similar::HashStore>,
    pub default_output_dir: String,
    st: Mutex<State>,
    /// 硬體編碼器測試完成（false = 測試中）
    hw_ready: watch::Sender<bool>,
    /// 第一次偵測（FFmpeg、螢幕、音訊裝置）完成；啟動時先開視窗，偵測在背景進行
    ready: watch::Sender<bool>,
    update_gen: AtomicU64,
    quitting: AtomicBool,
    /// 排程錄影（等待中）；改排程或取消時 schedule_gen 加一，等待中的工作就結束
    schedule: Mutex<Option<crate::schedule::Schedule>>,
    schedule_gen: AtomicU64,
    /// 錄影中不讓電腦睡眠
    awake: Mutex<Option<crate::power::KeepAwake>>,
    /// 錄影中每幾秒記一次在用的程式（自動命名用）：(執行檔, 視窗標題) → 次數；None = 沒在記
    rec_apps: Arc<Mutex<Option<AppCounts>>>,
    /// 剛錄完那支最常用的程式（存好時記到 shot_meta）
    last_rec_app: Mutex<Option<(String, String)>>,
    /// 錄完自動處理中（after_record.rs）
    pub(crate) after: Mutex<Option<crate::after_record::AfterStatus>>,
    /// 主畫面看得到時，介面目前的錄影設定（每一格更新；看不到時是 None）：選了攝影機就先顯示攝影機小視窗
    camera_preview: Mutex<Option<RecordConfig>>,
    /// 錄影控制列的樣式與位置：（設定版本, 設定）
    rec_bar: Mutex<(u64, crate::types::RecBarPrefs)>,
    /// 即時預覽：同時只保留一條
    live: Mutex<Option<Arc<Mutex<Option<tokio::process::Child>>>>>,
    notifier: Mutex<Option<Notifier>>,
    ui_opener: Mutex<Option<UiOpener>>,
    exiter: Mutex<Option<Exiter>>,
    /// 結束前要收拾的系統匣
    tray_dispose: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// 重新登記全域快速鍵（系統匣執行緒）
    hotkey_applier: Mutex<Option<HotkeyApplier>>,
}

/// (執行檔, 視窗標題) → 記到幾次
type AppCounts = HashMap<(String, String), u32>;

/// 錄影中最常用的程式（依執行檔加總）與它最常出現的視窗標題
fn most_used_app(counts: &AppCounts) -> Option<(String, String)> {
    let mut by_exe: HashMap<&str, u32> = HashMap::new();
    for ((exe, _), n) in counts {
        *by_exe.entry(exe).or_default() += n;
    }
    let exe = by_exe.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))?.0;
    let title = counts.iter().filter(|((e, _), _)| e == exe).max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0 .1.cmp(&a.0 .1))).map(|((_, t), _)| t.clone()).unwrap_or_default();
    Some((exe.to_string(), title))
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
    fn gpu_convert(&self) -> Option<GpuSupport> {
        self.0.upgrade()?.lock().ffmpeg.gpu
    }
    fn gpu_convert_failed(&self) {
        if let Some(a) = self.0.upgrade() {
            let mut st = a.lock();
            st.ffmpeg.gpu = None;
            st.ffmpeg.info.gpu_convert = None;
        }
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
    fn keep_awake(&self) {
        // 錄影中不讓電腦因為閒置而睡眠、螢幕也不關（暫停後繼續、移動範圍時沿用同一個；停止時 after_stop 放掉）
        if let Some(a) = self.0.upgrade() {
            a.awake.lock().unwrap().get_or_insert_with(|| crate::power::keep_awake(true));
            a.sample_apps();
        }
    }
    fn before_capture(&self, area: Rect) -> BoxFut<'_, ()> {
        Box::pin(async move {
            let minimized = tokio::task::spawn_blocking(move || crate::winui::minimize_ui_for_recording(&area)).await.unwrap_or(false);
            if minimized {
                tokio::time::sleep(Duration::from_millis(350)).await;
            }
        })
    }
    fn ui_in_area(&self, area: &Rect) -> bool {
        crate::winui::ui_in_area(area)
    }
    fn gfxcapture_usable(&self) -> bool {
        self.0.upgrade().is_some_and(|a| a.lock().ffmpeg.info.has_gfxcapture)
    }
    fn saved(&self, path: &str) {
        if let Some(a) = self.0.upgrade() {
            if let Some((app, title)) = a.last_rec_app.lock().unwrap().take() {
                a.shot_meta.record(path, crate::shot_meta::ShotMeta { app, title, src: "rec".into() });
            }
            tokio::spawn(a.after_record(path.to_string()));
        }
    }
    fn after_stop(&self) {
        crate::winui::restore_ui_after_recording();
        crate::winui::set_desktop_icons(true);
        crate::quiet::end();
        if let Some(a) = self.0.upgrade() {
            a.awake.lock().unwrap().take();
            let counts = a.rec_apps.lock().unwrap().take().unwrap_or_default();
            *a.last_rec_app.lock().unwrap() = most_used_app(&counts);
        }
    }
    fn save_markers(&self, output: &str, marks: &crate::recorder::Marks) {
        if let Some(a) = self.0.upgrade() {
            if let Err(e) = a.markers.save(output, output, serde_json::to_value(marks).unwrap_or_default()) {
                crate::warn!("無法儲存標記：{e}");
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
        let app = Self::with_data_dir(data_dir());
        crate::http::configure(app.settings.load().ui.as_ref());
        app
    }

    /// 設定與縮圖放在 dir（測試用暫存資料夾）
    pub fn with_data_dir(dir: PathBuf) -> Arc<App> {
        Arc::new_cyclic(|weak: &Weak<App>| App {
            settings: SettingsStore::new(dir.join("settings.json")),
            recorder: Recorder::new(Arc::new(RecDeps(weak.clone()))),
            exporter: Exporter::new(),
            downloader: Downloader::new(DownloadDeps::default()),
            components: Default::default(),
            installer: crate::selfupdate::Installer::default(),
            thumbs: Thumbnails::new(dir.join("thumbs")),
            projects: Arc::new(crate::projects::ProjectStore::new(dir.join("edits"))),
            markers: Arc::new(crate::projects::ProjectStore::new(dir.join("markers"))),
            pins: Arc::new(crate::pins::PinStore::new(dir.join("pins.json"))),
            ocr: Arc::new(crate::ocr_index::OcrIndex::new(dir.join("ocr_index.json"))),
            shot_meta: Arc::new(crate::shot_meta::ShotMetaStore::new(dir.join("shot_meta.json"))),
            vocab: Arc::new(crate::facets::VocabCache::default()),
            hashes: Arc::new(crate::similar::HashStore::new(dir.join("shot_hashes.json"))),
            cache: Arc::default(),
            default_output_dir: default_output_dir().display().to_string(),
            st: Mutex::default(),
            hw_ready: watch::channel(true).0,
            ready: watch::channel(false).0,
            update_gen: AtomicU64::new(0),
            quitting: AtomicBool::new(false),
            schedule: Mutex::default(),
            schedule_gen: AtomicU64::new(0),
            awake: Mutex::default(),
            rec_apps: Arc::default(),
            last_rec_app: Mutex::default(),
            after: Mutex::default(),
            camera_preview: Mutex::new(None),
            rec_bar: Mutex::new((0, crate::types::RecBarPrefs::default())),
            live: Mutex::default(),
            notifier: Mutex::default(),
            ui_opener: Mutex::default(),
            exiter: Mutex::default(),
            tray_dispose: Mutex::default(),
            hotkey_applier: Mutex::default(),
        })
    }

    /// 錄影開始：每 4 秒記一次在用的程式（已經在記就不重來；停止時 after_stop 收起來）
    fn sample_apps(&self) {
        {
            let mut g = self.rec_apps.lock().unwrap();
            if g.is_some() {
                return;
            }
            *g = Some(HashMap::new());
        }
        let apps = self.rec_apps.clone();
        std::thread::spawn(move || {
            let mut wait = Duration::from_secs(1);
            loop {
                std::thread::sleep(wait);
                wait = Duration::from_secs(4);
                let info = crate::winui::foreground_app();
                let mut g = apps.lock().unwrap();
                let Some(m) = g.as_mut() else { break };
                if let Some(i) = info.filter(|i| !i.exe.is_empty()) {
                    *m.entry((i.exe, i.title)).or_default() += 1;
                }
            }
        });
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

    /// 目前設定的全域快速鍵
    pub fn hotkeys(&self) -> crate::types::Hotkeys {
        self.settings.load().hotkeys.unwrap_or_default()
    }

    /// 全域快速鍵目前的登記結果（沒有系統匣時為 None）
    pub fn hotkey_status(&self) -> Option<HotkeyStatus> {
        self.lock().hotkeys
    }

    pub fn set_hotkey_applier(&self, f: HotkeyApplier) {
        *self.hotkey_applier.lock().unwrap() = Some(f);
    }

    /// 重新登記全域快速鍵（不存檔；設定時先暫停全部，才能按到原本的組合）。沒有系統匣時回傳 None
    pub fn apply_hotkeys(&self, k: &crate::types::Hotkeys) -> Option<HotkeyStatus> {
        let f = self.hotkey_applier.lock().unwrap().clone()?;
        let st = f(k)?;
        self.set_hotkeys(st);
        Some(st)
    }

    /// 儲存並套用自訂的全域快速鍵
    pub fn save_hotkeys(&self, k: crate::types::Hotkeys) -> Option<HotkeyStatus> {
        self.settings.save(crate::settings::SettingsPatch { hotkeys: Some(k), ..Default::default() });
        let st = self.apply_hotkeys(&k);
        let names: Vec<String> =
            (0..crate::types::HOTKEY_COUNT).map(|i| format!("{} {}", crate::types::hotkey_names()[i], if k.label(i).is_empty() { "停用".to_string() } else { k.label(i) })).collect();
        crate::info!("快速鍵改為：{}", names.join("、"));
        st
    }

    /// 介面的即時狀態
    pub fn status(&self) -> Status {
        // 先取出、放開鎖再組合：結構裡的暫時鎖會留到整個運算式結束，
        // 而 steps_count 在 Windows 上也要鎖同一把，寫在一起會卡住（主視窗打不開）
        let (shot, monitors_rev) = {
            let st = self.lock();
            (st.shot.clone(), st.monitors_rev)
        };
        Status {
            recorder: self.recorder.status(),
            export: self.exporter.status(),
            download: self.downloader.status(),
            settings_rev: self.settings.rev(),
            update: self.update().map(|u| u.version),
            install: Some(self.installer.status()).filter(|s| s.phase != crate::selfupdate::InstallPhase::Idle),
            shot,
            steps: self.steps_count(),
            monitors_rev,
        }
    }

    // ───────────── 步驟截圖 ─────────────

    /// 步驟截圖進行中：已經截了幾步
    pub fn steps_count(&self) -> Option<u32> {
        // 所有平台都上鎖：Linux 的測試也能抓到「同一把鎖鎖兩次」（status() 曾經這樣卡住）
        let st = self.lock();
        #[cfg(windows)]
        return st.steps.as_ref().map(|s| s.0.count());
        #[cfg(not(windows))]
        {
            drop(st);
            None
        }
    }

    /// 開始步驟截圖：之後每點一下滑鼠就截一張（圖先放在儲存資料夾的「教學_日期_時間」資料夾）；
    /// 畫面下方有小工具列可以完成或取消
    pub fn steps_start(self: &Arc<Self>, output_dir: &str) -> crate::Result<()> {
        if self.lock().steps_draft.is_some() {
            self.open_ui(UiPage::StepsReview);
            return Err(crate::Error::config(tr!(
                "上一次截的步驟還沒做成文件：請先在操作視窗做成文件或放棄",
                "The steps from last time haven't been made into a guide yet. Finish or discard them in the main window first"
            )));
        }
        #[cfg(not(windows))]
        {
            let _ = output_dir;
            Err(crate::Error::config(tr!("步驟截圖只支援 Windows", "Step capture is only available on Windows")))
        }
        #[cfg(windows)]
        {
            if self.lock().steps.is_some() {
                return Err(crate::Error::config(tr!("步驟截圖已經在進行中", "Step capture is already in progress")));
            }
            // 建資料夾時不鎖（儲存位置在很慢的網路磁碟時，介面與系統匣不會跟著卡住）
            let stamp = crate::paths::timestamp();
            let name = format!("教學_{stamp}");
            let dir = PathBuf::from(output_dir);
            // 小工具列的按鈕：在小工具列自己的執行緒上，直接停止（等截圖執行緒結束）
            let weak = Arc::downgrade(self);
            let on_bar = Arc::new(move |cmd: crate::steps_win::BarCmd| {
                if let Some(app) = weak.upgrade() {
                    app.steps_stop(cmd == crate::steps_win::BarCmd::Discard);
                }
            });
            let session = crate::steps_win::Session::start(dir.clone(), name.clone(), on_bar).map_err(|e| crate::Error::config(trf!("無法建立資料夾：{e}", "Couldn't create the folder: {e}")))?;
            let mut st = self.lock();
            if st.steps.is_some() {
                // 同時按了兩次：留下先開始的那個
                drop(st);
                session.finish();
                return Err(crate::Error::config(tr!("步驟截圖已經在進行中", "Step capture is already in progress")));
            }
            st.steps = Some((session, dir, name, chrono::Local::now().format("%Y/%m/%d %H:%M").to_string()));
            crate::info!("[截圖] 開始步驟截圖");
            Ok(())
        }
    }

    /// 停止步驟截圖（會等截圖執行緒結束，不要在 async 裡直接呼叫）：discard = 放棄（刪掉截好的圖），
    /// 否則開啟操作視窗整理。回傳 None = 沒有在截，Some(false) = 沒有截到任何步驟（或放棄了）
    #[cfg(windows)]
    fn steps_stop(&self, discard: bool) -> Option<bool> {
        let (session, dir, name, date) = self.lock().steps.take()?;
        let steps = session.finish();
        Some(self.steps_done(crate::steps::Draft { dir, name, date, steps }, discard))
    }

    /// 截好了：沒有步驟或放棄時刪掉資料夾，否則開啟操作視窗整理
    #[cfg(windows)]
    fn steps_done(&self, draft: crate::steps::Draft, discard: bool) -> bool {
        if discard || draft.steps.is_empty() {
            draft.discard();
            crate::info!("[截圖] 步驟截圖：{}", if discard { "放棄" } else { "沒有任何步驟" });
            return false;
        }
        crate::info!("[截圖] 步驟截圖：截了 {} 步，開啟操作視窗整理", draft.steps.len());
        self.steps_review(draft);
        true
    }

    /// 完成步驟截圖（系統匣、操作視窗的「完成」）：開啟操作視窗整理步驟；回傳 false = 沒有截到任何步驟
    pub async fn steps_finish(self: &Arc<Self>) -> crate::Result<bool> {
        #[cfg(not(windows))]
        return Err(crate::Error::config(tr!("步驟截圖只支援 Windows", "Step capture is only available on Windows")));
        #[cfg(windows)]
        {
            if self.lock().steps.is_none() {
                return Err(crate::Error::config(tr!("沒有在步驟截圖", "Step capture isn't running")));
            }
            let app = self.clone();
            Ok(tokio::task::spawn_blocking(move || app.steps_stop(false)).await.ok().flatten().unwrap_or(false))
        }
    }

    /// 開啟操作視窗整理這些步驟（開發測試也用）
    pub fn steps_review(&self, draft: crate::steps::Draft) {
        self.lock().steps_draft = Some(draft);
        self.open_ui(UiPage::StepsReview);
    }

    /// 等著整理的步驟
    pub fn steps_draft(&self) -> Option<crate::steps::Draft> {
        self.lock().steps_draft.clone()
    }

    /// 做成教學文件（一個 HTML 檔，圖片包在裡面）：steps = 整理後的步驟；成功後刪掉暫放的截圖，回傳文件路徑
    pub async fn steps_build(&self, title: String, steps: Vec<crate::steps::Step>) -> crate::Result<String> {
        self.steps_build_as(title, steps, crate::steps::DocFormat::Html).await
    }

    /// 同上，指定格式（HTML、Word、PowerPoint）
    pub async fn steps_build_as(&self, title: String, steps: Vec<crate::steps::Step>, format: crate::steps::DocFormat) -> crate::Result<String> {
        let draft = self.lock().steps_draft.clone().ok_or_else(|| crate::Error::config(tr!("沒有要整理的步驟", "There are no steps to organize")))?;
        let d = draft.clone();
        let n = steps.len();
        let path = tokio::task::spawn_blocking(move || crate::steps::build_as(&d, &title, &steps, format)).await.map_err(|e| crate::Error::other(e.to_string()))?.map_err(crate::Error::other)?;
        let mut st = self.lock();
        if st.steps_draft.as_ref() == Some(&draft) {
            st.steps_draft = None;
        }
        crate::info!("[截圖] 步驟截圖完成：{n} 步，{}", path.display());
        Ok(path.display().to_string())
    }

    /// 做成影片（MP4）：每一步一張畫面（截圖＋說明）至少顯示 secs 秒、淡入淡出；speak = 用 Windows 內建的語音唸出說明。
    /// 存成 儲存資料夾/文件名稱.mp4（已經有同名檔案時加上編號），成功後刪掉暫放的截圖
    pub async fn steps_build_video(&self, title: String, steps: Vec<crate::steps::Step>, secs: f64, speak: bool) -> crate::Result<String> {
        let draft = self.lock().steps_draft.clone().ok_or_else(|| crate::Error::config(tr!("沒有要整理的步驟", "There are no steps to organize")))?;
        if steps.is_empty() {
            return Err(crate::Error::config(tr!("沒有要整理的步驟", "There are no steps to organize")));
        }
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| crate::Error::config(tr!("找不到 FFmpeg，不能做成影片", "FFmpeg wasn't found, so the video can't be made")))?;
        let tmp = std::env::temp_dir().join(format!("ScreenRecorder-steps-{}-{}", std::process::id(), crate::paths::now_ms()));
        let title = if title.trim().is_empty() { draft.title() } else { title.trim().to_string() };
        let (d, list, t) = (draft.clone(), steps.clone(), title.clone());
        let t2 = tmp.clone();
        // 畫面與語音（背景執行緒）
        let prepared = tokio::task::spawn_blocking(move || -> Result<crate::steps::VideoParts, String> {
            std::fs::create_dir_all(&t2).map_err(|e| e.to_string())?;
            let (w, h) = crate::steps::VIDEO_SIZE;
            let mut frames = vec![];
            let mut voices = vec![];
            // 第一張是標題
            let subtitle = if t.contains(&d.date) { String::new() } else { d.date.clone() };
            let card = crate::cards::TitleCard { subtitle, ..crate::cards::TitleCard::new(&t) };
            let png = t2.join("title.png");
            crate::cards::save(&card, w, h, &png)?;
            frames.push((png.display().to_string(), 2.5));
            voices.push(None);
            for (i, s) in list.iter().enumerate() {
                let text = crate::steps::caption(&s.window);
                let png = t2.join(format!("frame_{i:03}.png"));
                crate::steps::video_frame(&d.image_path(s), i + 1, &text, w, h)?.save_png(&png).map_err(|e| e.to_string())?;
                let voice = if speak {
                    let wav = t2.join(format!("voice_{i:03}.wav"));
                    let said = trf!("步驟 {}，{}", "Step {}. {}", i + 1, text);
                    match crate::narration::speak_to_file(&said, "", 1.0, &wav) {
                        Ok(ms) => Some((wav.display().to_string(), ms)),
                        Err(e) => return Err(e),
                    }
                } else {
                    None
                };
                frames.push((png.display().to_string(), crate::steps::step_secs(secs, voice.as_ref().map(|v| v.1))));
                voices.push(voice.map(|v| v.0));
            }
            Ok((frames, voices))
        })
        .await
        .map_err(|e| crate::Error::other(e.to_string()))?;
        let (frames, voices) = match prepared {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(crate::Error::other(e));
            }
        };
        let mut path = draft.dir.join(format!("{}.mp4", draft.name));
        let mut n = 2;
        while path.exists() {
            path = draft.dir.join(format!("{} ({n}).mp4", draft.name));
            n += 1;
        }
        let out = crate::process::command(&ffmpeg).args(crate::steps::video_args(&frames, &voices, &path.display().to_string())).output().await;
        let _ = std::fs::remove_dir_all(&tmp);
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let _ = std::fs::remove_file(&path);
                let err = String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or_default().to_string();
                return Err(crate::Error::other(trf!("無法做成影片：{err}", "Couldn't make the video: {err}")));
            }
            Err(e) => return Err(crate::Error::other(e.to_string())),
        }
        draft.discard();
        let mut st = self.lock();
        if st.steps_draft.as_ref() == Some(&draft) {
            st.steps_draft = None;
        }
        crate::info!("[截圖] 步驟截圖做成影片：{} 步，{}", steps.len(), path.display());
        Ok(path.display().to_string())
    }

    /// 放棄整理中的步驟（刪掉截好的圖）
    pub fn steps_discard(&self) {
        if let Some(d) = self.lock().steps_draft.take() {
            crate::info!("[截圖] 步驟截圖：放棄 {} 步", d.steps.len());
            std::thread::spawn(move || d.discard());
        }
    }

    /// 截圖：與錄影相同的擷取範圍，存成原尺寸 PNG（儲存資料夾）並複製到剪貼簿。
    /// 操作視窗擋到範圍時先縮小、截完還原。
    pub async fn screenshot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<crate::types::ShotInfo> {
        {
            let mut st = self.lock();
            if st.shooting {
                return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
            }
            st.shooting = true;
        }
        self.note_app("");
        let r = self.take_screenshot(config).await;
        self.lock().shooting = false;
        let shot = r?;
        self.lock().shot = Some(shot.clone());
        self.shot_preview(&shot.path, None, crate::shot_toast::caption(shot.copied));
        crate::info!("[截圖] {}（{}×{}{}）", shot.path, shot.width, shot.height, if shot.copied { "，已複製到剪貼簿" } else { "" });
        Ok(shot)
    }

    /// 截圖後在右下角顯示小縮圖（設定可關掉；只有 Windows）
    pub fn shot_preview_enabled(&self) -> bool {
        cfg!(windows) && self.settings.load().ui.and_then(|u| u.get("shotPreview").and_then(serde_json::Value::as_bool)).unwrap_or(true)
    }

    /// 顯示截圖後的小縮圖：編輯、釘在桌面、顯示在資料夾、刪除。thumb：已經做好的縮圖（沒有時讀取 path）；
    /// caption：圖下面的說明（「已存檔並複製到剪貼簿」，長截圖提早停止時說明原因）
    fn shot_preview(self: &Arc<Self>, path: &str, thumb: Option<tiny_skia::Pixmap>, caption: &str) {
        if !self.shot_preview_enabled() {
            return;
        }
        #[cfg(windows)]
        {
            use crate::shot_toast_win::Cmd;
            let me = Arc::downgrade(self);
            crate::shot_toast_win::show(path.to_string(), thumb, caption.to_string(), move |cmd, p| {
                let Some(app) = me.upgrade() else { return false };
                match cmd {
                    Cmd::Edit => app.open_ui(UiPage::EditShot),
                    Cmd::Pin => {
                        if let Some(pm) = std::fs::read(p).ok().and_then(|b| tiny_skia::Pixmap::decode_png(&b).ok()) {
                            crate::pin_win::show(crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height());
                        }
                    }
                    Cmd::Reveal => crate::desktop::open_with_explorer(p, true),
                    Cmd::Delete => {
                        return match crate::recycle::move_to_recycle_bin(&[p.to_string()]) {
                            Ok(()) => {
                                crate::info!("[截圖] 從小縮圖刪除 {p}");
                                true
                            }
                            Err(e) => {
                                app.notify(tr!("刪除截圖", "Delete screenshot"), e.message(), true);
                                false
                            }
                        }
                    }
                }
                true
            });
        }
        #[cfg(not(windows))]
        let _ = (path, thumb, caption);
    }

    async fn take_screenshot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<crate::types::ShotInfo> {
        let monitors = self.lock().monitors.clone();
        // 選了「只錄聲音」時，截圖仍截螢幕
        let plan = crate::args::resolve_plan(&RecordConfig { audio_only: false, ..config.clone() }, &monitors)?;
        let out = self.new_shot_path(&config.shots_dir()).await?;
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
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| crate::Error::config(tr!("找不到 FFmpeg，無法截圖", "FFmpeg not found, can't take a screenshot")))?;
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
        Err(crate::Error::other(tr!("截圖失敗，詳見記錄檔", "Screenshot failed. See the log file for details")))
    }

    /// 從影片擷取 t 秒的那一格，存成截圖（原尺寸 PNG，存在截圖資料夾 shot_dir）並複製到剪貼簿
    pub async fn grab_frame(self: &Arc<Self>, video: &str, t: f64, shot_dir: &str) -> crate::Result<crate::types::ShotInfo> {
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| crate::Error::config(tr!("找不到 FFmpeg", "FFmpeg not found")))?;
        let out = self.new_shot_path(shot_dir).await?;
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
            .iter()
            .map(|s| s.to_string())
            .chain(crate::args::input_at(video, &format!("{t:.3}")))
            .chain(["-frames:v", "1", "-update", "1", "-y"].iter().map(|s| s.to_string()))
            .chain(std::iter::once(out.display().to_string()))
            .collect();
        let r = crate::process::run(&ffmpeg, &args, Duration::from_secs(30)).await;
        if r.code != 0 || !tokio::fs::metadata(&out).await.map(|m| m.len() > 0).unwrap_or(false) {
            let _ = tokio::fs::remove_file(&out).await;
            crate::info!("[截圖] 擷取影片畫面失敗：{}", r.stderr.trim());
            return Err(crate::Error::other(tr!("無法擷取這一格，請換個時間點再試", "Couldn't capture this frame. Try a different moment")));
        }
        let title = Path::new(video).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.lock().shot_meta = Some(crate::shot_meta::ShotMeta { app: String::new(), title, src: "frame".into() });
        let shot = self.finish_shot(&out, 0, 0).await;
        self.lock().shot = Some(shot.clone());
        crate::info!("[截圖] 從 {video} 的 {t:.2} 秒擷取 {}（{}×{}）", shot.path, shot.width, shot.height);
        Ok(shot)
    }

    /// 自動匯入資料夾（設定 → 儲存位置）：每 10 秒看一次，有寫完的新影片就匯入到儲存位置；
    /// 設定了「匯入後接著自動處理」時，再做錄完自動處理（字幕、剪掉沒動靜的片段…）
    pub fn spawn_folder_watcher(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut w = crate::watch::Watcher::load(Some(crate::paths::data_dir().join("watch_seen.json")));
            loop {
                tokio::time::sleep(Duration::from_secs(crate::watch::POLL_SEC)).await;
                let Some(app) = me.upgrade() else { return };
                let ui = app.settings.load().ui;
                let dirs = crate::watch::dirs_from_ui(ui.as_ref());
                if dirs.is_empty() {
                    continue;
                }
                // 錄影、轉檔、錄完自動處理進行中：先不做（錄影優先）
                if app.recorder.active() || app.exporter.running() || app.after_status().is_some() {
                    continue;
                }
                let Some(out) = app.settings.load().record_config().map(|c| c.output_dir).filter(|d| !d.trim().is_empty()) else { continue };
                let out = PathBuf::from(out);
                let found: Vec<(PathBuf, Vec<crate::watch::Seen>)> = tokio::task::spawn_blocking({
                    let (dirs, out) = (dirs.clone(), out.clone());
                    move || dirs.into_iter().filter(|d| !crate::watch::inside(d, &out)).map(|d| (d.clone(), crate::watch::list(&d))).collect()
                })
                .await
                .unwrap_or_default();
                let ready = w.ready(&found, crate::paths::now_ms() as f64);
                if ready.is_empty() {
                    continue;
                }
                let files: Vec<crate::watch::Seen> = found.iter().flat_map(|(_, f)| f.iter()).filter(|f| ready.contains(&f.path)).cloned().collect();
                crate::info!("[自動匯入] 發現 {} 支新影片：{}", ready.len(), ready.join("、"));
                let r = crate::actions::import_videos(&app, &ready, &out).await;
                w.finished(&files, &r.done_src, &r.failed_src);
                if r.done.is_empty() {
                    if !r.failed.is_empty() {
                        app.notify(tr!("自動匯入", "Auto import"), &trf!("{} 支影片無法匯入（詳見記錄檔）", "{} video(s) couldn't be imported (see the log file)", r.failed.len()), true);
                    }
                    continue;
                }
                let names: Vec<String> = r.done.iter().map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
                let after = crate::after_record::AfterRecord::from_ui(ui.as_ref());
                let process = crate::watch::after_from_ui(ui.as_ref()) && after.any();
                app.notify(
                    tr!("自動匯入", "Auto import"),
                    &if process {
                        trf!("已匯入 {}，接著自動處理", "Imported {}; processing it now", names.join(tr!("、", ", ")))
                    } else {
                        trf!("已匯入 {}", "Imported {}", names.join(tr!("、", ", ")))
                    },
                    false,
                );
                if process {
                    app.clone().batch_process(r.done.clone(), after).await;
                }
            }
        });
    }

    /// 「只錄這個視窗」：視窗移動後（停下來 0.4 秒）錄影範圍跟著移過去；大小改變時維持原本的大小
    pub fn spawn_window_follower(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut seen: Option<(i64, Rect)> = None;
            let mut warned: Option<i64> = None;
            let mut active = false;
            loop {
                // 跟著視窗錄影時每 0.4 秒看一次；其他時候 2 秒（閒置時少喚醒）
                tokio::time::sleep(Duration::from_millis(if active { 400 } else { 2000 })).await;
                let Some(app) = me.upgrade() else { return };
                let Some((id, area)) = app.recorder.follow_info() else {
                    seen = None;
                    active = false;
                    continue;
                };
                active = true;
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

    /// 多張截圖拼成一張（依給的順序；左右或上下排；spacing 間距：0 無、1 小、2 大），存成新的截圖並複製到剪貼簿
    pub async fn combine_shots(self: &Arc<Self>, paths: Vec<String>, vertical: bool, spacing: u8, shot_dir: &str) -> crate::Result<crate::types::ShotInfo> {
        if paths.len() < 2 {
            return Err(crate::Error::config(tr!("請選兩張以上的截圖", "Select two or more screenshots")));
        }
        if paths.len() > 20 {
            return Err(crate::Error::config(tr!("一次最多拼 20 張", "You can combine up to 20 screenshots at a time")));
        }
        let mut images = vec![];
        for p in &paths {
            let (rgba, w, h) = crate::actions::load_image(p).await?;
            let mut pm = tiny_skia::Pixmap::new(w, h).ok_or_else(|| crate::Error::config(tr!("圖片太大", "Image is too large")))?;
            for (d, s) in pm.pixels_mut().iter_mut().zip(rgba.as_chunks::<4>().0) {
                *d = tiny_skia::ColorU8::from_rgba(s[0], s[1], s[2], s[3]).premultiply();
            }
            images.push(pm);
        }
        let out = self.new_shot_path(shot_dir).await?;
        let o = out.clone();
        let (w, h) = tokio::task::spawn_blocking(move || {
            let gap = combine_gap(&images, spacing);
            let pm = crate::shot_edit::combine(&images, vertical, gap).ok_or_else(|| crate::Error::config(tr!("拼起來的圖太大", "Combined image is too large")))?;
            pm.save_png(&o).map_err(|e| crate::Error::other(trf!("無法儲存圖片：{e}", "Couldn't save the image: {e}")))?;
            Ok::<_, crate::Error>((pm.width(), pm.height()))
        })
        .await
        .map_err(|e| crate::Error::other(e.to_string()))??;
        self.lock().shot_meta = Some(crate::shot_meta::ShotMeta { src: "combine".into(), ..Default::default() });
        let shot = self.finish_shot(&out, w, h).await;
        self.lock().shot = Some(shot.clone());
        crate::info!("[截圖] {} 張{}拼成 {}（{w}×{h}）", paths.len(), if vertical { "上下" } else { "左右" }, shot.path);
        Ok(shot)
    }

    /// 比對兩張截圖（before 較舊、after 較新）：在 after 上用紅框標出不一樣的地方，存成新的截圖；回傳 (截圖, 幾處不一樣)
    pub async fn compare_shots(self: &Arc<Self>, before: String, after: String, shot_dir: &str) -> crate::Result<(crate::types::ShotInfo, usize)> {
        let (a, aw, ah) = crate::actions::load_image(&before).await?;
        let (b, bw, bh) = crate::actions::load_image(&after).await?;
        let out = self.new_shot_path(shot_dir).await?;
        let o = out.clone();
        let (n, w, h) = tokio::task::spawn_blocking(move || {
            // 框畫在後面那張（原本的大小）；比較時前面那張縮放成一樣大
            let boxes = crate::shot_diff::find(&b, bw, bh, &a, aw, ah);
            let mut pm = tiny_skia::Pixmap::new(bw, bh).ok_or_else(|| crate::Error::config(tr!("圖片太大", "Image is too large")))?;
            for (d, s) in pm.pixels_mut().iter_mut().zip(b.as_chunks::<4>().0) {
                *d = tiny_skia::ColorU8::from_rgba(s[0], s[1], s[2], s[3]).premultiply();
            }
            crate::shot_diff::mark(&mut pm, &boxes);
            pm.save_png(&o).map_err(|e| crate::Error::other(trf!("無法儲存圖片：{e}", "Couldn't save the image: {e}")))?;
            Ok::<_, crate::Error>((boxes.len(), bw, bh))
        })
        .await
        .map_err(|e| crate::Error::other(e.to_string()))??;
        self.lock().shot_meta = Some(crate::shot_meta::ShotMeta { src: "compare".into(), ..Default::default() });
        let shot = self.finish_shot(&out, w, h).await;
        self.lock().shot = Some(shot.clone());
        crate::info!("[截圖] 比對 {} 與 {}：{n} 處不一樣", before, after);
        Ok((shot, n))
    }

    /// 截好的 PNG：複製到剪貼簿（失敗不影響已存好的檔案），編上序號
    async fn finish_shot(&self, out: &Path, w: u32, h: u32) -> crate::types::ShotInfo {
        let png = out.to_path_buf();
        let (width, height, copied) = tokio::task::spawn_blocking(move || crate::clipboard::copy_png(&png)).await.unwrap_or((0, 0, false));
        let (width, height) = if width > 0 { (width, height) } else { (w, h) };
        self.shot_info(out, width, height, copied)
    }

    /// 記下這次截圖當下在用的程式（存檔時寫進截圖的來歷）；src：怎麼截的（見 ShotMeta）
    fn note_app(&self, src: &str) {
        let app = crate::winui::foreground_app();
        self.lock().shot_meta = Some(crate::shot_meta::ShotMeta { app: app.as_ref().map(|a| a.exe.clone()).unwrap_or_default(), title: app.map(|a| a.title).unwrap_or_default(), src: src.into() });
    }

    /// 存好的截圖編上序號，記下來歷
    fn shot_info(&self, out: &Path, width: u32, height: u32, copied: bool) -> crate::types::ShotInfo {
        if let Some(m) = self.lock().shot_meta.take() {
            self.shot_meta.record(&out.display().to_string(), m);
        }
        let seq = self.lock().shot.as_ref().map(|s| s.seq + 1).unwrap_or(1);
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
                return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
            }
            st.shooting = true;
        }
        self.note_app("");
        let src = self.snip_ready(self.snip_capture(config).await)?;
        #[cfg(windows)]
        return self.snip_native(src).await;
        #[cfg(not(windows))]
        {
            self.snip_offer(src);
            Ok(None)
        }
    }

    /// 延遲截圖：倒數 sec 秒後框選（先打開要截的選單或提示）。Windows 上畫面上方顯示「3」秒後框選（不會被截進去），
    /// 按 Esc 或點一下取消。取消時回傳 None
    pub async fn snip_delayed(self: &Arc<Self>, config: &RecordConfig, sec: u64) -> crate::Result<Option<crate::types::ShotInfo>> {
        if sec > 0 && !countdown(sec).await {
            crate::info!("[截圖] 取消延遲截圖");
            return Ok(None);
        }
        self.snip_begin(config).await
    }

    /// 在凍結的畫面上框選（Windows 原生視窗）：(範圍, 工具列選的動作)；取消時 None
    #[cfg(windows)]
    async fn snip_pick(&self, src: &SnipSource, record: crate::snip_win::Mode) -> Option<(crate::types::Rect, crate::snip_tools::SnipAction)> {
        self.lock().snipping = true;
        let (path, desk, windows) = (src.path.clone(), src.desktop, src.windows.clone());
        let instant = self.snip_instant();
        tokio::task::spawn_blocking(move || {
            let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).ok()?).ok()?;
            // 以實際截到的大小為準
            let desk = crate::types::Rect { width: pm.width() as i32, height: pm.height() as i32, ..desk };
            crate::snip_win::select(pm.data(), desk, windows, record, instant)
        })
        .await
        .ok()
        .flatten()
    }

    #[cfg(windows)]
    async fn snip_native(self: &Arc<Self>, src: SnipSource) -> crate::Result<Option<crate::types::ShotInfo>> {
        let sel = self.snip_pick(&src, crate::snip_win::Mode::Shot).await;
        let res = match sel {
            Some((r, action)) => self.snip_finish(&src, r, action).await,
            None => {
                crate::info!("[截圖] 取消框選");
                Ok(None)
            }
        };
        self.snip_end(Some(&src));
        res
    }

    /// 框選截圖「放開滑鼠就直接存檔」（設定的 ui.snipInstant；預設關：放開後可以調整範圍，再從工具列選要做什麼）
    pub fn snip_instant(&self) -> bool {
        self.settings.load().ui.and_then(|u| u.get("snipInstant").and_then(serde_json::Value::as_bool)).unwrap_or(false)
    }

    /// 框選好了，依工具列選的動作：存檔並複製（完成）、存檔後開啟編輯、只複製到剪貼簿、釘在桌面（後兩個不存檔）。
    /// 回傳存好的截圖（沒有存檔時 None）
    pub async fn snip_finish(self: &Arc<Self>, src: &SnipSource, rect: crate::types::Rect, action: crate::snip_tools::SnipAction) -> crate::Result<Option<crate::types::ShotInfo>> {
        use crate::snip_tools::SnipAction;
        match action {
            SnipAction::Save => self.snip_save(src, rect).await.map(Some),
            SnipAction::Edit => {
                // 直接開啟編輯：不用再顯示小縮圖
                self.snip_store(src, rect, false).await?;
                let edit_after = self.settings.load().ui.and_then(|u| u.get("editAfterShot").and_then(serde_json::Value::as_bool)).unwrap_or(false);
                // 設定了「截圖後直接開啟編輯」時介面自己會開
                if !edit_after {
                    self.open_ui(UiPage::EditShot);
                }
                Ok(None)
            }
            SnipAction::Copy | SnipAction::Pin => {
                let (from, desk) = (src.path.clone(), src.desktop);
                let pm = tokio::task::spawn_blocking(move || crop_pixmap(&from, desk, rect)).await.map_err(|e| crate::Error::other(e.to_string()))??;
                self.lock().last_snip = Some(rect);
                if action == SnipAction::Copy {
                    let ok = tokio::task::spawn_blocking(move || crate::clipboard::copy_pixmap(&pm)).await.unwrap_or(false);
                    if !ok {
                        return Err(crate::Error::other(tr!("無法複製到剪貼簿", "Couldn't copy to the clipboard")));
                    }
                    crate::info!("[截圖] 框選 {}×{} 只複製到剪貼簿", rect.width, rect.height);
                } else {
                    #[cfg(windows)]
                    crate::pin_win::show(crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height());
                    #[cfg(not(windows))]
                    drop(pm);
                    if cfg!(not(windows)) {
                        return Err(crate::Error::config(tr!("釘在桌面只支援 Windows", "Pin to desktop is only available on Windows")));
                    }
                    crate::info!("[截圖] 框選 {}×{} 釘在桌面", rect.width, rect.height);
                }
                Ok(None)
            }
        }
    }

    /// 截下整個桌面當作凍結的畫面，給介面的框選視窗用（例如框選錄影範圍）。用完要呼叫 snip_end
    pub async fn snip_source(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<SnipSource> {
        {
            let mut st = self.lock();
            if st.shooting || st.snipping {
                return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
            }
            st.shooting = true;
        }
        self.snip_ready(self.snip_capture(config).await)
    }

    /// 取色器（tool = false）或尺規（tool = true）：凍結畫面後顯示放大鏡。
    /// 取色器點一下把色碼（#1D4ED8）複製到剪貼簿並回傳；尺規只是量，回傳 None。只有 Windows
    pub async fn screen_tool(self: &Arc<Self>, config: &RecordConfig, ruler: bool) -> crate::Result<Option<String>> {
        #[cfg(not(windows))]
        {
            let _ = (config, ruler);
            Err(crate::Error::config(tr!("取色器與尺規只支援 Windows", "Color picker and ruler are only available on Windows")))
        }
        #[cfg(windows)]
        {
            {
                let mut st = self.lock();
                if st.shooting || st.snipping {
                    return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
                }
                st.shooting = true;
            }
            let src = self.snip_ready(self.snip_capture(config).await)?;
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
            Err(crate::Error::config(tr!(
                "讀取畫面上的 QR 碼只支援 Windows；可以在檢視器開啟截圖後按「讀取 QR 碼」",
                "Reading QR codes on screen is only available on Windows; open a screenshot in the viewer and click “Read QR code” instead"
            )))
        }
        #[cfg(windows)]
        {
            self.snip_pixels(config, crate::snip_win::Mode::Qr, |rgba, w, h| Ok(crate::qr::decode(&rgba, w, h))).await
        }
    }

    /// 只抓文字：在螢幕上框選，辨識裡面的字直接複製到剪貼簿（不存圖）；對齊的欄排成 Tab 分隔，可以直接貼到 Excel
    pub async fn text_snip(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<Option<crate::ocr::Layout>> {
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(crate::Error::config(tr!("只抓文字要用 Windows 的文字辨識，只支援 Windows", "Grab text uses Windows text recognition and is only available on Windows")))
        }
        #[cfg(windows)]
        {
            let r = self
                .snip_pixels(config, crate::snip_win::Mode::Text, |rgba, w, h| {
                    let lines = crate::ocr::recognize_words(&rgba, w, h).map_err(crate::Error::other)?;
                    Ok(crate::ocr::layout(&lines))
                })
                .await?;
            if let Some(l) = r.as_ref().filter(|l| !l.text.trim().is_empty()) {
                crate::clipboard::copy_text(&l.text);
            }
            // 框選完的地方馬上提示有沒有抓到（不靠 Windows 通知，通知可能被關掉；操作視窗也可能縮小著）
            if let Some(l) = &r {
                self.flash_near_cursor(grab_text_flash(l), Duration::from_millis(2200));
            }
            Ok(r)
        }
    }

    /// 凍結畫面讓使用者框選一塊（或點一下選視窗），把那塊的像素（RGBA）交給 f 處理；取消時 None
    #[cfg(windows)]
    async fn snip_pixels<T: Send + 'static>(
        self: &Arc<Self>,
        config: &RecordConfig,
        mode: crate::snip_win::Mode,
        f: impl FnOnce(Vec<u8>, u32, u32) -> crate::Result<T> + Send + 'static,
    ) -> crate::Result<Option<T>> {
        {
            let mut st = self.lock();
            if st.shooting || st.snipping {
                return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
            }
            st.shooting = true;
        }
        let src = self.snip_ready(self.snip_capture(config).await)?;
        let sel = self.snip_pick(&src, mode).await.map(|p| p.0);
        let path = src.path.clone();
        let desk = src.desktop;
        let res = match sel {
            None => Ok(None),
            Some(r) => tokio::task::spawn_blocking(move || -> crate::Result<T> {
                let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).map_err(|e| crate::Error::other(e.to_string()))?).map_err(|e| crate::Error::other(e.to_string()))?;
                let (x, y) = ((r.x - desk.x).max(0) as u32, (r.y - desk.y).max(0) as u32);
                let (w, h) = ((r.width as u32).min(pm.width().saturating_sub(x)), (r.height as u32).min(pm.height().saturating_sub(y)));
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                let data = crate::shot_edit::straight_rgba(&pm);
                for row in y..y + h {
                    let i = ((row * pm.width() + x) * 4) as usize;
                    rgba.extend_from_slice(&data[i..i + (w * 4) as usize]);
                }
                f(rgba, w, h)
            })
            .await
            .map_err(|e| crate::Error::other(e.to_string()))
            .and_then(|r| r.map(Some)),
        };
        // 不管成功失敗（包括背景工作出錯）都要結束框選，否則之後的截圖都會被當成「正在截圖」
        self.snip_end(Some(&src));
        res
    }

    /// 長截圖：框選範圍（或點一下選視窗）後，自動一邊往下捲一邊擷取，接成一張長圖，
    /// 存成新的截圖並複製到剪貼簿。捲到底、畫面對不起來、太長或按 Esc 時停止。只有 Windows
    pub async fn long_shot(self: &Arc<Self>, config: &RecordConfig) -> crate::Result<Option<crate::types::ShotInfo>> {
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(crate::Error::config(tr!("長截圖只支援 Windows", "Scrolling screenshot is only available on Windows")))
        }
        #[cfg(windows)]
        {
            {
                let mut st = self.lock();
                if st.shooting || st.snipping {
                    return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
                }
                st.shooting = true;
            }
            self.note_app("long");
            let src = self.snip_ready(self.snip_capture(config).await)?;
            let Some((area, _)) = self.snip_pick(&src, crate::snip_win::Mode::Scroll).await else {
                crate::info!("[截圖] 取消長截圖");
                self.snip_end(Some(&src));
                return Ok(None);
            };
            // 框選畫面關掉後等一下，讓下面的視窗重畫
            tokio::time::sleep(Duration::from_millis(250)).await;
            let res = tokio::task::spawn_blocking(move || scroll_capture(area)).await;
            // 先結束框選再處理錯誤（背景工作出錯時也不會一直卡在「正在截圖」）
            self.snip_end(Some(&src));
            let Some((img, steps, end)) = res.map_err(|e| crate::Error::other(e.to_string()))?? else {
                crate::info!("[截圖] 取消長截圖");
                return Ok(None);
            };
            let out = self.new_shot_path(&src.shot_dir).await?;
            let o = out.clone();
            let (w, h) = (img.w, img.h);
            let (copied, thumb) = tokio::task::spawn_blocking(move || save_long_shot(img, &o)).await.map_err(|e| crate::Error::other(e.to_string()))??;
            let shot = self.shot_info(&out, w, h, copied);
            self.lock().shot = Some(shot.clone());
            // 提早停下來（太長、對不起來）：小縮圖上說明只截到這裡（沒有小縮圖時用通知）
            let early = match end {
                Some(crate::longshot::Step::Full) => Some(tr!("已到長度上限，只截到這裡", "Reached the length limit, captured up to here")),
                Some(crate::longshot::Step::Lost) => Some(tr!("畫面對不起來，只截到這裡", "The content stopped lining up, captured up to here")),
                _ => None,
            };
            if let Some(msg) = early.filter(|_| !self.shot_preview_enabled()) {
                self.notify(tr!("長截圖", "Scrolling screenshot"), msg, false);
            }
            self.shot_preview(&shot.path, thumb, early.unwrap_or(crate::shot_toast::caption(copied)));
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
                return Err(crate::Error::config(tr!("正在截圖", "A screenshot is already in progress")));
            }
            st.shooting = true;
        }
        let src = self.snip_ready(self.snip_capture(config).await)?;
        #[cfg(windows)]
        let sel = self.snip_pick(&src, crate::snip_win::Mode::Record).await.map(|p| p.0);
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
        // 每次用不同的檔名：前一次的收尾不會刪到這一次的凍結畫面
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ScreenRecorder-snip-{}-{n}.png", std::process::id()));
        if crate::winui::minimize_ui(Some(&plan.rect)) {
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        if let Err(e) = self.capture_png(&plan, false, &path).await {
            crate::winui::restore_ui();
            return Err(e);
        }
        let windows = crate::winui::visible_windows();
        Ok(SnipSource { path, desktop: plan.rect, monitors: plan.monitors.clone(), windows, shot_dir: config.shots_dir() })
    }

    /// 麥克風靜音 / 取消靜音後：錄影範圍下方短暫提示（不會被錄進去）
    pub fn mic_mute_changed(&self, muted: bool) {
        let text =
            if muted { tr!("🔇 麥克風已靜音・再按一次取消", "🔇 Microphone muted · press again to unmute") } else { tr!("🎙 麥克風已取消靜音", "🎙 Microphone unmuted") };
        self.flash(text);
    }

    /// 在滑鼠旁邊短暫顯示一句話（例如只抓文字完成時）
    pub fn flash_near_cursor(&self, text: impl Into<String>, dur: Duration) {
        let text: String = text.into();
        #[cfg(windows)]
        {
            let near = crate::winui::cursor_pos().map(|(x, y)| (x - 8, y - 8, x + 8, y + 8));
            std::thread::spawn(move || {
                let b = crate::bubble_win::Bubble::show(crate::bubble::Content::text(text), crate::bubble_win::Options { clickable: false, esc: false, near, bottom: true, on: None });
                std::thread::sleep(dur);
                drop(b);
            });
        }
        #[cfg(not(windows))]
        let _ = (text, dur);
    }

    /// 錄影中：錄影範圍下方短暫顯示一句話（1.5 秒，不會被錄進去）
    pub fn flash(&self, text: impl Into<String>) {
        let text: String = text.into();
        crate::info!("[錄影] {text}");
        #[cfg(windows)]
        {
            let near = self.recorder.frame_info().map(|f| (f.area.x, f.area.y, f.area.x + f.area.width, f.area.y + f.area.height));
            std::thread::spawn(move || {
                let b = crate::bubble_win::Bubble::show(crate::bubble::Content::text(text), crate::bubble_win::Options { clickable: false, esc: false, near, bottom: true, on: None });
                std::thread::sleep(Duration::from_millis(1500));
                drop(b);
            });
        }
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
        self.snip_store(src, rect, true).await
    }

    /// 裁切、存檔並複製到剪貼簿；preview = 顯示右下角的小縮圖
    async fn snip_store(self: &Arc<Self>, src: &SnipSource, rect: crate::types::Rect, preview: bool) -> crate::Result<crate::types::ShotInfo> {
        let out = self.new_shot_path(&src.shot_dir).await?;
        let (from, to, desk) = (src.path.clone(), out.clone(), src.desktop);
        let r = tokio::task::spawn_blocking(move || crop_png(&from, desk, rect, &to)).await.map_err(|e| crate::Error::other(e.to_string()))?;
        let (w, h) = r?;
        let shot = self.finish_shot(&out, w, h).await;
        {
            let mut st = self.lock();
            st.shot = Some(shot.clone());
            st.last_snip = Some(rect);
        }
        if preview {
            self.shot_preview(&shot.path, None, crate::shot_toast::caption(shot.copied));
        }
        crate::info!("[截圖] 框選 {}（{}×{}{}）", shot.path, shot.width, shot.height, if shot.copied { "，已複製到剪貼簿" } else { "" });
        Ok(shot)
    }

    /// 凍結畫面截好了：「截圖中」換成「框選中」，同一次上鎖（中間不會插進另一個框選）
    fn snip_ready(&self, r: crate::Result<SnipSource>) -> crate::Result<SnipSource> {
        let mut st = self.lock();
        st.shooting = false;
        if r.is_ok() {
            st.snipping = true;
        }
        r
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

    /// 主畫面看得到、沒有被剪輯等視窗蓋住時，介面目前的錄影設定（介面每一格呼叫；看不到時 None）：
    /// 選了攝影機就先顯示小視窗，切換螢幕、範圍時小視窗馬上跟著移
    pub fn set_camera_preview(&self, config: Option<RecordConfig>) {
        *self.camera_preview.lock().unwrap() = config;
    }

    /// 攝影機小視窗要顯示在哪裡、長什麼樣子（None = 不顯示）。
    /// 錄影中（含倒數、暫停）：這次錄影有攝影機時，用錄影的範圍；位置、大小、形狀用最新的設定
    /// （錄影中在設定裡改了也跟著變）。錄影前：主畫面看得到時，用介面目前的設定（先擺好位置與大小）
    pub fn camera_bubble_info(&self) -> Option<crate::camera_bubble::BubbleInfo> {
        let preview = self.camera_preview.lock().unwrap().clone();
        let device = |c: &crate::types::CameraConfig| !c.device.trim().is_empty();
        if let Some(rec) = self.recorder.camera_info() {
            let latest = preview.as_ref().and_then(|c| c.camera.clone()).or_else(|| self.settings.load().record_config().and_then(|c| c.camera));
            return Some(crate::camera_bubble::BubbleInfo { camera: latest.filter(device).unwrap_or(rec.camera.clone()), ..rec });
        }
        // 倒數、錄影、暫停中但這次錄影沒有攝影機：不能顯示（會被錄進去）。儲存中照常預覽，攝影機不用關了又開
        if self.recorder.frame_info().is_some() {
            return None;
        }
        let config = preview.filter(|c| !c.audio_only)?;
        let camera = config.camera.clone().filter(device)?;
        let plan = crate::args::resolve_plan(&config, &self.lock().monitors).ok()?;
        Some(crate::camera_bubble::BubbleInfo { area: plan.rect, monitors: plan.monitors, camera })
    }

    /// 錄影控制列的樣式與位置（設定的 recBar、recBarPos）。控制列每 0.1 秒問一次：設定沒變就用上次讀到的
    pub fn rec_bar_prefs(&self) -> crate::types::RecBarPrefs {
        let rev = self.settings.rev();
        let mut cache = self.rec_bar.lock().unwrap();
        if cache.0 != rev {
            let ui = self.settings.load().ui;
            let get = |k: &str| ui.as_ref().and_then(|u| u.get(k)).cloned();
            let mode = get("recBar").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
            let pos = get("recBarPos").and_then(|v| serde_json::from_value::<[i32; 2]>(v).ok()).map(|[x, y]| (x, y));
            *cache = (rev, crate::types::RecBarPrefs { mode, pos });
        }
        cache.1
    }

    /// 在控制列上按了縮小 / 展開：存進介面設定，下次錄影也一樣
    pub fn set_rec_bar_mode(&self, mode: crate::types::RecBarMode) {
        self.patch_ui("recBar", Some(serde_json::json!(mode)));
    }

    /// 錄整個螢幕時把控制列拖到別的地方（None = 回到螢幕上方中間）：下次錄影也放在那裡
    pub fn set_rec_bar_pos(&self, pos: Option<(i32, i32)>) {
        self.patch_ui("recBarPos", pos.map(|(x, y)| serde_json::json!([x, y])));
    }

    /// 改介面設定的一個欄位（None = 刪掉）
    fn patch_ui(&self, key: &str, value: Option<serde_json::Value>) {
        let mut ui = self.settings.load().ui.unwrap_or_else(|| serde_json::json!({}));
        if let Some(m) = ui.as_object_mut() {
            match value {
                Some(v) => m.insert(key.into(), v),
                None => m.remove(key),
            };
        }
        self.settings.save(SettingsPatch { ui: Some(ui), ..Default::default() });
    }

    /// 攝影機小視窗拖曳、調整大小後的位置（擷取範圍內的相對位置，萬分比）與大小（短邊的百分比）：
    /// 存進介面設定與錄影設定（從系統匣、快速鍵開始錄影時用的是錄影設定），下次從這裡開始
    pub fn save_camera_layout(&self, pos: [u16; 2], size: u32) {
        let cur = self.settings.load();
        let mut ui = cur.ui.unwrap_or_else(|| serde_json::json!({}));
        if let Some(m) = ui.as_object_mut() {
            m.insert("cameraPos".into(), serde_json::json!(pos));
            m.insert("cameraSize".into(), serde_json::json!(size));
        }
        let mut config = cur.config;
        if let Some(cam) = config.as_mut().and_then(|c| c.get_mut("camera")).and_then(|c| c.as_object_mut()) {
            cam.insert("pos".into(), serde_json::json!(pos));
            cam.insert("size".into(), serde_json::json!(size));
        }
        self.settings.save(SettingsPatch { ui: Some(ui), config, ..Default::default() });
    }

    /// 儲存位置裡沒有收尾的錄影（意外中斷留下的分段；不含正在錄的）
    pub fn orphan_recordings(&self, output_dir: &str) -> Vec<crate::recovery::Orphan> {
        crate::recovery::find(Path::new(output_dir), self.recorder.parts_dir().as_deref())
    }

    /// 依序合併沒有收尾的錄影：(救回的檔案, 失敗原因)
    pub async fn recover_recordings(&self, list: Vec<crate::recovery::Orphan>) -> (Vec<String>, Vec<String>) {
        let Some(ffmpeg) = self.ffmpeg_path() else { return (Vec::new(), vec![tr!("找不到 ffmpeg.exe", "ffmpeg.exe not found").into()]) };
        let (mut saved, mut errors) = (Vec::new(), Vec::new());
        for o in &list {
            match crate::recovery::recover(&ffmpeg, o).await {
                Ok(r) => {
                    crate::info!("救回中斷的錄影：{}", r.path.as_deref().unwrap_or(""));
                    saved.extend(r.path);
                }
                Err(e) => {
                    crate::warn!("無法救回 {}：{}", o.dir.display(), e.message());
                    errors.push(e.message().to_string());
                }
            }
        }
        (saved, errors)
    }

    pub fn ffmpeg_path(&self) -> Option<PathBuf> {
        let st = self.lock();
        if st.ffmpeg.info.found {
            st.ffmpeg.info.path.clone().map(PathBuf::from)
        } else {
            None
        }
    }

    /// 測試用：直接指定 FFmpeg（不偵測）
    #[cfg(test)]
    pub(crate) fn set_ffmpeg_for_test(&self, path: &Path) {
        let mut st = self.lock();
        st.ffmpeg.info.found = true;
        st.ffmpeg.info.path = Some(path.display().to_string());
        st.ffmpeg.encoder = Some(crate::args::ENCODERS[0]);
    }

    pub fn export_ctx(&self) -> ExportCtx {
        ExportCtx { ffmpeg: self.ffmpeg_path(), encoder: self.lock().ffmpeg.encoder, cache: self.cache.clone() }
    }

    /// 軟體編碼器與實測可用的硬體編碼器（硬體測試還沒做完就先等它）
    async fn encoders(&self) -> Encoders {
        let mut rx = self.hw_ready.subscribe();
        let _ = rx.wait_for(|v| *v).await;
        let st = self.lock();
        Encoders {
            cpu: st.ffmpeg.encoder,
            gpu: st.ffmpeg.info.hw_encoders.clone().unwrap_or_default().iter().filter_map(|n| encoder_spec(n)).collect(),
            codec: st.ffmpeg.info.codec_encoders.clone().unwrap_or_default().iter().filter_map(|n| encoder_spec(n)).collect(),
        }
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
            default_shot_dir: crate::paths::default_shot_dir().display().to_string(),
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
        crate::quiet::recover();
        self.ready.send_replace(true);
    }

    /// 等第一次偵測完成（介面要讀環境資訊、系統匣要開始錄影時）；偵測卡住時最多等 60 秒
    pub async fn wait_ready(&self) {
        let mut rx = self.ready.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(60), rx.wait_for(|v| *v)).await;
    }

    /// 每 2 秒比對螢幕配置（很快）；變了就重新列舉螢幕與音訊裝置（HDMI 螢幕也會帶來播放裝置），
    /// 通知介面（Status::monitors_rev），錄影中交給錄影器處理（錄的螢幕不見就停止並存下，只是順序變了就繼續錄）
    pub fn watch_displays(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let sig = || async { tokio::task::spawn_blocking(crate::monitors::layout_signature).await.unwrap_or_default() };
            let mut last = sig().await;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let now = sig().await;
                // 空的：暫時拿不到（例如螢幕全部休眠、遠端桌面切換中），不算改變
                if now == last || now.is_empty() {
                    continue;
                }
                // 接上螢幕時 Windows 會連續調整好幾次：等穩定一點再讀
                tokio::time::sleep(Duration::from_millis(1500)).await;
                last = sig().await;
                let Some(app) = me.upgrade() else { return };
                let before: Vec<String> = app.lock().monitors.iter().map(MonitorInfo::label).collect();
                app.refresh_devices().await;
                let after: Vec<String> = {
                    let mut st = app.lock();
                    st.monitors_rev += 1;
                    st.monitors.iter().map(MonitorInfo::label).collect()
                };
                info!("[螢幕] 配置改變：{} → {}", before.join("、"), after.join("、"));
                app.recorder.displays_changed().await;
            }
        });
    }

    pub async fn refresh_devices(&self) {
        let (monitors, audio) =
            tokio::task::spawn_blocking(|| (crate::monitors::enumerate_monitors(), crate::audio::list_audio_devices())).await.unwrap_or_else(|e| (Err(e.to_string()), Err(e.to_string())));
        let mut st = self.lock();
        match monitors {
            Ok(m) => {
                st.monitor_error = m.is_empty().then(|| tr!("找不到任何螢幕", "No screens found").to_string());
                st.monitors = m;
            }
            Err(e) => {
                st.monitors = Vec::new();
                st.monitor_error = Some(trf!("無法列舉螢幕：{e}", "Couldn't list screens: {e}"));
            }
        }
        st.audio = match audio {
            Ok(a) => AudioEnv { render: a.render, captures: a.captures, error: None },
            Err(e) => AudioEnv { render: None, captures: Vec::new(), error: Some(trf!("無法列舉音訊裝置：{e}", "Couldn't list audio devices: {e}")) },
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
                probe.info.codec_encoders = old.codec_encoders.clone();
            }
            probe.info.prefer_gpu = Some(prefer_gpu);
            let info = &probe.info;
            let need_hw = info.found && info.hw_encoders.is_none();
            let need_dda = info.found && info.has_ddagrab && info.ddagrab_works.is_none();
            // 顯示卡處理的實測結果也保留（同一個執行檔）
            let (gpu_tested, gpu) = if probe.info.path == st.ffmpeg.info.path { (st.ffmpeg.gpu_tested, st.ffmpeg.gpu) } else { (false, None) };
            if gpu.is_some() {
                probe.info.gpu_convert = st.ffmpeg.info.gpu_convert.clone();
            }
            // 還要測顯示卡處理時先標成測試中：介面會一直更新到測完
            probe.info.gpu_testing = !gpu_tested && probe.info.has_ddagrab && !probe.gpu_filters.is_empty();
            st.ffmpeg = Ffmpeg { info: probe.info, encoder: probe.encoder, hw_listed: probe.hw_listed, gpu_filters: probe.gpu_filters, gpu_tested, gpu };
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
                // HEVC、AV1：只測同一家顯示卡（H.264 能用的那家）的，沒有顯示卡編碼器時不用測
                let codecs = crate::ffmpeg::test_codec_encoders(&PathBuf::from(&path), &listed, &list).await;
                {
                    let mut st = app.lock();
                    if st.ffmpeg.info.path.as_deref() == Some(path.as_str()) {
                        if list.is_empty() {
                            info!("沒有可用的 GPU 編碼器，將使用 CPU 編碼");
                        } else {
                            info!("可用的 GPU 編碼器：{}", list.join("、"));
                        }
                        if !codecs.is_empty() {
                            info!("可用的 HEVC / AV1 編碼器：{}", codecs.join("、"));
                        }
                        st.ffmpeg.info.hw_encoders = Some(list);
                        st.ffmpeg.info.codec_encoders = Some(codecs);
                    }
                }
                app.hw_ready.send_replace(true);
                app.test_gpu_convert();
            });
        }
        if need_dda {
            let app = self.clone();
            let test = tokio::spawn(async move {
                app.test_ddagrab().await;
                app.test_gpu_convert();
            });
            if wait_ddagrab {
                let _ = test.await;
            }
        }
        self.test_gpu_convert();
        self.mark_ready();
        self.env()
    }

    /// ddagrab 與硬體編碼器都測完後，在背景實測主螢幕那張顯示卡能不能縮放、轉色彩（Intel / AMD）。
    /// 每個 ffmpeg.exe 只測一次；失敗時錄影照舊用 CPU 轉換
    fn test_gpu_convert(self: &Arc<Self>) {
        let (path, monitor, convert, encoder) = {
            let mut st = self.lock();
            let f = &st.ffmpeg;
            // 等 ddagrab 與硬體編碼器都有結果才決定
            if f.gpu_tested || f.info.ddagrab_works.is_none() || f.info.hw_encoders.is_none() {
                return;
            }
            st.ffmpeg.gpu_tested = true;
            let primary = st.monitors.iter().find(|m| m.primary).or(st.monitors.first()).cloned();
            let target =
                primary.filter(|_| st.ffmpeg.info.ddagrab_works == Some(true)).and_then(|m| Some((GpuConvert::for_adapter(&m.adapter_name).filter(|c| st.ffmpeg.gpu_filters.contains(c))?, m)));
            let Some((convert, m)) = target else {
                st.ffmpeg.info.gpu_testing = false;
                return;
            };
            let encoder = st.ffmpeg.info.hw_encoders.as_ref().filter(|l| l.iter().any(|n| n == convert.encoder())).and_then(|_| encoder_spec(convert.encoder()));
            (st.ffmpeg.info.path.clone().unwrap_or_default(), m, convert, encoder)
        };
        let app = self.clone();
        tokio::spawn(async move {
            let r = test_gpu_convert(&PathBuf::from(&path), &monitor, convert, encoder).await;
            let mut st = app.lock();
            if st.ffmpeg.info.path.as_deref() != Some(path.as_str()) {
                return;
            }
            st.ffmpeg.info.gpu_testing = false;
            match r {
                Ok(g) => {
                    // 這裡說的是能力：「自動」編碼在 1080p60 以下用 CPU 壓縮，那時只在顯示卡上轉色彩、再下載
                    let label = g.convert.label();
                    let text = if g.zero_copy {
                        trf!(
                            "在顯示卡上縮放、轉色彩（{label}）；編碼器用 GPU 時畫面完全不經過 CPU",
                            "Scaling and color conversion on the GPU ({label}); with a GPU encoder, frames never pass through the CPU"
                        )
                    } else {
                        trf!("在顯示卡上縮放、轉色彩（{label}）", "Scaling and color conversion on the GPU ({label})")
                    };
                    info!("顯示卡處理測試成功：錄影時{text}");
                    st.ffmpeg.info.gpu_convert = Some(text);
                    st.ffmpeg.gpu = Some(g);
                }
                Err(e) => info!("顯示卡處理（{}）無法使用，錄影時用 CPU 轉換：{e}", convert.label()),
            }
        });
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

    /// 程式正在結束
    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    // ───────────── 排程錄影 ─────────────

    /// 等待中的排程錄影
    pub fn scheduled(&self) -> Option<crate::schedule::Schedule> {
        self.schedule.lock().unwrap().clone()
    }

    pub fn cancel_schedule(&self) {
        if self.schedule.lock().unwrap().take().is_some() {
            info!("[排程] 已取消");
        }
        self.schedule_gen.fetch_add(1, Ordering::SeqCst);
    }

    /// 排程錄影：等到指定時間用 s 的設定開始錄影（取代之前的排程）。
    /// 等待期間不讓電腦睡眠。回傳 Ok(true) = 已開始，Ok(false) = 取消或被新的排程取代
    pub async fn run_schedule(self: &Arc<Self>, s: crate::schedule::Schedule) -> crate::Result<bool> {
        use crate::schedule::{describe, title};
        let gen = self.schedule_gen.fetch_add(1, Ordering::SeqCst) + 1;
        info!("[排程] {}", describe(&s, chrono::Local::now()));
        *self.schedule.lock().unwrap() = Some(s.clone());
        let _awake = crate::power::keep_awake(false);
        loop {
            if self.schedule_gen.load(Ordering::SeqCst) != gen || self.quitting.load(Ordering::SeqCst) {
                return Ok(false);
            }
            if chrono::Local::now() >= s.at {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        self.schedule.lock().unwrap().take();
        let config = s.record_config();
        match crate::actions::record_start(self, config).await {
            Ok(()) => {
                let text = if s.minutes > 0 {
                    trf!("已開始錄影，{} 分鐘後自動停止", "Recording started. It stops automatically after {} min", s.minutes)
                } else {
                    tr!("已開始錄影", "Recording started").to_string()
                };
                info!("[排程] {text}");
                self.notify(title(), &text, false);
                Ok(true)
            }
            Err(e) => {
                warn!("[排程] 無法開始錄影：{}", e.message());
                self.notify(title(), &trf!("沒有開始錄影：{}", "Recording didn't start: {}", e.message()), true);
                Err(e)
            }
        }
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
                info!("有新版本 v{}", u.version);
                self.notify(tr!("有新版本", "Update available"), &trf!("v{} 已發佈，開啟操作視窗即可更新", "v{} is available. Open the main window to update", u.version), false);
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
            let args = preview_args(&mons, dda, PREVIEW_MAX_WIDTH, None, false);
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

    /// 即時預覽：FFmpeg 持續輸出固定大小的 RGBA 畫面（寬度最多 max_width 與 LIVE_MAX_WIDTH；回傳寬高）。
    /// 同時只保留一條：新的會結束舊的；回傳的 LiveGuard 被丟棄時 FFmpeg 也跟著結束。
    pub fn live_preview(&self, fps: f64, monitor_id: Option<&str>, max_width: u32) -> Option<(tokio::process::ChildStdout, LiveGuard, (u32, u32))> {
        let ffmpeg = self.ffmpeg_path()?;
        let max_width = max_width.clamp(2, LIVE_MAX_WIDTH);
        let dims = self.preview_dims(monitor_id, max_width)?;
        self.kill_live();
        let dda = self.ddagrab_usable() && !self.lock().monitors.is_empty();
        let skip_static = self.lock().ffmpeg.info.ddagrab_skip_static;
        let args = preview_args(&self.preview_monitors(monitor_id), dda, max_width, Some(fps), skip_static);
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

    /// 程式內更新：下載新版、取代 exe，完成後啟動新版並正常結束自己
    pub fn start_self_update(self: &Arc<Self>) -> crate::Result<()> {
        let Some(info) = self.update() else { return Err(crate::Error::config(tr!("目前沒有新版本", "No update available"))) };
        if self.recorder.active() {
            return Err(crate::Error::config(tr!("錄影中無法更新，請先停止錄影", "Can't update while recording. Stop recording first")));
        }
        if self.exporter.running() {
            return Err(crate::Error::config(tr!("正在製作加速版 / GIF，請等完成再更新", "A sped-up video / GIF is being made. Update after it finishes")));
        }
        if self.downloader.busy() {
            return Err(crate::Error::config(tr!("正在下載 FFmpeg，請等完成再更新", "FFmpeg is downloading. Update after it finishes")));
        }
        if cfg!(debug_assertions) {
            return Err(crate::Error::config(tr!("開發版無法自動更新", "Development builds can't update automatically")));
        }
        let exe = std::env::current_exe()?;
        if !exe.parent().is_some_and(crate::selfupdate::dir_writable) {
            return Err(crate::Error::config(tr!("程式所在的資料夾沒有寫入權限，請到下載頁面手動更新", "No write permission for the app's folder. Update manually from the download page")));
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
            self.installer.fail(trf!("已下載新版，但無法啟動：{e}。請手動重新開啟程式", "The new version was downloaded but couldn't start: {e}. Please reopen the app manually"));
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

/// 接好的長圖、捲了幾次、為什麼停下來（None = 按了 Esc、捲到次數上限或截不到畫面）
#[cfg(windows)]
type LongCapture = (crate::longshot::LongImage, usize, Option<crate::longshot::Step>);

/// 倒數 sec 秒；回傳 false = 取消（Windows：畫面上方顯示倒數，按 Esc 或點一下取消）
async fn countdown(sec: u64) -> bool {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || countdown_bubble(sec)).await.unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        tokio::time::sleep(Duration::from_secs(sec)).await;
        true
    }
}

/// 延遲截圖的倒數：「3」秒後框選截圖（先打開要截的選單・按 Esc 或點一下取消）
#[cfg(windows)]
fn countdown_bubble(sec: u64) -> bool {
    use crate::bubble::Content;
    use crate::bubble_win::{Bubble, Options};
    let content = |n: u64| Content {
        big: Some(n.to_string()),
        lines: vec![tr!("秒後框選截圖", "s until you select an area").into(), tr!("先打開要截的選單或提示・按 Esc 或點一下取消", "Open the menu or tooltip now · Esc or click to cancel").into()],
        buttons: vec![],
    };
    let bubble = Bubble::show(content(sec), Options { clickable: true, esc: true, near: None, bottom: false, on: None });
    let end = std::time::Instant::now() + Duration::from_secs(sec);
    loop {
        if !bubble.events().is_empty() {
            return false;
        }
        let left = end.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        bubble.set(content(left.as_secs_f64().ceil() as u64));
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(bubble);
    // 等提示關掉（不會被截進去，只是讓畫面先穩定）
    std::thread::sleep(Duration::from_millis(80));
    true
}

/// 一邊往下捲一邊擷取範圍、接成長圖；按 Esc 而且還沒接上任何一段時回傳 None
#[cfg(windows)]
fn scroll_capture(area: crate::types::Rect) -> crate::Result<Option<LongCapture>> {
    use crate::longshot::{Step, Stitcher};
    use crate::scroll_win::{esc_pressed, focus_at, grab_settled, park_cursor, scroll};
    crate::info!("[截圖] 開始長截圖：範圍 ({}, {}) {}×{}", area.x, area.y, area.width, area.height);
    // 範圍旁邊顯示進度（滑鼠穿過去、不會被截進去）
    let progress = |n: usize| {
        crate::bubble::Content::text(if crate::i18n::is_en() {
            format!("Scrolling screenshot · {n} part{} added · Esc to stop", if n == 1 { "" } else { "s" })
        } else {
            format!("長截圖中・已接 {n} 段・按 Esc 停止")
        })
    };
    let bubble = crate::bubble_win::Bubble::show(
        progress(0),
        crate::bubble_win::Options { clickable: false, esc: false, near: Some((area.x, area.y, area.x + area.width, area.y + area.height)), bottom: true, on: None },
    );
    let mut added = 0;
    // 先把範圍裡的視窗切到前面，再截第一張（切換時標題列會變色，要在截第一張之前）
    focus_at(area.x + area.width / 2, area.y + area.height / 2);
    park_cursor(area);
    let first = grab_settled(area, Duration::from_millis(1500)).ok_or_else(|| crate::Error::other(tr!("無法擷取畫面", "Couldn't capture the screen")))?;
    let (w, h) = (area.width as u32, area.height as u32);
    let mut st = Stitcher::new(first, w, h);
    // 每次捲多少（滾輪的單位，120 = 一格）：第一次捲一格量出一格捲幾列，之後每次捲大約半個範圍；
    // 對不起來時可能是一次捲太多（滑鼠設定成一次捲很多行），捲回去改少一點再試
    let mut delta = 120;
    let mut measured = false;
    let mut retries = 0;
    let _ = esc_pressed();
    let (mut steps, mut same) = (0, 0);
    // 停下來的原因（None = 捲到次數上限或截不到畫面）
    let mut end: Option<Step> = None;
    let mut esc = false;
    // 等滾輪送到範圍裡的視窗，再把游標移開（游標停在內容上會反白）、等捲動的動畫停下來再截
    let scroll_and_grab = |delta: i32| {
        scroll(area, delta);
        std::thread::sleep(Duration::from_millis(40));
        park_cursor(area);
        std::thread::sleep(Duration::from_millis(110));
        grab_settled(area, Duration::from_millis(1200))
    };
    while steps < 300 {
        let img = scroll_and_grab(delta);
        if esc_pressed() {
            esc = true;
            break;
        }
        let Some(img) = img else { break };
        steps += 1;
        match st.add(img) {
            Step::Added(s) => {
                same = 0;
                retries = 0;
                added += 1;
                bubble.set(progress(added));
                if !measured {
                    measured = true;
                    let per_notch = s as f64 * 120.0 / delta as f64;
                    let notches = (area.height as f64 * 0.45 / per_notch).floor().clamp(1.0, 5.0) as i32;
                    if delta >= 120 {
                        delta = notches * 120;
                    }
                }
            }
            // 有些網頁捲動後才載入內容：再捲一次看看
            Step::Same => {
                same += 1;
                if same >= 2 {
                    end = Some(Step::Same);
                    break;
                }
            }
            Step::Lost if retries < 2 => {
                retries += 1;
                let _ = scroll_and_grab(-delta);
                // 一格還是太多時改捲三分之一格（瀏覽器支援；其他程式累積到一格才捲）
                delta = if delta > 120 { (delta / 240).max(1) * 120 } else { 40 };
                crate::info!("[截圖] 長截圖：第 {steps} 次對不起來，捲回去改捲 {delta}（120 = 一格）再試");
            }
            s => {
                end = Some(s);
                break;
            }
        }
    }
    match (esc, end) {
        (true, _) => crate::info!("[截圖] 長截圖：按了 Esc，停止"),
        (_, Some(Step::Lost)) => crate::info!("[截圖] 長截圖：第 {steps} 次捲動後畫面對不起來，停止"),
        (_, Some(Step::Full)) => crate::info!("[截圖] 長截圖：已達最高 {} 像素，停止", crate::longshot::MAX_HEIGHT),
        _ => {}
    }
    // 一段都沒接上：說明原因，不要存一張和框選範圍一樣的圖讓人以為沒作用
    if !st.grew() {
        if esc {
            return Ok(None);
        }
        return Err(crate::Error::config(if end == Some(Step::Lost) {
            tr!(
                "捲動後的畫面對不起來，沒辦法接成長圖。請只框選會捲動的內容（例如網頁的內文），避開影片、動畫或會變動的廣告",
                "The scrolled content didn't line up, so it couldn't be stitched into one image. Select only the part that scrolls (e.g. the body of a web page) and avoid videos, animations or changing ads"
            )
        } else {
            tr!(
                "畫面沒有捲動：已經在最下面，或這個範圍不能用滑鼠滾輪捲動。請框選可以捲動的內容（例如網頁中間）再試一次",
                "The content didn't scroll: it's already at the bottom, or this area can't be scrolled with the mouse wheel. Select content that scrolls (e.g. the middle of a web page) and try again"
            )
        }));
    }
    drop(bubble);
    Ok(Some((st.finish(), steps, end)))
}

/// 存長截圖並複製到剪貼簿，回傳 (是否已複製, 小縮圖)。長截圖可能有上百 MB，所以：
/// - 分段存放的圖一列一列壓成 PNG 存檔（不用先接成一整塊）
/// - 剪貼簿的 PNG 用剛壓好的內容（不重新壓縮）；點陣圖在放掉原圖之後才解碼，記憶體最多只佔一份圖
/// - 小縮圖用原圖最上面一段（整張縮小會變成一條細線），不用再把檔案讀回來
#[cfg(windows)]
fn save_long_shot(img: crate::longshot::LongImage, out: &Path) -> crate::Result<(bool, Option<tiny_skia::Pixmap>)> {
    let mut png = Vec::new();
    img.write_png(&mut png).map_err(|e| crate::Error::other(trf!("無法儲存圖片：{e}", "Couldn't save the image: {e}")))?;
    std::fs::write(out, &png).map_err(|e| crate::Error::other(trf!("無法儲存圖片：{e}", "Couldn't save the image: {e}")))?;
    let thumb = long_shot_thumb(&img);
    drop(img);
    let copied = crate::clipboard::copy_png_bytes(&png).2;
    Ok((copied, thumb))
}

/// 長截圖的小縮圖：最上面 16:9 的一段（長截圖每個像素都不透明，可以直接當預乘的畫面）
#[cfg(any(windows, test))]
fn long_shot_thumb(img: &crate::longshot::LongImage) -> Option<tiny_skia::Pixmap> {
    let top = img.h.min((img.w * 9 / 16).max(1));
    let pm = tiny_skia::Pixmap::from_vec(img.top(top), tiny_skia::IntSize::from_wh(img.w, top)?)?;
    crate::shot_toast::thumbnail(&pm, 640)
}

/// 拼成一張的間距（像素）：0 無、1 小（依圖的大小 8～24）、2 大（小的三倍）
pub fn combine_gap(images: &[tiny_skia::Pixmap], spacing: u8) -> u32 {
    let auto = (images.iter().map(|i| i.width().min(i.height())).min().unwrap_or(0) / 40).clamp(8, 24);
    match spacing {
        0 => 0,
        1 => auto,
        _ => auto * 3,
    }
}

impl App {
    /// 新截圖的檔名：Shot_日期_時間.png（同一秒有好幾張時加 _2、_3…）；設了命名規則時依規則取名
    /// （{程式}、{視窗} 用這次截圖記下的程式；{內容} 截圖時還不知道，空白）
    async fn new_shot_path(&self, dir: &str) -> crate::Result<PathBuf> {
        let dir = PathBuf::from(dir);
        tokio::fs::create_dir_all(&dir).await.map_err(|e| crate::Error::config(trf!("無法建立儲存資料夾：{e}", "Couldn't create the save folder: {e}")))?;
        let stamp = crate::paths::timestamp();
        let rule = self.settings.load().ui.as_ref().and_then(|u| u.get(crate::after_record::SHOT_NAME_RULE_KEY)).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let base = if rule.trim().is_empty() {
            None
        } else {
            let meta = self.lock().shot_meta.clone().unwrap_or_default();
            let c = crate::naming::RuleCtx { app: meta.app, title: meta.title, ..crate::naming::RuleCtx::from_stem(&format!("Shot_{stamp}")) };
            let existing: Vec<String> = std::fs::read_dir(&dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            crate::naming::apply_rule(&rule, &c, "png", &existing)
        };
        let base = base.unwrap_or_else(|| format!("Shot_{stamp}"));
        Ok((1..).map(|i| dir.join(if i == 1 { format!("{base}.png") } else { format!("{base}_{i}.png") })).take(1000).find(|p| !p.exists()).unwrap_or_else(|| dir.join(format!("Shot_{stamp}.png"))))
    }
}

/// 從整個桌面的 PNG（左上角是 desk 的原點）裁切 rect，存成 PNG；回傳實際的寬高
fn crop_png(from: &Path, desk: crate::types::Rect, rect: crate::types::Rect, to: &Path) -> crate::Result<(u32, u32)> {
    let out = crop_pixmap(from, desk, rect)?;
    out.save_png(to).map_err(|e| crate::Error::other(trf!("無法儲存截圖：{e}", "Couldn't save the screenshot: {e}")))?;
    Ok((out.width(), out.height()))
}

/// 從整個桌面的 PNG 裁切 rect（超出桌面的部分裁掉）
fn crop_pixmap(from: &Path, desk: crate::types::Rect, rect: crate::types::Rect) -> crate::Result<tiny_skia::Pixmap> {
    let bytes = std::fs::read(from).map_err(|e| crate::Error::other(trf!("讀不到截下的畫面：{e}", "Couldn't read the captured screen: {e}")))?;
    let src = tiny_skia::Pixmap::decode_png(&bytes).map_err(|e| crate::Error::other(trf!("讀不到截下的畫面：{e}", "Couldn't read the captured screen: {e}")))?;
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let x0 = (rect.x - desk.x).clamp(0, sw);
    let y0 = (rect.y - desk.y).clamp(0, sh);
    let x1 = (rect.x + rect.width - desk.x).clamp(x0, sw);
    let y1 = (rect.y + rect.height - desk.y).clamp(y0, sh);
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut out = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or_else(|| crate::Error::config(tr!("範圍太小", "Area is too small")))?;
    let row = w as usize * 4;
    for y in 0..h as usize {
        let s0 = ((y0 as usize + y) * sw as usize + x0 as usize) * 4;
        out.data_mut()[y * row..(y + 1) * row].copy_from_slice(&src.data()[s0..s0 + row]);
    }
    Ok(out)
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

/// 只抓文字完成時，滑鼠旁的提示：「✓ 已複製 12 個字：報價單 2026…」
pub fn grab_text_flash(l: &crate::ocr::Layout) -> String {
    let n = l.text.chars().filter(|c| !c.is_whitespace()).count();
    if n == 0 {
        return tr!("沒有認出文字（框大一點、或把畫面放大再試試）", "No text recognized (try a larger area or zoom in)").into();
    }
    let flat: String = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = flat.chars().take(24).collect();
    if flat.chars().count() > 24 {
        preview.push('…');
    }
    if l.table {
        trf!("✓ 已複製成表格（{n} 個字），可以貼到 Excel", "✓ Copied as a table ({n} characters); paste into Excel")
    } else {
        trf!("✓ 已複製 {n} 個字：{preview}", "✓ Copied {n} characters: {preview}")
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn schedule_waits_cancels_and_starts() {
        use crate::schedule::Schedule;
        use crate::types::{AudioConfig, MethodPreference, SourceConfig};
        let dir = tempfile::tempdir().unwrap();
        let app = super::App::with_data_dir(dir.path().to_path_buf());
        app.mark_ready();
        let config = super::RecordConfig {
            source: SourceConfig::All,
            fps: 30.0,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: MethodPreference::Auto,
            output_dir: dir.path().display().to_string(),
            shot_dir: String::new(),
            audio: AudioConfig::default(),
            encoder: None,
            codec: Default::default(),
            auto_pause_sec: 0,
            countdown_sec: None,
            hide_ui: None,
            show_clicks: false,
            show_keys: false,
            cursor_halo: false,
            hide_icons: false,
            follow_window: None,
            camera: None,
            audio_only: false,
            window_capture: false,
            separate_audio: false,
            mic_denoise: false,
            quiet: false,
            masks: vec![],
            replay_sec: 0,
            monitor_key: None,
        };
        let later = Schedule { at: chrono::Local::now() + chrono::Duration::minutes(30), minutes: 10, config: config.clone() };
        let a = app.clone();
        let waiting = tokio::spawn(async move { a.run_schedule(later).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(app.scheduled().map(|s| s.minutes), Some(10));
        // 取消：等待中的工作結束，沒有開始錄影
        app.cancel_schedule();
        assert!(!waiting.await.unwrap().unwrap());
        assert!(app.scheduled().is_none());
        // 新的排程取代舊的
        let a = app.clone();
        let first = tokio::spawn({
            let s = Schedule { at: chrono::Local::now() + chrono::Duration::minutes(30), minutes: 0, config: config.clone() };
            async move { a.run_schedule(s).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // 時間到了：沒有 FFmpeg，開始錄影失敗（會通知）；排程清掉
        let now = Schedule { at: chrono::Local::now(), minutes: 5, config };
        assert!(app.run_schedule(now).await.unwrap_err().message().contains("ffmpeg"));
        assert!(!first.await.unwrap().unwrap());
        assert!(app.scheduled().is_none());
    }

    #[test]
    fn long_shot_thumb_uses_the_top() {
        // 400×3000 的長截圖：上面 225 列紅色、下面藍色；縮圖只有最上面 16:9 的一段（全紅）
        let (w, h) = (400u32, 3000u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *px = if (i as u32) < w * 225 { [200, 0, 0, 255] } else { [0, 0, 200, 255] };
        }
        let short = crate::longshot::LongImage::from_rgba(w, 100, rgba[..(w * 100 * 4) as usize].to_vec());
        let t = super::long_shot_thumb(&crate::longshot::LongImage::from_rgba(w, h, rgba)).unwrap();
        assert_eq!((t.width(), t.height()), (400, 225));
        assert!(t.pixels().iter().all(|p| p.red() == 200 && p.blue() == 0));
        // 比 16:9 矮的圖：整張
        assert_eq!(super::long_shot_thumb(&short).unwrap().height(), 100);
    }

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

    #[test]
    fn grab_text_flash_says_what_was_copied() {
        use crate::ocr::Layout;
        assert!(grab_text_flash(&Layout { text: " \n ".into(), table: false }).contains("沒有認出"));
        let t = grab_text_flash(&Layout { text: "報價單\n2026 年 10 月 16 日 總金額 NT$ 12,345 元整".into(), table: false });
        assert!(t.starts_with("✓ 已複製 ") && t.ends_with('…'), "{t}");
        assert!(grab_text_flash(&Layout { text: "a\tb\nc\td".into(), table: true }).contains("表格"));
    }

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
        app.wait_ready().await; // 已完成：立即傳回
    }

    /// 介面一開始就讀狀態：任何一個欄位都不能在持有鎖時再鎖一次（Windows 的步驟截圖曾經這樣卡住）
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
