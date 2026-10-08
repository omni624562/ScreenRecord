//! 錄影狀態機（原速錄影，對應 src/recorder.ts）。
//!
//! - 每次「開始 / 繼續」啟動一個 FFmpeg 行程寫一個分段；「暫停」對它送 q 正常結束。
//! - 「停止」送 q 收尾後，以 concat demuxer（-c copy）合併所有分段成單一 MP4。
//! - FFmpeg 意外結束（例如鎖定畫面、UAC 安全桌面造成 Desktop Duplication 中斷）時自動開新分段續錄。
//!
//! 公開操作（開始、暫停、繼續、停止）依序執行，避免連點造成競態；狀態放在一把短暫持有的鎖裡，
//! 不在持有鎖時等待（呼叫 FFmpeg、開啟音訊裝置），查詢狀態永遠不會被卡住。

use crate::args::{
    audio_end_args, choose_encoder, concat_args, concat_list, parse_media_info, resolve_plan, segment_args, startup_fallback, CapturePlan, EncoderSpec, FallbackInput, StartupFallback,
};
use crate::audio::{AudioSourceSpec, Opener};
use crate::audiopipe::{AudioPipe, LogFn};
use crate::format::js_round;
use crate::paths::{now_ms, timestamp, unique_path};
use crate::process::{command, last_lines, read_lines, run};
use crate::types::{CaptureMethod, LogEntry, LogLevel, MethodPreference, MonitorInfo, RecordConfig, RecorderState, RecorderStatus, RecordingResult, Rect, SourceConfig};
use crate::{Error, Result};
use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// CPU 與實測可用的 GPU 編碼器
#[derive(Debug, Clone, Default)]
pub struct Encoders {
    pub cpu: Option<EncoderSpec>,
    pub gpu: Vec<EncoderSpec>,
}

pub trait RecorderDeps: Send + Sync + 'static {
    fn ffmpeg_path(&self) -> Option<PathBuf>;
    fn encoders(&self) -> BoxFut<'_, Encoders>;
    /// 「自動」編碼是否曾偵測到 CPU 跟不上
    fn prefer_gpu(&self) -> bool;
    /// 記下「這台電腦 CPU 編碼跟不上」，之後的錄影自動改用 GPU
    fn learn_gpu(&self);
    fn monitors(&self) -> Vec<MonitorInfo>;
    /// ddagrab 是否可用（濾鏡存在且實測沒失敗；上次失敗時會重新測試）
    fn ddagrab_usable(&self) -> BoxFut<'_, bool>;
    /// 倒數結束、開始擷取前（縮小擋到擷取範圍的操作視窗等）；area 為擷取範圍（虛擬桌面的實體像素座標）
    fn before_capture(&self, _area: Rect) -> BoxFut<'_, ()> {
        Box::pin(async {})
    }
    /// 操作視窗是否在擷取範圍內（倒數畫面要不要蓋住操作視窗）
    fn ui_in_area(&self, _area: &Rect) -> bool {
        true
    }
    /// 錄影結束（已儲存或失敗）後（還原操作視窗等）
    fn after_stop(&self) {}
    /// 需要使用者注意的事（系統匣通知）
    fn notify(&self, _title: &str, _text: &str, _warn: bool) {}
    /// 開啟音訊擷取來源（Windows 上是 WASAPI）
    fn audio_opener(&self) -> Opener {
        Arc::new(crate::audio::open_wasapi)
    }
}

/// 剩餘空間低於這個值不開始錄影
const DISK_MIN_START: u64 = 1 << 30;
/// 錄影中剩餘空間低於這個值提醒一次
const DISK_WARN: u64 = 2 << 30;
/// 錄影中剩餘空間低於這個值自動停止並儲存（合併時還需要約一份影片大小的空間）
const DISK_STOP: u64 = 512 << 20;
const DISK_CHECK_MS: u64 = 10_000;

/// 儲存位置所在磁碟的剩餘空間；查不到（某些網路磁碟）回傳 None
pub async fn disk_free(dir: &str) -> Option<u64> {
    let dir = dir.to_string();
    tokio::task::spawn_blocking(move || disk_free_sync(&dir)).await.ok().flatten()
}

#[cfg(windows)]
fn disk_free_sync(dir: &str) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
    let mut avail = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut avail), None, None).ok()? };
    Some(avail)
}

#[cfg(not(windows))]
fn disk_free_sync(_dir: &str) -> Option<u64> {
    None
}

fn gb(n: u64) -> String {
    format!("{:.1} GB", n as f64 / (1u64 << 30) as f64)
}

const MAX_LOG: usize = 60;
/// 超過這麼久沒有新畫面就視為卡住
const STALL_MS: u64 = 15_000;
/// 送 q 後最多等這麼久
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// 錄到一半中斷時最多連續重試幾次（約 1 分鐘）；仍失敗就停止並合併已錄的部分
const MAX_RETRIES: u32 = 8;
/// 計時器兩次觸發的間隔超過這個值，視為電腦睡眠 / 休眠後恢復
const SLEEP_GAP_MS: u64 = 5_000;
const TICK: Duration = Duration::from_millis(250);

static PTS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Parsed_showinfo.*\bpts_time:\s*(-?[\d.]+)").unwrap());
static LEVEL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(warning|error|fatal|panic)\]").unwrap());
static FATAL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(error|fatal)\]").unwrap());
static GRAPH_FAIL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Error configuring filter graph|Could not open encoder|Error while filtering").unwrap());
static OUT_TIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^out_time_us=(\d+)").unwrap());

enum SegCmd {
    Write(&'static [u8]),
    Kill,
}

struct Segment {
    file: String,
    method: CaptureMethod,
    ctl: mpsc::UnboundedSender<SegCmd>,
    exited: watch::Receiver<bool>,
    frames: u64,
    bytes: u64,
    /// 目前這一輪 -progress 區塊的 key=value
    block: HashMap<String, String>,
    /// 近幾秒的取樣（時間、張數、重複張數），用來算實際 fps
    samples: VecDeque<(u64, f64, f64)>,
    actual_fps: Option<f64>,
    last_frame_at: u64,
    running: bool,
    stop_requested: bool,
    stderr: String,
    audio: Option<Arc<AudioPipe>>,
    /// 已要求 FFmpeg 降低訊息等級（聲音對齊完成後不再需要 showinfo）
    quieted: bool,
}

struct Retry {
    attempt: u32,
    message: String,
    /// 每次排定重試都換一個編號：取消（暫停、停止）或重新排定後，舊的計時器不再動作
    token: u64,
}

struct Countdown {
    ends_at: u64,
    cancel: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
struct St {
    state: RecorderState,
    busy: Option<String>,
    config: Option<RecordConfig>,
    plan: Option<CapturePlan>,
    method: Option<CaptureMethod>,
    segments: Vec<Segment>,
    current: Option<usize>,
    accumulated_ms: u64,
    span_start: Option<u64>,
    started_at: Option<u64>,
    ever_produced_frames: bool,
    parts_dir: Option<PathBuf>,
    final_path: Option<PathBuf>,
    result: Option<RecordingResult>,
    retry: Option<Retry>,
    seq: u64,
    audio_specs: Vec<AudioSourceSpec>,
    enc: Option<EncoderSpec>,
    cpu_encoder: Option<EncoderSpec>,
    gpu_available: bool,
    audio_desc: Option<String>,
    slow_since: Option<u64>,
    slow_warned: bool,
    log: Vec<LogEntry>,
    /// 目前計時器的編號（0 = 沒有）
    ticker: u64,
    last_tick_at: u64,
    countdown: Option<Countdown>,
    /// start() 已受理、但還沒進入錄影（準備中 / 倒數中 / 縮小視窗中）
    starting_now: bool,
    /// 開始的過程中按了停止 / 取消：在每個檢查點放棄開始
    cancel_requested: bool,
    disk_checking: bool,
    disk_free_bytes: Option<u64>,
    disk_checked_at: u64,
    disk_warned: bool,
    auto_stopping: bool,
    shutting_down: bool,
}

impl St {
    fn add_log(&mut self, level: LogLevel, text: &str) {
        self.log.push(LogEntry { t: now_ms(), level, text: text.to_string() });
        if self.log.len() > MAX_LOG {
            let extra = self.log.len() - MAX_LOG;
            self.log.drain(..extra);
        }
        let (tag, file_tag) = match level {
            LogLevel::Error => ("錯誤", "ERROR"),
            LogLevel::Warn => ("警告", "WARN "),
            LogLevel::Info => ("訊息", "INFO "),
        };
        crate::log::write(file_tag, &format!("{tag}：{text}"));
    }

    fn recorded_ms(&self) -> u64 {
        self.accumulated_ms + self.span_start.map(|s| now_ms().saturating_sub(s)).unwrap_or(0)
    }

    fn close_span(&mut self) {
        if let Some(s) = self.span_start.take() {
            self.accumulated_ms += now_ms().saturating_sub(s);
        }
        self.retry = None;
        self.slow_since = None;
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn config(&self) -> &RecordConfig {
        self.config.as_ref().expect("錄影設定")
    }
}

fn cleanup_parts(parts_dir: Option<&Path>) {
    let Some(dir) = parts_dir else { return };
    let _ = std::fs::remove_dir_all(dir);
    // 只有空資料夾才刪得掉（.parts 內還有其他合併失敗而保留的分段時屬正常）
    if let Some(parent) = dir.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// 保留最後 n 個字元
fn keep_tail(s: &mut String, n: usize) {
    let count = s.chars().count();
    if count > n {
        *s = s.chars().skip(count - n).collect();
    }
}

struct Inner {
    deps: Arc<dyn RecorderDeps>,
    st: Mutex<St>,
    /// 公開操作依序執行
    queue: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct Recorder(Arc<Inner>);

enum ExitAction {
    None,
    Start,
    Stop,
    RetryLater { delay: u64, token: u64, attempt: u32 },
}

impl Recorder {
    pub fn new(deps: Arc<dyn RecorderDeps>) -> Recorder {
        Recorder(Arc::new(Inner { deps, st: Mutex::new(St::default()), queue: tokio::sync::Mutex::new(()) }))
    }

    fn lock(&self) -> MutexGuard<'_, St> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn deps(&self) -> &dyn RecorderDeps {
        &*self.0.deps
    }

    // ───────────── 公開操作 ─────────────

    /// 開始錄影。有倒數時，進入倒數就先回覆（介面與系統匣以狀態顯示倒數，期間可取消）；
    /// 倒數之後才發生的錯誤會寫進事件紀錄並以系統匣通知。
    pub fn start(&self, config: RecordConfig) -> BoxFut<'static, Result<()>> {
        {
            let mut st = self.lock();
            // 連按兩次（例如快捷鍵）不要排出第二個開始：否則取消第一個後，第二個仍會倒數並錄影
            if st.starting_now || st.state != RecorderState::Idle {
                return Box::pin(async { Err(Error::config("目前已在錄影或正在準備開始")) });
            }
            st.starting_now = true;
            st.cancel_requested = false;
        }
        let (began_tx, mut began_rx) = oneshot::channel::<()>();
        let (done_tx, mut done_rx) = oneshot::channel::<Result<()>>();
        let me = self.clone();
        tokio::spawn(async move {
            let mut began = Some(began_tx);
            let r = {
                let _q = me.0.queue.lock().await;
                me.do_start(config, &mut began).await
            };
            me.lock().starting_now = false;
            if let Err(e) = &r {
                if began.is_none() {
                    me.deps().notify("無法開始錄影", e.message(), true);
                }
            }
            let _ = done_tx.send(r);
        });
        Box::pin(async move {
            tokio::select! {
                biased;
                r = &mut done_rx => r.unwrap_or(Ok(())),
                b = &mut began_rx => match b {
                    Ok(()) => Ok(()),
                    Err(_) => done_rx.await.unwrap_or(Ok(())),
                },
            }
        })
    }

    pub async fn pause(&self) -> Result<()> {
        let _q = self.0.queue.lock().await;
        self.do_pause().await
    }

    pub async fn resume(&self) -> Result<()> {
        let _q = self.0.queue.lock().await;
        self.do_resume()
    }

    /// 停止並儲存。開始的過程中（準備、倒數、縮小視窗）直接取消：start 還在佇列裡，不能排在它後面
    pub fn stop(&self, reason: Option<String>) -> BoxFut<'static, Result<Option<RecordingResult>>> {
        {
            let mut st = self.lock();
            if st.starting_now && matches!(st.state, RecorderState::Idle | RecorderState::Countdown) {
                st.cancel_requested = true;
                if let Some(tx) = st.countdown.as_mut().and_then(|c| c.cancel.take()) {
                    let _ = tx.send(());
                }
                return Box::pin(async { Ok(None) });
            }
        }
        let me = self.clone();
        Box::pin(async move {
            let _q = me.0.queue.lock().await;
            me.do_stop(reason).await
        })
    }

    /// 背景停止（錯誤只寫進記錄）
    fn stop_later(&self, reason: Option<String>) {
        let fut = self.stop(reason);
        tokio::spawn(async move {
            let _ = fut.await;
        });
    }

    pub fn active(&self) -> bool {
        self.lock().state != RecorderState::Idle
    }

    /// 錄影中（含儲存中）要寫入的成品路徑：不能改名或刪除
    pub fn output_path(&self) -> Option<PathBuf> {
        let st = self.lock();
        if st.state != RecorderState::Idle {
            st.final_path.clone()
        } else {
            None
        }
    }

    /// 正在準備開始（狀態可能仍是待命）；快捷鍵用來判斷再按一次是「取消」
    pub fn starting(&self) -> bool {
        self.lock().starting_now
    }

    pub fn status(&self) -> RecorderStatus {
        let now = now_ms();
        let (mut status, covers_area) = {
            let st = self.lock();
            let frames: u64 = st.segments.iter().map(|s| s.frames).sum();
            let bytes: u64 = st.segments.iter().map(|s| s.bytes).sum();
            let fps = st.config.as_ref().map(|c| c.fps).unwrap_or(30.0);
            let cur = st.current.map(|i| &st.segments[i]);
            let recording = st.state == RecorderState::Recording;
            let cur_running = cur.is_some_and(|c| c.running);
            let mut busy = st.busy.clone();
            if busy.is_none() && recording && cur_running && cur.is_some_and(|c| c.frames == 0) && st.retry.is_none() {
                busy = Some("正在啟動擷取…".into());
            }
            let counting = st.state == RecorderState::Countdown;
            let status = RecorderStatus {
                state: st.state,
                busy,
                method: st.method,
                tiles: if st.method == Some(CaptureMethod::Ddagrab) { st.plan.as_ref().and_then(|p| p.dda.as_ref()).map(|d| d.tiles.len() as u32) } else { None },
                encoder: st.enc.map(|e| e.name.to_string()),
                audio: st.audio_desc.clone(),
                segments: st.segments.iter().filter(|s| s.frames > 0).count() as u32,
                frames,
                video_sec: frames as f64 / fps,
                bytes,
                recorded_ms: st.recorded_ms(),
                max_ms: (st.config.as_ref().map(|c| c.max_minutes).unwrap_or(0.0) * 60_000.0).max(0.0) as u64,
                fps,
                out_width: st.plan.as_ref().map(|p| p.out_width).unwrap_or(0),
                out_height: st.plan.as_ref().map(|p| p.out_height).unwrap_or(0),
                actual_fps: if recording && cur_running { cur.and_then(|c| c.actual_fps) } else { None },
                slow: recording && st.slow_since.is_some_and(|s| now.saturating_sub(s) > 3000),
                retrying: st.retry.as_ref().map(|r| r.message.clone()),
                started_at: st.started_at,
                countdown_ms: if counting { st.countdown.as_ref().map(|c| c.ends_at.saturating_sub(now)) } else { None },
                countdown_covers_ui: None,
                disk_free_bytes: if st.state == RecorderState::Idle { None } else { st.disk_free_bytes },
                result: st.result.clone(),
                log: st.log.clone(),
            };
            (status, if counting { st.plan.as_ref().map(|p| p.rect) } else { None })
        };
        // 不在持有狀態鎖時呼叫外部（列舉視窗）
        if let Some(area) = covers_area {
            status.countdown_covers_ui = Some(self.deps().ui_in_area(&area));
        }
        status
    }

    /// 程式結束前呼叫：正常收尾並合併。
    pub fn shutdown(&self) -> BoxFut<'static, ()> {
        {
            let mut st = self.lock();
            st.shutting_down = true;
            if st.starting_now {
                // 還在準備 / 倒數：取消並等它收拾好（否則會留下空的分段資料夾）
                st.cancel_requested = true;
                if let Some(tx) = st.countdown.as_mut().and_then(|c| c.cancel.take()) {
                    let _ = tx.send(());
                }
            }
        }
        let me = self.clone();
        Box::pin(async move {
            drop(me.0.queue.lock().await);
            while me.lock().starting_now {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if me.active() {
                let _ = me.stop(Some("程式結束".into())).await;
            }
        })
    }

    // ───────────── 內部 ─────────────

    fn add_log(&self, level: LogLevel, text: &str) {
        self.lock().add_log(level, text);
    }

    /// 開始的過程中被取消：回到待命、刪掉空的分段資料夾
    fn abandon(&self, restore_window: bool) -> Result<()> {
        {
            let mut st = self.lock();
            st.countdown = None;
            st.state = RecorderState::Idle;
            cleanup_parts(st.parts_dir.as_deref());
            st.add_log(LogLevel::Info, "已取消，沒有開始錄影");
        }
        if restore_window {
            self.deps().after_stop();
        }
        Ok(())
    }

    fn canceled(&self) -> bool {
        let st = self.lock();
        st.cancel_requested || st.shutting_down
    }

    async fn do_start(&self, config: RecordConfig, began: &mut Option<oneshot::Sender<()>>) -> Result<()> {
        if self.lock().state != RecorderState::Idle {
            return Err(Error::config("目前已在錄影中"));
        }
        let deps = self.0.deps.clone();
        if deps.ffmpeg_path().is_none() {
            return Err(Error::config("找不到 ffmpeg.exe，請先依畫面指示下載"));
        }
        let plan = resolve_plan(&config, &deps.monitors())?;
        let encoders = deps.encoders().await;
        let enc_pref = config.encoder.unwrap_or_default();
        let (enc, reason) = choose_encoder(enc_pref, plan.out_width, plan.out_height, config.fps, encoders.cpu, &encoders.gpu, deps.prefer_gpu())?;
        let method = match config.method {
            MethodPreference::Gdigrab => CaptureMethod::Gdigrab,
            MethodPreference::Ddagrab => {
                if plan.dda.is_none() {
                    return Err(Error::config("此範圍涵蓋不同顯示卡上的螢幕，ddagrab 無法擷取，請改用 gdigrab 或自動"));
                }
                if !deps.ddagrab_usable().await {
                    return Err(Error::config("這台電腦無法使用 ddagrab，請改用 gdigrab 或自動"));
                }
                CaptureMethod::Ddagrab
            }
            MethodPreference::Auto => {
                if plan.dda.is_some() && deps.ddagrab_usable().await {
                    CaptureMethod::Ddagrab
                } else {
                    CaptureMethod::Gdigrab
                }
            }
        };

        let stamp = timestamp();
        let output_dir = config.output_dir.trim().to_string();
        std::fs::create_dir_all(&output_dir).map_err(|e| Error::config(format!("無法建立儲存資料夾：{e}")))?;
        let free = disk_free(&output_dir).await;
        if let Some(f) = free.filter(|f| *f < DISK_MIN_START) {
            return Err(Error::config(format!("儲存位置剩餘空間只有 {}，請清出空間或改存到其他磁碟", gb(f))));
        }
        let parts_dir = Path::new(&output_dir).join(".parts").join(&stamp);
        std::fs::create_dir_all(&parts_dir)?;
        let final_path = unique_path(Path::new(&output_dir), &format!("Rec_{stamp}"), ".mp4");
        let mut audio_specs = Vec::new();
        if config.audio.system {
            audio_specs.push(AudioSourceSpec { loopback: true, mic_id: String::new() });
        }
        if config.audio.mic {
            audio_specs.push(AudioSourceSpec { loopback: false, mic_id: config.audio.mic_id.clone() });
        }
        let hide_ui = config.hide_ui != Some(false);
        let countdown_sec = js_round(config.countdown_sec.unwrap_or(3.0)).clamp(0.0, 10.0) as u64;
        {
            let mut st = self.lock();
            st.disk_free_bytes = free;
            st.disk_checked_at = now_ms();
            st.disk_warned = false;
            st.parts_dir = Some(parts_dir);
            st.final_path = Some(final_path);
            st.config = Some(RecordConfig { output_dir, ..config.clone() });
            st.plan = Some(plan.clone());
            st.method = Some(method);
            st.enc = Some(enc);
            st.cpu_encoder = encoders.cpu;
            st.gpu_available = !encoders.gpu.is_empty();
            st.segments = Vec::new();
            st.current = None;
            st.accumulated_ms = 0;
            st.started_at = Some(now_ms());
            st.ever_produced_frames = false;
            st.result = None;
            st.retry = None;
            st.slow_since = None;
            st.slow_warned = false;
            st.auto_stopping = false;
            st.log = Vec::new();
            st.audio_specs = audio_specs;
            st.audio_desc = None;
        }
        if self.canceled() {
            return self.abandon(false);
        }

        // 倒數：讓使用者有時間切到要錄的畫面；期間按停止（或快捷鍵）可取消
        if countdown_sec > 0 {
            let (tx, rx) = oneshot::channel::<()>();
            {
                let mut st = self.lock();
                st.state = RecorderState::Countdown;
                st.countdown = Some(Countdown { ends_at: now_ms() + countdown_sec * 1000, cancel: Some(tx) });
            }
            if let Some(b) = began.take() {
                let _ = b.send(());
            }
            let canceled = tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(countdown_sec)) => false,
                r = rx => r.is_ok(),
            };
            if canceled || self.canceled() {
                return self.abandon(false);
            }
        }
        // 縮小視窗的那 0.35 秒仍算倒數（countdown 保留到這之後），期間取消也有效
        if hide_ui {
            deps.before_capture(plan.rect).await;
        }
        if self.canceled() {
            return self.abandon(hide_ui);
        }

        {
            let mut st = self.lock();
            st.countdown = None;
            let (width, height) = (plan.rect.width, plan.rect.height);
            let place = match &config.source {
                SourceConfig::Monitor { .. } => format!("螢幕 {}", plan.monitors.first().map(|m| m.display_number.to_string()).unwrap_or_else(|| "?".into())),
                SourceConfig::All => format!("所有螢幕（{} 個）", plan.monitors.len()),
                SourceConfig::Region { .. } => format!("範圍 ({}, {})", plan.rect.x, plan.rect.y),
            };
            let tiles = match &plan.dda {
                Some(d) if method == CaptureMethod::Ddagrab && d.tiles.len() > 1 => format!(" ×{} 合成", d.tiles.len()),
                _ => String::new(),
            };
            st.add_log(
                LogLevel::Info,
                &format!(
                    "開始錄影：{place} {width}×{height} → {}×{}，{} fps（{}{tiles} / {}）",
                    plan.out_width,
                    plan.out_height,
                    crate::format::num(config.fps),
                    method.as_str(),
                    enc.name
                ),
            );
            if Some(enc) != encoders.cpu {
                st.add_log(LogLevel::Info, reason);
            }
            if method == CaptureMethod::Gdigrab && plan.monitors.len() > 1 && plan.dda.is_none() && config.method == MethodPreference::Auto {
                st.add_log(LogLevel::Info, "範圍涵蓋不同顯示卡上的螢幕，ddagrab 無法合成，改用 gdigrab");
            }
            st.state = RecorderState::Recording;
            st.span_start = Some(now_ms());
        }
        if let Err(e) = self.start_segment() {
            // 例如 ffmpeg.exe 被移除 / 防毒隔離：回到待命，不留下錄影中的假狀態
            {
                let mut st = self.lock();
                st.add_log(LogLevel::Error, &format!("無法啟動 FFmpeg：{e}"));
                st.state = RecorderState::Idle;
                st.span_start = None;
                cleanup_parts(st.parts_dir.as_deref());
            }
            self.deps().after_stop(); // 已縮小的操作視窗要還原，使用者才看得到錯誤
            return Err(Error::config(format!("無法啟動 FFmpeg：{e}")));
        }
        self.start_ticker();
        Ok(())
    }

    async fn do_pause(&self) -> Result<()> {
        {
            let mut st = self.lock();
            if st.state != RecorderState::Recording {
                return Err(Error::config("目前不在錄影中"));
            }
            st.close_span();
            st.state = RecorderState::Paused;
            st.busy = Some("正在結束目前分段…".into());
        }
        self.stop_current().await;
        let mut st = self.lock();
        st.busy = None;
        st.add_log(LogLevel::Info, "已暫停");
        Ok(())
    }

    fn do_resume(&self) -> Result<()> {
        {
            let mut st = self.lock();
            if st.state != RecorderState::Paused {
                return Err(Error::config("目前不是暫停狀態"));
            }
            st.state = RecorderState::Recording;
            st.span_start = Some(now_ms());
            st.add_log(LogLevel::Info, "繼續錄影");
        }
        if !self.try_start_segment() {
            return Err(Error::config("無法繼續錄影，已停止並儲存先前錄到的部分"));
        }
        Ok(())
    }

    async fn do_stop(&self, reason: Option<String>) -> Result<Option<RecordingResult>> {
        {
            let mut st = self.lock();
            if st.state == RecorderState::Idle {
                return Err(Error::config("目前沒有在錄影"));
            }
            st.close_span();
            st.state = RecorderState::Stopping;
            st.busy = Some("正在結束錄影…".into());
            if let Some(r) = &reason {
                st.add_log(LogLevel::Info, r);
            }
        }
        self.stop_current().await;
        self.lock().busy = Some("正在合併分段…".into());
        let result = match self.finalize().await {
            Ok(r) => r,
            Err(e) => {
                // 例如磁碟已滿：要讓介面與系統匣看得到失敗，分段保留下來
                let parts = self.lock().parts_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                RecordingResult {
                    ok: false,
                    frames: 0,
                    video_sec: 0.0,
                    parts_dir: Some(parts.clone()),
                    message: format!("儲存失敗：{}（已錄的分段保留於 {parts}）", e.message()),
                    ..Default::default()
                }
            }
        };
        {
            let mut st = self.lock();
            st.add_log(if result.ok { LogLevel::Info } else { LogLevel::Error }, &result.message);
            st.result = Some(result.clone());
            st.ticker = 0;
            st.busy = None;
            st.state = RecorderState::Idle;
        }
        self.deps().after_stop();
        Ok(Some(result))
    }

    /// 在計時器 / 結束事件裡啟動分段：失敗時記錄並停止（保住已錄的部分）
    fn try_start_segment(&self) -> bool {
        match self.start_segment() {
            Ok(()) => true,
            Err(e) => {
                self.add_log(LogLevel::Error, &format!("無法啟動 FFmpeg：{e}"));
                self.stop_later(Some("無法繼續擷取，停止錄影".into()));
                false
            }
        }
    }

    fn start_segment(&self) -> std::result::Result<(), String> {
        let (ffmpeg, plan, config, method, enc, file, specs, index) = {
            let st = self.lock();
            let index = st.segments.len();
            let parts = st.parts_dir.clone().ok_or("沒有分段資料夾")?;
            (
                self.deps().ffmpeg_path().ok_or("找不到 ffmpeg.exe")?,
                st.plan.clone().ok_or("沒有擷取範圍")?,
                st.config().clone(),
                st.method.unwrap_or(CaptureMethod::Gdigrab),
                st.enc.ok_or("沒有編碼器")?,
                parts.join(format!("seg_{index:03}.mp4")).display().to_string(),
                st.audio_specs.clone(),
                index,
            )
        };
        // 錄聲音：每個分段各自開一組 WASAPI 擷取與 TCP 連線，時間零點對齊該分段的第一張畫面。
        // 開啟音訊裝置時不持有狀態鎖（音訊執行緒會寫事件紀錄）
        let audio = if specs.is_empty() {
            None
        } else {
            let weak = Arc::downgrade(&self.0);
            let log: LogFn = Arc::new(move |level, text| {
                if let Some(inner) = weak.upgrade() {
                    Recorder(inner).add_log(level, &text);
                }
            });
            Some(Arc::new(AudioPipe::new(specs, self.deps().audio_opener(), log).map_err(|e| format!("無法建立聲音管線：{e}"))?))
        };
        if let Some(a) = &audio {
            let mut st = self.lock();
            if st.audio_desc.is_none() {
                st.audio_desc = Some(a.describe().to_string());
                st.add_log(LogLevel::Info, &format!("錄製聲音：{}", a.describe()));
            }
        }
        let audio_args = audio.as_ref().map(|a| a.input_args());
        let args = segment_args(&plan, &config, method, &enc, &file, audio_args.as_deref()).map_err(|e| e.message().to_string())?;

        let mut cmd = command(&ffmpeg);
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (ctl, mut ctl_rx) = mpsc::unbounded_channel::<SegCmd>();
        let (exit_tx, exited) = watch::channel(false);
        let now = now_ms();
        {
            let mut st = self.lock();
            // 準備的這段時間內已經開始停止（例如磁碟不足自動停止）：不再開新分段
            if st.state != RecorderState::Recording || st.shutting_down || st.segments.len() != index {
                drop(st);
                let _ = child.start_kill();
                if let Some(a) = audio {
                    a.close();
                }
                return Ok(());
            }
            st.segments.push(Segment {
                file,
                method,
                ctl,
                exited,
                frames: 0,
                bytes: 0,
                block: HashMap::new(),
                samples: VecDeque::new(),
                actual_fps: None,
                last_frame_at: now,
                running: true,
                stop_requested: false,
                stderr: String::new(),
                audio,
                quieted: false,
            });
            st.current = Some(index);
        }

        let me = self.clone();
        let out_task = tokio::spawn(async move {
            if let Some(out) = stdout {
                read_lines(out, |line| me.on_progress(index, line)).await;
            }
        });
        let me = self.clone();
        let err_task = tokio::spawn(async move {
            if let Some(err) = stderr {
                read_lines(err, |line| me.on_stderr(index, line)).await;
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            let mut ctl_open = true;
            let status = loop {
                tokio::select! {
                    s = child.wait() => break s,
                    cmd = ctl_rx.recv(), if ctl_open => match cmd {
                        Some(SegCmd::Write(bytes)) => {
                            if let Some(w) = stdin.as_mut() {
                                if w.write_all(bytes).await.is_err() || w.flush().await.is_err() {
                                    stdin = None; // 行程剛好結束
                                }
                            }
                        }
                        Some(SegCmd::Kill) => {
                            let _ = child.start_kill();
                        }
                        None => ctl_open = false,
                    },
                }
            };
            // 讀完最後的進度與錯誤訊息再處理結束（張數才完整）
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                let _ = out_task.await;
                let _ = err_task.await;
            })
            .await;
            let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
            let _ = exit_tx.send(true);
            me.on_exit(index, code);
        });
        Ok(())
    }

    fn on_stderr(&self, index: usize, line: &str) {
        let arrival = crate::clock::now_100ns();
        let mut st = self.lock();
        let seg = &mut st.segments[index];
        if let Some(audio) = seg.audio.clone() {
            // level+info 模式：showinfo 每張畫面的時間給聲音對齊用，其餘只保留警告以上的訊息
            if let Some(c) = PTS_RE.captures(line) {
                if !audio.synced() {
                    if let Ok(pts) = c[1].parse::<f64>() {
                        audio.on_video_frame(pts, arrival);
                    }
                    return;
                }
                // 對齊完成：對 FFmpeg 按「-」把訊息等級從 info 降一級（仍會印錯誤），不再每張畫面印一行
                if !seg.quieted && !seg.stop_requested {
                    seg.quieted = true;
                    let _ = seg.ctl.send(SegCmd::Write(b"-"));
                }
                return;
            }
            if !LEVEL_RE.is_match(line) {
                return;
            }
            // 畫面這端失敗時，音訊輸入仍會讓 FFmpeg 卡著不結束；直接結束它，才能立即退回 gdigrab 或重試
            if FATAL_RE.is_match(line) && GRAPH_FAIL_RE.is_match(line) && !seg.stop_requested {
                seg.stderr.push_str(line);
                seg.stderr.push('\n');
                keep_tail(&mut seg.stderr, 4000);
                let _ = seg.ctl.send(SegCmd::Kill);
                return;
            }
        }
        seg.stderr.push_str(line);
        seg.stderr.push('\n');
        keep_tail(&mut seg.stderr, 4000);
    }

    /// -progress 每 0.5 秒輸出一組 key=value，以 progress=continue|end 結尾
    fn on_progress(&self, index: usize, line: &str) {
        let Some((key, value)) = line.split_once('=') else { return };
        let mut guard = self.lock();
        let st = &mut *guard;
        let seg = &mut st.segments[index];
        seg.block.insert(key.to_string(), value.trim().to_string());
        if key != "progress" {
            return;
        }
        let b = std::mem::take(&mut seg.block);
        let num = |k: &str| b.get(k).and_then(|v| v.parse::<f64>().ok());
        if let Some(size) = num("total_size").filter(|s| *s > 0.0) {
            seg.bytes = size as u64;
        }
        let Some(frame) = num("frame") else { return };
        let now = now_ms();
        if frame > seg.frames as f64 {
            seg.frames = frame as u64;
            seg.last_frame_at = now;
            st.ever_produced_frames = true;
            if st.retry.is_some() && st.current == Some(index) {
                st.add_log(LogLevel::Info, "擷取已恢復");
                st.retry = None;
            }
        }
        if frame > 0.0 {
            let learn = self.check_performance(st, index, now, frame, num("dup_frames").unwrap_or(0.0));
            if learn {
                drop(guard);
                self.deps().learn_gpu();
            }
        }
    }

    /// 以「近 5 秒實際寫入的張數」判斷是否跟得上即時錄影；回傳是否要記下「改用 GPU」。
    /// 不用 FFmpeg 的 speed=：它把啟動的 1～2 秒也算進去，開頭幾十秒都會偏低。
    /// dup_frames 增加代表擷取來源跟不上，CFR 只好重複前一張補位。
    fn check_performance(&self, st: &mut St, index: usize, now: u64, frame: f64, dup: f64) -> bool {
        let fps = st.config().fps;
        let auto = st.config().encoder.unwrap_or_default() == crate::types::EncoderPreference::Auto;
        let is_current = st.current == Some(index);
        let seg = &mut st.segments[index];
        seg.samples.push_back((now, frame, dup));
        while seg.samples.len() > 2 && now - seg.samples[0].0 > 5000 {
            seg.samples.pop_front();
        }
        let first = seg.samples[0];
        let span = (now - first.0) as f64 / 1000.0;
        if span < 3.0 || !is_current {
            return false;
        }
        let written = (frame - first.1) / span;
        let unique = written - (dup - first.2) / span;
        seg.actual_fps = Some(unique.max(0.0));
        if written < fps * 0.9 || unique < fps * 0.9 {
            let since = *st.slow_since.get_or_insert(now);
            if !st.slow_warned && now - since > 3000 {
                st.slow_warned = true;
                st.add_log(LogLevel::Warn, &format!("電腦跟不上即時錄影：實際約 {unique:.1} fps（設定 {} fps），建議降低解析度或 FPS", crate::format::num(fps)));
                // 寫入速度不足代表處理鏈（下載畫面、縮放、編碼）跟不上；擷取跟不上則是 dup 增加。
                // 改用 GPU 編碼可減輕 CPU 負擔，「自動」模式記下來，之後的錄影改用 GPU 編碼
                // （同一段錄影不中途切換，不同編碼器的分段無法無損合併；可在介面上重設）
                if auto && written < fps * 0.9 && st.enc == st.cpu_encoder && st.gpu_available && !self.deps().prefer_gpu() {
                    st.add_log(LogLevel::Warn, "電腦處理不及，之後的錄影會自動改用 GPU 編碼以減輕 CPU 負擔（可在「更多 → 編碼器」重設）");
                    return true;
                }
            }
        } else {
            st.slow_since = None;
        }
        false
    }

    fn on_exit(&self, index: usize, code: i32) {
        let mut close_audio = None;
        let action = {
            let mut guard = self.lock();
            let st = &mut *guard;
            let seg = &mut st.segments[index];
            seg.running = false;
            if !seg.stop_requested {
                close_audio = seg.audio.clone();
            }
            if seg.stop_requested || st.shutting_down || st.current != Some(index) || st.state != RecorderState::Recording {
                ExitAction::None
            } else {
                let tail = last_lines(&seg.stderr, 2);
                let detail = if tail.is_empty() { format!("結束代碼 {code}") } else { tail };
                let seg_method = seg.method;
                let method_auto = st.config().method == MethodPreference::Auto;
                if !st.ever_produced_frames {
                    let next = startup_fallback(&FallbackInput {
                        stderr: &st.segments[index].stderr,
                        gpu_encoder_in_use: st.cpu_encoder.is_some() && st.enc != st.cpu_encoder,
                        encoder_auto: st.config().encoder.unwrap_or_default() == crate::types::EncoderPreference::Auto,
                        has_cpu_encoder: st.cpu_encoder.is_some(),
                        ddagrab_in_use: seg_method == CaptureMethod::Ddagrab,
                        method_auto,
                    });
                    match next {
                        // GPU 編碼器一開始就失敗（驅動問題等）：「自動」模式退回 CPU 編碼再試
                        StartupFallback::CpuEncoder => {
                            let name = st.enc.map(|e| e.name).unwrap_or("");
                            st.add_log(LogLevel::Warn, &format!("GPU 編碼器 {name} 無法使用（{detail}），改用 CPU 編碼"));
                            st.enc = st.cpu_encoder;
                            ExitAction::Start
                        }
                        StartupFallback::Gdigrab => {
                            st.add_log(LogLevel::Warn, &format!("ddagrab 無法擷取（{detail}），改用 gdigrab"));
                            st.method = Some(CaptureMethod::Gdigrab);
                            ExitAction::Start
                        }
                        StartupFallback::Fatal => {
                            st.add_log(LogLevel::Error, &format!("無法開始擷取：{detail}"));
                            ExitAction::Stop
                        }
                    }
                } else {
                    // 錄到一半中斷：保留已錄分段，延遲後開新分段續錄
                    let attempt = st.retry.as_ref().map(|r| r.attempt).unwrap_or(0) + 1;
                    if attempt > MAX_RETRIES {
                        // 長時間無法恢復（螢幕被拔掉、磁碟已滿…）：ddagrab 先換 gdigrab 再試一輪，否則停止並合併已錄的部分
                        if seg_method == CaptureMethod::Ddagrab && method_auto {
                            st.add_log(LogLevel::Warn, &format!("ddagrab 連續 {MAX_RETRIES} 次無法擷取（{detail}），改用 gdigrab"));
                            st.method = Some(CaptureMethod::Gdigrab);
                            let token = st.next_seq();
                            st.retry = Some(Retry { attempt: 0, message: "改用 gdigrab 重試".into(), token });
                            ExitAction::Start
                        } else {
                            st.add_log(LogLevel::Error, &format!("連續 {MAX_RETRIES} 次無法恢復擷取（{detail}），停止錄影並儲存已錄的部分"));
                            st.retry = None;
                            ExitAction::Stop
                        }
                    } else {
                        let delay = (1000u64 << (attempt - 1)).min(10_000);
                        let message = format!("擷取中斷，{} 秒後重試（第 {attempt} 次）", js_round(delay as f64 / 1000.0));
                        st.add_log(LogLevel::Warn, &format!("FFmpeg 意外結束：{detail}；{message}"));
                        let token = st.next_seq();
                        st.retry = Some(Retry { attempt, message, token });
                        ExitAction::RetryLater { delay, token, attempt }
                    }
                }
            }
        };
        if let Some(a) = close_audio {
            a.close();
        }
        match action {
            ExitAction::None => {}
            ExitAction::Start => {
                self.try_start_segment();
            }
            ExitAction::Stop => self.stop_later(None),
            ExitAction::RetryLater { delay, token, attempt } => {
                let me = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    {
                        let mut st = me.lock();
                        let current_running = st.current.is_some_and(|i| st.segments[i].running);
                        let still = st.retry.as_ref().is_some_and(|r| r.token == token);
                        if st.state != RecorderState::Recording || st.shutting_down || current_running || !still {
                            return;
                        }
                        if let Some(r) = st.retry.as_mut() {
                            r.message = format!("擷取中斷，正在重試（第 {attempt} 次）");
                        }
                    }
                    me.try_start_segment();
                });
            }
        }
    }

    /// 對目前分段送 q 並等待結束；逾時才強制終止（fragmented MP4 確保已寫入的畫面不遺失）。
    async fn stop_current(&self) {
        let (ctl, mut exited, audio) = {
            let mut st = self.lock();
            let Some(i) = st.current else { return };
            let seg = &mut st.segments[i];
            if !seg.running {
                return;
            }
            seg.stop_requested = true;
            (seg.ctl.clone(), seg.exited.clone(), seg.audio.clone())
        };
        if let Some(a) = &audio {
            a.flush_to_now(); // 聲音先補到現在，結尾才不會比畫面短
        }
        let _ = ctl.send(SegCmd::Write(b"q"));
        // 聲音持續送到 FFmpeg 真正結束為止：收到 q 後畫面還會多處理幾百毫秒，
        // 提早送 EOF 會讓每個分段結尾少一截聲音
        let exited_in_time = tokio::time::timeout(STOP_TIMEOUT, exited.wait_for(|v| *v)).await.is_ok();
        if !exited_in_time {
            self.add_log(LogLevel::Warn, &format!("FFmpeg 未在 {} 秒內結束，已強制終止", STOP_TIMEOUT.as_secs()));
            let _ = ctl.send(SegCmd::Kill);
            let _ = exited.wait_for(|v| *v).await;
        }
        if let Some(a) = audio {
            a.close();
        }
    }

    fn start_ticker(&self) {
        let gen = {
            let mut st = self.lock();
            st.last_tick_at = now_ms();
            let gen = st.next_seq();
            st.ticker = gen;
            gen
        };
        let me = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(TICK);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            iv.tick().await;
            loop {
                iv.tick().await;
                if me.lock().ticker != gen {
                    break;
                }
                me.tick();
            }
        });
    }

    fn tick(&self) {
        let now = now_ms();
        let mut stop_reason = None;
        let mut check_disk = false;
        {
            let mut guard = self.lock();
            let st = &mut *guard;
            let gap = now.saturating_sub(st.last_tick_at);
            st.last_tick_at = now;
            if st.state != RecorderState::Recording {
                return;
            }
            if gap > SLEEP_GAP_MS && st.span_start.is_some() {
                // 電腦睡眠後恢復：睡著的時間不算錄影長度（否則會誤觸最長錄影時間）；
                // 重開分段，避免 FFmpeg 用重複畫面補滿這段空白
                st.span_start = st.span_start.map(|s| s + gap);
                st.add_log(LogLevel::Warn, &format!("電腦約 {} 秒沒有運作（睡眠 / 休眠），重新開始擷取", js_round(gap as f64 / 1000.0)));
                if let Some(seg) = st.current.map(|i| &mut st.segments[i]) {
                    if seg.running && !seg.stop_requested {
                        seg.last_frame_at = now;
                        let _ = seg.ctl.send(SegCmd::Kill);
                    }
                }
                return;
            }
            let max_ms = (st.config().max_minutes * 60_000.0).max(0.0) as u64;
            if max_ms > 0 && st.recorded_ms() >= max_ms && !st.auto_stopping {
                st.auto_stopping = true;
                stop_reason = Some("已達最長錄影時間，自動停止".to_string());
            } else {
                if now.saturating_sub(st.disk_checked_at) > DISK_CHECK_MS {
                    st.disk_checked_at = now;
                    check_disk = true;
                }
                let stalled = st.current.map(|i| &st.segments[i]).is_some_and(|s| s.running && !s.stop_requested && now.saturating_sub(s.last_frame_at) > STALL_MS);
                if stalled {
                    st.add_log(LogLevel::Warn, &format!("超過 {} 秒沒有擷取到新畫面，重新啟動 FFmpeg", STALL_MS / 1000));
                    let seg = &mut st.segments[st.current.unwrap()];
                    seg.last_frame_at = now;
                    let _ = seg.ctl.send(SegCmd::Kill);
                }
            }
        }
        if stop_reason.is_some() {
            self.stop_later(stop_reason);
        }
        if check_disk {
            let me = self.clone();
            tokio::spawn(async move { me.check_disk().await });
        }
    }

    /// 錄影中定期檢查剩餘空間：不足時先提醒，再不足就自動停止（磁碟寫滿會讓分段損毀、無法合併）
    async fn check_disk(&self) {
        let dir = {
            let mut st = self.lock();
            // 網路磁碟沒回應時可能超過 10 秒：上一次還沒回來就不再疊加
            if st.disk_checking {
                return;
            }
            st.disk_checking = true;
            st.config().output_dir.clone()
        };
        let free = disk_free(&dir).await;
        let mut notify = None;
        let mut stop_reason = None;
        {
            let mut st = self.lock();
            st.disk_checking = false;
            let Some(free) = free else { return };
            if st.state != RecorderState::Recording {
                return;
            }
            st.disk_free_bytes = Some(free);
            if free < DISK_STOP && !st.auto_stopping {
                st.auto_stopping = true;
                notify = Some(("磁碟空間不足", format!("只剩 {}，已自動停止並儲存", gb(free))));
                stop_reason = Some(format!("儲存位置只剩 {}，自動停止並儲存", gb(free)));
            } else if free < DISK_WARN && !st.disk_warned {
                st.disk_warned = true;
                st.add_log(LogLevel::Warn, &format!("儲存位置只剩 {}，低於 {} 時會自動停止", gb(free), gb(DISK_STOP)));
                notify = Some(("磁碟空間快不夠了", format!("儲存位置只剩 {}", gb(free))));
            }
        }
        if let Some((title, text)) = notify {
            self.deps().notify(title, &text, true);
        }
        if stop_reason.is_some() {
            self.stop_later(stop_reason);
        }
    }

    async fn finalize(&self) -> Result<RecordingResult> {
        let (fps, parts, has_audio, parts_dir, out) = {
            let st = self.lock();
            let parts: Vec<(String, u64)> = st
                .segments
                .iter()
                .filter(|s| s.frames > 0 && std::fs::metadata(&s.file).map(|m| m.len() > 0).unwrap_or(false))
                .map(|s| (s.file.clone(), s.frames))
                .collect();
            (st.config().fps, parts, !st.audio_specs.is_empty(), st.parts_dir.clone().unwrap_or_default(), st.final_path.clone().unwrap_or_default())
        };
        let frames: u64 = parts.iter().map(|p| p.1).sum();
        if parts.is_empty() {
            cleanup_parts(Some(&parts_dir));
            return Ok(RecordingResult { ok: false, message: "沒有擷取到任何畫面，未產生影片".into(), ..Default::default() });
        }
        let ffmpeg = self.deps().ffmpeg_path().ok_or_else(|| Error::other("找不到 ffmpeg.exe"))?;
        let files: Vec<String> = parts.iter().map(|p| p.0.clone()).collect();
        // 有錄聲音：每個分段在聲音結束處截斷（見 concat_list），聲音與畫面一起結束、分段之間也不留無聲的空隙
        let mut outpoints = Vec::new();
        if has_audio {
            let mut tasks = Vec::new();
            for f in &files {
                let (ffmpeg, args) = (ffmpeg.clone(), audio_end_args(f));
                tasks.push(tokio::spawn(async move {
                    let r = run(&ffmpeg, &args, Duration::from_secs(60)).await;
                    let us = OUT_TIME_RE.captures_iter(&r.stdout).last().and_then(|c| c[1].parse::<f64>().ok());
                    us.filter(|us| r.code == 0 && *us > 0.0).map(|us| us / 1e6)
                }));
            }
            for t in tasks {
                outpoints.push(t.await.ok().flatten());
            }
        }
        let list_file = parts_dir.join("concat.txt");
        std::fs::write(&list_file, concat_list(&files, &outpoints))?;
        let out_str = out.display().to_string();
        let parts_str = parts_dir.display().to_string();
        let r = run(&ffmpeg, &concat_args(&list_file.display().to_string(), &out_str), Duration::from_secs(30 * 60)).await;
        if r.code != 0 || !out.exists() {
            let tail = last_lines(&r.stderr, 3);
            let detail = if tail.is_empty() { format!("結束代碼 {}", r.code) } else { tail };
            return Ok(RecordingResult {
                ok: false,
                frames,
                video_sec: frames as f64 / fps,
                parts_dir: Some(parts_str.clone()),
                message: format!("合併失敗：{detail}（分段保留於 {parts_str}）"),
                ..Default::default()
            });
        }

        // 只讀成品的檔頭取得實際長度（毫秒級），不再整檔重讀一遍：長時間錄影停止時省下一半的等待。
        // 分段回報的張數可能多算被強制終止前還沒寫入的幾張，以檔頭長度為準
        let head = run(&ffmpeg, &["-hide_banner", "-i", &out_str], Duration::from_secs(15)).await;
        let duration = parse_media_info(&head.stderr).duration_sec;
        let real_frames = duration.map(|d| js_round(d * fps) as u64).unwrap_or(frames);
        cleanup_parts(Some(&parts_dir));
        Ok(RecordingResult {
            ok: true,
            path: Some(out_str.clone()),
            frames: real_frames,
            video_sec: duration.unwrap_or(frames as f64 / fps),
            bytes: std::fs::metadata(&out).map(|m| m.len()).ok(),
            message: format!("已儲存 {out_str}（{real_frames} 張，{} 個分段）", parts.len()),
            parts_dir: None,
        })
    }
}

#[cfg(test)]
mod tests {
    //! 開始錄影的「準備 / 倒數 / 縮小視窗」階段：取消與連按的競態。
    //! 用假的依賴（不會真的啟動 FFmpeg）；unix 上另有用假 FFmpeg 跑完整流程的測試。
    use super::*;
    use crate::args::ENCODERS;
    use crate::types::AudioConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;

    fn mon() -> MonitorInfo {
        MonitorInfo {
            id: "0:0".into(),
            adapter: 0,
            output: 0,
            adapter_name: "GPU".into(),
            device_name: r"\\.\DISPLAY1".into(),
            display_number: 1,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
            rotation: 1,
        }
    }

    #[derive(Default)]
    struct Fake {
        ffmpeg: Option<PathBuf>,
        gate: OnceLock<watch::Sender<bool>>,
        before: AtomicUsize,
        after: AtomicUsize,
        rec: OnceLock<Recorder>,
        stop_in_before: bool,
        before_areas: Mutex<Vec<Rect>>,
        ui_areas: Mutex<Vec<Rect>>,
        ui_in: Option<bool>,
    }

    impl RecorderDeps for Fake {
        fn ffmpeg_path(&self) -> Option<PathBuf> {
            Some(self.ffmpeg.clone().unwrap_or_else(|| "ffmpeg-not-used.exe".into()))
        }
        fn encoders(&self) -> BoxFut<'_, Encoders> {
            let mut rx = self.gate.get_or_init(|| watch::channel(false).0).subscribe();
            Box::pin(async move {
                let _ = rx.wait_for(|v| *v).await;
                Encoders { cpu: Some(ENCODERS[0]), gpu: vec![] }
            })
        }
        fn prefer_gpu(&self) -> bool {
            false
        }
        fn learn_gpu(&self) {}
        fn monitors(&self) -> Vec<MonitorInfo> {
            vec![mon()]
        }
        fn ddagrab_usable(&self) -> BoxFut<'_, bool> {
            let usable = self.ffmpeg.is_none();
            Box::pin(async move { usable })
        }
        fn before_capture(&self, area: Rect) -> BoxFut<'_, ()> {
            self.before.fetch_add(1, Ordering::SeqCst);
            self.before_areas.lock().unwrap().push(area);
            if self.stop_in_before {
                // 倒數剛結束、正在縮小視窗時按下取消
                drop(self.rec.get().unwrap().stop(None));
            }
            Box::pin(async { tokio::time::sleep(Duration::from_millis(10)).await })
        }
        fn ui_in_area(&self, area: &Rect) -> bool {
            self.ui_areas.lock().unwrap().push(*area);
            self.ui_in.unwrap_or(true)
        }
        fn after_stop(&self) {
            self.after.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Setup {
        rec: Recorder,
        deps: Arc<Fake>,
        dir: tempfile::TempDir,
    }

    impl Setup {
        fn new(fake: Fake) -> Setup {
            let deps = Arc::new(fake);
            deps.gate.get_or_init(|| watch::channel(false).0);
            let rec = Recorder::new(deps.clone());
            let _ = deps.rec.set(rec.clone());
            Setup { rec, deps, dir: tempfile::tempdir().unwrap() }
        }
        fn config(&self) -> RecordConfig {
            RecordConfig {
                source: SourceConfig::Monitor { monitor_id: "0:0".into() },
                fps: 30.0,
                scale: 100.0,
                draw_mouse: true,
                max_minutes: 0.0,
                method: MethodPreference::Auto,
                output_dir: self.dir.path().display().to_string(),
                audio: AudioConfig::default(),
                encoder: None,
                countdown_sec: Some(3.0),
                hide_ui: Some(true),
            }
        }
        fn release(&self) {
            self.deps.gate.get().unwrap().send_replace(true);
        }
        fn parts_left(&self) -> bool {
            std::fs::read_dir(self.dir.path().join(".parts")).map(|mut d| d.next().is_some()).unwrap_or(false)
        }
    }

    fn region(x: f64, y: f64, w: f64, h: f64) -> SourceConfig {
        SourceConfig::Region { x, y, width: w, height: h }
    }

    #[tokio::test]
    async fn cancel_while_preparing() {
        let s = Setup::new(Fake::default());
        let started = s.rec.start(s.config());
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert!(s.rec.starting());
        s.rec.stop(None).await.unwrap();
        s.release();
        started.await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert!(!s.rec.starting());
        assert_eq!(s.deps.before.load(Ordering::SeqCst), 0);
        assert!(!s.parts_left());
    }

    #[tokio::test]
    async fn second_start_while_preparing_is_rejected() {
        let s = Setup::new(Fake::default());
        let first = s.rec.start(s.config());
        let err = s.rec.start(s.config()).await.unwrap_err();
        assert!(err.message().contains("正在準備開始"));
        s.rec.stop(None).await.unwrap();
        s.release();
        first.await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Idle);
    }

    #[tokio::test]
    async fn cancel_during_countdown() {
        let s = Setup::new(Fake::default());
        s.release();
        s.rec.start(s.config()).await.unwrap(); // 進入倒數就回覆
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Countdown);
        assert!(st.countdown_ms.unwrap() > 2000);
        s.rec.stop(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert_eq!(s.deps.before.load(Ordering::SeqCst), 0);
        assert!(!s.parts_left());
    }

    #[tokio::test]
    async fn cancel_while_minimizing_restores_window() {
        let s = Setup::new(Fake { stop_in_before: true, ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), ..s.config() }).await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert_eq!(s.deps.after.load(Ordering::SeqCst), 1); // 已縮小的視窗被還原
        assert!(!s.parts_left());
    }

    #[tokio::test]
    async fn countdown_reports_whether_ui_covers_area() {
        let s = Setup::new(Fake { ui_in: Some(false), ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { source: region(0.0, 0.0, 800.0, 600.0), ..s.config() }).await.unwrap();
        assert_eq!(s.rec.status().countdown_covers_ui, Some(false));
        assert_eq!(s.deps.ui_areas.lock().unwrap().last(), Some(&Rect { x: 0, y: 0, width: 800, height: 600 }));
        s.rec.stop(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(s.rec.status().countdown_covers_ui, None); // 不在倒數就不回報
    }

    #[tokio::test]
    async fn minimizing_gets_capture_area() {
        let s = Setup::new(Fake { stop_in_before: true, ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), source: region(100.0, 50.0, 640.0, 480.0), ..s.config() }).await.unwrap();
        assert_eq!(s.deps.before_areas.lock().unwrap().first(), Some(&Rect { x: 100, y: 50, width: 640, height: 480 }));
    }

    #[tokio::test]
    async fn shutdown_while_preparing_waits_for_cleanup() {
        let s = Setup::new(Fake::default());
        let started = s.rec.start(s.config());
        let done = s.rec.shutdown();
        s.release();
        done.await;
        started.await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert!(!s.parts_left());
    }

    #[tokio::test]
    async fn status_json_matches_ui_contract() {
        let s = Setup::new(Fake::default());
        let v = serde_json::to_value(s.rec.status()).unwrap();
        assert_eq!(v["state"], "idle");
        assert_eq!(v["recordedMs"], 0);
        assert!(v.get("countdownMs").is_none());
        assert!(v.get("result").is_none());
    }

    /// 假的 FFmpeg：
    /// - 錄影分段（含 -progress）：回報進度、寫出分段檔，收到 q 才結束
    /// - 合併（-f concat）：寫出成品
    /// - 讀檔頭（-i 成品）：印出長度
    #[cfg(unix)]
    fn fake_ffmpeg(dir: &Path) -> PathBuf {
        let p = dir.join("ffmpeg");
        let script = r#"#!/bin/bash
for last; do :; done
case " $* " in
  *" -progress "*)
    echo data > "$last"
    i=0
    while true; do
      i=$((i+30))
      printf 'frame=%s\ndup_frames=0\ntotal_size=%s\nprogress=continue\n' "$i" "$((i*100))"
      if read -t 0.2 c; then :; fi
      case "$c" in q*) printf 'frame=%s\ntotal_size=%s\nprogress=end\n' "$i" "$((i*100))"; exit 0;; esac
      c=""
    done
    ;;
  *" concat "*)
    echo merged > "$last"
    exit 0
    ;;
  *)
    echo "  Duration: 00:00:02.50, start: 0" >&2
    exit 1
    ;;
esac
"#;
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn record_pause_resume_stop_merges_segments() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        let cfg = RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() };
        s.rec.start(cfg).await.unwrap();
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Recording);
        assert_eq!(st.method, Some(CaptureMethod::Gdigrab));
        let wait_frames = || async {
            for _ in 0..100 {
                if s.rec.status().frames > 0 && s.rec.status().busy.is_none() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("沒有收到進度");
        };
        wait_frames().await;
        assert!(s.rec.output_path().is_some());
        s.rec.pause().await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Paused);
        s.rec.resume().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let res = s.rec.stop(None).await.unwrap().unwrap();
        assert!(res.ok, "{}", res.message);
        assert_eq!(res.frames, 75); // 2.5 秒 × 30 fps（以檔頭長度為準）
        assert!(res.message.contains("2 個分段"), "{}", res.message);
        let path = PathBuf::from(res.path.unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "merged\n");
        assert!(!s.parts_left());
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Idle);
        assert_eq!(st.segments, 2);
        assert!(st.log.iter().any(|l| l.text.contains("已暫停")));
        assert!(!st.log.iter().any(|l| l.text.contains("強制終止")), "{:?}", st.log);
        assert!(s.rec.output_path().is_none());
        assert_eq!(s.deps.after.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ffmpeg_crash_before_first_frame_stops_with_error() {
        let tools = tempfile::tempdir().unwrap();
        let ff = tools.path().join("ffmpeg");
        std::fs::write(&ff, "#!/bin/sh\necho 'Could not find gdigrab device' >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ff, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(ff), ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
        for _ in 0..100 {
            if s.rec.status().state == RecorderState::Idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Idle);
        assert!(st.log.iter().any(|l| l.text.contains("無法開始擷取") && l.text.contains("gdigrab")), "{:?}", st.log);
        assert_eq!(st.result.unwrap().message, "沒有擷取到任何畫面，未產生影片");
    }
}
