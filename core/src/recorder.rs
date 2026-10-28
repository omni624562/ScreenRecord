//! 錄影狀態機（原速錄影）。
//!
//! - 每次「開始 / 繼續」啟動一個 FFmpeg 行程寫一個分段；「暫停」對它送 q 正常結束。
//! - 「停止」送 q 收尾後，以 concat demuxer（-c copy）合併所有分段成單一 MP4。
//! - FFmpeg 意外結束（例如鎖定畫面、UAC 安全桌面造成 Desktop Duplication 中斷）時自動開新分段續錄。
//!
//! - 回溯錄影（config.replay_sec > 0）：FFmpeg 每 10 秒換一個檔案、定期刪掉太舊的，「存下」時接起最近的幾個（見 replay.rs）；
//!   停止時全部刪掉。
//!
//! 公開操作（開始、暫停、繼續、停止）依序執行，避免連點時互相干擾；狀態放在一把短暫持有的鎖裡，
//! 不在持有鎖時等待（呼叫 FFmpeg、開啟音訊裝置），查詢狀態永遠不會被卡住。

use crate::args::{
    audio_end_args, choose_encoder, concat_args, concat_list, desktop_rect, parse_media_info, resolve_plan, segment_args, startup_fallback, CapturePlan, EncoderSpec, FallbackInput, GpuPath,
    GpuSupport, StartupFallback,
};
use crate::audio::{AudioSourceSpec, Opener};
use crate::audiopipe::{AudioPipe, LogFn};
use crate::format::js_round;
use crate::paths::{now_ms, timestamp, unique_path};
use crate::process::{command, last_errors, last_lines, read_lines, run};
use crate::types::{CaptureMethod, LogEntry, LogLevel, MethodPreference, MonitorInfo, RecordConfig, RecorderState, RecorderStatus, RecordingResult, Rect, ReplayStatus, SourceConfig};
use crate::{tr, trf, Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
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
    /// 實測可用的 HEVC、AV1 顯示卡編碼器
    pub codec: Vec<EncoderSpec>,
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
    /// 實測通過的顯示卡處理（在顯示卡上縮放、轉色彩）
    fn gpu_convert(&self) -> Option<GpuSupport> {
        None
    }
    /// 顯示卡處理在錄影時失敗：這次執行期間不再使用
    fn gpu_convert_failed(&self) {}
    /// 倒數結束、開始擷取前（一定會呼叫）：錄影中不讓電腦睡眠；停止時 after_stop 放掉
    fn keep_awake(&self) {}
    /// 倒數結束、開始擷取前（縮小擋到擷取範圍的操作視窗等；設定不縮小時不呼叫）；area 為擷取範圍（虛擬桌面的實體像素座標）
    fn before_capture(&self, _area: Rect) -> BoxFut<'_, ()> {
        Box::pin(async {})
    }
    /// 操作視窗是否在擷取範圍內（倒數畫面要不要蓋住操作視窗）
    fn ui_in_area(&self, _area: &Rect) -> bool {
        true
    }
    /// 錄影結束（已儲存或失敗）後（還原操作視窗等）
    fn after_stop(&self) {}
    /// 錄影存好了（錄完自動處理）
    fn saved(&self, _path: &str) {}
    /// 可以用 gfxcapture 只錄視窗內容（FFmpeg 有這個濾鏡）
    fn gfxcapture_usable(&self) -> bool {
        false
    }
    /// 視窗目前的範圍（「只錄這個視窗」）；關掉了是 None
    fn window_bounds(&self, id: i64) -> Option<Rect> {
        crate::winui::window_bounds(id)
    }
    /// 存好的錄影裡的標記（影片的秒數）
    fn save_markers(&self, _output: &str, _marks: &Marks) {}
    /// 需要使用者注意的事（系統匣通知）
    fn notify(&self, _title: &str, _text: &str, _warn: bool) {}
    /// 開啟音訊擷取來源（Windows 上是 WASAPI）
    fn audio_opener(&self) -> Opener {
        Arc::new(crate::audio::open_wasapi)
    }
}

/// 聲音來源改錄靜音時的通知（bits：audiopipe::SILENT_*）
fn silent_notice_text(bits: u8) -> (&'static str, &'static str) {
    use crate::audiopipe::{SILENT_MIC, SILENT_SYSTEM};
    let title = match (bits & SILENT_MIC != 0, bits & SILENT_SYSTEM != 0) {
        (true, true) => tr!("麥克風與系統聲音無法使用", "The microphone and system audio aren't working"),
        (true, false) => tr!("麥克風無法使用", "The microphone isn't working"),
        _ => tr!("系統聲音無法使用", "System audio isn't working"),
    };
    let text = if bits & SILENT_MIC != 0 {
        tr!(
            "目前錄到的是靜音。請確認麥克風有接好、沒有被其他程式（例如視訊會議）占用",
            "Silence is being recorded. Check that the microphone is connected and not in use by another app (such as a video call)"
        )
    } else {
        tr!("目前錄到的是靜音。請確認播放裝置（喇叭、耳機）正常", "Silence is being recorded. Check that your playback device (speakers or headphones) is working")
    };
    (title, text)
}

/// 剩餘空間低於這個值不開始錄影
const DISK_MIN_START: u64 = 1 << 30;
/// 錄影中剩餘空間低於這個值提醒一次
const DISK_WARN: u64 = 2 << 30;
/// 錄影中剩餘空間低於這個值自動停止並儲存（合併時還需要約一份影片大小的空間）
const DISK_STOP: u64 = 512 << 20;
const DISK_CHECK_MS: u64 = 10_000;
/// 回溯錄影：每隔多久刪掉太舊的檔案
const REPLAY_PRUNE_MS: u64 = 5_000;

/// 儲存位置所在磁碟的剩餘空間；查不到（某些網路磁碟）回傳 None
pub async fn disk_free(dir: &str) -> Option<u64> {
    let dir = dir.to_string();
    tokio::task::spawn_blocking(move || disk_free_sync(&dir)).await.ok().flatten()
}

#[cfg(windows)]
pub fn disk_free_sync(dir: &str) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
    let mut avail = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut avail), None, None).ok()? };
    Some(avail)
}

#[cfg(not(windows))]
pub fn disk_free_sync(_dir: &str) -> Option<u64> {
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
    /// 太久沒有新畫面，被錄影器結束
    stalled: bool,
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
    /// 標記（已錄的毫秒數，也就是影片裡的時間）
    markers: Vec<u64>,
    /// 滑鼠點擊：(已錄的毫秒數, 在擷取範圍內的位置 0～1)
    clicks: Vec<(u64, f32, f32)>,
    /// 操作步驟：點了什麼、按了哪些快速鍵（錄完可以整理成文字清單）
    actions: Vec<crate::steps_log::Action>,
    /// 最後一次有動靜（滑鼠移動、點擊、按鍵、有聲音）的時間（Unix 毫秒；沒動靜時自動暫停用）
    last_activity: u64,
    /// 上次記下的游標位置（有沒有移動）
    last_pos: Option<(i32, i32)>,
    /// 沒動靜自動暫停中：(暫停的時間)；一有動靜就繼續
    auto_paused: Option<u64>,
    /// 正在自動暫停 / 繼續（不要重複觸發）
    auto_switching: bool,
    /// 游標的位置（剪輯時換成平滑、放大的游標用；見 cursor.rs）
    cursor: Vec<crate::cursor::Sample>,
    /// 按快速鍵標記要放大的地方：(已錄的毫秒數, 在擷取範圍內的位置 0～1)
    zooms: Vec<(u64, f32, f32)>,
    /// 麥克風暫時靜音的時段（已錄的毫秒數）；muted_at = 目前這段從哪裡開始（靜音中）
    mutes: Vec<(u64, u64)>,
    muted_at: Option<u64>,
    busy: Option<String>,
    config: Option<RecordConfig>,
    plan: Option<CapturePlan>,
    method: Option<CaptureMethod>,
    /// 只錄視窗內容（gfxcapture）失敗時改用的擷取方式（跟著視窗的範圍）
    region_method: Option<CaptureMethod>,
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
    /// 在顯示卡上縮放、轉色彩（失敗時清掉，改回 CPU 轉換）
    gpu: Option<GpuPath>,
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
    /// 回溯錄影：正在存、這次存了幾支、最近存下的檔案、緩衝的大小、上次整理（刪掉太舊的檔案）的時間
    replay_saving: bool,
    replay_saved: u32,
    replay_last: Option<String>,
    replay_bytes: u64,
    replay_pruned_at: u64,
    /// 開始不了的原因（每次錄影重設）
    fatal: Option<String>,
    /// 聲音來源以靜音代替（audiopipe::SILENT_*）：現在、這次錄影曾經、曾經正常錄到的、已經通知過的
    silent_now: u8,
    silent_ever: u8,
    audio_ok_ever: u8,
    silent_notified: u8,
}

impl St {
    /// 有動靜（錄影或暫停中）：沒動靜自動暫停的計時重來；自動暫停中的話接著繼續
    fn note_activity(&mut self) {
        if matches!(self.state, RecorderState::Recording | RecorderState::Paused) {
            self.last_activity = now_ms();
        }
    }

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

    /// 錄影結果的 silent_audio：整支都是靜音的來源（SILENT_*）與只有一段是靜音的來源（PARTLY_*）
    fn silent_result(&self) -> u8 {
        let full = self.silent_ever & !self.audio_ok_ever;
        let partly = self.silent_ever & self.audio_ok_ever;
        full | (partly << 2)
    }

    /// None = 這次沒錄麥克風；Some(true) = 暫時靜音中
    fn mic_muted(&self) -> Option<bool> {
        self.config.as_ref().filter(|c| c.audio.mic).map(|_| self.muted_at.is_some())
    }

    /// 錄完：靜音的時段（還在靜音中的算到結尾），並取消靜音
    fn take_mutes(&mut self) -> Vec<[f64; 2]> {
        if let Some(a) = self.muted_at.take() {
            let t = self.recorded_ms();
            if t > a {
                self.mutes.push((a, t));
            }
        }
        crate::audiopipe::MIC_MUTED.store(false, std::sync::atomic::Ordering::Relaxed);
        std::mem::take(&mut self.mutes).into_iter().map(|(a, b)| [a as f64 / 1000.0, b as f64 / 1000.0]).collect()
    }

    fn reset_mutes(&mut self) {
        self.mutes.clear();
        self.muted_at = None;
        crate::audiopipe::MIC_MUTED.store(false, std::sync::atomic::Ordering::Relaxed);
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

    /// 回溯錄影的長度（秒）；一般錄影是 None
    fn replay_sec(&self) -> Option<u32> {
        self.config.as_ref().map(|c| c.replay_sec).filter(|s| *s > 0)
    }
}

/// 把分段接成一支影片（concat，不重新壓縮）。有錄聲音時每個分段在聲音結尾截斷（見 concat_list），
/// 聲音與畫面一起結束、分段之間也不留無聲的空隙。chapters：寫進影片的章節（錄影時加的標記）。
/// 先合併到分段資料夾裡的暫存檔，成功才重新命名成 out：合併失敗或逾時時不會留下看起來正常、其實殘缺的錄影
/// （分段留著，下次開啟時可以救回）。成功時回傳成品的長度（秒，讀不到時 None），失敗時回傳原因
pub(crate) async fn merge_segments(ffmpeg: &Path, files: &[String], parts_dir: &Path, out: &Path, has_audio: bool, chapters: Option<String>) -> std::result::Result<Option<f64>, String> {
    let mut outpoints = Vec::new();
    if has_audio {
        // 同時最多 AUDIO_END_CONCURRENCY 個：存重播時錄影還在進行，一次開幾十個 FFmpeg 會搶走錄影的 CPU、
        // 也會讓這些工作互相拖慢到逾時（讀不到結尾的分段就不截斷聲音）
        let sem = Arc::new(tokio::sync::Semaphore::new(AUDIO_END_CONCURRENCY));
        let mut tasks = Vec::new();
        for f in files {
            let (ffmpeg, args, sem) = (ffmpeg.to_path_buf(), audio_end_args(f), sem.clone());
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await;
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
    std::fs::write(&list_file, concat_list(files, &outpoints)).map_err(|e| crate::error::plain(&e))?;
    // 合併要和分段一樣大的空間：不夠時先說，不要做到一半才失敗
    let total: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
    let out_dir = out.parent().map(|d| d.display().to_string()).unwrap_or_default();
    if let Some(free) = disk_free(&out_dir).await {
        if free < total + MERGE_SPARE {
            return Err(trf!(
                "磁碟空間不夠合併（需要約 {}，剩 {}）。分段都還在，清出空間後重新開啟程式就能救回",
                "Not enough disk space to merge (needs about {}, {} free). The segments are kept; free up space and reopen the app to recover them",
                crate::format::format_bytes(total + MERGE_SPARE),
                crate::format::format_bytes(free)
            ));
        }
    }
    let tmp_out = parts_dir.join("merged.mp4");
    let _ = std::fs::remove_file(&tmp_out);
    let out_str = tmp_out.display().to_string();
    let meta = match chapters {
        Some(c) => {
            let f = parts_dir.join("chapters.txt");
            std::fs::write(&f, c).map_err(|e| crate::error::plain(&e))?;
            Some(f.display().to_string())
        }
        None => None,
    };
    // HEVC 的錄影：合併時標成 hvc1（Windows、Mac 的播放程式才認得）
    let hevc = match files.first() {
        Some(f) => run(ffmpeg, &["-hide_banner".to_string(), "-i".into(), f.clone()], Duration::from_secs(30)).await.stderr.contains("Video: hevc"),
        None => false,
    };
    let r = run(ffmpeg, &concat_args(&list_file.display().to_string(), &out_str, meta.as_deref(), hevc), merge_timeout(total)).await;
    if r.code != 0 || !tmp_out.exists() {
        let _ = std::fs::remove_file(&tmp_out);
        let tail = last_lines(&r.stderr, 3);
        return Err(if tail.is_empty() { trf!("結束代碼 {}", "exit code {}", r.code) } else { tail });
    }
    if let Err(e) = std::fs::rename(&tmp_out, out) {
        let _ = std::fs::remove_file(&tmp_out);
        return Err(e.to_string());
    }
    // 只讀成品的檔頭取得實際長度（毫秒級），不再整檔重讀一遍：長時間錄影停止時省下一半的等待
    let out_str = out.display().to_string();
    let head = run(ffmpeg, &["-hide_banner", "-i", &out_str], Duration::from_secs(15)).await;
    Ok(parse_media_info(&head.stderr).duration_sec)
}

/// 合併前讀各分段聲音結尾時，同時最多幾個 FFmpeg
const AUDIO_END_CONCURRENCY: usize = 3;

/// 合併時除了分段大小，另外要留的空間（檔頭、章節）
const MERGE_SPARE: u64 = 64 * 1024 * 1024;

/// 合併多久算卡住：至少 30 分鐘，大檔案依每秒 5 MB 估（存在 NAS、慢的硬碟上也夠）
fn merge_timeout(total_bytes: u64) -> Duration {
    Duration::from_secs(30 * 60 + total_bytes / (5 * 1024 * 1024))
}

pub(crate) fn cleanup_parts(parts_dir: Option<&Path>) {
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

/// 螢幕上的錄影範圍外框與控制列要顯示的內容
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    /// 擷取範圍（虛擬桌面的實體像素座標）
    pub area: Rect,
    pub state: RecorderState,
    pub recorded_ms: u64,
    /// 倒數中：剩下的毫秒數
    pub countdown_ms: Option<u64>,
    /// 加了幾個標記
    pub markers: u32,
    /// 錄整個螢幕（不是自訂範圍）：不顯示外框，控制列放在螢幕上方中間（可以拖到別的地方，例如另一個螢幕）
    pub full: bool,
    /// 回溯錄影中：控制列是「存下」與「停止」（不能暫停、加標記）
    pub replay: bool,
    /// 麥克風：None = 這次沒錄麥克風；Some(true) = 暫時靜音中
    pub mic_muted: Option<bool>,
}

/// 錄影時記下的時間點：標記與滑鼠點擊（存在 markers/，剪輯時用）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Marks {
    /// 標記（影片的秒數）
    #[serde(default)]
    pub markers: Vec<f64>,
    /// 滑鼠點擊：[影片的秒數, x, y]（x、y 是在畫面裡的位置 0～1）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clicks: Vec<[f64; 3]>,
    /// 游標的位置：[影片的秒數, x, y]（位置有變才記）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cursor: Vec<crate::cursor::Sample>,
    /// 錄影時沒有把游標錄進影片（剪輯時換成大游標才不會重疊）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_cursor: bool,
    /// 操作步驟：點了什麼、按了哪些快速鍵
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<crate::steps_log::Action>,
    /// 麥克風暫時靜音的時段：[開始秒, 結束秒]（剪輯時標在時間軸上）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutes: Vec<[f64; 2]>,
    /// 按快速鍵標記要放大的地方：[影片的秒數, x, y]（剪輯時的「跟著點擊放大」改用這些）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zooms: Vec<[f64; 3]>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.markers.is_empty() && self.clicks.is_empty() && self.cursor.is_empty() && self.mutes.is_empty() && self.zooms.is_empty() && self.actions.is_empty()
    }
}

/// 錄影時要在畫面上顯示點擊 / 按鍵用的資訊
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayInfo {
    /// 擷取範圍（虛擬桌面的實體像素座標）
    pub area: Rect,
    pub show_clicks: bool,
    pub show_keys: bool,
    pub cursor_halo: bool,
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
    pub fn start(&self, mut config: RecordConfig) -> BoxFut<'static, Result<()>> {
        // 只錄某個視窗：用視窗現在的位置與大小（選了之後可能動過）；視窗已關閉就照原本的範圍
        if let (Some(id), SourceConfig::Region { .. }) = (config.follow_window, &config.source) {
            match self.deps().window_bounds(id) {
                Some(b) => config.source = SourceConfig::Region { x: b.x as f64, y: b.y as f64, width: (b.width & !1) as f64, height: (b.height & !1) as f64 },
                None => config.follow_window = None,
            }
        }
        {
            let mut st = self.lock();
            // 連按兩次（例如快速鍵）不要排出第二個開始：否則取消第一個後，第二個仍會倒數並錄影
            if st.starting_now || st.state != RecorderState::Idle {
                return Box::pin(async { Err(Error::config(tr!("目前已在錄影或正在準備開始", "Already recording or getting ready to start"))) });
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
                    me.deps().notify(tr!("無法開始錄影", "Couldn't start recording"), e.message(), true);
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
        if self.lock().replay_sec().is_some() {
            return Err(Error::config(tr!("回溯錄影不能暫停", "The replay buffer can't be paused")));
        }
        let _q = self.0.queue.lock().await;
        self.do_pause().await
    }

    pub async fn resume(&self) -> Result<()> {
        let _q = self.0.queue.lock().await;
        self.do_resume()
    }

    /// 移動自訂範圍（大小不變，限制在桌面內），回傳移動後的範圍。
    /// 倒數或暫停中直接換位置；錄影中結束目前的分段，從新位置開新分段（中間約 0.5～1 秒沒錄到，停止時一樣合併成一個檔案）
    pub async fn move_region(&self, x: i32, y: i32) -> Result<Rect> {
        // 倒數期間 start 佔著佇列：不排隊，直接換（還沒有分段在錄）
        if self.lock().state != RecorderState::Countdown {
            let _q = self.0.queue.lock().await;
            return self.do_move(x, y).await;
        }
        self.do_move(x, y).await
    }

    async fn do_move(&self, x: i32, y: i32) -> Result<Rect> {
        let monitors = self.deps().monitors();
        let (plan, config, recording, gfx) = {
            let st = self.lock();
            let recording = match st.state {
                RecorderState::Recording => true,
                RecorderState::Countdown | RecorderState::Paused => false,
                _ => return Err(Error::config(tr!("目前沒有在錄影", "Not recording"))),
            };
            let (Some(cfg), Some(old)) = (st.config.as_ref(), st.plan.as_ref()) else {
                return Err(Error::config(tr!("目前沒有在錄影", "Not recording")));
            };
            if !matches!(cfg.source, SourceConfig::Region { .. }) {
                return Err(Error::config(tr!("只有自訂範圍可以移動", "Only a custom area can be moved")));
            }
            let r = old.rect;
            let dk = desktop_rect(&monitors);
            let (nx, ny) = if dk.width > 0 { (x.clamp(dk.x, (dk.x + dk.width - r.width).max(dk.x)), y.clamp(dk.y, (dk.y + dk.height - r.height).max(dk.y))) } else { (x, y) };
            if (nx, ny) == (r.x, r.y) {
                return Ok(r);
            }
            let config = RecordConfig { source: SourceConfig::Region { x: nx as f64, y: ny as f64, width: r.width as f64, height: r.height as f64 }, ..cfg.clone() };
            let plan = resolve_plan(&config, &monitors)?;
            let gfx = st.method == Some(CaptureMethod::Gfxcapture);
            if st.method == Some(CaptureMethod::Ddagrab) && plan.dda.is_none() {
                return Err(Error::config(tr!("新位置跨到不同顯示卡上的螢幕，這次錄影無法移過去", "The new position spans screens on different graphics cards, so this recording can't move there")));
            }
            (plan, config, recording, gfx)
        };
        let rect = plan.rect;
        // 只錄視窗內容：視窗移到哪裡都錄得到，不用重開分段；只更新範圍（錄影外框、點擊效果、改用範圍時的位置）
        if gfx {
            let mut st = self.lock();
            st.plan = Some(plan);
            st.config = Some(config);
            return Ok(rect);
        }
        if recording {
            {
                let mut st = self.lock();
                st.close_span();
                st.state = RecorderState::Paused;
                st.busy = Some(tr!("正在移動範圍…", "Moving area…").into());
            }
            self.stop_current().await;
        }
        {
            let mut st = self.lock();
            st.plan = Some(plan);
            st.config = Some(config);
            st.add_log(LogLevel::Info, &trf!("錄影範圍移到 ({}, {})", "Capture area moved to ({}, {})", rect.x, rect.y));
            if !recording {
                return Ok(rect);
            }
            st.busy = None;
            // 移動的期間按了停止：不再開新分段
            if st.state != RecorderState::Paused {
                return Ok(rect);
            }
            st.state = RecorderState::Recording;
            st.span_start = Some(now_ms());
        }
        if !self.try_start_segment() {
            return Err(Error::config(tr!("無法在新位置繼續錄影，已停止並儲存先前錄到的部分", "Couldn't continue recording at the new position. Stopped and saved what was recorded so far")));
        }
        Ok(rect)
    }

    /// 螢幕配置改變了（接上、拔掉外接螢幕、改解析度、改排列；App 偵測到時呼叫）：
    /// 錄的螢幕不見或改了解析度：馬上停止並存下已錄的部分（不再重試一兩分鐘）；
    /// 只是編號或位置變了（例如拔掉另一台）：重新對應到同一台螢幕，開新分段繼續錄（停止時一樣合併成一個檔案）
    pub async fn displays_changed(&self) {
        // 倒數期間 start 佔著佇列：不排隊（和移動範圍一樣）
        let _q = if self.lock().state != RecorderState::Countdown { Some(self.0.queue.lock().await) } else { None };
        let monitors = self.deps().monitors();
        let (change, recording, old_plan) = {
            let st = self.lock();
            let recording = match st.state {
                RecorderState::Recording => true,
                RecorderState::Countdown | RecorderState::Paused => false,
                _ => return,
            };
            // 只錄視窗內容：和螢幕排列無關
            if st.method == Some(CaptureMethod::Gfxcapture) || st.auto_stopping {
                return;
            }
            let (Some(cfg), Some(plan)) = (st.config.as_ref(), st.plan.as_ref()) else { return };
            (crate::displays::check(cfg, &plan.monitors, &monitors), recording, plan.clone())
        };
        let stop = |me: &Self, title: &str, text: String| {
            {
                let mut st = me.lock();
                st.auto_stopping = true;
                st.retry = None;
                st.add_log(LogLevel::Warn, &text);
            }
            me.deps().notify(title, &text, true);
            me.stop_later(Some(text));
        };
        let new_config = match change {
            crate::displays::LayoutChange::Same => return,
            crate::displays::LayoutChange::Gone(name) => {
                return stop(
                    self,
                    tr!("錄的螢幕不見了", "The screen being recorded is gone"),
                    trf!("{name} 已拔掉或關掉，已停止錄影並存下之前錄到的部分", "{name} was disconnected or turned off. Recording stopped and what was recorded so far was saved"),
                );
            }
            crate::displays::LayoutChange::Resized(name) => {
                return stop(
                    self,
                    tr!("螢幕解析度改變了", "Screen resolution changed"),
                    trf!("{name} 的解析度改變了，已停止錄影並存下之前錄到的部分", "The resolution of {name} changed. Recording stopped and what was recorded so far was saved"),
                );
            }
            crate::displays::LayoutChange::Remap(c) => *c,
        };
        // 重新對應：輸出大小要一樣才接得起來（同一個檔案）
        let plan = match resolve_plan(&new_config, &monitors) {
            Ok(p) if (p.out_width, p.out_height) == (old_plan.out_width, old_plan.out_height) => p,
            _ => {
                return stop(
                    self,
                    tr!("螢幕排列改變了", "Screen arrangement changed"),
                    tr!("螢幕的排列改變了，已停止錄影並存下之前錄到的部分", "The screen arrangement changed. Recording stopped and what was recorded so far was saved").to_string(),
                );
            }
        };
        if recording {
            {
                let mut st = self.lock();
                st.close_span();
                st.state = RecorderState::Paused;
                st.busy = Some(tr!("螢幕排列改變，重新對應中…", "Screens changed; reconnecting…").into());
            }
            self.stop_current().await;
        }
        {
            let mut st = self.lock();
            st.plan = Some(plan);
            st.config = Some(new_config);
            st.retry = None;
            st.add_log(LogLevel::Info, tr!("螢幕的編號或位置改變了，繼續錄同一台螢幕", "Screen numbering or position changed; still recording the same screen"));
            if !recording {
                return;
            }
            st.busy = None;
            // 這段期間按了停止：不再開新分段
            if st.state != RecorderState::Paused {
                return;
            }
            st.state = RecorderState::Recording;
            st.span_start = Some(now_ms());
        }
        if !self.try_start_segment() {
            self.stop_later(Some(
                tr!("螢幕改變後無法繼續錄影，已停止並儲存先前錄到的部分", "Couldn't continue recording after the screens changed. Stopped and saved what was recorded so far").into(),
            ));
        }
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

    /// 錄影中（含儲存中）要寫入的成品路徑：不能重新命名或刪除
    /// 正在錄影的分段資料夾（救回中斷的錄影時不能動它）。
    /// 已經停止（例如合併失敗）時是 None：那個資料夾就是要救回的
    pub fn parts_dir(&self) -> Option<PathBuf> {
        let st = self.lock();
        if st.state == RecorderState::Idle {
            return None;
        }
        st.parts_dir.clone()
    }

    /// 錄好的檔案重新命名了：結果卡跟著顯示新的檔案
    pub fn result_renamed(&self, from: &str, to: &str) {
        if let Some(r) = self.lock().result.as_mut() {
            if r.path.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(from)) {
                r.path = Some(to.to_string());
            }
        }
    }

    pub fn output_path(&self) -> Option<PathBuf> {
        let st = self.lock();
        if st.state != RecorderState::Idle && st.replay_sec().is_none() {
            st.final_path.clone()
        } else {
            None
        }
    }

    /// 錄自訂範圍時，螢幕上的外框與控制列要顯示的內容（倒數、錄影、暫停期間；其他時候 None）
    pub fn frame_info(&self) -> Option<FrameInfo> {
        let st = self.lock();
        if !matches!(st.state, RecorderState::Countdown | RecorderState::Recording | RecorderState::Paused) {
            return None;
        }
        // 只錄聲音：沒有範圍，只在螢幕上方顯示小控制列
        let c = st.config.as_ref()?;
        let full = c.audio_only || !matches!(c.source, SourceConfig::Region { .. });
        let countdown_ms = if st.state == RecorderState::Countdown { st.countdown.as_ref().map(|c| c.ends_at.saturating_sub(now_ms())) } else { None };
        Some(FrameInfo {
            area: st.plan.as_ref()?.rect,
            state: st.state,
            recorded_ms: st.recorded_ms(),
            countdown_ms,
            markers: st.markers.len() as u32,
            full,
            replay: st.replay_sec().is_some(),
            mic_muted: st.mic_muted(),
        })
    }

    /// 攝影機小視窗：倒數、錄影、暫停中，而且選了攝影機（只錄聲音時沒有）
    pub fn camera_info(&self) -> Option<crate::camera_bubble::BubbleInfo> {
        let st = self.lock();
        if !matches!(st.state, RecorderState::Countdown | RecorderState::Recording | RecorderState::Paused) {
            return None;
        }
        let c = st.config.as_ref().filter(|c| !c.audio_only)?;
        let camera = c.camera.clone().filter(|cam| !cam.device.trim().is_empty())?;
        let plan = st.plan.as_ref()?;
        Some(crate::camera_bubble::BubbleInfo { area: plan.rect, monitors: plan.monitors.clone(), camera })
    }

    /// 其他部分（攝影機小視窗等）的警告寫進這次錄影的事件紀錄
    pub fn log_warn(&self, text: &str) {
        self.add_log(LogLevel::Warn, text);
    }

    /// 錄影中或暫停中（不含倒數）：擷取範圍與要不要顯示點擊、按鍵
    pub fn overlay_info(&self) -> Option<OverlayInfo> {
        let st = self.lock();
        if !matches!(st.state, RecorderState::Recording | RecorderState::Paused) {
            return None;
        }
        let c = st.config.as_ref().filter(|c| !c.audio_only)?;
        Some(OverlayInfo { area: st.plan.as_ref()?.rect, show_clicks: c.show_clicks, show_keys: c.show_keys, cursor_halo: c.cursor_halo })
    }

    /// 「只錄這個視窗」：(視窗代碼, 目前的擷取範圍)；倒數、錄影、暫停中才有
    pub fn follow_info(&self) -> Option<(i64, Rect)> {
        let st = self.lock();
        if !matches!(st.state, RecorderState::Countdown | RecorderState::Recording | RecorderState::Paused) || st.busy.is_some() {
            return None;
        }
        Some((st.config.as_ref()?.follow_window?, st.plan.as_ref()?.rect))
    }

    /// 記下一次滑鼠點擊（桌面的實體像素座標）；範圍外、暫停中不記。回傳影片裡的時間（秒），之後用來記下點了什麼
    pub fn add_click(&self, x: i32, y: i32) -> Option<f64> {
        let mut st = self.lock();
        st.note_activity();
        if st.state != RecorderState::Recording || st.clicks.len() >= 20_000 || st.config.as_ref().is_some_and(|c| c.audio_only) {
            return None;
        }
        let r = st.plan.as_ref().map(|p| p.rect)?;
        if x < r.x || y < r.y || x >= r.x + r.width || y >= r.y + r.height || r.width <= 0 || r.height <= 0 {
            return None;
        }
        let t = st.recorded_ms();
        let (fx, fy) = ((x - r.x) as f32 / r.width as f32, (y - r.y) as f32 / r.height as f32);
        st.clicks.push((t, fx, fy));
        Some(t as f64 / 1000.0)
    }

    /// 記下一個操作步驟（點到的東西由另一個執行緒讀出來，可能晚一點才到；停止後就不收了）
    pub fn add_action(&self, a: crate::steps_log::Action) {
        let mut st = self.lock();
        if !matches!(st.state, RecorderState::Recording | RecorderState::Paused) || st.actions.len() >= crate::steps_log::MAX_ACTIONS {
            return;
        }
        st.actions.push(a);
    }

    /// 錄影中按了快速鍵（「Ctrl + S」）：記成操作步驟；暫停中、只錄聲音時不記。window：在哪個視窗
    pub fn add_key(&self, label: &str, window: &str) {
        let t = {
            let mut st = self.lock();
            st.note_activity();
            if st.state != RecorderState::Recording || st.config.as_ref().is_some_and(|c| c.audio_only) {
                return;
            }
            st.recorded_ms() as f64 / 1000.0
        };
        self.add_action(crate::steps_log::Action { t, kind: crate::steps_log::KEY, name: label.to_string(), role: String::new(), window: window.to_string() });
    }

    /// 記下游標的位置（桌面的實體像素座標；錄影中約每 30 毫秒一次）：剪輯時換成平滑、放大的游標
    pub fn add_cursor(&self, x: i32, y: i32) {
        let mut st = self.lock();
        if st.last_pos != Some((x, y)) {
            if st.last_pos.is_some() {
                st.note_activity();
            }
            st.last_pos = Some((x, y));
        }
        if st.state != RecorderState::Recording || st.config.as_ref().is_none_or(|c| c.audio_only || c.replay_sec > 0) {
            return;
        }
        let Some(r) = st.plan.as_ref().map(|p| p.rect).filter(|r| r.width > 0 && r.height > 0) else { return };
        let t = st.recorded_ms() as f64 / 1000.0;
        let (fx, fy) = ((x - r.x) as f64 / r.width as f64, (y - r.y) as f64 / r.height as f64);
        crate::cursor::push(&mut st.cursor, t, fx, fy);
    }

    /// 錄影中加標記（記下目前錄到的時間，剪輯時可以直接跳過去）；回傳這是第幾個標記
    pub fn add_marker(&self) -> Result<usize> {
        let mut st = self.lock();
        if !matches!(st.state, RecorderState::Recording | RecorderState::Paused) {
            return Err(Error::config(tr!("錄影中才能加標記", "Markers can only be added while recording")));
        }
        if st.replay_sec().is_some() {
            return Err(Error::config(tr!("回溯錄影不能加標記", "Markers can't be added to the replay buffer")));
        }
        let t = st.recorded_ms();
        // 連按兩次（半秒內）只算一個
        if st.markers.last().is_some_and(|m| t.saturating_sub(*m) < 500) {
            return Ok(st.markers.len());
        }
        st.markers.push(t);
        let n = st.markers.len();
        st.add_log(LogLevel::Info, &trf!("標記 {n}（{}）", "Marker {n} ({})", crate::format::video_clock(t as f64 / 1000.0)));
        Ok(n)
    }

    /// 錄影中麥克風靜音 / 取消靜音（只有這次有錄麥克風時）；回傳現在是不是靜音中
    pub fn toggle_mic_mute(&self) -> Result<bool> {
        let mut st = self.lock();
        if !matches!(st.state, RecorderState::Recording | RecorderState::Paused) {
            return Err(Error::config(tr!("錄影中才能把麥克風靜音", "The microphone can only be muted while recording")));
        }
        if st.mic_muted().is_none() {
            return Err(Error::config(tr!("這次錄影沒有錄麥克風", "This recording doesn't include the microphone")));
        }
        let t = st.recorded_ms();
        let muted = match st.muted_at.take() {
            Some(a) => {
                if t > a {
                    st.mutes.push((a, t));
                }
                false
            }
            None => {
                st.muted_at = Some(t);
                true
            }
        };
        crate::audiopipe::MIC_MUTED.store(muted, std::sync::atomic::Ordering::Relaxed);
        let at = crate::format::video_clock(t as f64 / 1000.0);
        st.add_log(LogLevel::Info, &if muted { trf!("麥克風靜音（{at}）", "Microphone muted ({at})") } else { trf!("麥克風取消靜音（{at}）", "Microphone unmuted ({at})") });
        Ok(muted)
    }

    /// 錄影中按快速鍵標記要放大的地方（游標現在的位置，桌面的實體像素座標）；回傳這是第幾個
    pub fn add_zoom_mark(&self, x: i32, y: i32) -> Result<usize> {
        let mut st = self.lock();
        if !matches!(st.state, RecorderState::Recording | RecorderState::Paused) {
            return Err(Error::config(tr!("錄影中才能標記要放大的地方", "Zoom spots can only be marked while recording")));
        }
        if st.replay_sec().is_some() || st.config.as_ref().is_some_and(|c| c.audio_only) {
            return Err(Error::config(tr!("回溯錄影、只錄聲音時不能標記要放大的地方", "Zoom spots can't be marked in the replay buffer or audio-only recordings")));
        }
        let Some(r) = st.plan.as_ref().map(|p| p.rect).filter(|r| r.width > 0 && r.height > 0) else { return Err(Error::config(tr!("還沒開始擷取畫面", "Capture hasn't started yet"))) };
        if x < r.x || y < r.y || x >= r.x + r.width || y >= r.y + r.height {
            return Err(Error::config(tr!("游標不在錄影範圍裡，請把游標移到要放大的地方再按", "The cursor isn't inside the recording area. Move it to the spot to zoom in on and press again")));
        }
        let t = st.recorded_ms();
        let (fx, fy) = ((x - r.x) as f32 / r.width as f32, (y - r.y) as f32 / r.height as f32);
        // 連按兩次（半秒內）只算一個，換成新的位置
        if st.zooms.last().is_some_and(|z| t.saturating_sub(z.0) < 500) {
            st.zooms.pop();
        }
        st.zooms.push((t, fx, fy));
        let n = st.zooms.len();
        st.add_log(LogLevel::Info, &trf!("標記要放大的地方 {n}（{}）", "Zoom spot {n} ({})", crate::format::video_clock(t as f64 / 1000.0)));
        Ok(n)
    }

    /// 正在準備開始（狀態可能仍是待命）；快速鍵用來判斷再按一次是「取消」
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
                busy = Some(tr!("正在啟動擷取…", "Starting capture…").into());
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
                markers: st.markers.len() as u32,
                mic_muted: if st.state == RecorderState::Idle { None } else { st.mic_muted() },
                auto_paused: st.state == RecorderState::Paused && st.auto_paused.is_some(),
                result: st.result.clone(),
                log: st.log.clone(),
                silent_audio: if st.state == RecorderState::Idle { 0 } else { st.silent_now },
                levels: if recording { cur.and_then(|c| c.audio.as_ref()).map(|a| a.levels()) } else { None },
                replay: st.replay_sec().filter(|_| st.state != RecorderState::Idle).map(|sec| ReplayStatus {
                    sec,
                    bytes: st.replay_bytes,
                    saving: st.replay_saving,
                    saved: st.replay_saved,
                    last: st.replay_last.clone(),
                }),
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
                let _ = me.stop(Some(tr!("程式結束", "App is closing").into())).await;
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
            st.add_log(LogLevel::Info, tr!("已取消，沒有開始錄影", "Canceled, recording not started"));
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

    async fn do_start(&self, mut config: RecordConfig, began: &mut Option<oneshot::Sender<()>>) -> Result<()> {
        if self.lock().state != RecorderState::Idle {
            return Err(Error::config(tr!("目前已在錄影中", "Already recording")));
        }
        let deps = self.0.deps.clone();
        if deps.ffmpeg_path().is_none() {
            return Err(Error::config(tr!("找不到 ffmpeg.exe，請先依畫面指示下載", "ffmpeg.exe not found. Download it first by following the on-screen instructions")));
        }
        if config.audio_only {
            if !config.audio.system && !config.audio.mic {
                return Err(Error::config(tr!("只錄聲音時，請至少開啟「系統聲音」或「麥克風」", "For audio-only recording, turn on “System audio” or “Microphone”")));
            }
            // 畫面是一張卡片：螢幕相關的設定都用不到，也不用縮小操作視窗
            config = RecordConfig {
                fps: crate::audio_card::FPS,
                scale: 100.0,
                draw_mouse: false,
                hide_ui: Some(false),
                show_clicks: false,
                show_keys: false,
                cursor_halo: false,
                hide_icons: false,
                follow_window: None,
                camera: None,
                ..config
            };
        }
        if config.replay_sec > 0 {
            // 回溯錄影：馬上開始、一直錄到停止（存下的時候才需要），操作視窗不縮小
            config = RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), max_minutes: 0.0, ..config };
        }
        // 單一螢幕：依實體螢幕找回現在的編號（拔掉、接上螢幕後順序可能變了；系統匣、快速鍵、排程開始時也一樣）
        crate::displays::rebind(&mut config, &deps.monitors()).map_err(Error::config)?;
        let mut plan = resolve_plan(&config, &deps.monitors())?;
        let encoders = deps.encoders().await;
        let enc_pref = config.encoder.unwrap_or_default();
        let (enc, reason) = choose_encoder(enc_pref, plan.out_width, plan.out_height, config.fps, encoders.cpu, &encoders.gpu, deps.prefer_gpu())?;
        // HEVC / AV1：換成同一張顯示卡的那種編碼器；不能用時維持 H.264（開始時失敗也會自動改回 H.264）
        let (enc, reason): (EncoderSpec, String) = match crate::args::choose_codec(config.codec, enc_pref, &enc, &encoders.codec) {
            Ok((e, why)) => (e, why),
            Err(why) if why.is_empty() => (enc, reason.to_string()),
            Err(why) => (enc, format!("{reason}；{why}")),
        };
        let method = match config.method {
            _ if config.audio_only => CaptureMethod::Gdigrab,
            MethodPreference::Gdigrab => CaptureMethod::Gdigrab,
            MethodPreference::Ddagrab => {
                if plan.dda.is_none() {
                    return Err(Error::config(tr!(
                        "此範圍涵蓋不同顯示卡上的螢幕，ddagrab 無法擷取，請改用 gdigrab 或自動",
                        "This area spans screens on different graphics cards, which ddagrab can't capture. Switch to gdigrab or Auto"
                    )));
                }
                if !deps.ddagrab_usable().await {
                    return Err(Error::config(tr!("這台電腦無法使用 ddagrab，請改用 gdigrab 或自動", "ddagrab isn't available on this computer. Switch to gdigrab or Auto")));
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
        // 只錄視窗內容：Windows Graphics Capture；失敗時改回上面選的方式（跟著視窗的範圍）
        let region_method = method;
        let method = if config.window_capture && config.follow_window.is_some() && !config.audio_only && deps.gfxcapture_usable() { CaptureMethod::Gfxcapture } else { region_method };

        let stamp = timestamp();
        let output_dir = config.output_dir.trim().to_string();
        std::fs::create_dir_all(&output_dir).map_err(|e| Error::config(trf!("無法建立儲存資料夾：{e}", "Couldn't create the save folder: {e}")))?;
        let free = disk_free(&output_dir).await;
        if let Some(f) = free.filter(|f| *f < DISK_MIN_START) {
            return Err(Error::config(trf!("儲存位置剩餘空間只有 {}，請清出空間或改存到其他磁碟", "Only {} free at the save location. Free up space or save to another drive", gb(f))));
        }
        let parts_dir = Path::new(&output_dir).join(".parts").join(&stamp);
        std::fs::create_dir_all(&parts_dir)?;
        crate::recovery::mark_owner(&parts_dir);
        if config.audio_only {
            // 卡片放在分段資料夾裡（意外中斷續錄時還要用，合併完一起刪掉）
            let card = parts_dir.join("card.png");
            let when = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            crate::audio_card::render(&when).and_then(|pm| pm.save_png(&card).ok()).ok_or_else(|| Error::config(tr!("無法建立錄音的畫面", "Couldn't create the image for the audio recording")))?;
            plan.card = Some(card.display().to_string());
        }
        let prefix = if config.audio_only { "錄音" } else { "Rec" };
        let final_path = unique_path(Path::new(&output_dir), &format!("{prefix}_{stamp}"), ".mp4");
        let mut audio_specs = Vec::new();
        if config.audio.system {
            audio_specs.push(AudioSourceSpec { loopback: true, mic_id: String::new(), app: config.audio.app.clone() });
        }
        if config.audio.mic {
            audio_specs.push(AudioSourceSpec { loopback: false, mic_id: config.audio.mic_id.clone(), app: String::new() });
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
            st.region_method = Some(region_method);
            st.enc = Some(enc);
            st.cpu_encoder = encoders.cpu;
            st.gpu_available = !encoders.gpu.is_empty();
            st.gpu = GpuPath::choose(deps.gpu_convert(), &plan, &config, method, &enc);
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
            st.markers = Vec::new();
            st.last_activity = now_ms();
            st.last_pos = None;
            st.auto_paused = None;
            st.auto_switching = false;
            st.clicks = Vec::new();
            st.actions = Vec::new();
            st.cursor = Vec::new();
            st.zooms = Vec::new();
            st.reset_mutes();
            st.audio_specs = audio_specs;
            st.audio_desc = None;
            (st.silent_now, st.silent_ever, st.audio_ok_ever, st.silent_notified) = (0, 0, 0, 0);
            st.replay_saving = false;
            st.replay_saved = 0;
            st.replay_last = None;
            st.replay_bytes = 0;
            st.replay_pruned_at = now_ms();
            st.fatal = None;
        }
        if self.canceled() {
            return self.abandon(false);
        }

        // 倒數：讓使用者有時間切到要錄的畫面；期間按停止（或快速鍵）可取消
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
        // 倒數中可能移動了範圍（螢幕上的控制列）
        let plan = self.lock().plan.clone().unwrap_or(plan);
        deps.keep_awake();
        // 縮小視窗的那 0.35 秒仍算倒數（countdown 保留到這之後），期間取消也有效
        if hide_ui {
            deps.before_capture(plan.rect).await;
        }
        // 錄影時隱藏桌面圖示（停止後由 after_stop 還原）
        if self.lock().config.as_ref().is_some_and(|c| c.hide_icons) {
            crate::winui::set_desktop_icons(false);
        }
        // 錄影時開啟勿擾（停止後由 after_stop 恢復）
        if self.lock().config.as_ref().is_some_and(|c| c.quiet) {
            crate::quiet::begin();
        }
        if self.canceled() {
            // 已經要求不睡眠（keep_awake）：after_stop 一定要呼叫才會放掉
            return self.abandon(true);
        }

        {
            let mut st = self.lock();
            st.countdown = None;
            let (width, height) = (plan.rect.width, plan.rect.height);
            let place = match &config.source {
                _ if config.audio_only => tr!("只錄聲音", "Audio only").to_string(),
                SourceConfig::Monitor { .. } => trf!("螢幕 {}", "Screen {}", plan.monitors.first().map(|m| m.display_number.to_string()).unwrap_or_else(|| "?".into())),
                SourceConfig::All => trf!("所有螢幕（{} 個）", "All screens ({})", plan.monitors.len()),
                SourceConfig::Region { .. } => trf!("範圍 ({}, {})", "Area ({}, {})", plan.rect.x, plan.rect.y),
            };
            let tiles = match &plan.dda {
                Some(d) if method == CaptureMethod::Ddagrab && d.tiles.len() > 1 => trf!(" ×{} 合成", " ×{} tiles", d.tiles.len()),
                _ => String::new(),
            };
            if config.replay_sec > 0 {
                let len = crate::replay::length_label(config.replay_sec);
                st.add_log(LogLevel::Info, &trf!("回溯錄影：保留最近 {len}，需要時再存下", "Replay buffer: keeping the last {len}; save it when you need it"));
            }
            st.add_log(
                LogLevel::Info,
                &trf!(
                    "開始錄影：{place} {width}×{height} → {}×{}，{} fps（{}{tiles} / {}）",
                    "Recording started: {place} {width}×{height} → {}×{}, {} fps ({}{tiles} / {})",
                    plan.out_width,
                    plan.out_height,
                    crate::format::num(config.fps),
                    method.as_str(),
                    enc.name
                ),
            );
            if Some(enc) != encoders.cpu || config.codec != crate::types::VideoCodec::H264 {
                st.add_log(LogLevel::Info, &reason);
            }
            if let Some(g) = st.gpu {
                st.add_log(LogLevel::Info, &g.describe());
            }
            if method == CaptureMethod::Gdigrab && plan.monitors.len() > 1 && plan.dda.is_none() && config.method == MethodPreference::Auto {
                st.add_log(
                    LogLevel::Info,
                    tr!("範圍涵蓋不同顯示卡上的螢幕，ddagrab 無法合成，改用 gdigrab", "The area spans screens on different graphics cards, which ddagrab can't combine; using gdigrab"),
                );
            }
            st.state = RecorderState::Recording;
            st.span_start = Some(now_ms());
        }
        if let Err(e) = self.start_segment() {
            // 例如 ffmpeg.exe 被移除 / 防毒隔離：回到待命，不留下錄影中的假狀態
            {
                let mut st = self.lock();
                st.add_log(LogLevel::Error, &trf!("無法啟動 FFmpeg：{e}", "Couldn't start FFmpeg: {e}"));
                st.state = RecorderState::Idle;
                st.span_start = None;
                cleanup_parts(st.parts_dir.as_deref());
            }
            self.deps().after_stop(); // 已縮小的操作視窗要還原，使用者才看得到錯誤
            return Err(Error::config(trf!("無法啟動 FFmpeg：{e}", "Couldn't start FFmpeg: {e}")));
        }
        self.start_ticker();
        Ok(())
    }

    async fn do_pause(&self) -> Result<()> {
        {
            let mut st = self.lock();
            if st.state != RecorderState::Recording {
                return Err(Error::config(tr!("目前不在錄影中", "Not recording")));
            }
            st.close_span();
            st.state = RecorderState::Paused;
            st.busy = Some(tr!("正在結束目前分段…", "Finishing the current segment…").into());
        }
        self.stop_current().await;
        let mut st = self.lock();
        st.busy = None;
        st.add_log(LogLevel::Info, tr!("已暫停", "Paused"));
        Ok(())
    }

    fn do_resume(&self) -> Result<()> {
        {
            let mut st = self.lock();
            if st.state != RecorderState::Paused {
                return Err(Error::config(tr!("目前不是暫停狀態", "Not paused")));
            }
            st.auto_paused = None;
            st.last_activity = now_ms();
            st.state = RecorderState::Recording;
            st.span_start = Some(now_ms());
            st.add_log(LogLevel::Info, tr!("繼續錄影", "Resumed recording"));
        }
        if !self.try_start_segment() {
            return Err(Error::config(tr!("無法繼續錄影，已停止並儲存先前錄到的部分", "Couldn't resume recording. Stopped and saved what was recorded so far")));
        }
        Ok(())
    }

    async fn do_stop(&self, reason: Option<String>) -> Result<Option<RecordingResult>> {
        {
            let mut st = self.lock();
            if st.state == RecorderState::Idle {
                return Err(Error::config(tr!("目前沒有在錄影", "Not recording")));
            }
            st.close_span();
            st.state = RecorderState::Stopping;
            st.busy = Some(tr!("正在結束錄影…", "Stopping recording…").into());
            if let Some(r) = &reason {
                st.add_log(LogLevel::Info, r);
            }
        }
        self.stop_current().await;
        if self.lock().replay_sec().is_some() {
            // 回溯錄影：沒存下的部分不要了
            {
                let mut st = self.lock();
                cleanup_parts(st.parts_dir.as_deref());
                let n = st.replay_saved;
                st.add_log(LogLevel::Info, &trf!("已停止回溯錄影（這次存下 {n} 支）", "Replay buffer stopped ({n} saved this time)"));
                st.result = None;
                st.markers.clear();
                st.clicks.clear();
                st.actions.clear();
                st.cursor.clear();
                st.zooms.clear();
                st.reset_mutes();
                st.ticker = 0;
                st.busy = None;
                st.state = RecorderState::Idle;
            }
            self.deps().after_stop();
            return Ok(None);
        }
        self.lock().busy = Some(tr!("正在合併分段…", "Merging segments…").into());
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
                    message: trf!("儲存失敗：{}（已錄的分段保留於 {parts}）", "Save failed: {} (recorded segments kept in {parts})", e.message()),
                    ..Default::default()
                }
            }
        };
        {
            let mut st = self.lock();
            st.add_log(if result.ok { LogLevel::Info } else { LogLevel::Error }, &result.message);
            st.result = Some(result.clone());
            let marks = Marks {
                markers: std::mem::take(&mut st.markers).into_iter().map(|m| m as f64 / 1000.0).collect(),
                clicks: std::mem::take(&mut st.clicks).into_iter().map(|(t, x, y)| [t as f64 / 1000.0, x as f64, y as f64]).collect(),
                actions: std::mem::take(&mut st.actions),
                cursor: std::mem::take(&mut st.cursor),
                no_cursor: st.config.as_ref().is_some_and(|c| !c.draw_mouse),
                mutes: st.take_mutes(),
                zooms: std::mem::take(&mut st.zooms).into_iter().map(|(t, x, y)| [t as f64 / 1000.0, x as f64, y as f64]).collect(),
            };
            if let (true, Some(p), false) = (result.ok, &result.path, marks.is_empty()) {
                self.deps().save_markers(p, &marks);
            }
            st.ticker = 0;
            st.busy = None;
            st.state = RecorderState::Idle;
        }
        self.deps().after_stop();
        if let (true, Some(p)) = (result.ok, &result.path) {
            self.deps().saved(p);
        }
        Ok(Some(result))
    }

    /// 在計時器 / 結束事件裡啟動分段：失敗時記錄並停止（保住已錄的部分）
    fn try_start_segment(&self) -> bool {
        match self.start_segment() {
            Ok(()) => true,
            Err(e) => {
                self.add_log(LogLevel::Error, &trf!("無法啟動 FFmpeg：{e}", "Couldn't start FFmpeg: {e}"));
                self.stop_later(Some(tr!("無法繼續擷取，停止錄影", "Couldn't continue capturing; recording stopped").into()));
                false
            }
        }
    }

    fn start_segment(&self) -> std::result::Result<(), String> {
        let (ffmpeg, plan, config, method, enc, file, specs, index, gpu) = {
            let st = self.lock();
            let index = st.segments.len();
            let parts = st.parts_dir.clone().ok_or(tr!("沒有分段資料夾", "No segment folder"))?;
            let enc = st.enc.ok_or(tr!("沒有編碼器", "No encoder"))?;
            (
                self.deps().ffmpeg_path().ok_or(tr!("找不到 ffmpeg.exe", "ffmpeg.exe not found"))?,
                st.plan.clone().ok_or(tr!("沒有擷取範圍", "No capture area"))?,
                st.config().clone(),
                st.method.unwrap_or(CaptureMethod::Gdigrab),
                enc,
                // 回溯錄影：FFmpeg 依樣式一直開新檔案
                parts.join(if st.replay_sec().is_some() { format!("rp_{index:03}_%05d.mp4") } else { format!("seg_{index:03}.mp4") }).display().to_string(),
                st.audio_specs.clone(),
                index,
                st.gpu.map(|g| g.for_encoder(&enc)),
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
            Some(Arc::new(AudioPipe::new(specs, self.deps().audio_opener(), log, config.pipe_separate()).map_err(|e| trf!("無法建立聲音管線：{e}", "Couldn't set up audio capture: {e}"))?))
        };
        if let Some(a) = &audio {
            let mut st = self.lock();
            if st.audio_desc.is_none() {
                st.audio_desc = Some(a.describe().to_string());
                st.add_log(LogLevel::Info, &trf!("錄製聲音：{}", "Recording audio: {}", a.describe()));
            }
        }
        let audio_args = audio.as_ref().map(|a| a.input_args());
        let args = segment_args(&plan, &config, method, &enc, &file, audio_args.as_deref(), gpu).map_err(|e| e.message().to_string())?;

        let mut cmd = command(&ffmpeg);
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        if let (Some(a), Some(pid)) = (&audio, child.id()) {
            a.allow(pid);
        }
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
                stalled: false,
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
                st.add_log(LogLevel::Info, tr!("擷取已恢復", "Capture resumed"));
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
                st.add_log(
                    LogLevel::Warn,
                    &trf!(
                        "電腦跟不上即時錄影：實際約 {unique:.1} fps（設定 {} fps），建議降低解析度或 FPS",
                        "The computer can't keep up with real-time recording: about {unique:.1} fps (set to {} fps). Try a lower resolution or FPS",
                        crate::format::num(fps)
                    ),
                );
                // 寫入速度不足代表處理鏈（下載畫面、縮放、編碼）跟不上；擷取跟不上則是 dup 增加。
                // 改用 GPU 編碼可減輕 CPU 負擔，「自動」模式記下來，之後的錄影改用 GPU 編碼
                // （同一段錄影不中途切換，不同編碼器的分段無法無損合併；可在介面上重設）
                if auto && written < fps * 0.9 && st.enc == st.cpu_encoder && st.gpu_available && !self.deps().prefer_gpu() {
                    st.add_log(
                        LogLevel::Warn,
                        tr!(
                            "電腦處理不及，之後的錄影會自動改用 GPU 編碼以減輕 CPU 負擔（可在「設定 → 進階 → 編碼器」重設）",
                            "The computer can't keep up. Future recordings will use GPU encoding to reduce CPU load (you can change this in Settings → Advanced → Encoder)"
                        ),
                    );
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
        // 通知等放開鎖之後再送（通知會用到系統匣的鎖，持有錄影器的鎖時呼叫可能互相等）
        let mut gpu_failed = false;
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
                // 只引用錯誤訊息（警告常常和失敗無關，例如聲音的聲道配置）
                let tail = last_errors(&seg.stderr, 2);
                let detail = match (tail.is_empty(), seg.stalled) {
                    (false, _) => tail,
                    (true, true) => trf!("超過 {} 秒沒有輸出畫面", "no frames for over {} s", STALL_MS / 1000),
                    (true, false) => trf!("結束代碼 {code}", "exit code {code}"),
                };
                let seg_method = seg.method;
                let method_auto = st.config().method == MethodPreference::Auto;
                if !st.ever_produced_frames {
                    let next = startup_fallback(&FallbackInput {
                        stderr: &st.segments[index].stderr,
                        gpu_encoder_in_use: st.cpu_encoder.is_some() && st.enc != st.cpu_encoder,
                        // HEVC / AV1 的編碼器開始就失敗：不管設定，都改用 CPU 的 H.264
                        encoder_auto: st.config().encoder.unwrap_or_default() == crate::types::EncoderPreference::Auto
                            || st.enc.is_some_and(|e| crate::args::codec_of(e.name) != crate::types::VideoCodec::H264),
                        has_cpu_encoder: st.cpu_encoder.is_some(),
                        ddagrab_in_use: seg_method == CaptureMethod::Ddagrab,
                        method_auto,
                        gpu_convert_in_use: seg_method == CaptureMethod::Ddagrab && st.gpu.zip(st.plan.as_ref()).is_some_and(|(g, p)| g.applies(p, st.config())),
                        gfx_in_use: seg_method == CaptureMethod::Gfxcapture,
                    });
                    match next {
                        // 顯示卡處理失敗（驅動不支援等）：改回 CPU 轉換再試，這次執行期間之後的錄影也不再用
                        StartupFallback::CpuConvert => {
                            st.add_log(LogLevel::Warn, &trf!("顯示卡處理畫面失敗（{detail}），改用 CPU 轉換", "GPU processing failed ({detail}); using CPU conversion"));
                            st.gpu = None;
                            gpu_failed = true;
                            ExitAction::Start
                        }
                        // GPU 編碼器一開始就失敗（驅動問題等）：「自動」模式退回 CPU 編碼再試
                        StartupFallback::CpuEncoder => {
                            let name = st.enc.map(|e| e.name).unwrap_or("");
                            st.add_log(LogLevel::Warn, &trf!("GPU 編碼器 {name} 無法使用（{detail}），改用 CPU 編碼", "GPU encoder {name} isn't working ({detail}); using CPU encoding"));
                            st.enc = st.cpu_encoder;
                            ExitAction::Start
                        }
                        StartupFallback::Gdigrab => {
                            st.add_log(LogLevel::Warn, &trf!("ddagrab 無法擷取（{detail}），改用 gdigrab", "ddagrab couldn't capture ({detail}); using gdigrab"));
                            st.method = Some(CaptureMethod::Gdigrab);
                            ExitAction::Start
                        }
                        StartupFallback::RegionCapture => {
                            st.add_log(
                                LogLevel::Warn,
                                &trf!("無法只錄視窗內容（{detail}），改用跟著視窗的範圍", "Couldn't capture just the window ({detail}); recording the area that follows the window instead"),
                            );
                            st.method = st.region_method.or(Some(CaptureMethod::Gdigrab));
                            ExitAction::Start
                        }
                        StartupFallback::Fatal => {
                            st.add_log(LogLevel::Error, &trf!("無法開始擷取：{detail}", "Couldn't start capturing: {detail}"));
                            st.fatal = Some(detail);
                            ExitAction::Stop
                        }
                    }
                } else if seg_method == CaptureMethod::Gfxcapture {
                    // 只錄視窗內容中斷（例如視窗關掉了）：馬上改用跟著視窗的範圍繼續錄
                    st.add_log(LogLevel::Warn, &trf!("只錄視窗內容中斷（{detail}），改用跟著視窗的範圍繼續錄", "Window capture stopped ({detail}); continuing with the area that follows the window"));
                    st.method = st.region_method.or(Some(CaptureMethod::Gdigrab));
                    ExitAction::Start
                } else {
                    // 錄到一半中斷：保留已錄分段，延遲後開新分段續錄
                    let attempt = st.retry.as_ref().map(|r| r.attempt).unwrap_or(0) + 1;
                    if attempt > MAX_RETRIES {
                        // 長時間無法恢復（螢幕被拔掉、磁碟已滿…）：ddagrab 先換 gdigrab 再試一輪，否則停止並合併已錄的部分
                        if seg_method == CaptureMethod::Ddagrab && method_auto {
                            st.add_log(
                                LogLevel::Warn,
                                &trf!("ddagrab 連續 {MAX_RETRIES} 次無法擷取（{detail}），改用 gdigrab", "ddagrab failed to capture {MAX_RETRIES} times in a row ({detail}); switching to gdigrab"),
                            );
                            st.method = Some(CaptureMethod::Gdigrab);
                            let token = st.next_seq();
                            st.retry = Some(Retry { attempt: 0, message: tr!("改用 gdigrab 重試", "Retrying with gdigrab").into(), token });
                            ExitAction::Start
                        } else {
                            st.add_log(
                                LogLevel::Error,
                                &trf!(
                                    "連續 {MAX_RETRIES} 次無法恢復擷取（{detail}），停止錄影並儲存已錄的部分",
                                    "Couldn't resume capturing after {MAX_RETRIES} tries ({detail}). Stopping and saving what was recorded"
                                ),
                            );
                            st.retry = None;
                            ExitAction::Stop
                        }
                    } else {
                        let delay = (1000u64 << (attempt - 1)).min(10_000);
                        let message = trf!("擷取中斷，{} 秒後重試（第 {attempt} 次）", "Capture interrupted, retrying in {} s (attempt {attempt})", js_round(delay as f64 / 1000.0));
                        st.add_log(LogLevel::Warn, &trf!("FFmpeg 意外結束：{detail}；{message}", "FFmpeg exited unexpectedly: {detail}; {message}"));
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
        if gpu_failed {
            self.deps().gpu_convert_failed();
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
                            r.message = trf!("擷取中斷，正在重試（第 {attempt} 次）", "Capture interrupted, retrying (attempt {attempt})");
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
            self.add_log(LogLevel::Warn, &trf!("FFmpeg 未在 {} 秒內結束，已強制終止", "FFmpeg didn't exit within {} s and was force-stopped", STOP_TIMEOUT.as_secs()));
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

    /// 沒動靜時自動暫停：錄影中超過設定的秒數沒有動靜就暫停；自動暫停後一有動靜就繼續
    fn auto_pause_check(&self, now: u64) {
        enum Do {
            Pause(u32),
            Resume,
        }
        let what = {
            let mut st = self.lock();
            let Some(cfg) = st.config.as_ref() else { return };
            let sec = cfg.auto_pause_sec;
            if sec == 0 || cfg.audio_only || cfg.replay_sec > 0 || st.busy.is_some() || st.auto_switching {
                return;
            }
            // 有聲音也算有動靜
            if let Some(t) = st.current.and_then(|i| st.segments[i].audio.as_ref()).map(|a| a.last_sound_ms()) {
                st.last_activity = st.last_activity.max(t);
            }
            match (st.state, st.auto_paused) {
                (RecorderState::Recording, _) if now.saturating_sub(st.last_activity) >= sec as u64 * 1000 => Do::Pause(sec),
                (RecorderState::Paused, Some(at)) if st.last_activity > at => Do::Resume,
                _ => return,
            }
        };
        self.lock().auto_switching = true;
        let me = self.clone();
        tokio::spawn(async move {
            match what {
                Do::Pause(sec) => {
                    // 先標成自動暫停、寫進紀錄（暫停的過程中狀態就會變成「暫停」）
                    {
                        let mut st = me.lock();
                        st.auto_paused = Some(now_ms());
                        st.add_log(
                            LogLevel::Info,
                            &trf!("{sec} 秒沒有動靜，自動暫停（動一下滑鼠或按鍵就繼續）", "Nothing happened for {sec} s; paused automatically (move the mouse or press a key to continue)"),
                        );
                    }
                    if me.pause().await.is_err() {
                        me.lock().auto_paused = None;
                    }
                }
                Do::Resume => {
                    // 使用者自己按了繼續 / 停止：pause 狀態已經不是自動暫停
                    let still = me.lock().auto_paused.is_some();
                    if still && me.resume().await.is_ok() {
                        me.add_log(LogLevel::Info, tr!("有動靜，自動繼續錄影", "Activity detected; resumed automatically"));
                    }
                }
            }
            me.lock().auto_switching = false;
        });
    }

    fn tick(&self) {
        let now = now_ms();
        self.auto_pause_check(now);
        let mut stop_reason = None;
        let mut check_disk = false;
        let mut prune = None;
        let mut silent_notice = 0;
        {
            let mut guard = self.lock();
            let st = &mut *guard;
            let gap = now.saturating_sub(st.last_tick_at);
            st.last_tick_at = now;
            if st.state != RecorderState::Recording {
                return;
            }
            // 麥克風、系統聲音打不開或中斷（改錄靜音）：錄影面板標成紅色，第一次發生時用系統通知提醒
            if let Some(a) = st.current.and_then(|i| st.segments[i].audio.as_ref()) {
                let (silent, sources) = (a.silent(), a.sources());
                st.silent_now = silent;
                st.silent_ever |= silent;
                st.audio_ok_ever |= sources & !silent;
                silent_notice = silent & !st.silent_notified;
                st.silent_notified |= silent;
            }
            if gap > SLEEP_GAP_MS && st.span_start.is_some() {
                // 電腦睡眠後恢復：睡著的時間不算錄影長度（否則會誤觸最長錄影時間）；
                // 重開分段，避免 FFmpeg 用重複畫面補滿這段空白
                st.span_start = st.span_start.map(|s| s + gap);
                st.add_log(
                    LogLevel::Warn,
                    &trf!("電腦約 {} 秒沒有運作（睡眠 / 休眠），重新開始擷取", "The computer was inactive for about {} s (sleep / hibernate); restarting capture", js_round(gap as f64 / 1000.0)),
                );
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
                stop_reason = Some(tr!("已達最長錄影時間，自動停止", "Maximum recording length reached; stopped automatically").to_string());
            } else {
                if now.saturating_sub(st.disk_checked_at) > DISK_CHECK_MS {
                    st.disk_checked_at = now;
                    check_disk = true;
                }
                if let (Some(sec), Some(dir)) = (st.replay_sec(), st.parts_dir.clone()) {
                    if now.saturating_sub(st.replay_pruned_at) >= REPLAY_PRUNE_MS {
                        st.replay_pruned_at = now;
                        prune = Some((dir, sec));
                    }
                }
                let stalled = st.current.map(|i| &st.segments[i]).is_some_and(|s| s.running && !s.stop_requested && now.saturating_sub(s.last_frame_at) > STALL_MS);
                if stalled {
                    st.add_log(LogLevel::Warn, &trf!("超過 {} 秒沒有擷取到新畫面，重新啟動 FFmpeg", "No new frames for over {} s; restarting FFmpeg", STALL_MS / 1000));
                    let seg = &mut st.segments[st.current.unwrap()];
                    seg.last_frame_at = now;
                    seg.stalled = true;
                    let _ = seg.ctl.send(SegCmd::Kill);
                }
            }
        }
        if silent_notice != 0 {
            let (title, text) = silent_notice_text(silent_notice);
            self.deps().notify(title, text, true);
        }
        if stop_reason.is_some() {
            self.stop_later(stop_reason);
        }
        if check_disk {
            let me = self.clone();
            tokio::spawn(async move { me.check_disk().await });
        }
        if let Some((dir, sec)) = prune {
            // 回溯錄影：刪掉太舊的檔案（存下時正在讀的不會被刪：見 replay::stale），記下緩衝的大小
            let me = self.clone();
            tokio::task::spawn_blocking(move || {
                let list = crate::replay::chunks(&dir);
                let old = crate::replay::stale(&list, std::time::SystemTime::now(), sec);
                for f in &old {
                    let _ = std::fs::remove_file(f);
                }
                let bytes = crate::replay::chunks(&dir).iter().map(|c| c.len).sum();
                me.lock().replay_bytes = bytes;
            });
        }
    }

    /// 回溯錄影：把最近的檔案接起來存成 Replay_<時間>.mp4（不重新壓縮）。與開始、停止依序執行：存的時候按停止會等存好
    pub fn save_replay(&self) -> BoxFut<'static, Result<RecordingResult>> {
        let me = self.clone();
        Box::pin(async move {
            {
                let mut st = me.lock();
                if st.replay_sec().is_none() || st.state != RecorderState::Recording {
                    return Err(Error::config(tr!("沒有在回溯錄影中", "The replay buffer isn't running")));
                }
                if st.replay_saving {
                    return Err(Error::config(tr!("正在存，請稍等", "Already saving; please wait")));
                }
                st.replay_saving = true;
            }
            let r = {
                let _q = me.0.queue.lock().await;
                me.do_save_replay().await
            };
            let mut st = me.lock();
            st.replay_saving = false;
            match &r {
                Ok(res) => {
                    st.replay_saved += 1;
                    st.replay_last = res.path.clone();
                    st.add_log(LogLevel::Info, &res.message);
                }
                Err(e) => st.add_log(LogLevel::Error, &trf!("無法存下回溯錄影：{}", "Couldn't save the replay: {}", e.message())),
            }
            r
        })
    }

    async fn do_save_replay(&self) -> Result<RecordingResult> {
        let (sec, dir, output_dir, has_audio, fps) = {
            let st = self.lock();
            if st.state != RecorderState::Recording {
                return Err(Error::config(tr!("沒有在回溯錄影中", "The replay buffer isn't running")));
            }
            let c = st.config();
            (st.replay_sec().unwrap_or(0), st.parts_dir.clone().unwrap_or_default(), c.output_dir.clone(), !st.audio_specs.is_empty(), c.fps)
        };
        let ffmpeg = self.deps().ffmpeg_path().ok_or_else(|| Error::other(tr!("找不到 ffmpeg.exe", "ffmpeg.exe not found")))?;
        let files: Vec<String> = crate::replay::pick(&crate::replay::chunks(&dir), std::time::SystemTime::now(), sec).iter().map(|p| p.display().to_string()).collect();
        if files.is_empty() {
            return Err(crate::replay::no_frames());
        }
        // 合併用的清單放在各自的資料夾（不和 FFmpeg 正在寫的檔案混在一起），存好就刪掉
        let work = dir.join(format!("save_{}", now_ms()));
        std::fs::create_dir_all(&work)?;
        let out = unique_path(Path::new(&output_dir), &format!("Replay_{}", timestamp()), ".mp4");
        let r = merge_segments(&ffmpeg, &files, &work, &out, has_audio, None).await;
        let _ = std::fs::remove_dir_all(&work);
        let duration = r.map_err(|detail| Error::other(trf!("合併失敗：{detail}", "Merge failed: {detail}")))?;
        let out_str = out.display().to_string();
        let video_sec = duration.unwrap_or(0.0);
        Ok(RecordingResult {
            ok: true,
            path: Some(out_str.clone()),
            frames: js_round(video_sec * fps) as u64,
            video_sec,
            bytes: std::fs::metadata(&out).map(|m| m.len()).ok(),
            message: trf!("已存下回溯錄影 {out_str}（{}）", "Saved replay {out_str} ({})", crate::format::video_clock(video_sec)),
            ..Default::default()
        })
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
            if free < DISK_STOP && !st.auto_stopping && st.replay_sec().is_some() {
                st.auto_stopping = true;
                notify = Some((tr!("磁碟空間不足", "Low disk space"), trf!("只剩 {}，已停止回溯錄影", "Only {} left. The replay buffer was stopped", gb(free))));
                stop_reason = Some(trf!("儲存位置只剩 {}，停止回溯錄影", "Only {} left at the save location; replay buffer stopped", gb(free)));
            } else if free < DISK_STOP && !st.auto_stopping {
                st.auto_stopping = true;
                notify = Some((tr!("磁碟空間不足", "Low disk space"), trf!("只剩 {}，已自動停止並儲存", "Only {} left. Recording was stopped and saved automatically", gb(free))));
                stop_reason = Some(trf!("儲存位置只剩 {}，自動停止並儲存", "Only {} left at the save location; stopped and saved automatically", gb(free)));
            } else if free < DISK_WARN && !st.disk_warned {
                st.disk_warned = true;
                st.add_log(LogLevel::Warn, &trf!("儲存位置只剩 {}，低於 {} 時會自動停止", "Only {} left at the save location; recording stops automatically below {}", gb(free), gb(DISK_STOP)));
                notify = Some((tr!("磁碟空間快不夠了", "Disk space is running low"), trf!("儲存位置只剩 {}", "Only {} left at the save location", gb(free))));
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
        let (fps, parts, has_audio, parts_dir, out, markers) = {
            let st = self.lock();
            let parts: Vec<(String, u64)> = st.segments.iter().filter(|s| s.frames > 0).map(|s| (s.file.clone(), s.frames)).collect();
            let markers: Vec<f64> = st.markers.iter().map(|m| *m as f64 / 1000.0).collect();
            (st.config().fps, parts, !st.audio_specs.is_empty(), st.parts_dir.clone().unwrap_or_default(), st.final_path.clone().unwrap_or_default(), markers)
        };
        // 放開錄影器的鎖再看檔案（網路磁碟上可能很慢，這段時間介面照樣讀得到狀態）
        let parts: Vec<(String, u64)> = parts.into_iter().filter(|(f, _)| std::fs::metadata(f).map(|m| m.len() > 0).unwrap_or(false)).collect();
        let frames: u64 = parts.iter().map(|p| p.1).sum();
        if parts.is_empty() {
            cleanup_parts(Some(&parts_dir));
            // 有開始不了的原因就一起說
            let message = match self.lock().fatal.clone() {
                Some(why) => trf!("沒有擷取到任何畫面，未產生影片（{why}）", "No frames were captured, so no video was created ({why})"),
                None => tr!("沒有擷取到任何畫面，未產生影片", "No frames were captured, so no video was created").into(),
            };
            return Ok(RecordingResult { ok: false, message, ..Default::default() });
        }
        let ffmpeg = self.deps().ffmpeg_path().ok_or_else(|| Error::other(tr!("找不到 ffmpeg.exe", "ffmpeg.exe not found")))?;
        let files: Vec<String> = parts.iter().map(|p| p.0.clone()).collect();
        let out_str = out.display().to_string();
        let parts_str = parts_dir.display().to_string();
        // 標記寫成章節（播放器可以跳章節）
        let length = frames as f64 / fps;
        let chapters = crate::chapters::from_markers(&markers, length);
        let meta = (!chapters.is_empty()).then(|| crate::chapters::ffmetadata(&chapters, length));
        let duration = match merge_segments(&ffmpeg, &files, &parts_dir, &out, has_audio, meta).await {
            Ok(d) => d,
            Err(detail) => {
                return Ok(RecordingResult {
                    ok: false,
                    frames,
                    video_sec: frames as f64 / fps,
                    parts_dir: Some(parts_str.clone()),
                    message: trf!("合併失敗：{detail}（分段保留於 {parts_str}）", "Merge failed: {detail} (segments kept in {parts_str})"),
                    ..Default::default()
                });
            }
        };
        let real_frames = duration.map(|d| js_round(d * fps) as u64).unwrap_or(frames);
        cleanup_parts(Some(&parts_dir));
        Ok(RecordingResult {
            ok: true,
            path: Some(out_str.clone()),
            frames: real_frames,
            video_sec: duration.unwrap_or(frames as f64 / fps),
            bytes: std::fs::metadata(&out).map(|m| m.len()).ok(),
            message: if crate::i18n::is_en() {
                let n = parts.len();
                format!("Saved {out_str} ({real_frames} frame{}, {n} segment{})", if real_frames == 1 { "" } else { "s" }, if n == 1 { "" } else { "s" })
            } else {
                format!("已儲存 {out_str}（{real_frames} 張，{} 個分段）", parts.len())
            },
            parts_dir: None,
            silent_audio: self.lock().silent_result(),
        })
    }
}

#[cfg(test)]
mod tests {
    //! 開始錄影的「準備 / 倒數 / 縮小視窗」階段：取消與連按同時發生的情況。
    //! 用假的相依（不會真的啟動 FFmpeg）；unix 上另有用假 FFmpeg 跑完整流程的測試。
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
            key: String::new(),
            name: String::new(),
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
        /// 有假的 FFmpeg 時也當作 ddagrab 可用
        dda: bool,
        gpu: Option<GpuSupport>,
        gpu_failed: AtomicUsize,
        /// 有 gfxcapture；視窗 1234 在 (100, 100) 640×360
        gfx: bool,
        /// 存下的標記、點擊、游標位置
        marks: Mutex<Option<Marks>>,
        /// 改過的螢幕清單（模擬接上、拔掉螢幕）；None = 只有 mon()
        screens: Mutex<Option<Vec<MonitorInfo>>>,
    }

    impl RecorderDeps for Fake {
        fn gfxcapture_usable(&self) -> bool {
            self.gfx
        }
        fn window_bounds(&self, id: i64) -> Option<Rect> {
            (self.gfx && id == 1234).then_some(Rect { x: 100, y: 100, width: 640, height: 360 })
        }
        fn ffmpeg_path(&self) -> Option<PathBuf> {
            Some(self.ffmpeg.clone().unwrap_or_else(|| "ffmpeg-not-used.exe".into()))
        }
        fn encoders(&self) -> BoxFut<'_, Encoders> {
            let mut rx = self.gate.get_or_init(|| watch::channel(false).0).subscribe();
            Box::pin(async move {
                let _ = rx.wait_for(|v| *v).await;
                Encoders { cpu: Some(ENCODERS[0]), gpu: vec![], codec: vec![] }
            })
        }
        fn prefer_gpu(&self) -> bool {
            false
        }
        fn learn_gpu(&self) {}
        fn monitors(&self) -> Vec<MonitorInfo> {
            self.screens.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| vec![mon()])
        }
        fn ddagrab_usable(&self) -> BoxFut<'_, bool> {
            let usable = self.ffmpeg.is_none() || self.dda;
            Box::pin(async move { usable })
        }
        fn gpu_convert(&self) -> Option<GpuSupport> {
            self.gpu
        }
        fn gpu_convert_failed(&self) {
            self.gpu_failed.fetch_add(1, Ordering::SeqCst);
        }
        fn before_capture(&self, area: Rect) -> BoxFut<'_, ()> {
            self.before.fetch_add(1, Ordering::SeqCst);
            self.before_areas.lock().unwrap_or_else(|e| e.into_inner()).push(area);
            if self.stop_in_before {
                // 倒數剛結束、正在縮小視窗時按下取消
                drop(self.rec.get().unwrap().stop(None));
            }
            Box::pin(async { tokio::time::sleep(Duration::from_millis(10)).await })
        }
        fn ui_in_area(&self, area: &Rect) -> bool {
            self.ui_areas.lock().unwrap_or_else(|e| e.into_inner()).push(*area);
            self.ui_in.unwrap_or(true)
        }
        fn after_stop(&self) {
            self.after.fetch_add(1, Ordering::SeqCst);
        }
        fn save_markers(&self, _output: &str, marks: &Marks) {
            *self.marks.lock().unwrap_or_else(|e| e.into_inner()) = Some(marks.clone());
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
                shot_dir: String::new(),
                audio: AudioConfig::default(),
                encoder: None,
                codec: Default::default(),
                auto_pause_sec: 0,
                countdown_sec: Some(3.0),
                hide_ui: Some(true),
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
    async fn audio_only_needs_a_sound_source() {
        let s = Setup::new(Fake::default());
        s.release();
        let cfg = RecordConfig { audio_only: true, audio: AudioConfig { system: false, mic: false, mic_id: String::new(), app: String::new() }, ..s.config() };
        let err = s.rec.start(cfg).await.unwrap_err();
        assert!(err.message().contains("只錄聲音"), "{}", err.message());
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert!(!s.parts_left());
    }

    #[tokio::test]
    async fn audio_only_countdown_draws_the_card() {
        let s = Setup::new(Fake::default());
        s.release();
        let cfg = RecordConfig { audio_only: true, audio: AudioConfig { system: true, mic: false, mic_id: String::new(), app: String::new() }, show_clicks: true, ..s.config() };
        s.rec.start(cfg).await.unwrap(); // 進入倒數就回覆
        {
            let st = s.rec.lock();
            let plan = st.plan.as_ref().unwrap();
            assert_eq!((plan.out_width, plan.out_height), (640, 360));
            assert!(plan.card.as_ref().is_some_and(|c| Path::new(c).is_file()));
            assert!(st.final_path.as_ref().unwrap().file_name().unwrap().to_string_lossy().starts_with("錄音_"));
            let c = st.config.as_ref().unwrap();
            assert_eq!((c.fps, c.show_clicks, c.hide_ui), (5.0, false, Some(false)));
        }
        // 沒有範圍外框，只在螢幕上方顯示小控制列
        assert!(s.rec.frame_info().unwrap().full);
        s.rec.stop(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(s.rec.status().state, RecorderState::Idle);
        assert!(!s.parts_left());
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
        assert_eq!(s.deps.ui_areas.lock().unwrap_or_else(|e| e.into_inner()).last(), Some(&Rect { x: 0, y: 0, width: 800, height: 600 }));
        s.rec.stop(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(s.rec.status().countdown_covers_ui, None); // 不在倒數就不回報
    }

    #[tokio::test]
    async fn frame_info_while_active() {
        let s = Setup::new(Fake::default());
        s.release();
        assert_eq!(s.rec.frame_info(), None);
        s.rec.start(RecordConfig { source: region(100.0, 50.0, 640.0, 480.0), ..s.config() }).await.unwrap();
        let f = s.rec.frame_info().unwrap();
        assert_eq!(f.area, Rect { x: 100, y: 50, width: 640, height: 480 });
        assert_eq!(f.state, RecorderState::Countdown);
        assert!(f.countdown_ms.unwrap() > 2000);
        s.rec.stop(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(s.rec.frame_info(), None);
        // 整個螢幕：只有控制列（full = true，不畫外框、不能拖曳）
        s.rec.start(s.config()).await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Countdown);
        assert!(s.rec.frame_info().unwrap().full);
        s.rec.stop(None).await.unwrap();
    }

    #[tokio::test]
    async fn move_region_during_countdown_keeps_size() {
        let s = Setup::new(Fake::default());
        s.release();
        s.rec.start(RecordConfig { source: region(100.0, 50.0, 640.0, 480.0), ..s.config() }).await.unwrap();
        let r = s.rec.move_region(300, 200).await.unwrap();
        assert_eq!(r, Rect { x: 300, y: 200, width: 640, height: 480 });
        assert_eq!(s.rec.frame_info().unwrap().area, r);
        // 超出桌面時限制在桌面內（假的螢幕是 1920×1080）
        let r = s.rec.move_region(5000, -50).await.unwrap();
        assert_eq!(r, Rect { x: 1920 - 640, y: 0, width: 640, height: 480 });
        s.rec.stop(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(s.rec.move_region(0, 0).await.is_err());
    }

    #[tokio::test]
    async fn minimizing_gets_capture_area() {
        let s = Setup::new(Fake { stop_in_before: true, ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), source: region(100.0, 50.0, 640.0, 480.0), ..s.config() }).await.unwrap();
        assert_eq!(s.deps.before_areas.lock().unwrap_or_else(|e| e.into_inner()).first(), Some(&Rect { x: 100, y: 50, width: 640, height: 480 }));
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
    /// - 錄影分段（含 -progress）：回報進度、寫出分段檔（回溯錄影的檔名樣式寫出第一個檔案），收到 q 才結束
    /// - 合併（-f concat）：寫出成品
    /// - 讀檔頭（-i 成品）：印出長度
    #[cfg(unix)]
    fn fake_ffmpeg(dir: &Path) -> PathBuf {
        let p = dir.join("ffmpeg");
        let script = r#"#!/bin/bash
for last; do :; done
case " $* " in
  *" -f null - -progress "*)
    # 量分段的聲音長度（合併時用）
    printf 'out_time_us=2400000\nprogress=end\n'
    exit 0
    ;;
  *" -progress "*)
    case "$last" in
      *%05d*) head -c 5000 /dev/zero > "$(printf '%s' "$last" | sed 's/%05d/00000/')";;
      *) echo data > "$last";;
    esac
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

    /// 假的 FFmpeg：遇到顯示卡處理的濾鏡就失敗（驅動不支援），其他照 fake_ffmpeg
    #[cfg(unix)]
    fn fake_ffmpeg_no_gpu(dir: &Path) -> PathBuf {
        let inner = fake_ffmpeg(dir);
        let p = dir.join("ffmpeg-no-gpu");
        let script = format!("#!/bin/bash\ncase \" $* \" in *vpp_qsv*) echo '[vpp_qsv @ 0x1] Error creating a MFX session: -9' >&2; exit 1;; esac\nexec {} \"$@\"\n", inner.display());
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// 假的 FFmpeg：遇到 gfxcapture 就失敗（舊版 Windows、不能擷取的視窗），其他照 fake_ffmpeg
    #[cfg(unix)]
    fn fake_ffmpeg_no_gfx(dir: &Path) -> PathBuf {
        let inner = fake_ffmpeg(dir);
        let p = dir.join("ffmpeg-no-gfx");
        let script = format!("#!/bin/bash\ncase \" $* \" in *gfxcapture*) echo '[gfxcapture @ 0x1] Failed to create capture item' >&2; exit 1;; esac\nexec {} \"$@\"\n", inner.display());
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn window_capture_falls_back_to_following_area() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg_no_gfx(tools.path())), gfx: true, ..Default::default() });
        s.release();
        let region = SourceConfig::Region { x: 0.0, y: 0.0, width: 320.0, height: 240.0 };
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), source: region, follow_window: Some(1234), window_capture: true, ..s.config() }).await.unwrap();
        for _ in 0..150 {
            if s.rec.status().frames > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = s.rec.status();
        assert!(st.frames > 0, "改用範圍後應該繼續錄影");
        // gfxcapture 失敗：改用跟著視窗的範圍（視窗現在的位置與大小）
        assert_eq!(st.method, Some(CaptureMethod::Gdigrab));
        assert_eq!((st.out_width, st.out_height), (640, 360));
        let log: Vec<String> = st.log.iter().map(|l| l.text.clone()).collect();
        assert!(log.iter().any(|l| l.contains("無法只錄視窗內容") && l.contains("改用跟著視窗的範圍")), "{log:?}");
        let res = s.rec.stop(None).await.unwrap().unwrap();
        assert!(res.ok, "{}", res.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gpu_convert_failure_falls_back_to_cpu() {
        let tools = tempfile::tempdir().unwrap();
        let gpu = GpuSupport { adapter: mon().adapter, convert: crate::args::GpuConvert::Qsv, zero_copy: false };
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg_no_gpu(tools.path())), dda: true, gpu: Some(gpu), ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
        for _ in 0..150 {
            if s.rec.status().frames > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = s.rec.status();
        assert!(st.frames > 0, "退回 CPU 轉換後應該繼續錄影");
        // 仍然用 ddagrab，只是改回 CPU 轉換；app 記下這次執行不再用顯示卡處理
        assert_eq!(st.method, Some(CaptureMethod::Ddagrab));
        assert_eq!(s.deps.gpu_failed.load(Ordering::SeqCst), 1);
        let log: Vec<String> = st.log.iter().map(|l| l.text.clone()).collect();
        assert!(log.iter().any(|l| l.contains("畫面在顯示卡上縮放、轉色彩（Intel QSV）")), "{log:?}");
        assert!(log.iter().any(|l| l.contains("顯示卡處理畫面失敗") && l.contains("改用 CPU 轉換")), "{log:?}");
        let res = s.rec.stop(None).await.unwrap().unwrap();
        assert!(res.ok, "{}", res.message);
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
    async fn cursor_positions_are_saved_with_the_recording() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        s.rec.add_cursor(10, 10); // 還沒開始：不記
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), draw_mouse: false, ..s.config() }).await.unwrap();
        for _ in 0..100 {
            if s.rec.status().frames > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        s.rec.add_cursor(960, 540);
        tokio::time::sleep(Duration::from_millis(40)).await;
        s.rec.add_cursor(960, 540); // 沒動：不記
        tokio::time::sleep(Duration::from_millis(40)).await;
        s.rec.add_cursor(1920, 0);
        assert!(s.rec.stop(None).await.unwrap().unwrap().ok);
        let m = s.deps.marks.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("有游標位置就要存");
        assert!(m.no_cursor && m.markers.is_empty());
        assert_eq!(m.cursor.iter().map(|c| (c[1], c[2])).collect::<Vec<_>>(), [(0.5, 0.5), (1.0, 0.0)]);
        assert!(m.cursor[1][0] > m.cursor[0][0]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pauses_when_nothing_happens_and_resumes_on_activity() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), auto_pause_sec: 1, ..s.config() }).await.unwrap();
        let wait = |want: RecorderState, ms: u64| {
            let rec = s.rec.clone();
            async move {
                for _ in 0..ms / 20 {
                    if rec.status().state == want {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                false
            }
        };
        assert!(wait(RecorderState::Recording, 3000).await);
        // 一直有動靜（移動滑鼠）：不暫停
        for i in 0..8 {
            s.rec.add_cursor(100 + i, 100);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert_eq!(s.rec.status().state, RecorderState::Recording);
        // 1 秒沒動靜：自動暫停
        assert!(wait(RecorderState::Paused, 4000).await, "應該要自動暫停");
        let st = s.rec.status();
        assert!(st.auto_paused);
        assert!(st.log.iter().any(|l| l.text.contains("自動暫停")), "{:?}", st.log);
        // 游標沒動（同一個位置）不算
        s.rec.add_cursor(107, 100);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(s.rec.status().state, RecorderState::Paused);
        // 動一下滑鼠：自動繼續
        s.rec.add_cursor(300, 300);
        assert!(wait(RecorderState::Recording, 3000).await, "應該要自動繼續");
        assert!(!s.rec.status().auto_paused);
        // 自己按暫停：有動靜也不會自動繼續
        s.rec.pause().await.unwrap();
        s.rec.add_cursor(400, 400);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(s.rec.status().state, RecorderState::Paused);
        assert!(!s.rec.status().auto_paused);
        assert!(s.rec.stop(None).await.unwrap().unwrap().ok);
    }

    #[test]
    fn loud_sound_counts_as_activity() {
        let quiet: Vec<u8> = [0.001f32, -0.02, 0.01].iter().flat_map(|v| v.to_le_bytes()).collect();
        let speech: Vec<u8> = [0.001f32, -0.2, 0.01].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert!(!crate::audiopipe::loud(&quiet));
        assert!(crate::audiopipe::loud(&speech));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn steps_are_saved_with_the_recording() {
        use crate::steps_log::{Action, CLICK, KEY};
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        // 還沒開始：不記
        assert!(s.rec.add_click(10, 10).is_none());
        s.rec.add_key("Ctrl + C", "");
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
        for _ in 0..100 {
            if s.rec.status().frames > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let t = s.rec.add_click(960, 540).expect("範圍內的點擊");
        s.rec.add_action(Action { t, kind: CLICK, name: "存檔".into(), role: "按鈕".into(), window: "記事本".into() });
        tokio::time::sleep(Duration::from_millis(40)).await;
        s.rec.add_key("Ctrl + S", "記事本");
        assert!(s.rec.stop(None).await.unwrap().unwrap().ok);
        // 停止後才讀到的不收
        s.rec.add_action(Action { t: 9.0, kind: CLICK, name: "晚到".into(), role: String::new(), window: String::new() });
        let m = s.deps.marks.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("有操作步驟就要存");
        assert_eq!(m.actions.iter().map(|a| (a.kind, a.name.as_str())).collect::<Vec<_>>(), [(CLICK, "存檔"), (KEY, "Ctrl + S")]);
        assert!(m.actions[1].t >= m.actions[0].t);
        let text = crate::steps_log::to_text(&m.actions, "Rec");
        assert!(text.contains("在「記事本」點「存檔」（按鈕）") && text.contains("按 Ctrl + S"), "{text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn zoom_marks_are_saved_with_the_recording() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        assert!(s.rec.add_zoom_mark(10, 10).is_err(), "沒在錄影");
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
        let r = s.rec.frame_info().unwrap().area;
        // 範圍外：不算，說明要把游標移進去
        assert!(s.rec.add_zoom_mark(r.x - 5, r.y).unwrap_err().message().contains("不在錄影範圍"));
        assert_eq!(s.rec.add_zoom_mark(r.x + r.width / 4, r.y + r.height / 2).unwrap(), 1);
        // 半秒內連按：換成新的位置，不多算一個
        assert_eq!(s.rec.add_zoom_mark(r.x + r.width / 2, r.y + r.height / 2).unwrap(), 1);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(s.rec.add_zoom_mark(r.x, r.y).unwrap(), 2);
        assert!(s.rec.stop(None).await.unwrap().unwrap().ok);
        let m = s.deps.marks.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("有標記就要存");
        assert_eq!(m.zooms.len(), 2);
        assert_eq!((m.zooms[0][1], m.zooms[0][2]), (0.5, 0.5));
        assert_eq!((m.zooms[1][1], m.zooms[1][2]), (0.0, 0.0));
        assert!(m.zooms[1][0] > m.zooms[0][0]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mic_mute_is_toggled_and_saved_with_the_recording() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        assert!(s.rec.toggle_mic_mute().is_err(), "沒在錄影");
        // 沒錄麥克風：不能靜音
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
        assert_eq!(s.rec.status().mic_muted, None);
        assert!(s.rec.toggle_mic_mute().unwrap_err().message().contains("沒有錄麥克風"));
        assert!(s.rec.stop(None).await.unwrap().unwrap().ok);
        // 有錄麥克風：靜音一段、取消，再靜音到結尾
        let audio = AudioConfig { system: false, mic: true, mic_id: String::new(), app: String::new() };
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), audio, ..s.config() }).await.unwrap();
        assert_eq!(s.rec.status().mic_muted, Some(false));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(s.rec.toggle_mic_mute().unwrap());
        assert!(crate::audiopipe::MIC_MUTED.load(Ordering::Relaxed));
        assert_eq!(s.rec.status().mic_muted, Some(true));
        assert_eq!(s.rec.frame_info().unwrap().mic_muted, Some(true));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(!s.rec.toggle_mic_mute().unwrap());
        assert!(!crate::audiopipe::MIC_MUTED.load(Ordering::Relaxed));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(s.rec.toggle_mic_mute().unwrap());
        tokio::time::sleep(Duration::from_millis(40)).await;
        let st = s.rec.status();
        assert!(st.log.iter().any(|l| l.text.contains("麥克風靜音")), "{:?}", st.log);
        assert!(s.rec.stop(None).await.unwrap().unwrap().ok);
        // 錄完：取消靜音，兩段都存下來（第二段算到結尾）
        assert!(!crate::audiopipe::MIC_MUTED.load(Ordering::Relaxed));
        assert_eq!(s.rec.status().mic_muted, None);
        let m = s.deps.marks.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("有靜音的時段就要存");
        assert_eq!(m.mutes.len(), 2, "{:?}", m.mutes);
        assert!(m.mutes[0][1] - m.mutes[0][0] >= 0.07 && m.mutes[1][0] > m.mutes[0][1], "{:?}", m.mutes);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replay_buffer_saves_recent_part_and_discards_the_rest() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        // 倒數、最長錄影時間、縮小視窗都不適用
        s.rec.start(RecordConfig { replay_sec: 60, max_minutes: 1.0, ..s.config() }).await.unwrap();
        assert_eq!(s.rec.status().state, RecorderState::Recording);
        for _ in 0..100 {
            if s.rec.status().frames > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = s.rec.status();
        assert!(st.frames > 0);
        assert_eq!((st.replay.as_ref().map(|r| r.sec), st.max_ms), (Some(60), 0));
        assert_eq!(s.deps.before.load(Ordering::SeqCst), 0);
        assert!(s.rec.output_path().is_none());
        assert!(st.log.iter().any(|l| l.text.contains("回溯錄影：保留最近 1 分鐘")), "{:?}", st.log);
        // 不能暫停、加標記
        assert!(s.rec.pause().await.unwrap_err().message().contains("不能暫停"));
        assert!(s.rec.add_marker().is_err());
        // 存下：接起緩衝裡的檔案，存成 Replay_<時間>.mp4；可以存好幾次
        let a = s.rec.save_replay().await.unwrap();
        let b = s.rec.save_replay().await.unwrap();
        let (a, b) = (a.path.unwrap(), b.path.unwrap());
        assert_ne!(a, b);
        for p in [&a, &b] {
            assert!(Path::new(p).file_name().unwrap().to_string_lossy().starts_with("Replay_"), "{p}");
            assert_eq!(std::fs::read_to_string(p).unwrap(), "merged\n");
        }
        let st = s.rec.status();
        assert_eq!(st.replay.as_ref().map(|r| (r.saved, r.last.clone(), r.saving)), Some((2, Some(b.clone()), false)));
        assert_eq!(st.state, RecorderState::Recording);
        // 停止：沒存下的部分刪掉，不顯示「錄影已儲存」
        assert!(s.rec.stop(None).await.unwrap().is_none());
        let st = s.rec.status();
        assert_eq!((st.state, st.result.is_none(), st.replay.is_none()), (RecorderState::Idle, true, true));
        assert!(!s.parts_left());
        assert!(st.log.iter().any(|l| l.text.contains("已停止回溯錄影（這次存下 2 支）")), "{:?}", st.log);
        assert_eq!(s.deps.after.load(Ordering::SeqCst), 1);
        assert!(s.rec.save_replay().await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recorded_screen_unplugged_stops_and_saves_at_once() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
        for _ in 0..100 {
            if s.rec.status().frames > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // 只剩另一台螢幕（錄的那台拔掉了）
        *s.deps.screens.lock().unwrap_or_else(|e| e.into_inner()) = Some(vec![MonitorInfo { id: "0:0".into(), device_name: r"\\.\DISPLAY2".into(), display_number: 2, ..mon() }]);
        s.rec.displays_changed().await;
        for _ in 0..200 {
            if s.rec.status().state == RecorderState::Idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Idle);
        assert!(st.log.iter().any(|l| l.text.contains("已拔掉或關掉")), "{:?}", st.log);
        // 之前錄到的部分有存下來
        assert!(st.result.as_ref().is_some_and(|r| r.ok), "{:?}", st.result);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn renumbered_screen_keeps_recording_the_same_one() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        s.rec.start(RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), ..s.config() }).await.unwrap();
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
        // 同一台螢幕，編號變了（例如拔掉了另一台）
        *s.deps.screens.lock().unwrap_or_else(|e| e.into_inner()) = Some(vec![MonitorInfo { id: "1:0".into(), adapter: 1, ..mon() }]);
        s.rec.displays_changed().await;
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Recording);
        assert!(st.log.iter().any(|l| l.text.contains("繼續錄同一台螢幕")), "{:?}", st.log);
        // 沒變：什麼都不做
        s.rec.displays_changed().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let res = s.rec.stop(None).await.unwrap().unwrap();
        assert!(res.ok, "{}", res.message);
        assert!(res.message.contains("2 個分段"), "{}", res.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn move_region_while_recording_starts_a_new_segment() {
        let tools = tempfile::tempdir().unwrap();
        let s = Setup::new(Fake { ffmpeg: Some(fake_ffmpeg(tools.path())), ..Default::default() });
        s.release();
        let cfg = RecordConfig { countdown_sec: Some(0.0), hide_ui: Some(false), source: region(0.0, 0.0, 640.0, 480.0), ..s.config() };
        s.rec.start(cfg).await.unwrap();
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
        let r = s.rec.move_region(400, 300).await.unwrap();
        assert_eq!(r, Rect { x: 400, y: 300, width: 640, height: 480 });
        let st = s.rec.status();
        assert_eq!(st.state, RecorderState::Recording);
        assert!(st.log.iter().any(|l| l.text.contains("錄影範圍移到 (400, 300)")));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let res = s.rec.stop(None).await.unwrap().unwrap();
        assert!(res.ok, "{}", res.message);
        assert!(res.message.contains("2 個分段"), "{}", res.message);
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
        // 結果也說出原因
        assert_eq!(st.result.unwrap().message, "沒有擷取到任何畫面，未產生影片（Could not find gdigrab device）");
    }
}
