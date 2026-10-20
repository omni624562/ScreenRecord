//! 操作視窗（egui）：主畫面、製作加速版 / GIF、全部錄影、剪輯、更新說明。
//! 直接呼叫後端（screenrecorder-core），非同步的工作在 tokio 上執行，完成後把結果交回介面執行緒。

#[cfg(debug_assertions)]
mod dev;
pub mod dialogs;
pub mod editor;
pub mod entry_menu;
pub mod export_dialog;
pub mod import;
pub mod inline_play;
pub mod jellyfin_browse;
pub mod library_dialog;
pub mod library_send;
pub mod main_view;
pub mod notes;
pub mod ocr;
pub mod playback;
pub mod presets;
pub mod preview;
pub mod replay;
pub mod schedule;
pub mod settings;
pub mod settings_dialog;
pub mod snip;
pub mod steps_review;
pub mod theme;
pub mod thumbs;
mod tour;
pub mod video_tags;
pub mod viewer;
mod whats_new;

use eframe::egui;
use screenrecorder_core::actions::{self, UpdateState};
use screenrecorder_core::app::{App, Status, UiPage};
use screenrecorder_core::types::{DownloadPhase, EnvInfo, LibraryEntry, LibraryPage, RecorderState};
use screenrecorder_core::{tr, trf};
use settings::UiSettings;
use std::collections::HashMap;
use std::future::Future;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 交回介面執行緒執行的工作
pub type Job = Box<dyn FnOnce(&mut UiApp) + Send>;

/// 系統匣或再次啟動程式時要求開啟視窗
#[derive(Clone, Default)]
pub struct OpenRequests(Arc<(Mutex<Vec<UiPage>>, std::sync::Condvar)>);

impl OpenRequests {
    pub fn push(&self, page: UiPage) {
        self.0 .0.lock().unwrap().push(page);
        self.0 .1.notify_all();
    }
    fn take(&self) -> Vec<UiPage> {
        std::mem::take(&mut *self.0 .0.lock().unwrap())
    }
    /// 等到有開啟視窗的要求（操作視窗還沒建立時，主執行緒在這裡等）；timeout 內有要求就回傳 true
    pub fn wait(&self, timeout: Duration) -> bool {
        let (list, cv) = &*self.0;
        let g = cv.wait_timeout_while(list.lock().unwrap(), timeout, |l| l.is_empty()).unwrap().0;
        !g.is_empty()
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
    /// 提示上的按鈕（例如「在資料夾中顯示」）；有按鈕時停留比較久、滑鼠在上面時不會消失
    pub actions: Vec<ToastAction>,
}

/// 提示上的一個按鈕
pub struct ToastAction {
    pub label: String,
    pub run: Box<dyn FnOnce(&mut UiApp)>,
}

impl ToastAction {
    pub fn new(label: impl Into<String>, run: impl FnOnce(&mut UiApp) + 'static) -> Self {
        Self { label: label.into(), run: Box::new(run) }
    }
}

/// 開檔類型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAction {
    /// 在內建檢視器開啟
    Play,
    /// 在主畫面（即時預覽的位置）播放；截圖仍用檢視器
    PlayHere,
    /// 用 Windows 預設的程式開啟
    External,
    Reveal,
    Edit,
    Export,
    /// 把檔案複製到剪貼簿（貼到 LINE、Teams、資料夾）
    CopyFile,
    /// 只留下聲音，另存成 .m4a
    SaveAudio,
    /// 章節（錄影時加的標記）複製成 YouTube 說明欄的格式
    CopyChapters,
    /// 錄影時記下的操作步驟（點了什麼、按了哪些快速鍵）複製成文字清單
    CopySteps,
    /// 錄影時記下的操作步驟做成步驟教學（每一步附上那一刻的畫面），在步驟整理視窗整理後做成文件
    StepsGuide,
    /// 原速壓縮（依畫質，或壓縮到某個大小以內傳 LINE / Email）：開啟製作視窗的「壓縮」
    Compress,
    /// 做成直式影片（Shorts / Reels）：開啟剪輯並選好「直式影片」
    Vertical,
    /// MOV、MKV 等其他格式轉成 MP4（存在原檔旁邊）
    Convert,
    /// 設為封面：開啟播放器，找到要的那一格再按「設為封面」
    SetThumb,
    /// 影片資訊（標題、作者、說明…）
    Tags,
    /// 傳到媒體庫資料夾（NAS、Jellyfin 媒體庫），並通知 Jellyfin
    SendToLibrary,
    /// 旋轉影片（改旋轉標記，不重新壓縮）：向左 90°、向右 90°、180°
    RotateLeft,
    RotateRight,
    Rotate180,
    /// 另存畫面真的轉好的影片（重新壓縮，舊播放器也正確）
    Upright,
    /// 重新命名（加速版、GIF 一起改；媒體庫那一份也跟著改）
    Rename,
    /// 加上 / 移除星號
    Star,
    /// 移到資源回收筒（連同底下的加速版；媒體庫也有時問要不要一起刪）
    Trash,
    /// 複製這支在 Jellyfin 的網址
    JellyfinLink,
    /// 用瀏覽器打開這支在 Jellyfin 的頁面
    OpenJellyfin,
}

pub struct UiApp {
    pub core: Arc<App>,
    pub rt: tokio::runtime::Handle,
    pub ctx: egui::Context,
    tx: Sender<Job>,
    rx: Receiver<Job>,
    open_requests: OpenRequests,
    /// 系統匣可以用：關掉視窗時只是隱藏（開機時系統匣可能晚一點才建立好）
    pub tray_ok: Arc<std::sync::atomic::AtomicBool>,
    pub visible: bool,
    /// 主視窗看得到（背景比對狀態時用：看不到就不叫介面重畫）
    shown_flag: Arc<std::sync::atomic::AtomicBool>,

    pub env: EnvInfo,
    pub env_ready: bool,
    pub s: UiSettings,
    settings_rev: u64,
    /// 講稿小視窗
    pub notes: Option<notes::Notes>,
    /// 排程錄影的對話框
    pub schedule_dlg: Option<schedule::ScheduleDlg>,
    /// 錄影設定組合（settings.json 的 presets）
    pub presets: Vec<screenrecorder_core::presets::Preset>,
    save_at: Option<Instant>,
    pub status: Status,
    pub status_at: Instant,
    pub update: UpdateState,

    pub toast: Option<Toast>,
    pub ask: Option<dialogs::Ask>,
    /// 「傳到媒體庫」選子資料夾的視窗
    pub send_dlg: Option<library_send::SendDlg>,
    /// 媒體庫的檔名索引（清單上標「已在媒體庫」）；正在背景讀
    pub lib_index: Option<library_send::LibIndex>,
    pub lib_index_loading: bool,
    /// 待傳清單：上次看 NAS 連不連得到的時間、正在補傳
    pub pending_check: Instant,
    pub pending_busy: bool,
    /// Jellyfin 上誰看過哪支、我的最愛（什麼時候讀的）；正在背景讀
    pub jf_watch: Option<(Instant, screenrecorder_core::jellyfin::WatchState)>,
    pub jf_watch_loading: bool,
    /// 瀏覽 Jellyfin 的視窗
    pub jf_browse: Option<jellyfin_browse::JfBrowse>,
    pub changelog_open: bool,
    /// 找到的攝影機（None = 還沒找完）；主畫面的攝影機選單與設定視窗共用
    pub cameras: Option<Vec<String>>,
    cameras_loading: bool,
    /// 新功能介紹（更新後第一次開啟時顯示一次）
    pub whats_new_open: bool,
    whats_new_checked: bool,
    /// 一步一步的教學（第一次錄影、各功能第一次打開的提示）
    pub tour: Option<tour::Tour>,
    /// 已檢查過要不要問搬移截圖（每次啟動一次）
    shot_move_checked: bool,

    pub preview: preview::Preview,
    pub thumbs: thumbs::Thumbs,
    pub recent: Option<LibraryPage>,
    pub recent_page: usize,
    recent_seq: u64,
    /// 最近錄影上次讀的時間；視窗上次有沒有焦點、檔案庫上次開著沒（從檔案總管、手機同步放進資料夾的影片，
    /// 程式不會知道：切回視窗、關掉檔案庫、每 2 分鐘重讀一次）
    recent_at: Instant,
    was_focused: bool,
    library_was_open: bool,
    pub recent_per_page: usize,
    /// 已知的錄影（最近錄影、全部錄影），操作按鈕用完整資訊
    pub known: HashMap<String, LibraryEntry>,

    pub export_dlg: Option<export_dialog::ExportDialog>,
    pub library: Option<library_dialog::LibraryDialog>,
    pub viewer: Option<viewer::Viewer>,
    /// 介面的 OpenGL（mpv 直接畫進材質，見 playback.rs）
    pub gl: Option<Arc<eframe::glow::Context>>,
    pub proc_address: Option<screenrecorder_core::mpv::ProcAddress>,
    pub video_surface: Option<playback::VideoSurface>,
    /// 主畫面內播放用的（和檢視器分開：檢視器蓋在上面時主畫面的播放只是暫停）
    pub inline_surface: Option<playback::VideoSurface>,
    /// 主畫面內播放（最近錄影的播放鍵）
    pub inline: Option<inline_play::Inline>,
    /// 文字辨識的結果
    pub ocr: Option<ocr::Ocr>,
    /// 錄影前的音量表（主畫面看得到、沒在錄影時才開）
    pub meter: Option<screenrecorder_core::meter::Meter>,
    /// 按了「測試音量」：量到這個時間（不一直開著麥克風，Windows 才不會一直顯示麥克風使用中）
    pub meter_until: Option<Instant>,
    pub settings_dlg: Option<settings_dialog::SettingsDialog>,
    /// 在螢幕上框選截圖
    pub snip: Option<snip::Snip>,
    /// 整理步驟截圖（做成教學文件前）
    pub steps_review: Option<steps_review::Review>,
    /// 正在匯入影片（拖進來的、「匯入影片…」）
    pub importing: bool,
    /// 影片資訊
    pub video_tags: Option<video_tags::TagsDialog>,
    /// 目前設定的全域快速鍵（顯示用）
    pub keys: screenrecorder_core::types::Hotkeys,
    pub editor: Option<editor::Editor>,

    pub dismissed_job: Option<u64>,
    last_export_state: Option<screenrecorder_core::types::ExportState>,
    last_result_path: Option<String>,
    /// 回溯錄影最近存下的檔案（快速鍵、系統匣存下時也更新最近錄影）
    last_replay_path: Option<String>,
    last_state: RecorderState,
    /// 看過的螢幕配置改變次數（core 的 Status::monitors_rev）
    monitors_rev: u64,
    pub main: main_view::MainState,
    ddagrab_watch: Option<Instant>,
    /// 已處理過的截圖（介面或快速鍵截好時更新清單、顯示提示）
    pub last_shot_seq: u64,
    /// 剛把檔案拖到其他程式：系統拖曳吃掉了放開滑鼠的訊息，下一個畫面補一個「放開」
    pub release_pointer: bool,
    /// 剛從清單拖出去的檔案（放回自己的視窗時不匯入）
    pub dragged_out: Option<(Vec<String>, std::time::Instant)>,
    /// 已經套用的外觀（跟著 Windows／淺色／深色）
    applied_theme: Option<String>,
    /// 上一格各部分花的時間（毫秒）：畫面處理太慢時寫進記錄檔，找出卡在哪裡
    frame_parts: Vec<(&'static str, f32)>,
    slow_logged: Option<Instant>,
    /// 開啟時螢幕上放不下原本的大小：第一個畫面顯示之後最大化
    start_max: bool,
    /// 上次檢查視窗位置的時間、當時的螢幕配置（改變時把視窗拉回看得到的地方）
    win_check: Instant,
    screens_sig: String,
    #[cfg(debug_assertions)]
    dev: dev::Dev,
}

/// 把還沒顯示的操作視窗放到螢幕上看得到的位置（Windows）：記住的位置，或滑鼠所在的螢幕、在扣掉工作列的範圍置中；
/// 回傳要不要最大化（螢幕放不下原本的大小）
fn place_window(cc: &eframe::CreationContext, saved: Option<screenrecorder_core::main_window::Placement>) -> bool {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) {
            screenrecorder_core::main_window::attach(h.hwnd.get());
        }
    }
    #[cfg(not(windows))]
    let _ = cc;
    screenrecorder_core::main_window::place_at_start(saved, settings::WINDOW_SIZE)
}

impl UiApp {
    /// seen_shot：已經處理過的截圖（之後才建立操作視窗時，之前的截圖不再顯示「已截圖」）
    pub fn new(
        cc: &eframe::CreationContext,
        core: Arc<App>,
        rt: tokio::runtime::Handle,
        open_requests: OpenRequests,
        tray_ok: Arc<std::sync::atomic::AtomicBool>,
        visible: bool,
        seen_shot: u64,
    ) -> UiApp {
        let ctx = cc.egui_ctx.clone();
        theme::setup_fonts(&ctx);
        theme::setup_visuals(&ctx);
        let (tx, rx) = channel();
        // 第一次偵測（FFmpeg、螢幕、音訊裝置）在背景進行：先用目前知道的資訊畫出視窗
        let env = core.env();
        let saved = core.settings.load();
        let s = UiSettings::from_saved(saved.ui.as_ref(), &env);
        // 視窗還沒顯示：先放到螢幕上看得到的位置（扣掉工作列）
        let start_max = place_window(cc, s.window);
        let status = core.status();
        let app = UiApp {
            update: actions::update_state(&core),
            core,
            rt,
            ctx: ctx.clone(),
            tx,
            rx,
            open_requests,
            tray_ok,
            visible,
            shown_flag: Arc::new(std::sync::atomic::AtomicBool::new(visible)),
            env,
            env_ready: false,
            s,
            settings_rev: saved.rev,
            presets: saved.presets.clone(),
            notes: None,
            schedule_dlg: None,
            save_at: None,
            last_state: status.recorder.state,
            monitors_rev: status.monitors_rev,
            status,
            status_at: Instant::now(),
            toast: None,
            ask: None,
            send_dlg: None,
            lib_index: None,
            lib_index_loading: false,
            pending_check: Instant::now(),
            pending_busy: false,
            jf_watch: None,
            jf_watch_loading: false,
            jf_browse: None,
            changelog_open: false,
            cameras: None,
            cameras_loading: false,
            whats_new_open: false,
            whats_new_checked: false,
            tour: None,
            shot_move_checked: false,
            preview: preview::Preview::default(),
            thumbs: thumbs::Thumbs::default(),
            recent: None,
            recent_page: 1,
            recent_seq: 0,
            recent_at: Instant::now(),
            was_focused: true,
            library_was_open: false,
            recent_per_page: 4,
            known: HashMap::new(),
            export_dlg: None,
            library: None,
            viewer: None,
            gl: cc.gl.clone(),
            proc_address: cc.get_proc_address.clone(),
            video_surface: None,
            inline_surface: None,
            inline: None,
            ocr: None,
            meter: None,
            meter_until: None,
            settings_dlg: None,
            snip: None,
            steps_review: None,
            importing: false,
            video_tags: None,
            keys: saved.hotkeys.unwrap_or_default(),
            editor: None,
            dismissed_job: None,
            last_export_state: None,
            last_result_path: None,
            last_replay_path: None,
            main: main_view::MainState::default(),
            ddagrab_watch: None,
            last_shot_seq: seen_shot,
            release_pointer: false,
            dragged_out: None,
            applied_theme: None,
            frame_parts: Vec::new(),
            slow_logged: None,
            start_max,
            win_check: Instant::now(),
            screens_sig: screenrecorder_core::main_window::screens_signature(),
            #[cfg(debug_assertions)]
            dev: Default::default(),
        };
        app.watch_status();
        let core = app.core.clone();
        app.spawn(async move { core.wait_ready().await }, |app, _| {
            app.env = app.core.env();
            app.env_ready = true;
            let saved = app.core.settings.load();
            let (s, notes) = UiSettings::from_saved_noted(saved.ui.as_ref(), &app.env);
            app.s = s;
            // 上次選的螢幕沒接上等：說明改成什麼（並存回設定，系統匣、快速鍵開始的錄影也用新的）
            if !notes.is_empty() {
                app.toast(notes.join("\n"), false);
                app.save_settings();
            }
            app.settings_rev = saved.rev;
            app.presets = saved.presets;
            app.preview.reset();
            app.watch_ddagrab();
            app.load_recent();
            app.check_orphans();
        });
        app
    }

    /// 上次錄影時程式意外結束（當機、停電、被強制關掉）留下分段：問要不要救回
    fn check_orphans(&mut self) {
        self.find_orphans(false);
    }

    /// 錄影失敗卡的「救回已錄的部分」：找到時問要不要救回，找不到時說明
    pub fn recover_now(&mut self) {
        self.find_orphans(true);
    }

    fn find_orphans(&mut self, tell_if_none: bool) {
        let (core, dir) = (self.core.clone(), self.s.out_dir(&self.env));
        self.spawn(async move { tokio::task::spawn_blocking(move || core.orphan_recordings(&dir)).await.unwrap_or_default() }, move |app, list| {
            if list.is_empty() {
                if tell_if_none {
                    app.toast(tr!("儲存位置裡找不到可以救回的分段", "No recorded parts to recover were found at the save location"), true);
                }
                return;
            }
            let names = list
                .iter()
                .map(|o| {
                    // 2026-10-10_14-30-05 → 2026-10-10 14:30
                    let s = &o.stamp;
                    let when = match (s.get(..10), s.get(11..13), s.get(14..16)) {
                        (Some(d), Some(h), Some(m)) => format!("{d} {h}:{m}"),
                        _ => s.clone(),
                    };
                    format!("{when}　{}", screenrecorder_core::format::format_bytes(o.bytes))
                })
                .collect();
            let (keep, drop) = (list.clone(), list);
            let mut ask = dialogs::Ask::confirm(
                tr!("找到沒有存好的錄影", "Found unsaved recordings"),
                tr!(
                    "上次錄影時程式意外結束（例如當機、停電），留下了下面這些錄影片段。要合併成影片嗎？",
                    "The app stopped unexpectedly during a recording (for example a crash or power loss) and left these pieces behind. Merge them into videos?"
                ),
                tr!("救回", "Recover"),
                move |app, _| app.recover_orphans(keep),
            )
            .with_alt(tr!("刪除", "Delete"), move |app, _| {
                for o in &drop {
                    screenrecorder_core::recovery::discard(o);
                }
                app.toast(tr!("已刪除沒有存好的錄影片段", "Deleted the unsaved recording pieces"), false);
            })
            .with_cancel(tr!("稍後", "Later"));
            ask.list = names;
            app.ask = Some(ask);
        });
    }

    fn recover_orphans(&mut self, list: Vec<screenrecorder_core::recovery::Orphan>) {
        self.toast(tr!("正在救回錄影…", "Recovering recordings…"), false);
        let core = self.core.clone();
        self.spawn(async move { core.recover_recordings(list).await }, |app, (saved, errors): (Vec<String>, Vec<String>)| {
            app.load_recent();
            if errors.is_empty() {
                let n = saved.len();
                app.toast(if screenrecorder_core::i18n::is_en() { format!("Recovered {n} recording{}", if n == 1 { "" } else { "s" }) } else { format!("已救回 {n} 支錄影") }, false);
            } else {
                app.toast(errors.join("\n"), true);
            }
        });
    }

    /// 閒置時介面不定時重畫：背景每 0.25 秒比對一次狀態（錄影、轉檔、下載、截圖、設定…），
    /// 有變化才叫介面重畫。比對只是讀幾個欄位，比重畫整個視窗（排版、繪圖）省得多
    fn watch_status(&self) {
        let (core, ctx) = (self.core.clone(), self.ctx.clone());
        self.rt.spawn(watch_changes(Duration::from_millis(250), self.shown_flag.clone(), move || core.status(), move || ctx.request_repaint()));
    }

    // ───────────── 非同步工作 ─────────────

    /// 在背景執行 fut，完成後在介面執行緒呼叫 done
    pub fn spawn<T: Send + 'static>(&self, fut: impl Future<Output = T> + Send + 'static, done: impl FnOnce(&mut UiApp, T) + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let r = fut.await;
            let _ = tx.send(Box::new(move |app: &mut UiApp| done(app, r)));
            ctx.request_repaint();
        });
    }

    /// 只抓文字的結果（core 已經複製到剪貼簿）：提示幾個字，可以打開來看
    pub fn show_grabbed_text(&mut self, l: screenrecorder_core::ocr::Layout) {
        if l.text.trim().is_empty() {
            return self.toast(tr!("沒有認出文字（框大一點、或把畫面放大再試試）", "No text recognized (try a larger area or zoom in)"), true);
        }
        let n = l.text.chars().filter(|c| !c.is_whitespace()).count();
        let msg = if l.table {
            trf!("已複製成表格（{n} 個字），可以直接貼到 Excel", "Copied as a table ({n} characters); paste it straight into Excel")
        } else {
            trf!("已複製 {n} 個字", "Copied {n} characters")
        };
        let text = l.text.clone();
        self.toast_with(msg, false, vec![ToastAction::new(tr!("看內容", "Show"), move |app| app.ocr = Some(ocr::Ocr { text: Some(text), lang: String::new() }))]);
    }

    /// 媒體庫資料夾的名稱（選單上顯示）；沒設定時 None
    pub fn library_folder(&self) -> Option<String> {
        let d = self.s.after_copy_to.trim();
        (!d.is_empty()).then(|| std::path::Path::new(d).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| d.to_string()))
    }

    /// 「傳到」選單項目的名稱與說明；還沒選資料夾時是「傳到媒體庫…」（按了打開設定）
    pub fn send_label(&self) -> (String, &'static str) {
        match self.library_folder() {
            Some(folder) => (
                trf!("傳到「{}」…", "Send to “{}”…", folder),
                tr!(
                    "選要放進媒體庫的哪個子資料夾，複製過去（同一個資料夾裡同名的覆蓋，連同字幕），有設定 Jellyfin 時通知它。媒體庫在「設定 → 存檔與分享」",
                    "Pick a subfolder of the library and copy it there (replacing one with the same name in that folder, subtitles included), then notify Jellyfin if set up. The library is in Settings → Saving & sharing"
                ),
            ),
            None => (
                tr!("傳到媒體庫…", "Send to library…").to_string(),
                tr!(
                    "還沒選資料夾：按了打開「設定 → 存檔與分享」，選 Jellyfin 媒體庫或 NAS 的資料夾",
                    "No folder yet: opens Settings → Saving & sharing so you can pick a Jellyfin library or NAS folder"
                ),
            ),
        }
    }

    /// 移到資源回收筒前的確認。媒體庫資料夾（Jellyfin 媒體庫）裡也有同名的那一份時，
    /// 問要兩邊都刪，還是只刪這台電腦的；done：刪好之後（清單、檢視器跟著更新）
    pub fn confirm_trash(&mut self, title: String, message: &str, paths: Vec<String>, done: fn(&mut UiApp, &[String], bool)) {
        let copy_to = self.s.after_copy_to.clone();
        let message = message.to_string();
        if copy_to.trim().is_empty() {
            return self.ask_trash(title, &message, paths, vec![], done);
        }
        // 媒體庫（NAS）裡同名而且大小一樣的那一份：整個媒體庫找一遍，NAS 大的時候要一下子，在背景找
        let files: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
        self.spawn(async move { tokio::task::spawn_blocking(move || screenrecorder_core::jellyfin::library_copies_of(&copy_to, &files)).await.unwrap_or_default() }, move |app, copies| {
            app.ask_trash(title, &message, paths, copies, done)
        });
    }

    /// 移到資源回收筒的確認視窗（copies：媒體庫裡的那一份，有的話問要不要一起刪）
    fn ask_trash(&mut self, title: String, message: &str, paths: Vec<String>, copies: Vec<std::path::PathBuf>, done: fn(&mut UiApp, &[String], bool)) {
        use screenrecorder_core::jellyfin::in_library;
        let copy_to = self.s.after_copy_to.clone();
        // 檔案本來就放在媒體庫裡：刪了就是從媒體庫刪掉，請 Jellyfin 重新掃描
        let in_lib = paths.iter().any(|p| in_library(&copy_to, std::path::Path::new(p)));
        let names: Vec<String> = paths.iter().map(|p| dialogs::file_name(p)).collect();
        let mut ask = if copies.is_empty() {
            dialogs::Ask::confirm(title, message, tr!("移到資源回收筒", "Move to Recycle Bin"), move |app, _| trash_now(app, paths, vec![], in_lib, done))
        } else {
            let folder = self.library_folder().unwrap_or_default();
            let n = copies.len();
            let more = trf!(
                "媒體庫「{folder}」裡也有 {n} 個同名的檔案（影片、字幕），要一起刪掉嗎？NAS 上的檔案可能無法從資源回收筒還原。",
                "The library “{folder}” also has {n} matching file(s) (video, subtitles). Delete them too? Files on a NAS may not be restorable from the Recycle Bin."
            );
            let (p2, c2) = (paths.clone(), copies.clone());
            let mut ask = dialogs::Ask::confirm(title, format!("{message}\n{more}"), tr!("兩邊都刪", "Delete both"), move |app, _| trash_now(app, p2, c2, true, done));
            ask.alt = Some((tr!("只刪這台電腦的", "Only on this PC").to_string(), Box::new(move |app, _| trash_now(app, paths, vec![], in_lib, done))));
            ask.alt_danger = true;
            ask
        };
        ask.danger = true;
        ask.list = names;
        self.ask = Some(ask);
    }

    /// 改名後：媒體庫裡那一份（連同字幕、加速版）也換成新名稱，並請 Jellyfin 重新掃描
    pub fn sync_renamed(&mut self, pairs: Vec<(std::path::PathBuf, std::path::PathBuf)>) {
        use screenrecorder_core::jellyfin::{in_library, rename_copies};
        self.lib_index = None;
        let copy_to = self.s.after_copy_to.clone();
        if copy_to.trim().is_empty() {
            return;
        }
        let in_lib = pairs.iter().any(|(_, new)| in_library(&copy_to, new));
        let jf = self.jellyfin();
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
                    let mut renamed = vec![];
                    for (old, new) in &pairs {
                        screenrecorder_core::transfer::rename_pending(&old.display().to_string(), &new.display().to_string());
                        renamed.extend(rename_copies(&copy_to, old, new)?);
                    }
                    if renamed.is_empty() && !in_lib {
                        return Ok(None);
                    }
                    let done = if !renamed.is_empty() {
                        tr!("媒體庫裡的那一份也改名了", "The copy in the library was renamed too")
                    } else {
                        tr!("媒體庫裡的檔案已改名", "Renamed in the library")
                    };
                    // 只掃描改了名的那個資料夾
                    let scan = renamed.first().cloned().or_else(|| pairs.first().map(|(_, new)| new.clone()));
                    Ok(Some(match jf.zip(scan).map(|(jf, at)| jf.scan(&at)) {
                        Some(Ok(_)) => format!("{done}{}", tr!("，已請 Jellyfin 重新掃描", "; Jellyfin was asked to rescan")),
                        Some(Err(e)) => format!("{done}{}", trf!("，但沒有通知到 Jellyfin：{e}", ", but Jellyfin couldn't be notified: {e}")),
                        None => done.to_string(),
                    }))
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            |app, r| match r {
                Ok(Some(msg)) => app.toast(msg, false),
                Ok(None) => {}
                Err(e) => app.toast(trf!("媒體庫裡的那一份沒有改名：{e}", "The copy in the library wasn't renamed: {e}"), true),
            },
        );
    }

    /// 設定好的 Jellyfin（網址和 API 金鑰都有填），帶著媒體庫資料夾（傳好只掃描放進去的資料夾用）
    pub fn jellyfin(&self) -> Option<screenrecorder_core::jellyfin::Jellyfin> {
        screenrecorder_core::jellyfin::Jellyfin::new(&self.s.jellyfin_url, &self.s.jellyfin_key)
            .map(|j| screenrecorder_core::jellyfin::Jellyfin { collections: self.s.jellyfin_collections, ..j.with_root(&self.s.after_copy_to) })
    }

    /// 這支影片在 Jellyfin 的網址（要先傳到媒體庫）：open 時用瀏覽器打開，不然複製
    pub fn jellyfin_link(&mut self, path: &str, open: bool) {
        let name = dialogs::file_name(path).to_string();
        let Some(jf) = self.jellyfin() else { return };
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || -> Result<Option<String>, String> { Ok(jf.find(&name)?.first().map(|id| jf.link(id, jf.server_id().as_deref()))) })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |app, r| match r {
                Ok(Some(url)) if open => {
                    if let Err(e) = actions::open_url(&url) {
                        app.toast(trf!("打不開瀏覽器：{}", "Couldn't open the browser: {}", e.message()), true);
                    }
                }
                Ok(Some(url)) => {
                    app.ctx.copy_text(url.clone());
                    app.toast(trf!("已複製 Jellyfin 連結：{url}", "Jellyfin link copied: {url}"), false);
                }
                Ok(None) => app.toast(tr!("Jellyfin 還沒有這支影片：先按「傳到媒體庫」，或等錄完自動處理複製過去", "Jellyfin doesn't have this video yet. Send it to the library first"), true),
                Err(e) => app.toast(trf!("查不到 Jellyfin：{e}", "Couldn't check Jellyfin: {e}"), true),
            },
        );
    }

    /// 改了影片（封面、影片資訊、畫面）之後：媒體庫資料夾（Jellyfin 的媒體庫）裡的那一份也跟著更新：
    /// 只換封面、影片資訊時直接透過 Jellyfin 改（不重傳整支影片），其他情況換成改好的檔案並請 Jellyfin 重新讀取
    pub fn sync_edited(&mut self, path: &str, edit: screenrecorder_core::jellyfin::Edit) {
        let jf = self.jellyfin();
        let copy_to = self.s.after_copy_to.clone();
        if copy_to.trim().is_empty() {
            return;
        }
        let video = std::path::PathBuf::from(path);
        let ffmpeg = self.core.ffmpeg_path();
        self.spawn(
            async move {
                let extras = screenrecorder_core::jellyfin::Extras::read(ffmpeg.as_deref(), &video).await;
                // 換了封面：取出新封面，直接傳給 Jellyfin
                let mut cover = None;
                if let (screenrecorder_core::jellyfin::Edit::Cover, Some(ff), Some(_)) = (edit, ffmpeg.as_deref(), jf.as_ref()) {
                    if let Some((jpg, _)) = screenrecorder_core::cover::extract_cover(ff, &video.display().to_string(), &video, 0).await {
                        cover = std::fs::read(&jpg).ok();
                        let _ = std::fs::remove_file(&jpg);
                    }
                }
                tokio::task::spawn_blocking(move || screenrecorder_core::jellyfin::sync_edited(jf.as_ref(), &copy_to, &video, &extras, edit, cover.as_deref())).await.ok().flatten()
            },
            |app, r| match r {
                Some(Ok(msg)) => app.toast(msg, false),
                Some(Err(e)) => app.toast(e, true),
                None => {}
            },
        );
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast_with(text, error, vec![]);
    }

    /// 帶按鈕的提示（例如「在資料夾中顯示」）：停留 8 秒，滑鼠移上去時不會消失，可按 ✕ 關閉
    pub fn toast_with(&mut self, text: impl Into<String>, error: bool, actions: Vec<ToastAction>) {
        let text = text.into();
        let secs = if !actions.is_empty() {
            8
        } else if error {
            6
        } else {
            3
        };
        self.toast = Some(Toast { text, error, until: Instant::now() + Duration::from_secs(secs), actions });
        self.ctx.request_repaint();
    }

    /// 存好一個檔案的提示：[開啟] [在資料夾中顯示]
    pub fn toast_file(&mut self, text: impl Into<String>, path: &str) {
        let (p1, p2) = (path.to_string(), path.to_string());
        let mut actions = vec![];
        let lower = path.to_lowercase();
        if [".mp4", ".gif", ".webp", ".webm", ".m4a"].iter().any(|e| lower.ends_with(e)) || dialogs::is_image(path) {
            actions.push(ToastAction::new(tr!("開啟", "Open"), move |app: &mut UiApp| app.act_path(EntryAction::Play, p1)));
        }
        actions.push(ToastAction::new(tr!("在資料夾中顯示", "Show in folder"), move |app: &mut UiApp| {
            if let Err(e) = actions::open(actions::OpenAction::Reveal, &p2) {
                app.toast(e.message().to_string(), true);
            }
        }));
        self.toast_with(text, false, actions);
    }

    /// 執行可能失敗的動作：失敗時顯示錯誤
    pub fn guarded<T: Send + 'static>(&self, fut: impl Future<Output = screenrecorder_core::Result<T>> + Send + 'static, ok: impl FnOnce(&mut UiApp, T) + Send + 'static) {
        self.spawn(fut, |app, r| match r {
            Ok(v) => ok(app, v),
            Err(e) => app.toast(e.message().to_string(), true),
        });
    }

    // ───────────── 設定 ─────────────

    /// 設定改了：稍後存檔（連續變更只存一次）
    /// 套用設定裡的介面語言：之後畫的介面、系統匣選單、通知都換成這個語言
    pub fn apply_language(&mut self) {
        let lang = screenrecorder_core::i18n::resolve(Some(self.s.language.as_str()));
        screenrecorder_core::i18n::set_lang(lang);
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(screenrecorder_core::winui::app_title()));
        self.ctx.request_repaint();
    }

    pub fn save_settings(&mut self) {
        self.save_at = Some(Instant::now() + Duration::from_millis(300));
        // logic() 在 ui() 之前，這一格算下次醒來時還不知道要存檔
        self.ctx.request_repaint_after(Duration::from_millis(310));
    }

    /// 馬上存檔（core 接著就要讀這個設定時）
    pub fn save_settings_now(&mut self) {
        self.save_at = Some(Instant::now());
        self.flush_settings();
    }

    fn flush_settings(&mut self) {
        if self.save_at.is_some_and(|t| Instant::now() >= t) {
            self.save_at = None;
            let config = self.s.record_config(&self.env);
            self.settings_rev = actions::save_settings(&self.core, Some(self.s.to_value()), Some(&config));
        }
    }

    /// 其他地方（系統匣選單）改了設定時重新讀取
    fn reload_settings_if_changed(&mut self) {
        if self.status.settings_rev != self.settings_rev && self.save_at.is_none() {
            let saved = self.core.settings.load();
            self.settings_rev = saved.rev;
            if saved.ui.is_some() {
                self.s = UiSettings::from_saved(saved.ui.as_ref(), &self.env);
            }
            self.presets = saved.presets;
        }
    }

    /// 上一格處理超過 60 毫秒：把各部分的時間寫進記錄檔（最多每 5 秒一次），方便找出哪裡卡
    fn log_slow_frame(&mut self, frame: &eframe::Frame) {
        let parts = std::mem::take(&mut self.frame_parts);
        let Some(cpu) = frame.info().cpu_usage else { return };
        if cpu < 0.06 || self.slow_logged.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.slow_logged = Some(Instant::now());
        let detail: Vec<String> = parts.iter().filter(|(_, ms)| *ms >= 1.0).map(|(n, ms)| format!("{n} {ms:.0}")).collect();
        screenrecorder_core::info!("[介面] 一格畫面花了 {:.0} 毫秒（{}）", cpu * 1000.0, if detail.is_empty() { "大多在繪製".into() } else { detail.join("、") });
    }

    // ───────────── 狀態 ─────────────

    pub fn locked(&self) -> bool {
        self.status.recorder.state != RecorderState::Idle
    }

    pub fn exporting(&self) -> bool {
        self.status.export.as_ref().is_some_and(|e| e.state == screenrecorder_core::types::ExportState::Running)
    }

    /// 進行中、而且是手動開始的轉檔（不是錄完自動處理開始的）：開始錄影前要先停止它
    pub fn manual_export(&self) -> Option<screenrecorder_core::types::ExportStatus> {
        self.status.export.clone().filter(|e| e.state == screenrecorder_core::types::ExportState::Running && !self.core.after_owns_export(e.id))
    }

    fn poll_status(&mut self) {
        let prev_update = self.status.update.clone();
        let prev_install = self.status.install.clone();
        self.status = self.core.status();
        #[cfg(debug_assertions)]
        dev::fake_status(self);
        self.status_at = Instant::now();
        if self.status.update != prev_update {
            self.update = actions::update_state(&self.core);
            self.env.update = self.update.update.clone();
        }
        if let Some(i) = &self.status.install {
            if i.phase == screenrecorder_core::selfupdate::InstallPhase::Error && prev_install.as_ref().map(|p| (&p.phase, &p.message)) != Some((&i.phase, &i.message)) {
                let msg = trf!("更新失敗：{}", "Update failed: {}", i.message.clone().unwrap_or_default());
                self.toast(msg, true);
            }
        }
        // FFmpeg 下載完成：重新偵測
        if self.status.download.phase == screenrecorder_core::types::DownloadPhase::Done && !self.env.ffmpeg.found {
            self.env = self.core.env();
            if self.env.ffmpeg.found {
                let msg = self.status.download.message.clone().unwrap_or_else(|| tr!("FFmpeg 已安裝", "FFmpeg installed").into());
                self.toast(msg, false);
                self.preview.reset();
                self.watch_ddagrab();
                self.load_recent();
            }
        }
        self.reload_settings_if_changed();
        // 等待中的排程：設定裡的副本跟著 core 的狀態（開始、取消後不會被這裡存回去）
        let scheduled = self.core.scheduled();
        if scheduled.is_none() != self.s.schedule.is_none() {
            self.s.schedule = scheduled.as_ref().map(screenrecorder_core::schedule::to_value);
        }
        if let Some(shot) = self.status.shot.clone().filter(|s| s.seq != self.last_shot_seq) {
            self.last_shot_seq = shot.seq;
            let name = dialogs::file_name(&shot.path);
            self.toast(if shot.copied { trf!("已截圖並複製到剪貼簿：{name}", "Screenshot copied to the clipboard: {name}") } else { trf!("已截圖：{name}", "Screenshot saved: {name}") }, false);
            self.shots_changed();
            // 設定「截圖後直接編輯」：開啟操作視窗與編輯（其他視窗開著時不打斷）
            if self.s.edit_after_shot && self.editor.is_none() && self.export_dlg.is_none() {
                self.core.open_ui(screenrecorder_core::app::UiPage::Main);
                editor::shot::open(self, shot.path.clone());
            }
        }
        // 接上、拔掉外接螢幕、改解析度或排列（core 偵測到）：重新讀螢幕、調整選的螢幕與範圍、重開預覽
        if self.status.monitors_rev != self.monitors_rev {
            self.monitors_rev = self.status.monitors_rev;
            if self.env_ready {
                self.displays_changed();
            }
        }
        let state = self.status.recorder.state;
        if state != self.last_state {
            // 開始 / 結束錄影時調整即時預覽的張數
            if (state == RecorderState::Idle) != (self.last_state == RecorderState::Idle) {
                self.preview.reset();
            }
            self.last_state = state;
        }
        let replay_last = self.status.recorder.replay.as_ref().and_then(|r| r.last.clone());
        if replay_last.is_some() && replay_last != self.last_replay_path {
            self.last_replay_path = replay_last;
            self.recent_page = 1;
            self.load_recent();
        }
        if let Some(r) = &self.status.recorder.result {
            if r.ok && r.path.is_some() && r.path != self.last_result_path {
                self.last_result_path = r.path.clone();
                self.recent_page = 1;
                self.load_recent();
            }
        }
        let exp_state = self.status.export.as_ref().map(|e| e.state);
        if exp_state != self.last_export_state {
            let finished = self.last_export_state == Some(screenrecorder_core::types::ExportState::Running) && exp_state != self.last_export_state;
            self.last_export_state = exp_state;
            if finished {
                self.load_recent();
                if let Some(l) = &mut self.library {
                    l.dirty = true;
                }
                // 錄完自動處理開始的轉檔：由錄完自動處理的卡片說明
                // 匯入中的轉檔：匯入完成時一次說明（不要每一支都跳提示）
                let importing = self.importing && self.status.export.as_ref().is_some_and(|e| e.kind == screenrecorder_core::types::ExportKind::Convert);
                if let Some(e) = self.status.export.clone().filter(|e| !self.core.after_owns_export(e.id) && !importing) {
                    if e.state == screenrecorder_core::types::ExportState::Done {
                        let what = match e.kind {
                            screenrecorder_core::types::ExportKind::Cut => tr!("剪輯完成", "Edited video ready"),
                            screenrecorder_core::types::ExportKind::Merge => tr!("合併完成", "Merged video ready"),
                            screenrecorder_core::types::ExportKind::Convert => tr!("已轉成 MP4", "Converted to MP4"),
                            screenrecorder_core::types::ExportKind::Upright => tr!("已另存畫面轉好的影片", "Upright copy saved"),
                            screenrecorder_core::types::ExportKind::Gif => tr!("GIF 製作完成", "GIF ready"),
                            screenrecorder_core::types::ExportKind::Webp => tr!("WebP 動圖製作完成", "WebP ready"),
                            screenrecorder_core::types::ExportKind::Speed if e.speed <= 1.0 => tr!("壓縮完成", "Compressed video ready"),
                            screenrecorder_core::types::ExportKind::Speed => tr!("加速版製作完成", "Sped-up video ready"),
                        };
                        // 提示上直接有「開啟」「在資料夾中顯示」
                        self.toast_file(format!("{what}{}{}", tr!("：", ": "), dialogs::file_name(&e.output)), &e.output);
                        // 剪輯、加速版、壓縮、合併的影片：也傳到媒體庫資料夾（NAS、Jellyfin）
                        let video = matches!(e.kind, screenrecorder_core::types::ExportKind::Cut | screenrecorder_core::types::ExportKind::Speed | screenrecorder_core::types::ExportKind::Merge);
                        if video && self.s.copy_exports && self.library_folder().is_some() {
                            self.send_auto(&e.output.clone());
                        }
                    } else {
                        // 剪輯失敗或取消：提示並提供「回到剪輯」
                        let e = e.clone();
                        editor::cut_ended(self, &e);
                    }
                }
            }
        }
        // ddagrab / 硬體編碼器測試中：每秒更新環境
        if self.ddagrab_watch.is_some_and(|t| Instant::now() >= t) {
            let tested = self.env.ffmpeg.ddagrab_works.is_some();
            self.env = self.core.env();
            if !tested && self.env.ffmpeg.ddagrab_works.is_some() {
                // ddagrab 測試結果出來：測試期間即時預覽會先試 ddagrab，失敗時改成單張，現在用確定的方式重開
                self.preview.retry_if_failed();
            }
            let f = &self.env.ffmpeg;
            let pending = (f.has_ddagrab && f.ddagrab_works.is_none()) || (f.found && f.hw_encoders.is_none()) || f.gpu_testing;
            self.ddagrab_watch = pending.then(|| Instant::now() + Duration::from_secs(1));
        }
    }

    pub fn watch_ddagrab(&mut self) {
        self.ddagrab_watch = Some(Instant::now() + Duration::from_secs(1));
    }

    /// 螢幕配置改變了：重新讀螢幕清單、調整選的螢幕與範圍、講稿小視窗的位置，重開即時預覽並說明
    fn displays_changed(&mut self) {
        let mut env = self.core.env();
        env.update = self.env.update.clone();
        self.apply_displays(env);
    }

    /// 套用新的螢幕清單（測試時可以直接給）
    pub fn apply_displays(&mut self, env: screenrecorder_core::types::EnvInfo) {
        let before = self.env.monitors.len();
        self.env = env;
        let mut notes = self.s.fix_monitor(&self.env);
        notes.extend(notes::fix_position(self));
        self.save_settings();
        self.preview.reset();
        self.preview.retry_live();
        let n = self.env.monitors.len();
        let head = match n.cmp(&before) {
            std::cmp::Ordering::Greater => trf!("接上了螢幕，現在有 {n} 個螢幕", "A screen was connected; {n} screens now"),
            std::cmp::Ordering::Less => trf!("拔掉了螢幕，現在有 {n} 個螢幕", "A screen was disconnected; {n} screens now"),
            std::cmp::Ordering::Equal => tr!("螢幕的解析度或排列改變了", "Screen resolution or arrangement changed").into(),
        };
        // 錄影中的處理（停止或繼續）由錄影器說明，這裡只說螢幕的變化
        let text = std::iter::once(head).chain(notes).collect::<Vec<_>>().join("\n");
        self.toast(text, false);
    }

    /// 重新偵測環境（FFmpeg、螢幕、音訊裝置）
    pub fn refresh_env(&mut self, then: impl FnOnce(&mut UiApp) + Send + 'static) {
        let core = self.core.clone();
        self.spawn(async move { core.refresh(false).await }, move |app, env| {
            app.env = env;
            let notes = app.s.fix_monitor(&app.env);
            if !notes.is_empty() {
                app.toast(notes.join("\n"), false);
                app.save_settings();
            }
            app.preview.reset();
            app.watch_ddagrab();
            app.load_recent();
            then(app);
        });
    }

    // ───────────── 最近錄影 ─────────────

    /// 尋找攝影機（找的時候 cameras 是 None）；force 時重新找
    pub fn load_cameras(&mut self, force: bool) {
        if self.cameras_loading || (self.cameras.is_some() && !force) {
            return;
        }
        self.cameras = None;
        self.cameras_loading = true;
        let core = self.core.clone();
        self.spawn(async move { actions::list_cameras(&core).await }, |app, list| {
            app.cameras = Some(list);
            app.cameras_loading = false;
        });
    }

    pub fn load_recent(&mut self) {
        self.recent_at = Instant::now();
        self.recent_seq += 1;
        let seq = self.recent_seq;
        let core = self.core.clone();
        let dir = self.s.out_dir(&self.env);
        let query = screenrecorder_core::types::LibraryQuery { page: Some(self.recent_page as f64), page_size: Some(self.recent_per_page as f64), ..Default::default() };
        self.spawn(async move { actions::library(&core, &dir, &query).await }, move |app, page| {
            if seq != app.recent_seq {
                return; // 較舊的要求晚回來：丟掉
            }
            app.recent_page = page.page.max(1);
            for e in &page.items {
                app.known.insert(e.media.path.clone(), e.clone());
            }
            app.recent = Some(page);
        });
    }

    /// 資料夾裡的影片可能被別的程式加進去、刪掉（檔案總管、手機同步）：切回這個視窗、關掉檔案庫時、
    /// 或每 2 分鐘，重讀最近錄影
    fn refresh_recent_if_stale(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let library_open = self.library.is_some();
        let back = (focused && !self.was_focused) || (self.library_was_open && !library_open);
        (self.was_focused, self.library_was_open) = (focused, library_open);
        if self.visible && self.recent.is_some() && (back || self.recent_at.elapsed() > Duration::from_secs(120)) {
            self.load_recent();
        }
    }

    /// 旋轉影片：檢視器正在播這支就先放開，改好再從同一個位置播；縮圖、清單重新讀
    pub fn rotate(&mut self, path: &str, turn: screenrecorder_core::rotate::Turn) {
        let t = viewer::release(self, path);
        let core = self.core.clone();
        let p = path.to_string();
        self.spawn(async move { actions::rotate_video(&core, &p, turn).await }, {
            let path = path.to_string();
            move |app, r| {
                match r {
                    Ok(info) => {
                        // 檢視器裡這支的寬高跟著換（畫面依這個大小擺）
                        if let Some(v) = app.viewer.as_mut() {
                            for e in v.list.iter_mut().filter(|e| e.media.path == path) {
                                (e.media.width, e.media.height) = (info.width, info.height);
                            }
                        }
                        app.thumbs.forget(&path);
                        app.toast(tr!("已旋轉（再轉一次可以轉回來）", "Rotated (rotate again to undo)"), false);
                        app.sync_edited(&path, screenrecorder_core::jellyfin::Edit::Video);
                        app.shots_changed();
                        app.load_recent();
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                }
                viewer::reload(app, &path, t);
            }
        });
    }

    /// 截圖新增或改了：重新讀取清單
    pub fn shots_changed(&mut self) {
        if let Some(d) = &mut self.library {
            d.dirty = true;
        }
    }

    /// 播放、顯示、剪輯、製作加速版
    pub fn act(&mut self, action: EntryAction, entry: LibraryEntry) {
        use actions::OpenAction;
        let path = entry.media.path.clone();
        // 主畫面正在播這個檔案：剪輯、製作、改名等要用到檔案時先停下來（Windows 上播放中的檔案不能改名、取代）
        let keeps = matches!(
            action,
            EntryAction::PlayHere
                | EntryAction::Reveal
                | EntryAction::CopyFile
                | EntryAction::CopyChapters
                | EntryAction::CopySteps
                | EntryAction::Star
                | EntryAction::JellyfinLink
                | EntryAction::OpenJellyfin
        );
        if !keeps && self.inline.as_ref().is_some_and(|i| i.is(&path)) {
            self.inline = None;
        }
        match action {
            EntryAction::Rename => library_dialog::rename_entry(self, entry),
            EntryAction::Star => {
                let on = !self.core.pins.contains(&path);
                library_dialog::set_star(self, &path, on);
                self.toast(if on { tr!("已加上星號：清理時會略過", "Starred: cleanup will skip it") } else { tr!("已移除星號", "Star removed") }, false);
            }
            EntryAction::Trash => library_dialog::trash_entry(self, &entry),
            EntryAction::JellyfinLink => self.jellyfin_link(&path, false),
            EntryAction::OpenJellyfin => self.jellyfin_link(&path, true),
            EntryAction::RotateLeft => self.rotate(&path, screenrecorder_core::rotate::Turn::Left),
            EntryAction::RotateRight => self.rotate(&path, screenrecorder_core::rotate::Turn::Right),
            EntryAction::Rotate180 => self.rotate(&path, screenrecorder_core::rotate::Turn::Half),
            EntryAction::Upright => {
                let core = self.core.clone();
                self.spawn(async move { actions::upright_start(&core, &path).await }, |app, r| match r {
                    Ok(_) => {
                        app.dismissed_job = None;
                        app.toast(tr!("已開始另存畫面轉好的影片，進度顯示在右側", "Saving an upright copy. Progress is shown on the right"), false);
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                });
            }
            EntryAction::Play => viewer::open(self, entry),
            EntryAction::PlayHere => inline_play::open(self, entry),
            EntryAction::SaveAudio => {
                let core = self.core.clone();
                self.toast(tr!("正在存成 M4A…", "Saving as M4A…"), false);
                self.spawn(async move { actions::save_audio(&core, &path).await }, |app, r| match r {
                    Ok(out) => {
                        app.toast_file(trf!("已存成 {}（和影片在同一個資料夾）", "Saved as {} (in the same folder as the video)", dialogs::file_name(&out)), &out);
                        // 全部錄影開著：列出新的聲音檔
                        app.shots_changed();
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                });
            }
            EntryAction::CopyChapters => {
                let duration = entry.media.duration_sec.unwrap_or(0.0);
                self.ctx.copy_text(screenrecorder_core::chapters::youtube_text(&entry.media.chapters, duration));
                self.toast(tr!("已複製章節，可以貼到 YouTube 影片的說明欄", "Chapters copied. Paste them into the YouTube video description"), false);
            }
            EntryAction::CopySteps => {
                let core = self.core.clone();
                self.spawn(async move { actions::steps_text(&core, &path).await }, |app, text| match text {
                    Some(t) => {
                        let n = t.lines().filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit())).count();
                        app.ctx.copy_text(t);
                        app.toast(trf!("已複製 {n} 個操作步驟，可以貼到文件或信件裡", "Copied {n} steps. Paste them into a document or email"), false);
                    }
                    None => app.toast(
                        tr!(
                            "這支錄影沒有記到操作步驟（3.21 以後在 Windows 上錄的影片，錄影中點的按鈕、按的快速鍵才會記下來）",
                            "No steps were recorded for this video (buttons clicked and shortcuts pressed are recorded on Windows from version 3.21)"
                        ),
                        true,
                    ),
                });
            }
            EntryAction::StepsGuide => {
                let core = self.core.clone();
                self.toast(tr!("正在擷取每一步的畫面…", "Capturing a frame for each step…"), false);
                self.spawn(async move { actions::steps_from_recording(&core, &path).await }, |app, r| match r {
                    Ok(0) => app.toast(
                        tr!(
                            "這支錄影沒有記到操作步驟（3.21 以後在 Windows 上錄的影片，錄影中點的按鈕、按的快速鍵才會記下來）",
                            "No steps were recorded for this video (buttons clicked and shortcuts pressed are recorded on Windows from version 3.21)"
                        ),
                        true,
                    ),
                    Ok(n) => app.toast(trf!("擷取了 {n} 步，整理好後做成文件", "Captured {n} steps. Tidy them up, then make the guide"), false),
                    Err(e) => app.toast(e.message().to_string(), true),
                });
            }
            EntryAction::Convert => self.convert(path),
            // 其他格式（MOV、MKV…）要先轉成 MP4 才能剪輯、製作加速版：直接開始轉，轉好再做
            EntryAction::Edit | EntryAction::Export | EntryAction::Compress | EntryAction::Vertical if screenrecorder_core::import::is_other_video(&path) => {
                self.toast(
                    trf!(
                        "{} 檔要先轉成 MP4 才能這樣做，現在開始轉換；轉好後在清單裡選 MP4 那一筆",
                        "{} files must be converted to MP4 first. Converting now; afterwards use the MP4 version in the list",
                        screenrecorder_core::import::format_label(&path)
                    ),
                    false,
                );
                self.convert(path)
            }
            EntryAction::Tags => video_tags::open(self, entry),
            EntryAction::SendToLibrary => self.send_to_library(&path),
            EntryAction::SetThumb => {
                viewer::open(self, entry);
                self.toast(tr!("播放或拖曳到想要的那一格，暫停後按上方的「設為封面」", "Play or drag to the frame you want, pause, then click \"Set as cover\" at the top"), false);
            }
            EntryAction::CopyFile => match screenrecorder_core::clipboard::copy_files(std::slice::from_ref(&path)) {
                Ok(()) => self.toast(tr!("已複製檔案，可以直接貼到 LINE、Teams、信件或資料夾", "File copied. Paste it into LINE, Teams, an email or a folder"), false),
                Err(e) => self.toast(e, true),
            },
            EntryAction::External | EntryAction::Reveal => {
                if action == EntryAction::External {
                    self.toast(tr!("正在以 Windows 預設的程式開啟…", "Opening with the Windows default app…"), false);
                }
                let a = if action == EntryAction::External { OpenAction::Play } else { OpenAction::Reveal };
                if let Err(e) = actions::open(a, &path) {
                    self.toast(e.message().to_string(), true);
                }
            }
            EntryAction::Edit if dialogs::is_image(&path) => editor::shot::open(self, path),
            EntryAction::Edit | EntryAction::Export | EntryAction::Compress | EntryAction::Vertical => {
                if let Some(why) = self.blocked(action) {
                    return self.toast(why, true);
                }
                if entry.media.duration_sec.unwrap_or(0.0) <= 0.0 {
                    return self.toast(tr!("無法讀取影片長度", "Couldn't read the video duration"), true);
                }
                match action {
                    EntryAction::Edit => editor::open(self, entry),
                    EntryAction::Vertical => editor::open_vertical(self, entry),
                    _ => self.export_dlg = Some(export_dialog::ExportDialog::new(entry, &self.s, action == EntryAction::Compress)),
                }
            }
        }
    }

    /// 轉成 MP4（存在原檔旁邊，原檔保留）；完成時由轉檔完成的提示說明
    fn convert(&mut self, path: String) {
        if let Some(why) = self.blocked(EntryAction::Export) {
            return self.toast(why, true);
        }
        let core = self.core.clone();
        self.spawn(async move { actions::convert_start(&core, &path).await }, |app, r| {
            if let Err(e) = r {
                app.toast(e.message().to_string(), true);
            }
        });
    }

    /// 現在不能剪輯、製作加速版（錄影中、轉檔進行中）的原因；清單、檢視器先停用按鈕並在滑鼠提示說明，不用關掉清單才發現
    pub fn blocked(&self, action: EntryAction) -> Option<String> {
        if !matches!(action, EntryAction::Edit | EntryAction::Export | EntryAction::Compress | EntryAction::Vertical) {
            return None;
        }
        let edit = matches!(action, EntryAction::Edit | EntryAction::Vertical);
        if self.locked() {
            let what = if edit { tr!("剪輯", "edit") } else { tr!("製作", "make it") };
            return Some(trf!("錄影中無法{what}，請先停止錄影", "Can't {what} while recording. Stop the recording first"));
        }
        if self.exporting() {
            return Some(
                if edit {
                    tr!("轉檔進行中，完成後才能剪輯", "A conversion is in progress; you can edit when it finishes")
                } else {
                    tr!("轉檔進行中，完成後才能製作", "A conversion is in progress; you can make it when it finishes")
                }
                .into(),
            );
        }
        None
    }

    /// 由路徑找到完整資訊後執行（剛錄好的檔案可能還不在清單裡：重新讀取）
    pub fn act_path(&mut self, action: EntryAction, path: String) {
        // 截圖不用讀影片資訊
        if action == EntryAction::Edit && dialogs::is_image(&path) {
            return editor::shot::open(self, path);
        }
        if let Some(e) = self.known.get(&path).filter(|e| e.media.duration_sec.is_some()).cloned() {
            return self.act(action, e);
        }
        let core = self.core.clone();
        let p = path.clone();
        self.spawn(
            async move {
                let ff = core.ffmpeg_path()?;
                core.cache.probe(&ff, &p).await.ok()
            },
            move |app, m| match m {
                Some(m) => app.act(action, LibraryEntry { media: m, exports: vec![], hits: vec![] }),
                None => app.toast(tr!("清單中找不到這個檔案", "This file isn't in the list"), true),
            },
        );
    }

    // ───────────── 視窗 ─────────────

    fn handle_open_requests(&mut self, ctx: &egui::Context) {
        for page in self.open_requests.take() {
            // 框選截圖：不開啟操作視窗
            if page == UiPage::Snip {
                snip::start(self, ctx);
                ctx.request_repaint();
                continue;
            }
            if !self.visible {
                // 隱藏期間螢幕可能改了（拔掉外接螢幕、改解析度）：顯示前先放回看得到的地方
                screenrecorder_core::main_window::keep_on_screen();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if !self.visible {
                self.visible = true;
                // 從系統匣叫回來：上次即時預覽失敗（例如當時在鎖定畫面）也重新試
                self.preview.retry_live();
                self.load_recent();
            }
            if page == UiPage::About {
                settings_dialog::open(self, settings_dialog::Page::About);
            }
            if page == UiPage::StepsReview {
                steps_review::open(self);
            }
            if page == UiPage::ReplayStop {
                // 系統匣、錄影控制列的「停止回溯錄影」：還有沒存下的內容時先問
                match self.status.recorder.replay.clone() {
                    Some(rp) => replay::ask_stop(self, &rp, main_view::live_recorded_ms(self)),
                    None => {
                        let core = self.core.clone();
                        self.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
                    }
                }
            }
            if page == UiPage::EditShot {
                match self.core.last_shot() {
                    Some(s) if self.editor.is_none() => editor::shot::open(self, s.path),
                    Some(_) => self.toast(tr!("編輯視窗已經開著，請先關閉", "The editor is already open. Close it first"), true),
                    None => self.toast(tr!("還沒有截圖", "No screenshots yet"), true),
                }
            }
        }
    }

    /// 套用設定裡的外觀：跟著 Windows、淺色或深色（改了才套用）
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.applied_theme.as_deref() == Some(self.s.theme.as_str()) {
            return;
        }
        self.applied_theme = Some(self.s.theme.clone());
        ctx.set_theme(match self.s.theme.as_str() {
            "light" => egui::ThemePreference::Light,
            "dark" => egui::ThemePreference::Dark,
            _ => egui::ThemePreference::System,
        });
    }

    /// 結束程式（設定裡的「結束程式」）：剪輯視窗有還沒儲存的修改、或有排程錄影時先問
    pub fn request_quit(&mut self) {
        let unsaved = self.editor.as_ref().is_some_and(|e| e.has_unsaved());
        if unsaved || self.core.scheduled().is_some() {
            self.ask_quit();
        } else {
            let core = self.core.clone();
            self.rt.spawn(async move { core.quit(0).await });
        }
    }

    /// 結束前的確認：剪輯視窗有還沒儲存的修改、有排程錄影
    fn ask_quit(&mut self) {
        let unsaved = self.editor.as_ref().is_some_and(|e| e.has_unsaved());
        let schedule = self.core.scheduled();
        let title = if unsaved {
            tr!("剪輯視窗有未儲存的修改，仍要結束嗎？", "The editor has unsaved changes. Quit anyway?")
        } else {
            tr!("有排程錄影，仍要結束嗎？", "A recording is scheduled. Quit anyway?")
        };
        let mut lines = vec![];
        if unsaved {
            lines.push(tr!("結束後，這次的剪輯、標註會不見。", "Your edits and annotations will be lost.").to_string());
        }
        if let Some(s) = &schedule {
            let when = screenrecorder_core::schedule::describe(s, chrono::Local::now());
            lines.push(trf!("有排程錄影（{when}），結束程式後不會錄。", "A recording is scheduled ({when}). It won't happen if the app isn't running."));
        }
        let mut ask = dialogs::Ask::confirm(title, lines.join("\n"), tr!("結束程式", "Quit"), |app, _| {
            app.ask = None;
            let core = app.core.clone();
            app.rt.spawn(async move { core.quit(0).await });
        });
        ask.danger = true;
        self.ask = Some(ask);
    }

    /// 每秒檢查一次：螢幕配置變了（解析度、縮放比例、外接螢幕、工作列）就把視窗拉回看得到的地方；位置變了就記下來
    fn watch_window(&mut self) {
        if self.win_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.win_check = Instant::now();
        let sig = screenrecorder_core::main_window::screens_signature();
        if sig != self.screens_sig {
            self.screens_sig = sig;
            screenrecorder_core::main_window::keep_on_screen();
        }
        self.remember_window();
    }

    /// 記下視窗目前的位置、大小、是否最大化（下次開啟時用）
    fn remember_window(&mut self) {
        if let Some(p) = screenrecorder_core::main_window::current() {
            if self.s.window != Some(p) {
                self.s.window = Some(p);
                self.save_settings();
            }
        }
    }

    /// 關掉視窗：系統匣可以用時只是隱藏（程式繼續常駐）
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.tray_ok.load(std::sync::atomic::Ordering::Relaxed) {
            self.remember_window();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.visible = false;
            // 第一次關到系統匣：用系統通知說明程式還在（只說一次）
            if !self.s.hide_notice_shown {
                self.s.hide_notice_shown = true;
                self.save_settings();
                self.core.notify(
                    tr!("螢幕錄影仍在背景執行", "Screen Recorder is still running"),
                    tr!("可從右下角的系統匣圖示開啟或結束（也可以在「設定 → 一般」結束程式）", "Open or quit it from the tray icon at the bottom right (or quit from Settings → General)"),
                    false,
                );
            }
            self.preview.stop(&self.core);
            if let Some(e) = &mut self.editor {
                e.pause();
            }
            if let Some(v) = &mut self.viewer {
                v.pause();
            }
            settings_dialog::cancel_key_capture(self);
        } else {
            // 沒有系統匣：關掉視窗就結束程式
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            let core = self.core.clone();
            self.rt.spawn(async move { core.quit(0).await });
        }
    }
}

impl eframe::App for UiApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.log_slow_frame(frame);
        let t0 = Instant::now();
        while let Ok(job) = self.rx.try_recv() {
            job(self);
        }
        self.handle_open_requests(ctx);
        if std::mem::take(&mut self.start_max) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        self.watch_window();
        self.flush_settings();
        self.flush_pending();
        self.refresh_recent_if_stale(ctx);
        self.poll_status();
        self.handle_close(ctx);
        self.apply_theme(ctx);
        // 系統匣「結束」時剪輯視窗有沒有還沒儲存的修改；要先問時在這裡顯示確認
        screenrecorder_core::tray::set_ui_unsaved(self.editor.as_ref().is_some_and(|e| e.has_unsaved()));
        if screenrecorder_core::tray::take_quit_request() {
            self.ask_quit();
        }
        // 視窗縮小、被其他視窗完全蓋住或隱藏時 ui() 不會執行（開始錄影時操作視窗也會縮小）：
        // 在這裡停掉即時預覽（否則多開一個 FFmpeg 擷取整個桌面，跟錄影搶資源）與音量測試
        let shown = self.visible && ctx.input(|i| i.viewport().visible()) != Some(false);
        if !shown {
            if self.preview.running() {
                self.preview.stop(&self.core);
            }
            if self.meter.is_some() {
                self.meter = None;
                self.meter_until = None;
            }
        }
        // 介面大小（設定 → 一般）
        let zoom = self.s.ui_scale as f32 / 100.0;
        if (ctx.zoom_factor() - zoom).abs() > 0.001 {
            ctx.set_zoom_factor(zoom);
        }
        self.frame_parts.push(("背景狀態", t0.elapsed().as_secs_f32() * 1000.0));
        // 錄影中（計時器）、開著剪輯 / 檢視 / 製作 / 全部錄影、轉檔或下載中：每 0.25 秒更新。
        // 主畫面閒置時不定時重畫：狀態有變化由 watch_status 叫醒，其餘每 5 秒保險一次；視窗隱藏時 2 秒
        self.shown_flag.store(shown, std::sync::atomic::Ordering::Relaxed);
        // 選了攝影機時，主畫面看得到（沒有開著剪輯、檢視、製作、全部錄影）就先顯示攝影機小視窗
        let covered = self.editor.is_some() || self.viewer.is_some() || self.export_dlg.is_some() || self.library.is_some() || self.jf_browse.is_some();
        self.core.set_camera_preview((shown && !covered && self.env_ready).then(|| self.s.record_config(&self.env)));
        let busy = self.status.recorder.state != RecorderState::Idle
            || self.editor.is_some()
            || self.viewer.is_some()
            || self.export_dlg.is_some()
            || self.library.is_some()
            || self.jf_browse.is_some()
            || self.status.export.as_ref().is_some_and(|e| e.state == screenrecorder_core::types::ExportState::Running)
            || matches!(self.status.download.phase, DownloadPhase::Downloading | DownloadPhase::Verifying | DownloadPhase::Extracting)
            || self.status.install.is_some();
        let mut next = match (shown, busy) {
            (false, _) => Duration::from_secs(2),
            (true, true) => Duration::from_millis(250),
            (true, false) => Duration::from_secs(5),
        };
        // 等著到時間要做的事（設定存檔、儲存位置改了重新讀取、ddagrab 測試中）：每一畫格都重算，
        // 中途因滑鼠等輸入多畫了一格也不會錯過
        let now = Instant::now();
        let dir_reload = self.main.dir_changed_at.map(|t| t + Duration::from_millis(610));
        for t in [self.save_at, self.ddagrab_watch, dir_reload].into_iter().flatten() {
            next = next.min(t.saturating_duration_since(now));
        }
        ctx.request_repaint_after(next);
    }

    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        dev::input(self, ctx, raw);
        if std::mem::take(&mut self.release_pointer) {
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: ctx.input(|i| i.modifiers) });
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // 視窗縮到最小時不畫影片（mpv）：睡眠、顯示卡重設後繪圖環境可能已失效，畫上去會讓程式整個當掉
        if !ui.ctx().input(|i| i.viewport().minimized == Some(true)) {
            viewer::render(self, frame);
            inline_play::render(self, frame);
        }
        // 關到系統匣（視窗隱藏）時什麼都不畫：eframe 只看「縮小 / 被蓋住」判斷看不看得到，
        // 隱藏的視窗仍會一直呼叫這裡；畫主畫面會重新開始即時預覽，背景就一直有 FFmpeg 在擷取桌面
        if !self.visible {
            return;
        }
        let ctx = ui.ctx().clone();
        let mut t = Instant::now();
        let mut part = |app: &mut UiApp, name: &'static str| {
            app.frame_parts.push((name, t.elapsed().as_secs_f32() * 1000.0));
            t = Instant::now();
        };
        // 教學顯示中按 Esc 只結束教學（要在各視窗檢查 Esc 之前）
        tour::esc(self, &ctx);
        if self.snip.is_some() {
            snip::show(self, &ctx);
            part(self, "框選截圖");
        }
        main_view::show(self, ui);
        part(self, "主畫面");
        if self.notes.is_some() {
            notes::show(self, &ctx);
            part(self, "講稿");
        }
        if self.schedule_dlg.is_some() {
            schedule::show(self, &ctx);
        }
        if self.export_dlg.is_some() {
            export_dialog::show(self, &ctx);
            part(self, "製作視窗");
        }
        if self.library.is_some() {
            library_dialog::show(self, &ctx);
            part(self, "全部錄影");
        }
        if self.jf_browse.is_some() {
            jellyfin_browse::show(self, &ctx);
        }
        inline_play::tick(self, &ctx);
        if self.viewer.is_some() {
            viewer::show(self, &ctx);
            part(self, "檢視器");
        }
        if self.editor.is_some() {
            editor::show(self, &ctx);
            part(self, "剪輯視窗");
        }
        if self.ocr.is_some() {
            ocr::show(self, &ctx);
        }
        if self.steps_review.is_some() {
            steps_review::show(self, &ctx);
        }
        if self.video_tags.is_some() {
            video_tags::show(self, &ctx);
        }
        library_send::transfer_bar(self, &ctx);
        // 設定視窗在其他視窗之後畫：從剪輯視窗、全部錄影開啟時（例如「前往外部元件」）在最上面
        if self.settings_dlg.is_some() {
            settings_dialog::show(self, &ctx);
            settings_dialog::check_key_capture(self);
        }
        // 新功能介紹：主畫面第一次看得到、沒有其他視窗時檢查一次
        if !self.whats_new_checked && self.visible && self.env_ready && self.editor.is_none() && self.snip.is_none() {
            self.whats_new_checked = true;
            let dev = std::env::var("SCREENRECORDER_DEV").is_ok();
            // 第一次安裝：一步一步的教學（不顯示新功能介紹）；上次教學看到一半：問要不要接著看
            if !dev && tour::on_launch(self, self.s.seen_version.is_empty()) {
                self.s.seen_version = whats_new::minor_version();
                self.save_settings();
            } else {
                self.whats_new_open = whats_new::should_show(self) && !dev;
            }
        }
        if self.whats_new_open {
            whats_new::show(self, &ctx);
        }
        // 截圖改成和錄影分開存：第一次啟動時問一次要不要把錄影資料夾裡的截圖搬到截圖資料夾（新功能介紹關掉之後）
        if !self.shot_move_checked && self.whats_new_checked && !self.whats_new_open && self.tour.is_none() && self.ask.is_none() && self.editor.is_none() && self.snip.is_none() {
            self.shot_move_checked = true;
            let dev = std::env::var("SCREENRECORDER_DEV").is_ok_and(|v| !v.contains("shotmove"));
            if !self.s.shot_move_asked && !dev {
                let from = self.s.out_dir(&self.env);
                settings_dialog::offer_shot_move(self, from, true);
            }
        }
        if self.changelog_open {
            dialogs::changelog(self, &ctx);
        }
        if self.send_dlg.is_some() {
            library_send::show(self, &ctx);
        }
        if self.ask.is_some() {
            dialogs::show_ask(self, &ctx);
        }
        import::handle_drop(self, &ctx);
        tour::show(self, &ctx);
        dialogs::show_toast(self, &ctx);
        main_view::countdown_overlay(self, &ctx);
        #[cfg(debug_assertions)]
        dev::tick(self, &ctx);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

/// 每隔 every 讀一次 poll()，和上一次不同就呼叫 wake()。shown 為 false 時不讀
/// （看不到時不比對；再顯示時一定會重畫，從那時重新比）
async fn watch_changes<T: PartialEq>(every: Duration, shown: Arc<std::sync::atomic::AtomicBool>, poll: impl Fn() -> T, wake: impl Fn()) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last: Option<T> = None;
    loop {
        tick.tick().await;
        if !shown.load(std::sync::atomic::Ordering::Relaxed) {
            last = None;
            continue;
        }
        let now = poll();
        if last.as_ref().is_some_and(|l| *l != now) {
            wake();
        }
        last = Some(now);
    }
}

/// 移到資源回收筒（confirm_trash 確認後）：copies 是媒體庫裡也要刪的那一份；notify：刪好後請 Jellyfin 重新掃描
fn trash_now(app: &mut UiApp, paths: Vec<String>, copies: Vec<std::path::PathBuf>, notify: bool, done: fn(&mut UiApp, &[String], bool)) {
    // 媒體庫的檔案可能跟著刪：「已在媒體庫」的標註重讀
    app.lib_index = None;
    app.ask = None;
    // 先停止播放（Windows 上檔案使用中無法刪除）
    if app.inline.as_ref().is_some_and(|i| paths.iter().any(|p| i.is(p))) {
        app.inline = None;
    }
    if let Some(v) = app.viewer.as_mut().filter(|v| paths.contains(&v.cur().media.path)) {
        v.clear_media();
    }
    match actions::delete(&app.core, &paths) {
        Ok(n) => {
            // 刪掉的就不用再補傳
            for p in &paths {
                screenrecorder_core::transfer::remove_pending(p);
            }
            app.toast(trf!("已將 {n} 個檔案移到資源回收筒", "Moved {n} file(s) to the Recycle Bin"), false);
            if notify {
                let jf = app.jellyfin();
                let first = paths.first().map(std::path::PathBuf::from);
                app.spawn(
                    async move {
                        tokio::task::spawn_blocking(move || -> Result<String, String> {
                            let gone = if copies.is_empty() {
                                ""
                            } else {
                                let list: Vec<String> = copies.iter().map(|c| c.display().to_string()).collect();
                                screenrecorder_core::recycle::move_to_recycle_bin(&list).map_err(|e| e.message().to_string())?;
                                screenrecorder_core::info!("[傳到資料夾] 媒體庫的那一份也刪了：{}", list.join("、"));
                                tr!("媒體庫裡的那一份也刪了", "The copy in the library was deleted too")
                            };
                            // 只掃描刪掉的那個資料夾
                            let scan = copies.first().cloned().or(first);
                            Ok(match jf.zip(scan).map(|(jf, at)| jf.scan(&at)) {
                                Some(Ok(_)) if gone.is_empty() => tr!("已請 Jellyfin 重新掃描", "Jellyfin was asked to rescan").to_string(),
                                Some(Ok(_)) => format!("{gone}{}", tr!("，已請 Jellyfin 重新掃描", "; Jellyfin was asked to rescan")),
                                Some(Err(e)) => format!("{gone}{}", trf!("（沒有通知到 Jellyfin：{e}）", " (Jellyfin couldn't be notified: {e})")),
                                None => gone.to_string(),
                            })
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    |app, r| match r {
                        Ok(msg) if msg.is_empty() => {}
                        Ok(msg) => app.toast(msg, false),
                        Err(e) => app.toast(trf!("媒體庫裡的那一份沒有刪掉：{e}", "The copy in the library wasn't deleted: {e}"), true),
                    },
                );
            }
            done(app, &paths, true);
        }
        Err(e) => {
            app.toast(e.message().to_string(), true);
            done(app, &paths, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    #[tokio::test(start_paused = true)]
    async fn wakes_only_on_change() {
        let value = Arc::new(AtomicU32::new(0));
        let wakes = Arc::new(AtomicU32::new(0));
        let shown = Arc::new(AtomicBool::new(true));
        let (v, w) = (value.clone(), wakes.clone());
        let task = tokio::spawn(watch_changes(
            Duration::from_millis(250),
            shown.clone(),
            move || v.load(Ordering::SeqCst),
            move || {
                w.fetch_add(1, Ordering::SeqCst);
            },
        ));
        let step = |ms| tokio::time::sleep(Duration::from_millis(ms));
        // 沒有變化：不叫醒
        step(2000).await;
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
        // 變了一次：叫醒一次
        value.store(1, Ordering::SeqCst);
        step(1000).await;
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        // 看不到時變化：不叫醒；再看得到之後也不會因為先前的變化叫醒
        shown.store(false, Ordering::SeqCst);
        value.store(2, Ordering::SeqCst);
        step(1000).await;
        shown.store(true, Ordering::SeqCst);
        step(1000).await;
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        // 之後的變化照常
        value.store(3, Ordering::SeqCst);
        step(300).await;
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        task.abort();
    }

    /// 操作視窗還沒建立時，主執行緒等開啟視窗的要求：沒有要求時等到逾時，有要求時（包括之前就送來的）馬上回來
    #[test]
    fn open_requests_wake_the_waiting_thread() {
        let reqs = OpenRequests::default();
        assert!(!reqs.wait(Duration::from_millis(50)));
        let r2 = reqs.clone();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            r2.push(UiPage::Main);
        });
        let t = Instant::now();
        assert!(reqs.wait(Duration::from_secs(10)));
        assert!(t.elapsed() < Duration::from_secs(5));
        sender.join().unwrap();
        // 已經有要求：不用等
        assert!(reqs.wait(Duration::from_secs(10)));
        assert_eq!(reqs.take(), vec![UiPage::Main]);
    }
}
