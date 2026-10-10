//! 系統匣控制：推送狀態給系統匣圖示、執行選單與快速鍵指令、錄影完成時顯示通知。
//! 圖示與選單本身在 tray_win.rs（獨立執行緒；選單開著時會卡住所在的執行緒）。

use crate::app::{App, UiPage};
use crate::format::{clock, video_clock};
use crate::info;
use crate::paths::now_ms;
use crate::settings::SettingsPatch;
use crate::types::{AudioConfig, HotkeyStatus, Hotkeys, MethodPreference, RecordConfig, RecorderState, SourceConfig};
use crate::version::APP_VERSION;
use crate::{tr, trf};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    StartLast,
    StartMonitor(String),
    StartAll,
    /// 在螢幕上框選範圍或點選視窗後開始錄影
    StartSelect,
    /// 錄上次框選的範圍
    StartLastSnip,
    Pause,
    Resume,
    Stop,
    ToggleSystem,
    ToggleMic,
    OpenFolder,
    PlayLast,
    Autostart,
    Changelog,
    HotkeyRecord,
    HotkeyPause,
    /// 截圖：與錄影相同的範圍（選單與快速鍵）
    Screenshot,
    /// 截圖：整個指定的螢幕
    ScreenshotMonitor(String),
    /// 截圖：所有螢幕
    ScreenshotAll,
    /// 在螢幕上框選範圍或點選視窗截圖（選單與快速鍵）
    ScreenshotSelect,
    /// 再截一次上次框選的範圍
    ScreenshotLast,
    /// 幾秒後再框選（先打開要截的選單、提示）
    ScreenshotDelay(u64),
    /// 長截圖（框選後自動往下捲，接成長圖）
    ScreenshotScroll,
    /// 讀取畫面上的 QR 碼
    ScreenshotQr,
    /// 取色器（點一下複製色碼）
    ScreenColor,
    /// 尺規（量畫面上的距離）
    ScreenRuler,
    /// 開始步驟截圖（每點一下截一張）
    StepsStart,
    /// 完成步驟截圖（做成教學文件）
    StepsFinish,
    /// 編輯最近的截圖
    EditLastShot,
    /// 點了通知：剛截圖的通知開啟編輯，其他開啟操作視窗
    BalloonClick,
    /// 錄影中加標記
    Mark,
    /// 螢幕畫筆（開 / 關）
    Pen,
    OpenUpdate,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayMonitor {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayState {
    pub rec: RecorderState,
    /// 滑鼠停在圖示上的提示文字（最多 127 字）
    pub tip: String,
    /// 「開始錄影」旁顯示的上次範圍，例如「螢幕 1」
    pub last_source: String,
    pub monitors: Vec<TrayMonitor>,
    pub audio_system: bool,
    pub audio_mic: bool,
    /// 是否能開始錄影（有 FFmpeg、沒有轉檔工作）
    pub can_record: bool,
    /// 是否能截圖（有 FFmpeg）
    pub can_shot: bool,
    /// 有上次框選的範圍（「重複上次框選」）
    pub has_last_snip: bool,
    /// 有截過圖（「編輯上次截圖」）
    pub has_shot: bool,
    /// 步驟截圖進行中：已經截了幾步
    pub steps: Option<u32>,
    pub last_result: Option<String>,
    /// None = 無法設定（開發版）
    pub autostart: Option<bool>,
    pub version: String,
    /// 有新版本時顯示在選單
    pub update: Option<String>,
    /// 快速鍵名稱（選單右側顯示；停用時是空字串）：錄影、暫停、截圖、框選截圖
    pub keys: [String; crate::types::HOTKEY_COUNT],
}

/// 系統匣圖示（Windows 實作在 tray_win.rs）
pub trait TrayUi: Send + Sync {
    fn set_state(&self, s: TrayState);
    fn balloon(&self, title: &str, text: &str, warn: bool);
    /// 結束前移除圖示（否則會殘留到滑鼠移過去才消失）
    fn dispose(&self);
    /// 重新登記全域快速鍵
    fn set_hotkeys(&self, k: &Hotkeys) -> Option<HotkeyStatus>;
}

fn state_text(s: RecorderState) -> &'static str {
    match s {
        RecorderState::Idle => tr!("待命", "Ready"),
        RecorderState::Countdown => tr!("倒數中", "Counting down"),
        RecorderState::Recording => tr!("錄影中", "Recording"),
        RecorderState::Paused => tr!("已暫停", "Paused"),
        RecorderState::Stopping => tr!("儲存中", "Saving…"),
    }
}

#[derive(Default)]
struct Ctl {
    last: Option<TrayState>,
    autostart: Option<bool>,
    last_result_path: Option<String>,
    last_result_msg: Option<String>,
    exists_at: u64,
    exists_cache: bool,
    /// 最近的截圖還在不在：(路徑, 查的時間, 結果)
    shot_exists: Option<(String, u64, bool)>,
    /// 最後一個通知是截圖的（點通知開啟編輯）
    balloon_shot: bool,
}

pub struct TrayController {
    app: Arc<App>,
    ui: Arc<dyn TrayUi>,
    ctl: Mutex<Ctl>,
}

impl TrayController {
    pub fn new(app: Arc<App>, ui: Arc<dyn TrayUi>, autostart: Option<bool>) -> Arc<TrayController> {
        Arc::new(TrayController { app, ui, ctl: Mutex::new(Ctl { autostart, ..Default::default() }) })
    }

    pub fn notify(&self, title: &str, text: &str, warn: bool) {
        self.ctl.lock().unwrap().balloon_shot = false;
        self.ui.balloon(title, text, warn);
    }

    /// 每秒：推送狀態；錄影結束時通知（點通知會開啟操作視窗）
    pub fn tick(&self) {
        self.push(false);
        let Some(res) = self.app.recorder.status().result else { return };
        let mut c = self.ctl.lock().unwrap();
        let key = res.path.clone().unwrap_or_else(|| res.message.clone());
        let last = c.last_result_path.clone().or_else(|| c.last_result_msg.clone());
        if Some(&key) == last.as_ref() {
            return;
        }
        c.last_result_path = res.path.clone();
        c.last_result_msg = Some(res.message.clone());
        c.exists_at = 0;
        drop(c);
        match (&res.path, res.ok) {
            (Some(p), true) => {
                let name = Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notify(tr!("錄影已儲存", "Recording saved"), &trf!("{name}（{}）", "{name} ({})", video_clock(res.video_sec)), false);
            }
            _ => self.notify(tr!("錄影未完成", "Recording not completed"), &res.message, true),
        }
    }

    pub fn state(&self) -> TrayState {
        let st = self.app.recorder.status();
        let env = self.app.env();
        let cfg = self.config();
        let time = match st.state {
            RecorderState::Countdown => format!(" {}", (st.countdown_ms.unwrap_or(0) as f64 / 1000.0).ceil()),
            RecorderState::Idle => String::new(),
            _ => format!(" {}", clock(st.recorded_ms as f64)),
        };
        let last_source = match &cfg.source {
            _ if cfg.audio_only => tr!("只錄聲音", "Audio only").to_string(),
            SourceConfig::All => tr!("所有螢幕", "All screens").to_string(),
            SourceConfig::Region { width, height, .. } => trf!("範圍 {width}×{height}", "Area {width}×{height}"),
            SourceConfig::Monitor { monitor_id } => trf!("螢幕 {}", "Screen {}", env.monitors.iter().find(|m| &m.id == monitor_id).map(|m| m.display_number).unwrap_or(1)),
        };
        let last_result = self.last_result_exists();
        let has_shot = self.app.last_shot().is_some_and(|s| self.shot_exists(&s.path));
        // 檔案、設定都先查好再鎖：鎖住時不讀磁碟（網路磁碟可能卡住好幾秒，其他地方會跟著等）
        let (can_record, can_shot, has_last_snip, steps, update, keys) = (
            self.app.ffmpeg_path().is_some() && !self.app.exporter.running(),
            self.app.ffmpeg_path().is_some(),
            self.app.last_snip().is_some(),
            self.app.steps_count(),
            self.app.update().map(|u| u.version),
            {
                let k = self.app.hotkeys();
                std::array::from_fn(|i| k.label(i))
            },
        );
        let c = self.ctl.lock().unwrap();
        TrayState {
            rec: st.state,
            tip: trf!("螢幕錄影 {APP_VERSION} — {}{time}", "Screen Recorder {APP_VERSION} — {}{time}", state_text(st.state)),
            last_source,
            monitors: env
                .monitors
                .iter()
                .map(|m| TrayMonitor {
                    id: m.id.clone(),
                    label: trf!("螢幕 {}{}　{}×{}", "Screen {}{}   {}×{}", m.display_number, if m.primary { tr!("（主螢幕）", " (primary)") } else { "" }, m.width, m.height),
                })
                .collect(),
            audio_system: cfg.audio.system,
            audio_mic: cfg.audio.mic,
            can_record,
            can_shot,
            has_last_snip,
            has_shot,
            steps,
            last_result: if last_result { c.last_result_path.clone() } else { None },
            autostart: c.autostart,
            version: APP_VERSION.to_string(),
            update,
            keys,
        }
    }

    /// 選單「播放最近的錄影」是否可用：每 10 秒才查一次檔案（網路磁碟上可能卡住數秒；查的時候不鎖）
    fn last_result_exists(&self) -> bool {
        let p = {
            let c = self.ctl.lock().unwrap();
            let Some(p) = c.last_result_path.clone() else { return false };
            if now_ms().saturating_sub(c.exists_at) <= 10_000 {
                return c.exists_cache;
            }
            p
        };
        let ok = Path::new(&p).exists();
        let mut c = self.ctl.lock().unwrap();
        c.exists_at = now_ms();
        c.exists_cache = ok;
        ok
    }

    /// 最近的截圖還在不在：同一個檔案每 10 秒才查一次（換了截圖立刻查）
    fn shot_exists(&self, path: &str) -> bool {
        if let Some((p, at, ok)) = &self.ctl.lock().unwrap().shot_exists {
            if p == path && now_ms().saturating_sub(*at) <= 10_000 {
                return *ok;
            }
        }
        let ok = Path::new(path).is_file();
        self.ctl.lock().unwrap().shot_exists = Some((path.to_string(), now_ms(), ok));
        ok
    }

    pub fn push(&self, force: bool) {
        let s = self.state();
        {
            let mut c = self.ctl.lock().unwrap();
            if !force && c.last.as_ref() == Some(&s) {
                return;
            }
            c.last = Some(s.clone());
        }
        self.ui.set_state(s);
    }

    /// 最後一次的錄影設定；沒有時用預設值（主螢幕、30 fps、100%、錄系統聲音與麥克風）
    fn config(&self) -> RecordConfig {
        if let Some(c) = self.app.settings.load().record_config() {
            return c;
        }
        let env = self.app.env();
        let primary = env.monitors.iter().find(|m| m.primary).or(env.monitors.first());
        RecordConfig {
            source: match primary {
                Some(m) => SourceConfig::Monitor { monitor_id: m.id.clone() },
                None => SourceConfig::All,
            },
            fps: 30.0,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: MethodPreference::Auto,
            output_dir: self.app.default_output_dir.clone(),
            // 與介面相同：預設錄系統聲音與麥克風
            audio: AudioConfig { system: true, mic: true, mic_id: String::new() },
            encoder: None,
            countdown_sec: None,
            hide_ui: None,
            show_clicks: false,
            show_keys: false,
            cursor_halo: false,
            hide_icons: false,
            follow_window: None,
            camera: None,
            audio_only: false,
        }
    }

    /// 改錄音設定時同步更新操作視窗的設定（看到 rev 變了會重新讀取）
    fn update_audio(&self, system: bool) {
        let mut cfg = self.config();
        let on = if system {
            cfg.audio.system = !cfg.audio.system;
            cfg.audio.system
        } else {
            cfg.audio.mic = !cfg.audio.mic;
            cfg.audio.mic
        };
        let mut ui = self.app.settings.load().ui.filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({}));
        ui[if system { "audioSystem" } else { "audioMic" }] = serde_json::Value::Bool(on);
        self.app.settings.save(SettingsPatch { config: serde_json::to_value(&cfg).ok(), ui: Some(ui), ..Default::default() });
    }

    pub async fn run(self: &Arc<Self>, cmd: TrayCommand) {
        // 剛啟動時（偵測還沒完成）按快速鍵：等偵測完成，才不會誤報「找不到 ffmpeg.exe」
        self.app.wait_ready().await;
        if let Err(e) = self.run_inner(cmd).await {
            self.notify(tr!("無法執行", "Couldn't run this"), &e, true);
        }
        self.push(false);
    }

    /// 截好了：顯示通知（存在哪裡、有沒有複製到剪貼簿）
    fn notify_shot(&self, shot: &crate::types::ShotInfo) {
        // 已經在右下角顯示小縮圖了：不用再跳通知
        if self.app.shot_preview_enabled() {
            return;
        }
        let name = std::path::Path::new(&shot.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = if shot.copied {
            trf!("已複製到剪貼簿，存成 {name}。點這裡編輯", "Copied to the clipboard and saved as {name}. Click here to edit.")
        } else {
            trf!("已存成 {name}。點這裡編輯", "Saved as {name}. Click here to edit.")
        };
        self.notify(tr!("已截圖", "Screenshot taken"), &text, false);
        self.ctl.lock().unwrap().balloon_shot = true;
    }

    async fn run_inner(self: &Arc<Self>, cmd: TrayCommand) -> Result<(), String> {
        let app = &self.app;
        let rec = &app.recorder;
        let err = |e: crate::Error| e.message().to_string();
        let mut cfg = match cmd {
            TrayCommand::Open => {
                app.open_ui(UiPage::Main);
                return Ok(());
            }
            TrayCommand::Changelog => {
                app.open_ui(UiPage::Changelog);
                return Ok(());
            }
            TrayCommand::EditLastShot => {
                app.open_ui(UiPage::EditShot);
                return Ok(());
            }
            TrayCommand::BalloonClick => {
                let shot = std::mem::take(&mut self.ctl.lock().unwrap().balloon_shot);
                app.open_ui(if shot { UiPage::EditShot } else { UiPage::Main });
                return Ok(());
            }
            TrayCommand::Quit => {
                let app = app.clone();
                tokio::spawn(async move { app.quit(0).await });
                return Ok(());
            }
            // 開啟操作視窗：右上角的「有新版本」可以直接更新
            TrayCommand::OpenUpdate => {
                app.open_ui(UiPage::Main);
                return Ok(());
            }
            // 快速鍵：同一組鍵依狀態切換（待命→開始、倒數→取消、錄影中→停止）
            TrayCommand::HotkeyRecord => {
                let st = rec.status().state;
                if st == RecorderState::Stopping {
                    return Ok(());
                }
                // 準備開始的期間（狀態仍是待命）再按一次也是取消
                if st != RecorderState::Idle || rec.starting() {
                    return rec.stop(None).await.map(|_| ()).map_err(err);
                }
                self.config()
            }
            TrayCommand::HotkeyPause => {
                return match rec.status().state {
                    RecorderState::Recording => rec.pause().await.map_err(err),
                    RecorderState::Paused => rec.resume().await.map_err(err),
                    _ => Ok(()),
                };
            }
            TrayCommand::Pause => return rec.pause().await.map_err(err),
            // 沒在錄影時按快速鍵：不提示
            TrayCommand::Pen => {
                #[cfg(windows)]
                crate::screen_pen_win::toggle();
                return Ok(());
            }
            TrayCommand::Mark => {
                let _ = rec.add_marker();
                return Ok(());
            }
            TrayCommand::Resume => return rec.resume().await.map_err(err),
            TrayCommand::Stop => return rec.stop(None).await.map(|_| ()).map_err(err),
            TrayCommand::ToggleSystem | TrayCommand::ToggleMic => {
                self.update_audio(cmd == TrayCommand::ToggleSystem);
                return Ok(());
            }
            TrayCommand::OpenFolder => {
                let dir = self.config().output_dir;
                let _ = std::fs::create_dir_all(&dir);
                crate::desktop::open_with_explorer(&dir, false);
                return Ok(());
            }
            TrayCommand::ScreenshotQr => {
                let cfg = self.config();
                let Some(list) = app.qr_snip(&cfg).await.map_err(err)? else { return Ok(()) };
                if list.is_empty() {
                    self.notify(tr!("讀取 QR 碼", "Read QR code"), tr!("沒有找到 QR 碼（框大一點再試試）", "No QR code found (try selecting a larger area)"), true);
                } else {
                    let text = list.join("\n");
                    crate::clipboard::copy_text(&text);
                    let short: String = text.chars().take(100).collect();
                    self.notify(tr!("QR 碼內容（已複製）", "QR code content (copied)"), &short, false);
                }
                return Ok(());
            }
            TrayCommand::ScreenColor | TrayCommand::ScreenRuler => {
                let cfg = self.config();
                if let Some(hex) = app.screen_tool(&cfg, cmd == TrayCommand::ScreenRuler).await.map_err(err)? {
                    self.notify(tr!("取色器", "Color picker"), &trf!("{hex}（已複製到剪貼簿）", "{hex} (copied to the clipboard)"), false);
                }
                return Ok(());
            }
            TrayCommand::StepsStart => {
                let dir = self.config().output_dir;
                app.steps_start(&dir).map_err(err)?;
                self.notify(
                    tr!("步驟截圖", "Step capture"),
                    tr!(
                        "開始了：之後每點一下滑鼠就截一張。做完後在系統匣選「完成步驟截圖」，就會做成教學文件",
                        "Started: each mouse click now takes a screenshot. When you're done, choose “Finish step capture” in the system tray to make a step-by-step guide."
                    ),
                    false,
                );
                return Ok(());
            }
            TrayCommand::StepsFinish => {
                match app.steps_finish().await.map_err(err)? {
                    Some(path) => {
                        crate::desktop::open_with_explorer(&path, false);
                        self.notify(
                            tr!("步驟截圖", "Step capture"),
                            tr!(
                                "教學文件做好了，已用瀏覽器開啟；文字可以直接修改，再列印成 PDF",
                                "The step-by-step guide is ready and open in your browser. You can edit the text, then print it to PDF."
                            ),
                            false,
                        );
                    }
                    None => self.notify(tr!("步驟截圖", "Step capture"), tr!("沒有截到任何步驟", "No steps were captured"), false),
                }
                return Ok(());
            }
            TrayCommand::ScreenshotScroll => {
                let cfg = self.config();
                let Some(shot) = app.long_shot(&cfg).await.map_err(err)? else { return Ok(()) };
                self.notify_shot(&shot);
                return Ok(());
            }
            TrayCommand::Screenshot
            | TrayCommand::ScreenshotMonitor(_)
            | TrayCommand::ScreenshotAll
            | TrayCommand::ScreenshotLast
            | TrayCommand::ScreenshotSelect
            | TrayCommand::ScreenshotDelay(_) => {
                let mut cfg = self.config();
                if let TrayCommand::ScreenshotDelay(sec) = cmd {
                    self.notify(
                        tr!("延遲截圖", "Delayed screenshot"),
                        &trf!(
                            "{sec} 秒後畫面會凍結讓你框選：現在先打開要截的選單或提示",
                            "The screen will freeze in {sec} s so you can select an area. Open the menu or tooltip you want to capture now."
                        ),
                        false,
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(sec)).await;
                }
                match cmd {
                    // 框選：取消時不顯示通知
                    TrayCommand::ScreenshotSelect | TrayCommand::ScreenshotDelay(_) => {
                        let Some(shot) = app.snip_begin(&cfg).await.map_err(err)? else { return Ok(()) };
                        self.notify_shot(&shot);
                        return Ok(());
                    }
                    TrayCommand::ScreenshotMonitor(id) => cfg.source = SourceConfig::Monitor { monitor_id: id },
                    TrayCommand::ScreenshotAll => cfg.source = SourceConfig::All,
                    TrayCommand::ScreenshotLast => {
                        let r = app.last_snip().ok_or(tr!("還沒有框選過範圍", "You haven't selected an area yet"))?;
                        cfg.source = SourceConfig::Region { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 };
                    }
                    _ => {}
                }
                let shot = app.screenshot(&cfg).await.map_err(err)?;
                self.notify_shot(&shot);
                return Ok(());
            }
            TrayCommand::PlayLast => {
                let p = self.ctl.lock().unwrap().last_result_path.clone();
                if let Some(p) = p {
                    crate::desktop::open_with_explorer(&p, false);
                }
                return Ok(());
            }
            TrayCommand::Autostart => {
                let cur = self.ctl.lock().unwrap().autostart;
                crate::desktop::set_autostart(cur != Some(true)).await.map_err(err)?;
                let now = crate::desktop::get_autostart().await;
                self.ctl.lock().unwrap().autostart = now;
                self.notify(
                    tr!("開機自動啟動", "Start with Windows"),
                    if now == Some(true) {
                        tr!("已開啟：登入 Windows 後會自動常駐在系統匣", "On: the app will start in the system tray when you sign in to Windows")
                    } else {
                        tr!("已關閉", "Off")
                    },
                    false,
                );
                return Ok(());
            }
            TrayCommand::StartLast | TrayCommand::StartAll | TrayCommand::StartMonitor(_) | TrayCommand::StartSelect | TrayCommand::StartLastSnip => self.config(),
        };
        // 開始錄影
        if app.exporter.running() {
            return Err(tr!("轉檔進行中，請等它完成再錄影", "A video is being processed. Wait for it to finish before recording.").into());
        }
        // 指定了要錄的範圍：錄畫面（不是只錄聲音）
        if cmd != TrayCommand::StartLast {
            cfg.audio_only = false;
        }
        match cmd {
            TrayCommand::StartAll => (cfg.source, cfg.follow_window) = (SourceConfig::All, None),
            TrayCommand::StartMonitor(id) => (cfg.source, cfg.follow_window) = (SourceConfig::Monitor { monitor_id: id }, None),
            TrayCommand::StartSelect | TrayCommand::StartLastSnip => {
                let r = if cmd == TrayCommand::StartSelect {
                    // 取消框選時不顯示通知
                    let Some(r) = app.select_record_region(&cfg).await.map_err(err)? else { return Ok(()) };
                    r
                } else {
                    app.last_snip().ok_or(tr!("還沒有框選過範圍", "You haven't selected an area yet"))?
                };
                if r.width < 16 || r.height < 16 {
                    return Err(trf!("範圍太小（{}×{}），寬高至少 16 像素", "The area is too small ({}×{}). It must be at least 16 pixels wide and high.", r.width, r.height));
                }
                cfg.source = SourceConfig::Region { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 };
                // 點一下選的是視窗：錄影範圍跟著那個視窗移動
                let near = |a: i32, b: i32| (a - b).abs() <= 2;
                cfg.follow_window =
                    crate::winui::app_windows().into_iter().find(|w| near(w.rect.x, r.x) && near(w.rect.y, r.y) && near(w.rect.width, r.width) && near(w.rect.height, r.height)).map(|w| {
                        crate::info!("[錄影] 只錄視窗「{}」（跟著視窗移動）", w.title);
                        w.id
                    });
            }
            _ => {}
        }
        rec.start(cfg).await.map_err(err)
    }
}

/// 建立系統匣圖示與全域快速鍵；失敗時回傳 false（程式改用「沒有開著的視窗就自動結束」）
pub async fn start(app: &Arc<App>) -> bool {
    #[cfg(windows)]
    {
        let autostart = crate::desktop::get_autostart().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TrayCommand>();
        let keys = app.hotkeys();
        let started = tokio::task::spawn_blocking(move || crate::tray_win::start(tx, keys)).await;
        let (ui, hotkeys) = match started {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                crate::warn!("系統匣無法啟動：{e}");
                return false;
            }
            Err(e) => {
                crate::warn!("系統匣無法啟動：{e}");
                return false;
            }
        };
        report_hotkeys(app, &keys, hotkeys);
        let u = ui.clone();
        app.set_hotkey_applier(Arc::new(move |k| u.set_hotkeys(k)));
        let ctl = TrayController::new(app.clone(), ui.clone(), autostart);
        let n = ctl.clone();
        app.set_notifier(Arc::new(move |title, text, warn| n.notify(title, text, warn)));
        let d = ui.clone();
        app.set_tray_dispose(Box::new(move || d.dispose()));
        ctl.push(true);
        let c = ctl.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let c = c.clone();
                tokio::spawn(async move { c.run(cmd).await });
            }
        });
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                iv.tick().await;
                ctl.tick();
            }
        });
        true
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        false
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn report_hotkeys(app: &App, k: &Hotkeys, h: HotkeyStatus) {
    let ok = h.all();
    let busy: Vec<String> = (0..ok.len()).filter(|i| !ok[*i]).map(|i| k.label(i)).collect();
    if busy.is_empty() {
        let list: Vec<String> = (0..ok.len()).filter(|i| !k.label(*i).is_empty()).map(|i| format!("{} {}", k.label(i), crate::types::hotkey_names()[i])).collect();
        info!("快速鍵：{}", list.join("，"));
    } else {
        info!("快速鍵 {} 已被其他程式使用，無法登記", busy.join("、"));
    }
    app.set_hotkeys(h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeUi {
        states: Mutex<Vec<TrayState>>,
        balloons: Mutex<Vec<(String, String, bool)>>,
    }

    impl TrayUi for FakeUi {
        fn set_state(&self, s: TrayState) {
            self.states.lock().unwrap().push(s);
        }
        fn balloon(&self, title: &str, text: &str, warn: bool) {
            self.balloons.lock().unwrap().push((title.into(), text.into(), warn));
        }
        fn dispose(&self) {}
        fn set_hotkeys(&self, _: &Hotkeys) -> Option<HotkeyStatus> {
            None
        }
    }

    #[tokio::test]
    async fn state_and_audio_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_data_dir(dir.path().to_path_buf());
        let ui = Arc::new(FakeUi::default());
        let ctl = TrayController::new(app.clone(), ui.clone(), None);
        app.mark_ready();
        ctl.push(true);
        ctl.push(false); // 沒有變化就不重送
        let states = ui.states.lock().unwrap().clone();
        assert_eq!(states.len(), 1);
        let s = &states[0];
        assert_eq!(s.rec, RecorderState::Idle);
        assert_eq!(s.tip, format!("螢幕錄影 {APP_VERSION} — 待命"));
        assert_eq!(s.last_source, "所有螢幕"); // 沒有螢幕資訊時
        assert!(!s.can_record); // 沒有 FFmpeg
        assert!(s.audio_system && s.audio_mic); // 預設錄系統聲音與麥克風

        ctl.run(TrayCommand::ToggleSystem).await;
        let saved = app.settings.load();
        assert!(!saved.record_config().unwrap().audio.system);
        assert_eq!(saved.ui.unwrap()["audioSystem"], false);
        assert!(!ui.states.lock().unwrap().last().unwrap().audio_system);

        // 沒有 FFmpeg 時開始錄影：以通知告知
        ctl.run(TrayCommand::StartLast).await;
        let b = ui.balloons.lock().unwrap().clone();
        assert_eq!(b.last().unwrap().0, "無法執行");
        assert!(b.last().unwrap().1.contains("ffmpeg"));
    }
}
