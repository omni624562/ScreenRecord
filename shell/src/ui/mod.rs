//! 操作視窗（egui）：主畫面、製作加速版 / GIF、全部錄影、剪輯、更新說明。
//! 直接呼叫後端（screenrecorder-core），非同步的工作在 tokio 上執行，完成後把結果交回介面執行緒。

#[cfg(debug_assertions)]
mod dev;
pub mod dialogs;
pub mod editor;
pub mod export_dialog;
pub mod library_dialog;
pub mod main_view;
pub mod ocr;
pub mod preview;
pub mod settings;
pub mod settings_dialog;
pub mod snip;
pub mod theme;
pub mod thumbs;
pub mod viewer;

use eframe::egui;
use screenrecorder_core::actions::{self, UpdateState};
use screenrecorder_core::app::{App, Status, UiPage};
use screenrecorder_core::types::{EnvInfo, LibraryEntry, LibraryPage, RecorderState};
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
pub struct OpenRequests(Arc<Mutex<Vec<UiPage>>>);

impl OpenRequests {
    pub fn push(&self, page: UiPage) {
        self.0.lock().unwrap().push(page);
    }
    fn take(&self) -> Vec<UiPage> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// 開檔類型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAction {
    /// 在內建檢視器開啟
    Play,
    /// 用 Windows 預設的程式開啟
    External,
    Reveal,
    Edit,
    Export,
    /// 把檔案複製到剪貼簿（貼到 LINE、Teams、資料夾）
    CopyFile,
}

pub struct UiApp {
    pub core: Arc<App>,
    pub rt: tokio::runtime::Handle,
    pub ctx: egui::Context,
    tx: Sender<Job>,
    rx: Receiver<Job>,
    open_requests: OpenRequests,
    /// 系統匣可以用：關掉視窗時只是隱藏
    pub tray_ok: bool,
    visible: bool,

    pub env: EnvInfo,
    pub env_ready: bool,
    pub s: UiSettings,
    settings_rev: u64,
    save_at: Option<Instant>,
    pub status: Status,
    pub status_at: Instant,
    pub update: UpdateState,

    pub toast: Option<Toast>,
    pub ask: Option<dialogs::Ask>,
    pub changelog_open: bool,

    pub preview: preview::Preview,
    pub thumbs: thumbs::Thumbs,
    pub recent: Option<LibraryPage>,
    pub recent_page: usize,
    recent_seq: u64,
    pub recent_per_page: usize,
    /// 已知的錄影（最近錄影、全部錄影），操作按鈕用完整資訊
    pub known: HashMap<String, LibraryEntry>,

    pub export_dlg: Option<export_dialog::ExportDialog>,
    pub library: Option<library_dialog::LibraryDialog>,
    pub viewer: Option<viewer::Viewer>,
    /// 文字辨識的結果
    pub ocr: Option<ocr::Ocr>,
    pub settings_dlg: Option<settings_dialog::SettingsDialog>,
    /// 在螢幕上框選截圖
    pub snip: Option<snip::Snip>,
    /// 目前設定的全域快捷鍵（顯示用）
    pub keys: screenrecorder_core::types::Hotkeys,
    pub editor: Option<editor::Editor>,

    pub dismissed_job: Option<u64>,
    last_export_state: Option<screenrecorder_core::types::ExportState>,
    last_result_path: Option<String>,
    last_state: RecorderState,
    pub main: main_view::MainState,
    ddagrab_watch: Option<Instant>,
    /// 已處理過的截圖（介面或快捷鍵截好時更新清單、顯示提示）
    last_shot_seq: u64,
    /// 上一格各部分花的時間（毫秒）：畫面處理太慢時寫進記錄檔，找出卡在哪裡
    frame_parts: Vec<(&'static str, f32)>,
    slow_logged: Option<Instant>,
    #[cfg(debug_assertions)]
    dev: dev::Dev,
}

impl UiApp {
    pub fn new(cc: &eframe::CreationContext, core: Arc<App>, rt: tokio::runtime::Handle, open_requests: OpenRequests, tray_ok: bool, visible: bool) -> UiApp {
        let ctx = cc.egui_ctx.clone();
        theme::setup_fonts(&ctx);
        theme::setup_visuals(&ctx);
        let (tx, rx) = channel();
        // 第一次偵測（FFmpeg、螢幕、音訊裝置）在背景進行：先用目前知道的資訊畫出視窗
        let env = core.env();
        let saved = core.settings.load();
        let s = UiSettings::from_saved(saved.ui.as_ref(), &env);
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
            env,
            env_ready: false,
            s,
            settings_rev: saved.rev,
            save_at: None,
            last_state: status.recorder.state,
            status,
            status_at: Instant::now(),
            toast: None,
            ask: None,
            changelog_open: false,
            preview: preview::Preview::default(),
            thumbs: thumbs::Thumbs::default(),
            recent: None,
            recent_page: 1,
            recent_seq: 0,
            recent_per_page: 4,
            known: HashMap::new(),
            export_dlg: None,
            library: None,
            viewer: None,
            ocr: None,
            settings_dlg: None,
            snip: None,
            keys: saved.hotkeys.unwrap_or_default(),
            editor: None,
            dismissed_job: None,
            last_export_state: None,
            last_result_path: None,
            main: main_view::MainState::default(),
            ddagrab_watch: None,
            last_shot_seq: 0,
            frame_parts: Vec::new(),
            slow_logged: None,
            #[cfg(debug_assertions)]
            dev: Default::default(),
        };
        let core = app.core.clone();
        app.spawn(async move { core.wait_ready().await }, |app, _| {
            app.env = app.core.env();
            app.env_ready = true;
            let saved = app.core.settings.load();
            app.s = UiSettings::from_saved(saved.ui.as_ref(), &app.env);
            app.settings_rev = saved.rev;
            app.preview.reset();
            app.watch_ddagrab();
            app.load_recent();
        });
        app
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

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        let secs = if error { 6 } else { 3 };
        self.toast = Some(Toast { text, error, until: Instant::now() + Duration::from_secs(secs) });
        self.ctx.request_repaint();
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
    pub fn save_settings(&mut self) {
        self.save_at = Some(Instant::now() + Duration::from_millis(300));
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

    fn poll_status(&mut self) {
        let prev_update = self.status.update.clone();
        let prev_install = self.status.install.clone();
        self.status = self.core.status();
        self.status_at = Instant::now();
        if self.status.update != prev_update {
            self.update = actions::update_state(&self.core);
            self.env.update = self.update.update.clone();
        }
        if let Some(i) = &self.status.install {
            if i.phase == screenrecorder_core::selfupdate::InstallPhase::Error && prev_install.as_ref().map(|p| (&p.phase, &p.message)) != Some((&i.phase, &i.message)) {
                let msg = format!("更新失敗：{}", i.message.clone().unwrap_or_default());
                self.toast(msg, true);
            }
        }
        // FFmpeg 下載完成：重新偵測
        if self.status.download.phase == screenrecorder_core::types::DownloadPhase::Done && !self.env.ffmpeg.found {
            self.env = self.core.env();
            if self.env.ffmpeg.found {
                let msg = self.status.download.message.clone().unwrap_or_else(|| "FFmpeg 已安裝".into());
                self.toast(msg, false);
                self.preview.reset();
                self.watch_ddagrab();
                self.load_recent();
            }
        }
        self.reload_settings_if_changed();
        if let Some(shot) = self.status.shot.clone().filter(|s| s.seq != self.last_shot_seq) {
            self.last_shot_seq = shot.seq;
            let name = dialogs::file_name(&shot.path);
            self.toast(if shot.copied { format!("已截圖並複製到剪貼簿：{name}") } else { format!("已截圖：{name}") }, false);
            self.shots_changed();
            // 設定「截圖後直接編輯」：開啟操作視窗與編輯（其他視窗開著時不打斷）
            if self.s.edit_after_shot && self.editor.is_none() && self.export_dlg.is_none() {
                self.core.open_ui(screenrecorder_core::app::UiPage::Main);
                editor::shot::open(self, shot.path.clone());
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
                if let Some(e) = &self.status.export {
                    if e.state == screenrecorder_core::types::ExportState::Done {
                        let what = match e.kind {
                            screenrecorder_core::types::ExportKind::Cut => "剪輯完成",
                            screenrecorder_core::types::ExportKind::Gif => "GIF 製作完成",
                            screenrecorder_core::types::ExportKind::Speed => "加速版製作完成",
                        };
                        self.toast(what, false);
                    }
                }
            }
        }
        // ddagrab / 硬體編碼器測試中：每秒更新環境
        if self.ddagrab_watch.is_some_and(|t| Instant::now() >= t) {
            self.env = self.core.env();
            let pending = (self.env.ffmpeg.has_ddagrab && self.env.ffmpeg.ddagrab_works.is_none()) || (self.env.ffmpeg.found && self.env.ffmpeg.hw_encoders.is_none());
            self.ddagrab_watch = pending.then(|| Instant::now() + Duration::from_secs(1));
        }
    }

    pub fn watch_ddagrab(&mut self) {
        self.ddagrab_watch = Some(Instant::now() + Duration::from_secs(1));
    }

    /// 重新偵測環境（FFmpeg、螢幕、音訊裝置）
    pub fn refresh_env(&mut self, then: impl FnOnce(&mut UiApp) + Send + 'static) {
        let core = self.core.clone();
        self.spawn(async move { core.refresh(false).await }, move |app, env| {
            app.env = env;
            app.s.fix_monitor(&app.env);
            app.preview.reset();
            app.watch_ddagrab();
            app.load_recent();
            then(app);
        });
    }

    // ───────────── 最近錄影 ─────────────

    pub fn load_recent(&mut self) {
        self.recent_seq += 1;
        let seq = self.recent_seq;
        let core = self.core.clone();
        let dir = self.s.out_dir(&self.env);
        let query = screenrecorder_core::types::LibraryQuery { page: Some(self.recent_page as f64), page_size: Some(self.recent_per_page as f64), ..Default::default() };
        self.spawn(async move { actions::library(&core, &dir, &query).await }, move |app, page| {
            if seq != app.recent_seq {
                return; // 較舊的請求晚回來：丟掉
            }
            app.recent_page = page.page.max(1);
            for e in &page.items {
                app.known.insert(e.media.path.clone(), e.clone());
            }
            app.recent = Some(page);
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
        match action {
            EntryAction::Play => viewer::open(self, entry),
            EntryAction::CopyFile => match screenrecorder_core::clipboard::copy_files(std::slice::from_ref(&path)) {
                Ok(()) => self.toast("已複製檔案，可以直接貼到 LINE、Teams、信件或資料夾", false),
                Err(e) => self.toast(e, true),
            },
            EntryAction::External | EntryAction::Reveal => {
                if action == EntryAction::External {
                    self.toast("正在以 Windows 預設的程式開啟…", false);
                }
                let a = if action == EntryAction::External { OpenAction::Play } else { OpenAction::Reveal };
                if let Err(e) = actions::open(a, &path) {
                    self.toast(e.message().to_string(), true);
                }
            }
            EntryAction::Edit if dialogs::is_image(&path) => editor::shot::open(self, path),
            EntryAction::Edit | EntryAction::Export => {
                let what = if action == EntryAction::Edit { "剪輯" } else { "製作加速版 / GIF" };
                if self.locked() {
                    return self.toast(format!("錄影中無法{what}，請先停止錄影"), true);
                }
                if self.exporting() {
                    return self.toast("目前有轉檔工作進行中，請等它完成", true);
                }
                if entry.media.duration_sec.unwrap_or(0.0) <= 0.0 {
                    return self.toast("無法讀取影片長度", true);
                }
                if action == EntryAction::Edit {
                    editor::open(self, entry);
                } else {
                    self.export_dlg = Some(export_dialog::ExportDialog::new(entry));
                }
            }
        }
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
                Some(m) => app.act(action, LibraryEntry { media: m, exports: vec![] }),
                None => app.toast("清單中找不到這個檔案", true),
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if !self.visible {
                self.visible = true;
                self.preview.reset();
                self.load_recent();
            }
            if page == UiPage::Changelog {
                self.changelog_open = true;
            }
            if page == UiPage::EditShot {
                match self.core.last_shot() {
                    Some(s) if self.editor.is_none() => editor::shot::open(self, s.path),
                    Some(_) => self.toast("編輯視窗已經開著，請先關閉", true),
                    None => self.toast("還沒有截圖", true),
                }
            }
        }
    }

    /// 關掉視窗：系統匣可以用時只是隱藏（程式繼續常駐）
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.tray_ok {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.visible = false;
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
        self.flush_settings();
        self.poll_status();
        self.handle_close(ctx);
        self.frame_parts.push(("背景狀態", t0.elapsed().as_secs_f32() * 1000.0));
        // 狀態每 0.25 秒更新一次（錄影中計時器、轉檔進度）；視窗隱藏時放慢
        ctx.request_repaint_after(if self.visible { Duration::from_millis(250) } else { Duration::from_secs(2) });
    }

    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        dev::input(self, ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut t = Instant::now();
        let mut part = |app: &mut UiApp, name: &'static str| {
            app.frame_parts.push((name, t.elapsed().as_secs_f32() * 1000.0));
            t = Instant::now();
        };
        if self.snip.is_some() {
            snip::show(self, &ctx);
            part(self, "框選截圖");
        }
        main_view::show(self, ui);
        part(self, "主畫面");
        if self.export_dlg.is_some() {
            export_dialog::show(self, &ctx);
            part(self, "製作視窗");
        }
        if self.library.is_some() {
            library_dialog::show(self, &ctx);
            part(self, "全部錄影");
        }
        if self.viewer.is_some() {
            viewer::show(self, &ctx);
            part(self, "檢視器");
        }
        if self.settings_dlg.is_some() {
            settings_dialog::show(self, &ctx);
            settings_dialog::check_key_capture(self);
        }
        if self.editor.is_some() {
            editor::show(self, &ctx);
            part(self, "剪輯視窗");
        }
        if self.ocr.is_some() {
            ocr::show(self, &ctx);
        }
        if self.changelog_open {
            dialogs::changelog(self, &ctx);
        }
        if self.ask.is_some() {
            dialogs::show_ask(self, &ctx);
        }
        dialogs::show_toast(self, &ctx);
        main_view::countdown_overlay(self, &ctx);
        #[cfg(debug_assertions)]
        dev::tick(self, &ctx);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}
