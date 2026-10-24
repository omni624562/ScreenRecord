//! 剪輯視窗的影片播放：用 FFmpeg 解碼成固定大小的 RGBA 畫面（不需要瀏覽器或系統解碼器）。
//! - 暫停時跳到某個時間：解出那一張（拖曳時只保留最新的要求，前一張解完才解下一張）。
//!   解碼器保持開著：往後一張、往後幾秒直接接著解（不用每次從關鍵影格重新解）；
//!   最近解過的畫面記在快取裡，往前一張也是立即顯示。
//! - 播放：一個 FFmpeg 連續輸出畫面、另一個輸出聲音（WASAPI 播放）；聲音開始出聲時開始計時，
//!   畫面依時間顯示（跟不上時跳過）
//! - 時間軸縮圖：只解關鍵影格，快速取得各時間點的小圖
//!
//! 解出的畫面放在共用的位置，介面每一畫格取走最新的一張（wake 會在有新畫面時被呼叫，用來要求重畫）。

use crate::process::std_command;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 一張畫面（RGBA，由上而下）
#[derive(Clone)]
pub struct Frame {
    /// 原影片中的時間（秒）
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({:.3}s, {}x{})", self.time, self.width, self.height)
    }
}

#[derive(Debug, Clone)]
pub struct MediaSpec {
    pub path: String,
    pub duration: f64,
    pub fps: f64,
    pub has_audio: bool,
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// 解碼大小：放進 max_w × max_h（不放大），偶數
pub fn fit_size(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (max_w.max(2) / 2 * 2, max_h.max(2) / 2 * 2);
    }
    let k = (max_w as f64 / src_w as f64).min(max_h as f64 / src_h as f64).min(1.0);
    let w = ((src_w as f64 * k).round() as u32 / 2 * 2).max(2);
    let h = ((src_h as f64 * k).round() as u32 / 2 * 2).max(2);
    (w, h)
}

/// 連續播放時每秒最多送出幾張：大畫面高張數（手機 4K 60p）解碼、縮小、轉色彩、送到介面都很吃力，
/// 跟不上時會一直跳張（看起來卡卡的）；預覽用 30 張就夠順
pub const LIGHT_FPS: f64 = 30.0;

/// 大於 1080p 的畫面（4K、2K）
pub fn big(src_w: u32, src_h: u32) -> bool {
    (src_w as u64 * src_h as u64) > 1920 * 1080 * 11 / 10
}

/// 大於 1080p 又超過 30 fps 的影片：連續播放時改用省力的方式（見 LIGHT_FPS）
pub fn heavy(src_w: u32, src_h: u32, fps: f64) -> bool {
    big(src_w, src_h) && fps > LIGHT_FPS + 2.0
}

/// 旋轉標記（見 args::parse_rotation）換成縮小之後再轉正的濾鏡：不轉時空字串；不是 90° 的倍數時 None（交給 FFmpeg 自動轉正）
pub fn turn_filter(rotation: f64) -> Option<&'static str> {
    let r = rotation.round();
    if (rotation - r).abs() > 0.5 {
        return None;
    }
    // FFmpeg 自動轉正的做法：順時針轉 -rotation 度
    match (-(r as i64)).rem_euclid(360) {
        0 => Some(""),
        90 => Some("transpose=clock"),
        180 => Some("hflip,vflip"),
        270 => Some("transpose=cclock"),
        _ => None,
    }
}

/// 讀影片的旋轉標記（ffmpeg -i，很快）：讀不到時 None
fn probe_turn(ffmpeg: &Path, path: &str) -> Option<&'static str> {
    let out = std_command(ffmpeg).args(["-hide_banner", "-nostdin", "-i", path]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().ok()?;
    turn_filter(crate::args::parse_rotation(&String::from_utf8_lossy(&out.stderr))?)
}

/// 連續播放的參數（w × h：轉正後的大小）。
/// - rate：固定成每秒幾張（時間依張數計算，手機影片的張數不固定）；None = 照影片原本的。
///   CPU 解碼時有 rate 就是省力播放：解碼略過去區塊濾波、較快的縮小方式
/// - turn：大畫面不讓 FFmpeg 先把整張轉正，縮小後再轉（見 turn_filter）；None = 照 FFmpeg 自動轉正
/// - gpu：在顯示卡上解碼、縮小（D3D11），只把縮小後的畫面拿回來；不然每一張 4K 都要從顯示卡搬回來、用 CPU 縮小
fn play_args(path: &str, t: f64, w: u32, h: u32, rate: Option<f64>, turn: Option<&str>, gpu: bool) -> Vec<String> {
    let light = rate.is_some() && !gpu;
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].iter().map(|s| s.to_string()).collect();
    let hw: &[&str] = if gpu {
        &["-init_hw_device", "d3d11va=gpu", "-filter_hw_device", "gpu", "-hwaccel", "d3d11va", "-hwaccel_device", "gpu", "-hwaccel_output_format", "d3d11"]
    } else {
        // 不支援時 FFmpeg 自動改用軟體解碼
        &["-hwaccel", "auto"]
    };
    a.extend(hw.iter().map(|s| s.to_string()));
    if light {
        a.extend(["-skip_loop_filter".into(), "all".into()]);
    }
    if turn.is_some() {
        a.push("-noautorotate".into());
    }
    a.extend(crate::args::input_at(path, &format!("{:.3}", t.max(0.0))));
    a.extend(["-an".into(), "-sn".into()]);
    // 轉正之前的大小
    let (sw, sh) = if turn.is_some_and(|t| t.starts_with("transpose")) { (h, w) } else { (w, h) };
    let mut f: Vec<String> = Vec::new();
    if let Some(r) = rate {
        f.push(format!("fps={}", crate::format::num(r)));
    }
    if gpu {
        f.extend([format!("scale_d3d11=width={sw}:height={sh}:format=nv12"), "hwdownload".into(), "format=nv12".into()]);
    } else {
        f.push(format!("scale={sw}:{sh}:flags={}", if light { "fast_bilinear" } else { "bilinear" }));
    }
    if let Some(t) = turn.filter(|t| !t.is_empty()) {
        f.push(t.into());
    }
    f.push("format=rgba".into());
    a.extend(["-vf".into(), f.join(","), "-f".into(), "rawvideo".into(), "-pix_fmt".into(), "rgba".into(), "-".into()]);
    a
}

/// 解出單張畫面（暫停時、時間軸縮圖）的參數
fn video_args(path: &str, t: f64, w: u32, h: u32, frames: Option<u32>, keyframes_only: bool) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].iter().map(|s| s.to_string()).collect();
    if keyframes_only {
        a.extend(["-skip_frame".into(), "nokey".into()]);
    }
    a.extend(crate::args::input_at(path, &format!("{:.3}", t.max(0.0))));
    a.extend(["-an".into(), "-sn".into()]);
    if let Some(n) = frames {
        a.extend(["-frames:v".into(), n.to_string()]);
    }
    a.extend(["-vf".into(), format!("scale={w}:{h}:flags=bilinear,format=rgba"), "-f".into(), "rawvideo".into(), "-pix_fmt".into(), "rgba".into(), "-".into()]);
    a
}

/// rate：播放速度（1 = 原速；變速不變調）
fn audio_args(path: &str, t: f64, rate: f64) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].iter().map(|s| s.to_string()).collect();
    a.extend(crate::args::input_at(path, &format!("{:.3}", t.max(0.0))));
    a.extend(["-vn".into(), "-sn".into()]);
    if (rate - 1.0).abs() > 1e-3 {
        a.extend(["-af".into(), format!("atempo={}", crate::format::num(rate))]);
    }
    a.extend(["-f".into(), "f32le".into(), "-ac".into(), "2".into(), "-ar".into(), crate::audio_out::SAMPLE_RATE.to_string(), "-".into()]);
    a
}

fn spawn(ffmpeg: &Path, args: &[String]) -> Option<Child> {
    std_command(ffmpeg).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()
}

/// 讀滿一張畫面；資料結束時回傳 false
fn read_frame(r: &mut impl Read, buf: &mut [u8]) -> bool {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) | Err(_) => return false,
            Ok(k) => n += k,
        }
    }
    true
}

/// 解出時間 t 的一張畫面
pub fn decode_frame(ffmpeg: &Path, path: &str, t: f64, w: u32, h: u32, keyframes_only: bool) -> Option<Vec<u8>> {
    let mut child = spawn(ffmpeg, &video_args(path, t, w, h, Some(1), keyframes_only))?;
    let mut out = child.stdout.take()?;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let ok = read_frame(&mut out, &mut buf);
    let _ = child.kill();
    let _ = child.wait();
    ok.then_some(buf)
}

/// 暫停時用的解碼器：保持開著，往後跳一點點時接著讀下一張；最近的畫面放在快取
#[derive(Default)]
struct Scrub {
    child: Option<Child>,
    out: Option<ChildStdout>,
    /// 解碼器下一張會輸出第幾張（以 fps 換算的張數）
    next_idx: i64,
    /// 最近解過的畫面：(第幾張, RGBA)
    cache: VecDeque<(i64, Arc<Vec<u8>>)>,
    /// 上一次要求的是第幾張（判斷往前或往後）
    last_idx: Option<i64>,
}

impl Scrub {
    fn close(&mut self) {
        self.out = None;
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 快取最多佔用的記憶體
const SCRUB_CACHE_BYTES: usize = 96 * 1024 * 1024;
/// 往後跳這麼多秒以內：接著讀，不重新開始
const SCRUB_FORWARD_SEC: f64 = 3.0;
/// 往前逐張移動時，先多解這麼多秒放進快取（之後再往前就不用重新解）
const SCRUB_BACK_SEC: f64 = 1.0;

/// 解出第 idx 張（暫停時用）；能用快取或接著讀就不重新開始
fn scrub_frame(sc: &mut Scrub, ffmpeg: &Path, path: &str, fps: f64, idx: i64, w: u32, h: u32) -> Option<Arc<Vec<u8>>> {
    let size = (w * h * 4) as usize;
    let max_frames = (SCRUB_CACHE_BYTES / size.max(1)).max(8);
    let backward_step = sc.last_idx.is_some_and(|l| idx < l && l - idx <= 3);
    sc.last_idx = Some(idx);
    if let Some((_, f)) = sc.cache.iter().find(|(i, _)| *i == idx) {
        return Some(f.clone());
    }
    let ahead = idx - sc.next_idx;
    if sc.out.is_none() || ahead < 0 || ahead as f64 > SCRUB_FORWARD_SEC * fps {
        // 重新開始：往前逐張移動時從前面一點開始，順便把前面的畫面放進快取
        sc.close();
        let start = if backward_step { (idx - (SCRUB_BACK_SEC * fps) as i64 + 1).max(0) } else { idx };
        // 從半張前開始找，第一張輸出的就是第 start 張
        let ss = ((start as f64 - 0.5) / fps).max(0.0);
        let mut args = video_args(path, ss, w, h, None, false);
        let at = args.iter().position(|a| a == "-vf").unwrap_or(args.len());
        args.splice(at..at, ["-fps_mode".to_string(), "cfr".into(), "-r".into(), format!("{fps}")]);
        let mut child = spawn(ffmpeg, &args)?;
        sc.out = child.stdout.take();
        sc.child = Some(child);
        sc.next_idx = start;
    }
    let out = sc.out.as_mut()?;
    while sc.next_idx <= idx {
        let mut buf = vec![0u8; size];
        if !read_frame(out, &mut buf) {
            // 影片結束：用最後一張
            sc.close();
            return sc.cache.iter().filter(|(i, _)| *i <= idx).max_by_key(|(i, _)| *i).map(|(_, f)| f.clone());
        }
        let f = Arc::new(buf);
        sc.cache.retain(|(i, _)| *i != sc.next_idx);
        sc.cache.push_back((sc.next_idx, f.clone()));
        while sc.cache.len() > max_frames {
            sc.cache.pop_front();
        }
        sc.next_idx += 1;
        if sc.next_idx > idx {
            return Some(f);
        }
    }
    None
}

#[derive(Default)]
struct Inner {
    /// 最新、還沒被取走的畫面
    frame: Option<Frame>,
    playing: bool,
    /// 播放：從哪個時間開始、何時開始計時（還沒開始出聲 / 出畫面時為 None）
    play_from: f64,
    clock: Option<Instant>,
    /// 暫停時的位置
    pos: f64,
    /// 播放到結尾
    ended: bool,
    /// 每次播放 / 暫停 / 跳到其他時間加一：舊的執行緒看到不同就停止
    gen: u64,
    /// 暫停時要解的那一張（拖曳時只保留最新的）
    still: Option<f64>,
    still_busy: bool,
    /// 播放速度（0 = 還沒設定，當作 1）
    rate: f64,
}

impl Inner {
    fn rate(&self) -> f64 {
        if self.rate > 0.0 {
            self.rate
        } else {
            1.0
        }
    }
}

pub struct Player {
    ffmpeg: PathBuf,
    spec: MediaSpec,
    pub width: u32,
    pub height: u32,
    inner: Arc<Mutex<Inner>>,
    children: Arc<Mutex<Vec<Child>>>,
    scrub: Arc<Mutex<Scrub>>,
    stop_audio: Arc<Mutex<Arc<AtomicBool>>>,
    volume: crate::audio_out::Volume,
    wake: Wake,
    /// 只有聲音（M4A）：不解畫面
    audio_only: bool,
    /// 大畫面高張數：連續播放時用省力的方式（見 heavy）
    light: bool,
    /// 大於 1080p（見 big）：縮小後再轉正，Windows 上在顯示卡縮小
    big: bool,
    /// 縮小後再轉正的濾鏡（第一次播放時讀）
    turn: Arc<OnceLock<Option<&'static str>>>,
    /// 在顯示卡縮小失敗過（不支援）：之後都用一般方式
    gpu_failed: Arc<AtomicBool>,
}

/// 播放速度可選的範圍
pub const RATE_MIN: f64 = 0.5;
pub const RATE_MAX: f64 = 2.0;

impl Player {
    /// src_w × src_h：原影片尺寸；解碼成不超過 max_w × max_h 的大小
    pub fn new(ffmpeg: PathBuf, spec: MediaSpec, src_w: u32, src_h: u32, max_w: u32, max_h: u32, wake: Wake) -> Player {
        let (width, height) = fit_size(src_w, src_h, max_w, max_h);
        let light = heavy(src_w, src_h, spec.fps);
        Player {
            ffmpeg,
            spec,
            width,
            height,
            inner: Arc::default(),
            children: Arc::default(),
            scrub: Arc::default(),
            stop_audio: Arc::new(Mutex::new(Arc::new(AtomicBool::new(false)))),
            volume: crate::audio_out::volume(1.0),
            wake,
            audio_only: false,
            light,
            big: big(src_w, src_h),
            turn: Arc::default(),
            gpu_failed: Arc::default(),
        }
    }

    /// 只有聲音的檔案（M4A）：只播放聲音，沒有畫面
    pub fn new_audio(ffmpeg: PathBuf, spec: MediaSpec, wake: Wake) -> Player {
        let mut p = Player::new(ffmpeg, MediaSpec { has_audio: true, ..spec }, 2, 2, 2, 2, wake);
        p.audio_only = true;
        p
    }

    /// 播放速度（RATE_MIN～RATE_MAX，聲音變速不變調）；播放中立即從目前的位置換成新的速度
    pub fn set_rate(&self, rate: f64) {
        let rate = if rate.is_finite() { rate.clamp(RATE_MIN, RATE_MAX) } else { 1.0 };
        let playing = {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if (i.rate() - rate).abs() < 1e-6 {
                return;
            }
            crate::info!("[播放] 播放速度 {}×", crate::format::num(rate));
            // 換速度前先記下目前的位置（之後的時間用新的速度算）
            let t = Self::time_of(&i, self.spec.duration);
            i.rate = rate;
            if !i.playing {
                i.pos = t;
            }
            i.playing.then_some(t)
        };
        if let Some(t) = playing {
            self.play_from(t);
        }
    }

    pub fn rate(&self) -> f64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).rate()
    }

    /// 音量 0～1（播放中立即生效）
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn spec(&self) -> &MediaSpec {
        &self.spec
    }

    fn last_frame_time(&self) -> f64 {
        (self.spec.duration - 1.0 / self.spec.fps.max(1.0)).max(0.0)
    }

    /// 目前時間（播放中依時鐘推算）
    pub fn time(&self) -> f64 {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::time_of(&i, self.spec.duration)
    }

    fn time_of(i: &Inner, duration: f64) -> f64 {
        if i.playing {
            match i.clock {
                Some(c) => (i.play_from + c.elapsed().as_secs_f64() * i.rate()).min(duration),
                None => i.play_from,
            }
        } else {
            i.pos
        }
    }

    pub fn is_playing(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).playing
    }

    /// 播放到結尾後停止（取一次後清除）
    pub fn take_ended(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap_or_else(|e| e.into_inner()).ended)
    }

    /// 取走最新的畫面
    pub fn take_frame(&self) -> Option<Frame> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).frame.take()
    }

    fn kill_children(&self) {
        self.stop_audio.lock().unwrap_or_else(|e| e.into_inner()).store(true, Ordering::Relaxed);
        let list: Vec<Child> = self.children.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        if !list.is_empty() {
            // 在背景結束（Windows 上等行程結束可能要上百毫秒，不卡住介面）
            std::thread::spawn(move || {
                for mut c in list {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            });
        }
    }

    /// 暫停（保留目前時間）
    pub fn pause(&self) {
        {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !i.playing {
                return;
            }
            i.pos = Self::time_of(&i, self.spec.duration);
            i.playing = false;
            i.clock = None;
            i.gen += 1;
        }
        self.kill_children();
    }

    /// 跳到時間 t：播放中從那裡繼續播放，暫停時解出那一張
    pub fn seek(&self, t: f64) {
        let t = t.clamp(0.0, self.spec.duration.max(0.0));
        let playing = self.is_playing();
        if playing {
            self.play_from(t);
            return;
        }
        {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            i.pos = t;
            // 只有聲音：沒有畫面要解
            if self.audio_only {
                return;
            }
            i.still = Some(t);
            if i.still_busy {
                return;
            }
            i.still_busy = true;
        }
        let (inner, ffmpeg, path, w, h, wake, last) = (self.inner.clone(), self.ffmpeg.clone(), self.spec.path.clone(), self.width, self.height, self.wake.clone(), self.last_frame_time());
        let (scrub, fps) = (self.scrub.clone(), self.spec.fps.max(1.0));
        std::thread::spawn(move || loop {
            let (t, gen) = {
                let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                match i.still.take() {
                    Some(t) => (t, i.gen),
                    None => {
                        i.still_busy = false;
                        return;
                    }
                }
            };
            let idx = (t.min(last) * fps + 1e-6).floor() as i64;
            let got = scrub_frame(&mut scrub.lock().unwrap_or_else(|e| e.into_inner()), &ffmpeg, &path, fps, idx, w, h);
            if let Some(rgba) = got {
                let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                if i.gen == gen && !i.playing {
                    i.frame = Some(Frame { time: t, width: w, height: h, rgba: (*rgba).clone() });
                    drop(i);
                    wake();
                }
            }
        });
    }

    /// 從時間 t 開始播放（不像先 seek 再 play：暫停時的 seek 會多開一個 FFmpeg 解那一張）
    pub fn play_at(&self, t: f64) {
        self.play_from(t.clamp(0.0, self.spec.duration.max(0.0)));
    }

    /// 從目前位置開始播放
    pub fn play(&self) {
        let t = self.time();
        self.play_from(t);
    }

    fn play_from(&self, t: f64) {
        self.kill_children();
        let gen = {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            i.gen += 1;
            i.playing = true;
            i.play_from = t.clamp(0.0, self.last_frame_time());
            i.pos = i.play_from;
            i.clock = None;
            i.ended = false;
            i.still = None;
            i.gen
        };
        let from = self.inner.lock().unwrap_or_else(|e| e.into_inner()).play_from;
        let stop = Arc::new(AtomicBool::new(false));
        *self.stop_audio.lock().unwrap_or_else(|e| e.into_inner()) = stop.clone();

        // 開 FFmpeg 在背景做（Windows 上啟動行程要幾十到上百毫秒，不卡住介面）
        let (ffmpeg, spec, children, inner, wake, w, h) = (self.ffmpeg.clone(), self.spec.clone(), self.children.clone(), self.inner.clone(), self.wake.clone(), self.width, self.height);
        let volume = self.volume.clone();
        let (audio_only, light, rate) = (self.audio_only, self.light, self.inner.lock().unwrap_or_else(|e| e.into_inner()).rate());
        let (big, turn_cell, gpu_failed) = (self.big, self.turn.clone(), self.gpu_failed.clone());
        std::thread::spawn(move || {
            // 記下子行程以便停止；已經換了一輪（暫停 / 跳到其他時間）就直接結束它
            let register = |mut child: Child| -> bool {
                let mut list = children.lock().unwrap_or_else(|e| e.into_inner());
                if inner.lock().unwrap_or_else(|e| e.into_inner()).gen != gen {
                    drop(list);
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                list.push(child);
                true
            };
            // 聲音：開始出聲時開始計時（以聲音為準，畫面配合）
            let mut audio_started = false;
            if spec.has_audio {
                if let Some(mut child) = spawn(&ffmpeg, &audio_args(&spec.path, from, rate)) {
                    if let Some(out) = child.stdout.take() {
                        if !register(child) {
                            return;
                        }
                        let inner = inner.clone();
                        let stop2 = stop.clone();
                        audio_started = true;
                        std::thread::spawn(move || {
                            let start_clock = |latency: f64| {
                                let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                                if i.gen == gen && i.clock.is_none() {
                                    i.clock = Some(Instant::now() + Duration::from_secs_f64(latency.clamp(0.0, 0.5)));
                                }
                            };
                            if crate::audio_out::play(out, stop2, volume, start_clock).is_err() {
                                // 沒有播放裝置：改由畫面計時
                                let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                                if i.gen == gen && i.clock.is_none() {
                                    i.clock = Some(Instant::now());
                                }
                            }
                        });
                    }
                }
            }

            // 只有聲音：依時間判斷播完（沒有播放裝置時也照時間走）
            if audio_only {
                let started = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                    let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                    if i.gen != gen {
                        return;
                    }
                    if i.clock.is_none() && started.elapsed() > Duration::from_millis(800) {
                        i.clock = Some(Instant::now());
                    }
                    if Self::time_of(&i, spec.duration) >= spec.duration - 0.01 {
                        i.playing = false;
                        i.clock = None;
                        i.pos = spec.duration;
                        i.ended = true;
                        drop(i);
                        wake();
                        return;
                    }
                }
            }
            // 連續播放用硬體解碼（大畫面時省很多 CPU）
            let turn = if big { *turn_cell.get_or_init(|| probe_turn(&ffmpeg, &spec.path)) } else { None };
            // 顯示卡：解得動，照原本的張數（60p 就是每秒 60 張）；CPU：大畫面高張數時每秒 LIGHT_FPS 張
            let native = spec.fps.max(1.0);
            let rate_of = |gpu: bool| if light && !gpu { LIGHT_FPS } else { native };
            let start = |gpu: bool| -> Option<ChildStdout> {
                let fixed = (gpu || light).then(|| rate_of(gpu));
                let mut child = spawn(&ffmpeg, &play_args(&spec.path, from, w, h, fixed, turn, gpu))?;
                let out = child.stdout.take()?;
                register(child).then_some(out)
            };
            let try_gpu = cfg!(windows) && big && turn.is_some() && !gpu_failed.load(Ordering::Relaxed);
            let Some(mut out) = start(try_gpu) else { return };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let mut first = read_frame(&mut out, &mut buf);
            let mut used_gpu = try_gpu;
            if try_gpu {
                if first {
                    crate::info!("[播放] 在顯示卡縮小畫面：{}", crate::jellyfin::redact(&spec.path));
                } else if inner.lock().unwrap_or_else(|e| e.into_inner()).gen == gen {
                    // 顯示卡或 FFmpeg 不支援（例如太舊的顯示卡解不了 HEVC 10-bit）：改用一般方式，之後也不再試
                    gpu_failed.store(true, Ordering::Relaxed);
                    crate::info!("[播放] 無法在顯示卡縮小畫面，改用一般方式：{}", crate::jellyfin::redact(&spec.path));
                    used_gpu = false;
                    let Some(o) = start(false) else { return };
                    out = o;
                    first = read_frame(&mut out, &mut buf);
                }
            }
            // 時間依送出的張數計算
            let (fps, duration) = (rate_of(used_gpu), spec.duration);
            std::thread::spawn(move || {
                let mut pending = Some(first);
                let mut n: u64 = 0;
                let waited_for_audio = Instant::now();
                let mut last_shown = Instant::now();
                loop {
                    if !pending.take().unwrap_or_else(|| read_frame(&mut out, &mut buf)) {
                        let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                        if i.gen == gen {
                            i.playing = false;
                            i.clock = None;
                            i.pos = (from + n as f64 / fps).min(duration);
                            i.ended = true;
                            drop(i);
                            wake();
                        }
                        return;
                    }
                    let t = from + n as f64 / fps;
                    n += 1;
                    // 等到這張的時間；第一張立即顯示
                    loop {
                        let now_t = {
                            let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                            if i.gen != gen {
                                return;
                            }
                            if i.clock.is_none() && (!audio_started || waited_for_audio.elapsed() > Duration::from_millis(800)) {
                                i.clock = Some(Instant::now());
                            }
                            match i.clock {
                                Some(c) if Instant::now() >= c => i.play_from + c.elapsed().as_secs_f64() * i.rate(),
                                _ => i.play_from,
                            }
                        };
                        if t <= now_t + 0.004 || n == 1 {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs_f64((t - now_t).min(0.02)));
                    }
                    // 跟不上：已經晚了兩張以上就跳過（不顯示）；但最多 0.15 秒就要顯示一張，
                    // 不然解碼一直落後時每一張都「晚了」，畫面會整個停住
                    let late = {
                        let i = inner.lock().unwrap_or_else(|e| e.into_inner());
                        let now_t = Self::time_of(&i, duration);
                        n > 1 && now_t - t > 2.0 / fps
                    };
                    if late && last_shown.elapsed() < Duration::from_millis(150) {
                        continue;
                    }
                    last_shown = Instant::now();
                    let mut i = inner.lock().unwrap_or_else(|e| e.into_inner());
                    if i.gen != gen {
                        return;
                    }
                    i.frame = Some(Frame { time: t, width: w, height: h, rgba: buf.clone() });
                    drop(i);
                    wake();
                }
            });
        });
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).gen += 1;
        self.kill_children();
        // 解碼中可能還拿著鎖：在背景關閉，不卡住介面
        let scrub = self.scrub.clone();
        std::thread::spawn(move || scrub.lock().unwrap_or_else(|e| e.into_inner()).close());
    }
}

/// 時間軸縮圖：依序解出各時間點的小圖（只解關鍵影格，很快）。cancel 被設定時停止。
pub fn strip_frames(ffmpeg: &Path, path: &str, times: &[f64], w: u32, h: u32, cancel: &AtomicBool, mut on_frame: impl FnMut(usize, Frame)) {
    for (k, &t) in times.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Some(rgba) = decode_frame(ffmpeg, path, t, w, h, true).or_else(|| decode_frame(ffmpeg, path, t, w, h, false)) {
            on_frame(k, Frame { time: t, width: w, height: h, rgba });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_videos_play_lighter() {
        // 手機 4K 60p（直的、橫的）：省力播放；1080p 60p、4K 30p、一般錄影：照原本的方式
        assert!(heavy(3840, 2160, 60.0) && heavy(2160, 3840, 59.94));
        assert!(!heavy(1920, 1080, 60.0) && !heavy(3840, 2160, 30.0) && !heavy(2560, 1440, 30.0));
        assert!(big(3840, 2160) && big(2560, 1440) && !big(1920, 1080));
        // 一般影片：和以前一樣（硬體解碼、FFmpeg 自動轉正）
        let a = play_args("a.mp4", 1.0, 1280, 720, None, None, false).join(" ");
        assert!(a.starts_with("-hide_banner -loglevel error -nostdin -hwaccel auto -ss 1.000 -i a.mp4"), "{a}");
        assert!(a.contains("-vf scale=1280:720:flags=bilinear,format=rgba -f rawvideo"), "{a}");
        // 4K 60p：每秒 30 張、略過去區塊濾波
        let b = play_args("a.mp4", 1.0, 1280, 720, Some(LIGHT_FPS), Some(""), false).join(" ");
        assert!(b.contains("-skip_loop_filter all -noautorotate -ss 1.000 -i a.mp4"), "{b}");
        assert!(b.contains("-vf fps=30,scale=1280:720:flags=fast_bilinear,format=rgba"), "{b}");
        // 直拍：先縮成橫的小畫面再轉正（不轉整張 4K）
        let c = play_args("a.mp4", 0.0, 360, 640, Some(LIGHT_FPS), Some("transpose=clock"), false).join(" ");
        assert!(c.contains("-vf fps=30,scale=640:360:flags=fast_bilinear,transpose=clock,format=rgba"), "{c}");
        // 顯示卡：解碼、縮小都在顯示卡上，只拿回小畫面；照原本的張數
        let d = play_args("a.mp4", 0.0, 360, 640, Some(59.94), Some("transpose=cclock"), true).join(" ");
        assert!(d.contains("-hwaccel d3d11va -hwaccel_device gpu -hwaccel_output_format d3d11 -noautorotate"), "{d}");
        assert!(d.contains("-vf fps=59.94,scale_d3d11=width=640:height=360:format=nv12,hwdownload,format=nv12,transpose=cclock,format=rgba"), "{d}");
        assert!(!d.contains("-hwaccel auto"), "{d}");
    }

    #[test]
    fn turn_filters_match_ffmpeg_autorotate() {
        assert_eq!(turn_filter(0.0), Some(""));
        assert_eq!(turn_filter(-90.0), Some("transpose=clock"));
        assert_eq!(turn_filter(90.0), Some("transpose=cclock"));
        assert_eq!(turn_filter(270.0), Some("transpose=clock"));
        assert_eq!(turn_filter(180.0), Some("hflip,vflip"));
        assert_eq!(turn_filter(-180.0), Some("hflip,vflip"));
        assert_eq!(turn_filter(45.0), None);
        assert_eq!(turn_filter(89.6), Some("transpose=cclock"));
    }

    /// 縮小後再轉正，和 FFmpeg 自動轉正（先轉再縮）看起來一樣
    #[test]
    fn turning_after_scaling_matches_autorotate() {
        let Some(ffmpeg) = which_ffmpeg() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.mp4");
        let made = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=320x180:r=30:d=1", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-y"])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        for rot in ["90", "-90", "180"] {
            let file = dir.path().join(format!("rot{rot}.mp4"));
            let ok = std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-display_rotation", rot, "-i"])
                .arg(&src)
                .args(["-c", "copy", "-y"])
                .arg(&file)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                return; // 太舊的 FFmpeg 沒有 -display_rotation
            }
            let path = file.display().to_string();
            let (w, h) = if rot == "180" { (160, 90) } else { (90, 160) };
            let turn = probe_turn(&ffmpeg, &path);
            assert!(turn.is_some_and(|t| !t.is_empty()), "{rot}: {turn:?}");
            let auto = decode_frame(&ffmpeg, &path, 0.0, w, h, false).expect("自動轉正");
            let mut args = play_args(&path, 0.0, w, h, Some(LIGHT_FPS), turn, false);
            args.splice(args.len() - 7..args.len() - 7, ["-frames:v".to_string(), "1".into()]);
            let mut child = spawn(&ffmpeg, &args).unwrap();
            let mut ours = vec![0u8; (w * h * 4) as usize];
            assert!(read_frame(child.stdout.as_mut().unwrap(), &mut ours), "{rot}: 沒有畫面");
            let _ = child.wait();
            let diff = auto.iter().zip(&ours).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64).sum::<u64>() / auto.len() as u64;
            assert!(diff < 12, "{rot}: 平均差 {diff}");
        }
    }

    /// 測試影片：每一張畫面的亮度 = 張數 × 8（看得出是第幾張），30 fps、2 秒，有聲音
    fn test_video(dir: &Path) -> Option<(PathBuf, String)> {
        let ffmpeg = which_ffmpeg()?;
        let out = dir.join("t.mp4");
        let ok = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=30:d=2,geq=lum='N*4':cb=128:cr=128", "-f", "lavfi", "-i", "sine=f=440:d=2"])
            .args(["-c:v", "libx264", "-g", "10", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", "-y"])
            .arg(&out)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        ok.then(|| (ffmpeg, out.display().to_string()))
    }

    fn which_ffmpeg() -> Option<PathBuf> {
        ["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"].iter().map(PathBuf::from).find(|p| p.exists())
    }

    fn brightness(f: &Frame) -> u8 {
        f.rgba[(f.rgba.len() / 2) & !3]
    }

    #[test]
    fn sizes() {
        assert_eq!(fit_size(3840, 1080, 1280, 720), (1280, 360));
        assert_eq!(fit_size(640, 360, 1280, 720), (640, 360));
        assert_eq!(fit_size(1920, 1080, 1001, 1001), (1000, 562));
    }

    #[test]
    fn scrub_steps_reuse_the_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let Some((ffmpeg, path)) = test_video(dir.path()) else { return };
        let mut sc = Scrub::default();
        // 第 n 張的亮度：Y = n×4（limited range 換成 RGB）
        let expect = |n: i64| (((n * 4 - 16) as f64) * 255.0 / 219.0).clamp(0.0, 255.0);
        let check = |sc: &mut Scrub, n: i64| {
            let f = scrub_frame(sc, &ffmpeg, &path, 30.0, n, 64, 36).expect("沒有畫面");
            let b = f[(f.len() / 2) & !3] as f64;
            assert!((b - expect(n)).abs() < 8.0, "第 {n} 張：亮度 {b}，應約 {}", expect(n));
        };
        check(&mut sc, 30);
        let pid = sc.child.as_ref().map(|c| c.id());
        // 往後一張、幾張：接著讀，不重新開始
        check(&mut sc, 31);
        check(&mut sc, 35);
        assert_eq!(sc.child.as_ref().map(|c| c.id()), pid);
        // 往前一張：在快取裡
        check(&mut sc, 34);
        check(&mut sc, 30);
        assert_eq!(sc.child.as_ref().map(|c| c.id()), pid);
        // 跳到前面沒解過的地方：重新開始，並把前面一段放進快取
        check(&mut sc, 12);
        check(&mut sc, 11);
        check(&mut sc, 10);
        // 跳到後面很遠：重新開始
        check(&mut sc, 50);
        check(&mut sc, 5);
    }

    #[test]
    fn seek_play_pause() {
        let dir = tempfile::tempdir().unwrap();
        let Some((ffmpeg, path)) = test_video(dir.path()) else { return };
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w2 = woke.clone();
        let p = Player::new(
            ffmpeg.clone(),
            MediaSpec { path: path.clone(), duration: 2.0, fps: 30.0, has_audio: true },
            64,
            36,
            64,
            36,
            Arc::new(move || {
                w2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let wait_frame = |p: &Player| {
            let start = Instant::now();
            loop {
                if let Some(f) = p.take_frame() {
                    return f;
                }
                assert!(start.elapsed() < Duration::from_secs(10), "沒有畫面");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        // 暫停時跳到其他時間：解出那一張（第 30 張，亮度約 120）
        p.seek(1.0);
        let f = wait_frame(&p);
        assert_eq!((f.width, f.height), (64, 36));
        assert!((brightness(&f) as i32 - 120).abs() < 12, "{}", brightness(&f));
        assert!(woke.load(Ordering::SeqCst) >= 1);
        // 播放：時間往前走，畫面跟著變
        p.play();
        assert!(p.is_playing());
        std::thread::sleep(Duration::from_millis(500));
        let t = p.time();
        assert!(t > 1.2 && t < 1.9, "{t}");
        let f = wait_frame(&p);
        assert!(f.time > 1.0);
        // 暫停：時間停住
        p.pause();
        let t1 = p.time();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(p.time(), t1);
        // 播到結尾：自動停止
        p.seek(1.8);
        p.play();
        let start = Instant::now();
        while p.is_playing() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!p.is_playing());
        assert!(p.take_ended());
        assert!(!p.take_ended());

        // 兩倍速：時間走得快一倍；換回原速
        p.seek(0.0);
        p.set_rate(2.0);
        p.play();
        std::thread::sleep(Duration::from_millis(400));
        let t = p.time();
        assert!(t > 0.5 && t < 1.2, "{t}");
        p.pause();
        p.set_rate(1.0);
        assert_eq!(p.rate(), 1.0);

        // 只有聲音：沒有畫面，時間照走、播到結尾停止
        let a = Player::new_audio(ffmpeg.clone(), MediaSpec { path: path.clone(), duration: 2.0, fps: 30.0, has_audio: true }, Arc::new(|| {}));
        a.seek(1.5);
        a.play();
        std::thread::sleep(Duration::from_millis(200));
        assert!(a.is_playing() && a.time() >= 1.5);
        let start = Instant::now();
        while a.is_playing() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(a.take_ended());
        assert!(a.take_frame().is_none());

        // 時間軸縮圖
        let cancel = AtomicBool::new(false);
        let mut got = vec![];
        strip_frames(&ffmpeg, &path, &[0.1, 1.0, 1.9], 32, 18, &cancel, |k, f| got.push((k, f.width)));
        assert_eq!(got, vec![(0, 32), (1, 32), (2, 32)]);
    }
}
