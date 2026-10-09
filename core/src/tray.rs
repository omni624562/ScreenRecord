//! 系統匣控制：推送狀態給系統匣圖示、執行選單與快捷鍵指令、錄影完成時顯示通知。
//! 圖示與選單本身在 tray_win.rs（獨立執行緒；選單開著時會卡住所在的執行緒）。

use crate::app::{App, UiPage};
use crate::format::{clock, video_clock};
use crate::info;
use crate::paths::now_ms;
use crate::settings::SettingsPatch;
use crate::types::{AudioConfig, HotkeyStatus, Hotkeys, MethodPreference, RecordConfig, RecorderState, SourceConfig, HOTKEY_NAMES};
use crate::version::APP_VERSION;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    StartLast,
    StartMonitor(String),
    StartAll,
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
    /// 截圖：與錄影相同的範圍（選單與快捷鍵）
    Screenshot,
    /// 截圖：整個指定的螢幕
    ScreenshotMonitor(String),
    /// 截圖：所有螢幕
    ScreenshotAll,
    /// 在螢幕上框選範圍或點選視窗截圖（選單與快捷鍵）
    ScreenshotSelect,
    /// 再截一次上次框選的範圍
    ScreenshotLast,
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
    pub last_result: Option<String>,
    /// None = 無法設定（開發版）
    pub autostart: Option<bool>,
    pub version: String,
    /// 有新版本時顯示在選單
    pub update: Option<String>,
    /// 快捷鍵名稱（選單右側顯示；停用時是空字串）：錄影、暫停、截圖、框選截圖
    pub keys: [String; 4],
}

/// 系統匣圖示（Windows 實作在 tray_win.rs）
pub trait TrayUi: Send + Sync {
    fn set_state(&self, s: TrayState);
    fn balloon(&self, title: &str, text: &str, warn: bool);
    /// 結束前移除圖示（否則會殘留到滑鼠移過去才消失）
    fn dispose(&self);
    /// 重新登記全域快捷鍵
    fn set_hotkeys(&self, k: &Hotkeys) -> Option<HotkeyStatus>;
}

fn state_text(s: RecorderState) -> &'static str {
    match s {
        RecorderState::Idle => "待命",
        RecorderState::Countdown => "倒數中",
        RecorderState::Recording => "錄影中",
        RecorderState::Paused => "已暫停",
        RecorderState::Stopping => "儲存中",
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
                self.notify("錄影已儲存", &format!("{name}（{}）", video_clock(res.video_sec)), false);
            }
            _ => self.notify("錄影未完成", &res.message, true),
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
            SourceConfig::All => "所有螢幕".to_string(),
            SourceConfig::Region { width, height, .. } => format!("範圍 {width}×{height}"),
            SourceConfig::Monitor { monitor_id } => format!("螢幕 {}", env.monitors.iter().find(|m| &m.id == monitor_id).map(|m| m.display_number).unwrap_or(1)),
        };
        let last_result = self.last_result_exists();
        let c = self.ctl.lock().unwrap();
        TrayState {
            rec: st.state,
            tip: format!("螢幕錄影 {APP_VERSION} — {}{time}", state_text(st.state)),
            last_source,
            monitors: env
                .monitors
                .iter()
                .map(|m| TrayMonitor { id: m.id.clone(), label: format!("螢幕 {}{}　{}×{}", m.display_number, if m.primary { "（主螢幕）" } else { "" }, m.width, m.height) })
                .collect(),
            audio_system: cfg.audio.system,
            audio_mic: cfg.audio.mic,
            can_record: self.app.ffmpeg_path().is_some() && !self.app.exporter.running(),
            can_shot: self.app.ffmpeg_path().is_some(),
            has_last_snip: self.app.last_snip().is_some(),
            last_result: if last_result { c.last_result_path.clone() } else { None },
            autostart: c.autostart,
            version: APP_VERSION.to_string(),
            update: self.app.update().map(|u| u.version),
            keys: {
                let k = self.app.hotkeys();
                std::array::from_fn(|i| k.label(i))
            },
        }
    }

    /// 選單「播放最近的錄影」是否可用：每 10 秒才查一次檔案（網路磁碟上可能卡住數秒）
    fn last_result_exists(&self) -> bool {
        let mut c = self.ctl.lock().unwrap();
        let Some(p) = c.last_result_path.clone() else { return false };
        if now_ms().saturating_sub(c.exists_at) > 10_000 {
            c.exists_at = now_ms();
            c.exists_cache = Path::new(&p).exists();
        }
        c.exists_cache
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
        // 剛啟動時（偵測還沒完成）按快捷鍵：等偵測完成，才不會誤報「找不到 ffmpeg.exe」
        self.app.wait_ready().await;
        if let Err(e) = self.run_inner(cmd).await {
            self.notify("無法執行", &e, true);
        }
        self.push(false);
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
            // 快捷鍵：同一組鍵依狀態切換（待命→開始、倒數→取消、錄影中→停止）
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
            TrayCommand::ScreenshotSelect => return app.snip_begin(&self.config()).await.map_err(err),
            TrayCommand::Screenshot | TrayCommand::ScreenshotMonitor(_) | TrayCommand::ScreenshotAll | TrayCommand::ScreenshotLast => {
                let mut cfg = self.config();
                match cmd {
                    TrayCommand::ScreenshotMonitor(id) => cfg.source = SourceConfig::Monitor { monitor_id: id },
                    TrayCommand::ScreenshotAll => cfg.source = SourceConfig::All,
                    TrayCommand::ScreenshotLast => {
                        let r = app.last_snip().ok_or("還沒有框選過範圍")?;
                        cfg.source = SourceConfig::Region { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 };
                    }
                    _ => {}
                }
                let shot = app.screenshot(&cfg).await.map_err(err)?;
                let name = std::path::Path::new(&shot.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let text = if shot.copied { format!("已複製到剪貼簿，存成 {name}") } else { format!("已存成 {name}") };
                self.notify("已截圖", &text, false);
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
                self.notify("開機自動啟動", if now == Some(true) { "已開啟：登入 Windows 後會自動常駐在系統匣" } else { "已關閉" }, false);
                return Ok(());
            }
            TrayCommand::StartLast | TrayCommand::StartAll | TrayCommand::StartMonitor(_) => self.config(),
        };
        // 開始錄影
        if app.exporter.running() {
            return Err("轉檔進行中，請等它完成再錄影".into());
        }
        match cmd {
            TrayCommand::StartAll => cfg.source = SourceConfig::All,
            TrayCommand::StartMonitor(id) => cfg.source = SourceConfig::Monitor { monitor_id: id },
            _ => {}
        }
        rec.start(cfg).await.map_err(err)
    }
}

/// 建立系統匣圖示與全域快捷鍵；失敗時回傳 false（程式改用「沒有開著的視窗就自動結束」）
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
    let ok = [h.record, h.pause, h.shot, h.snip];
    let busy: Vec<String> = (0..4).filter(|i| !ok[*i]).map(|i| k.label(i)).collect();
    if busy.is_empty() {
        let list: Vec<String> = (0..4).filter(|i| !k.label(*i).is_empty()).map(|i| format!("{} {}", k.label(i), HOTKEY_NAMES[i])).collect();
        info!("快捷鍵：{}", list.join("，"));
    } else {
        info!("快捷鍵 {} 已被其他程式使用，無法登記", busy.join("、"));
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
