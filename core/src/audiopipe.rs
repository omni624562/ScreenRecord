//! 把 WASAPI 擷取的聲音對齊畫面時間後，以 raw float32 經本機 TCP 送進 FFmpeg。
//!
//! 時間對齊：
//! - 畫面：FFmpeg 的 showinfo 每張畫面印一行 pts；畫面時間零點 ≈ min(收到該行的 QPC 時間 − pts)，
//!   取開頭約 1 秒的最小值，排除管線延遲。
//! - 聲音：每個 WASAPI 封包都有 QPC 時間戳，依「封包時間 − 畫面零點」決定它在時間軸上的位置；
//!   落後就補靜音、超前就裁掉，長時間錄影也不會因音效卡時鐘誤差而漂移。
//! - 系統聲音沒有播放時 WASAPI 不送資料，依 QPC 時鐘補靜音維持連續。
//!
//! 執行緒：WASAPI 的 COM 物件不能跨執行緒，所以擷取、混音與送出都在同一條專用執行緒，
//! 其他執行緒透過指令通道（畫面時間、補到現在、關閉）跟它溝通。

use crate::audio::{AudioSourceSpec, Capture, Opener, CHANNELS, SAMPLE_RATE};
use crate::clock::now_100ns;
use crate::types::LogLevel;
use std::collections::VecDeque;
use std::io::{ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 每 25ms 取一次聲音：WASAPI 緩衝有 2 秒，間隔拉長不會掉資料
const TICK: Duration = Duration::from_millis(25);
/// 偏差超過 20ms 才修正（補靜音 / 裁切），避免頻繁微調
const TOLERANCE: i64 = SAMPLE_RATE as i64 / 50;
/// 補靜音只補到「現在 − 150ms」，給尚未取出的封包留時間
const SILENCE_MARGIN_100NS: i64 = 1_500_000;
/// 收集畫面時間樣本的時間長度
const T0_WINDOW_100NS: i64 = 10_000_000;
/// 開始前最多保留的音訊
const PRE_BUFFER_100NS: i64 = 60_000_000;
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;
/// 一次要補的靜音超過這個長度，代表程式曾停住（電腦睡眠）：時間軸直接跳過，不配置大量靜音
const MAX_SILENCE_FRAMES: i64 = SAMPLE_RATE as i64 * 2;
/// 每個來源的環狀緩衝（取樣數 / 每聲道）：一般只會累積一兩個 tick 的量，不夠時自動加大
const RING_FRAMES: usize = SAMPLE_RATE as usize * 4;

fn frames_100ns(d: f64) -> i64 {
    (d * SAMPLE_RATE as f64 / 1e7 + 0.5).floor() as i64
}

pub type LogFn = Arc<dyn Fn(LogLevel, String) + Send + Sync>;

/// 固定容量的 float32 環狀緩衝（交錯立體聲，以「每聲道取樣數」計）
pub struct Ring {
    buf: Vec<f32>,
    /// 讀取位置（float 索引）
    head: usize,
    /// 目前取樣數（每聲道）
    pub size: usize,
}

impl Ring {
    pub fn new(frames: usize) -> Ring {
        Ring { buf: vec![0.0; frames.max(1) * CHANNELS], head: 0, size: 0 }
    }

    fn ensure(&mut self, extra_frames: usize) {
        let need = (self.size + extra_frames) * CHANNELS;
        if need <= self.buf.len() {
            return;
        }
        // 罕見（程式停住很久）：加大並整理成從 0 開始
        let mut next = vec![0.0; need.max(self.buf.len() * 2)];
        self.copy_out(&mut next, self.size, false);
        self.buf = next;
        self.head = 0;
    }

    /// 寫入資料（None = 靜音）
    pub fn push(&mut self, data: Option<&[f32]>, frames: usize) {
        if frames == 0 {
            return;
        }
        self.ensure(frames);
        let cap = self.buf.len();
        let mut w = (self.head + self.size * CHANNELS) % cap;
        let mut left = frames * CHANNELS;
        let mut src = 0;
        while left > 0 {
            let n = left.min(cap - w);
            match data {
                Some(d) => self.buf[w..w + n].copy_from_slice(&d[src..src + n]),
                None => self.buf[w..w + n].fill(0.0),
            }
            w = (w + n) % cap;
            src += n;
            left -= n;
        }
        self.size += frames;
    }

    /// 取出 frames 個取樣到 out 的開頭：add = 相加（混音），否則覆寫
    pub fn take(&mut self, out: &mut [f32], frames: usize, add: bool) {
        self.copy_out(out, frames, add);
        self.head = (self.head + frames * CHANNELS) % self.buf.len();
        self.size -= frames;
    }

    fn copy_out(&self, out: &mut [f32], frames: usize, add: bool) {
        let cap = self.buf.len();
        let mut r = self.head;
        let mut left = frames * CHANNELS;
        let mut dst = 0;
        while left > 0 {
            let n = left.min(cap - r);
            let src = &self.buf[r..r + n];
            if add {
                for (o, v) in out[dst..dst + n].iter_mut().zip(src) {
                    *o += *v;
                }
            } else {
                out[dst..dst + n].copy_from_slice(src);
            }
            r = (r + n) % cap;
            dst += n;
            left -= n;
        }
    }
}

/// 畫面零點確定前收到的封包（只在開頭約 1 秒）
struct PrePacket {
    qpc: Option<i64>,
    /// None = 靜音
    data: Option<Vec<f32>>,
    frames: usize,
}

struct Source {
    spec: AudioSourceSpec,
    label: &'static str,
    cap: Option<Box<dyn Capture>>,
    reopen_at: Option<i64>,
    pre: VecDeque<PrePacket>,
    /// 已排進時間軸的取樣數（每聲道）
    written: i64,
    /// 等待混音的資料
    ring: Ring,
}

impl Source {
    /// 依封包時間戳放進時間軸（data = None 為靜音）
    fn place(&mut self, t0: f64, data: Option<&[f32]>, frames: usize, qpc: Option<i64>) {
        let pos = match qpc {
            Some(q) => frames_100ns(q as f64 - t0),
            None => self.written,
        };
        if pos > self.written + TOLERANCE {
            self.fill_silence(pos);
        }
        let mut skip = 0usize;
        if pos < self.written - TOLERANCE {
            skip = (self.written - pos) as usize; // 與已寫入的資料重疊（通常是零點之前的聲音）
        }
        if skip >= frames {
            return;
        }
        self.append(data.map(|d| &d[skip * CHANNELS..]), frames - skip);
    }

    fn fill_silence(&mut self, up_to: i64) {
        if up_to > self.written {
            self.append(None, (up_to - self.written) as usize);
        }
    }

    fn append(&mut self, data: Option<&[f32]>, frames: usize) {
        self.ring.push(data, frames);
        self.written += frames as i64;
    }
}

/// 時間軸與混音（不含執行緒與網路，方便測試）
pub struct Timeline {
    sources: Vec<Source>,
    opener: Opener,
    log: LogFn,
    t0: Option<f64>,
    first_video_at: Option<i64>,
    t0_estimate: f64,
    /// 混音輸出（重複使用）
    mix_buf: Vec<f32>,
}

impl Timeline {
    pub fn new(specs: &[AudioSourceSpec], opener: Opener, log: LogFn, now: i64) -> Timeline {
        let sources = specs
            .iter()
            .map(|spec| Source {
                spec: spec.clone(),
                label: if spec.loopback { "系統聲音" } else { "麥克風" },
                cap: None,
                reopen_at: None,
                pre: VecDeque::new(),
                written: 0,
                ring: Ring::new(RING_FRAMES),
            })
            .collect();
        let mut t = Timeline { sources, opener, log, t0: None, first_video_at: None, t0_estimate: f64::INFINITY, mix_buf: vec![0.0; SAMPLE_RATE as usize * CHANNELS] };
        for i in 0..t.sources.len() {
            t.open(i, true, now);
        }
        t
    }

    pub fn describe(&self) -> String {
        self.sources
            .iter()
            .map(|s| match &s.cap {
                Some(c) => format!("{}（{}）", s.label, c.name()),
                None => format!("{}（無法使用，改錄靜音）", s.label),
            })
            .collect::<Vec<_>>()
            .join("、")
    }

    /// 畫面時間零點已確定：之後不再需要 showinfo 的輸出
    pub fn synced(&self) -> bool {
        self.t0.is_some()
    }

    /// showinfo 回報一張畫面：pts（秒）與收到的 QPC 時間
    pub fn on_video_frame(&mut self, pts_sec: f64, arrival: i64) {
        if self.t0.is_some() {
            return;
        }
        self.first_video_at.get_or_insert(arrival);
        self.t0_estimate = self.t0_estimate.min(arrival as f64 - pts_sec * 1e7);
    }

    /// 要停止這個分段時（送 q 之前）：把聲音立即送到 at 為止，回傳要送出的資料
    pub fn flush_to(&mut self, at: i64) -> &[u8] {
        if self.t0.is_none() && self.first_video_at.is_some() {
            self.fix_t0();
        }
        let Some(t0) = self.t0 else { return &[] };
        self.pull(at);
        let upto = frames_100ns(at as f64 - t0);
        for s in &mut self.sources {
            s.fill_silence(upto);
        }
        self.mix()
    }

    /// 定期呼叫：取出新封包、補靜音、混音，回傳要送出的資料
    pub fn tick(&mut self, now: i64) -> &[u8] {
        if self.t0.is_none() {
            if let Some(first) = self.first_video_at {
                if now - first >= T0_WINDOW_100NS {
                    self.fix_t0();
                }
            }
        }
        self.pull(now);
        let Some(mut t0) = self.t0 else { return &[] };
        let target = frames_100ns((now - SILENCE_MARGIN_100NS) as f64 - t0);
        let max_written = self.sources.iter().map(|s| s.written).max().unwrap_or(0).max(0);
        let behind = target - max_written;
        if behind > MAX_SILENCE_FRAMES {
            // 睡眠一小時要補約 1.4 GB 的靜音；改為把零點往後移，錄影端也會在恢復後重開分段
            t0 += (behind as f64 * 1e7 / SAMPLE_RATE as f64 + 0.5).floor();
            self.t0 = Some(t0);
            (self.log)(LogLevel::Warn, format!("聲音中斷約 {} 秒（電腦睡眠？），已跳過這段", (behind as f64 / SAMPLE_RATE as f64 + 0.5).floor()));
        }
        let capped = frames_100ns((now - SILENCE_MARGIN_100NS) as f64 - t0);
        for s in &mut self.sources {
            s.fill_silence(capped);
        }
        self.mix()
    }

    fn open(&mut self, i: usize, first: bool, now: i64) {
        let s = &mut self.sources[i];
        match (self.opener)(&s.spec) {
            Ok(cap) => {
                if !first {
                    (self.log)(LogLevel::Info, format!("{}已恢復（{}）", s.label, cap.name()));
                }
                s.cap = Some(cap);
                s.reopen_at = None;
            }
            Err(e) => {
                s.cap = None;
                s.reopen_at = Some(now + 30_000_000);
                if first {
                    (self.log)(LogLevel::Warn, format!("{}無法開啟：{e}，這段先以靜音代替", s.label));
                }
            }
        }
    }

    fn fix_t0(&mut self) {
        let t0 = self.t0_estimate;
        self.t0 = Some(t0);
        for s in &mut self.sources {
            let pre = std::mem::take(&mut s.pre);
            for p in pre {
                s.place(t0, p.data.as_deref(), p.frames, p.qpc);
            }
        }
    }

    /// 讀取所有來源的新封包；裝置失效時關閉並定期嘗試重新開啟
    fn pull(&mut self, now: i64) {
        for i in 0..self.sources.len() {
            if self.sources[i].cap.is_none() {
                if self.sources[i].reopen_at.is_some_and(|at| now >= at) {
                    self.open(i, false, now);
                }
                if self.sources[i].cap.is_none() {
                    continue;
                }
            }
            let t0 = self.t0;
            let s = &mut self.sources[i];
            let mut cap = s.cap.take().unwrap();
            let result = match t0 {
                None => {
                    // 零點確定前先保留（只有開頭約 1 秒）
                    let r = cap.read(&mut |data, frames, qpc| s.pre.push_back(PrePacket { qpc, frames, data: data.map(|d| d[..frames * CHANNELS].to_vec()) }));
                    while s.pre.front().is_some_and(|p| p.qpc.is_some_and(|q| now - q > PRE_BUFFER_100NS)) {
                        s.pre.pop_front();
                    }
                    r
                }
                Some(t0) => cap.read(&mut |data, frames, qpc| s.place(t0, data.map(|d| &d[..frames * CHANNELS]), frames, qpc)),
            };
            match result {
                Ok(()) => s.cap = Some(cap),
                Err(e) => {
                    (self.log)(LogLevel::Warn, format!("{}中斷：{e}，先以靜音代替並嘗試重新連接", s.label));
                    drop(cap);
                    s.reopen_at = Some(now + 10_000_000);
                }
            }
        }
    }

    /// 所有來源都有資料的部分相加後回傳
    fn mix(&mut self) -> &[u8] {
        let Some(n) = self.sources.iter().map(|s| s.ring.size).min() else { return &[] };
        if n == 0 {
            return &[];
        }
        if self.mix_buf.len() < n * CHANNELS {
            self.mix_buf.resize(n * CHANNELS, 0.0);
        }
        for (i, s) in self.sources.iter_mut().enumerate() {
            s.ring.take(&mut self.mix_buf, n, i > 0);
        }
        let out = &self.mix_buf[..n * CHANNELS];
        // float32 little-endian（Windows / x86 皆為 little-endian），即 FFmpeg 的 f32le
        unsafe { std::slice::from_raw_parts(out.as_ptr() as *const u8, std::mem::size_of_val(out)) }
    }
}

/// 送往 FFmpeg 的 TCP 連線（非阻塞；寫不完的部分保留到下一次）
struct Output {
    listener: TcpListener,
    sock: Option<TcpStream>,
    pending: VecDeque<u8>,
    overflow_warned: bool,
    log: LogFn,
}

impl Output {
    fn accept(&mut self) {
        // 只接受 FFmpeg 的一條連線，其他的直接關掉
        while let Ok((sock, _)) = self.listener.accept() {
            if self.sock.is_none() && sock.set_nonblocking(true).is_ok() {
                let _ = sock.set_nodelay(true);
                self.sock = Some(sock);
            }
        }
    }

    /// 盡量送出；回傳實際送出的位元組數（連線中斷時丟掉連線）
    fn send(&mut self, bytes: &[u8]) -> usize {
        let Some(sock) = self.sock.as_mut() else { return 0 };
        let mut off = 0;
        while off < bytes.len() {
            match sock.write(&bytes[off..]) {
                Ok(0) => {
                    self.sock = None;
                    break;
                }
                Ok(n) => off += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.sock = None;
                    break;
                }
            }
        }
        off
    }

    fn write(&mut self, bytes: &[u8]) {
        self.accept();
        self.flush();
        let mut off = 0;
        if self.pending.is_empty() {
            off = self.send(bytes);
            if off >= bytes.len() {
                return;
            }
        }
        let rest = &bytes[off..];
        if self.pending.len() + rest.len() > MAX_PENDING_BYTES {
            if !self.overflow_warned {
                (self.log)(LogLevel::Warn, "FFmpeg 沒有在讀取聲音資料，部分聲音已捨棄".into());
            }
            self.overflow_warned = true;
            return;
        }
        self.pending.extend(rest);
    }

    fn flush(&mut self) {
        while !self.pending.is_empty() && self.sock.is_some() {
            let (head, _) = self.pending.as_slices();
            let len = head.len();
            let head = head.to_vec();
            let n = self.send(&head);
            self.pending.drain(..n);
            if n < len {
                return;
            }
        }
    }
}

enum Cmd {
    Frame(f64, i64),
    Flush(i64, mpsc::Sender<()>),
    Close,
}

/// 錄一個分段期間的聲音管線
pub struct AudioPipe {
    port: u16,
    desc: String,
    synced: Arc<AtomicBool>,
    tx: mpsc::Sender<Cmd>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl AudioPipe {
    pub fn new(specs: Vec<AudioSourceSpec>, opener: Opener, log: LogFn) -> std::io::Result<AudioPipe> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let synced = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (desc_tx, desc_rx) = mpsc::channel::<String>();
        let flag = synced.clone();
        let thread = std::thread::Builder::new().name("audio".into()).spawn(move || {
            let mut tl = Timeline::new(&specs, opener, log.clone(), now_100ns());
            let _ = desc_tx.send(tl.describe());
            let mut out = Output { listener, sock: None, pending: VecDeque::new(), overflow_warned: false, log };
            let mut next = Instant::now() + TICK;
            loop {
                let wait = next.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(Cmd::Frame(pts, arrival)) => tl.on_video_frame(pts, arrival),
                    Ok(Cmd::Flush(at, reply)) => {
                        let bytes = tl.flush_to(at);
                        out.write(bytes);
                        flag.store(tl.synced(), Ordering::Relaxed);
                        let _ = reply.send(());
                    }
                    Ok(Cmd::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        next = Instant::now() + TICK;
                        let bytes = tl.tick(now_100ns());
                        out.write(bytes);
                        flag.store(tl.synced(), Ordering::Relaxed);
                    }
                }
            }
            // 擷取來源在這條執行緒結束時釋放（COM 物件必須在建立它的執行緒釋放）
            drop(tl);
            if let Some(sock) = out.sock.take() {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        })?;
        // 開啟音訊裝置卡住時不要一直等（錄影的開始、暫停、停止都在等這裡）：裝置稍後好了照樣會送聲音
        let desc = desc_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or_else(|_| "音訊裝置沒有回應".to_string());
        Ok(AudioPipe { port, desc, synced, tx, thread: std::sync::Mutex::new(Some(thread)) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// FFmpeg 的音訊輸入參數
    pub fn input_args(&self) -> Vec<String> {
        ["-thread_queue_size", "4096", "-f", "f32le", "-ar", &SAMPLE_RATE.to_string(), "-ac", &CHANNELS.to_string(), "-i", &format!("tcp://127.0.0.1:{}", self.port)]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    pub fn describe(&self) -> &str {
        &self.desc
    }

    pub fn synced(&self) -> bool {
        self.synced.load(Ordering::Relaxed)
    }

    pub fn on_video_frame(&self, pts_sec: f64, arrival: i64) {
        let _ = self.tx.send(Cmd::Frame(pts_sec, arrival));
    }

    /// 要停止這個分段時（送 q 之前）：把聲音立即送到現在為止。
    /// 平常只送到「現在 − 150ms」（等晚到的封包），不先補上的話每個分段結尾會少約 0.25 秒聲音。
    pub fn flush_to_now(&self) {
        let (reply, done) = mpsc::channel();
        if self.tx.send(Cmd::Flush(now_100ns(), reply)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }

    /// 立即釋放（FFmpeg 已結束時）
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
        let thread = self.thread.lock().unwrap().take();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}

impl Drop for AudioPipe {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 第 i 個取樣的左右聲道 = [i, -i]
    fn seq(from: usize, frames: usize) -> Vec<f32> {
        (0..frames).flat_map(|i| [(from + i) as f32, -((from + i) as f32)]).collect()
    }

    #[test]
    fn ring_wraps_silence_and_order() {
        let mut r = Ring::new(10);
        let mut out = vec![0.0; 20 * CHANNELS];
        r.push(Some(&seq(0, 6)), 6);
        r.take(&mut out, 4, false);
        assert_eq!(out[..8], seq(0, 4)[..]);
        r.push(Some(&seq(6, 5)), 5); // 寫入時繞回開頭
        r.push(None, 1); // 靜音
        assert_eq!(r.size, 8);
        r.take(&mut out, 8, false);
        assert_eq!(out[..14], seq(4, 7)[..]);
        assert_eq!(out[14..16], [0.0, 0.0]);
        assert_eq!(r.size, 0);
    }

    #[test]
    fn ring_grows_when_full() {
        let mut r = Ring::new(4);
        r.push(Some(&seq(0, 3)), 3);
        let mut out = vec![0.0; 20 * CHANNELS];
        r.take(&mut out, 2, false);
        r.push(Some(&seq(3, 12)), 12); // 遠超過容量 4
        assert_eq!(r.size, 13);
        r.take(&mut out, 13, false);
        assert_eq!(out[..26], seq(2, 13)[..]);
    }

    #[test]
    fn ring_mix_adds() {
        let (mut a, mut b) = (Ring::new(8), Ring::new(8));
        a.push(Some(&seq(1, 3)), 3);
        b.push(Some(&seq(10, 3)), 3);
        let mut out = vec![0.0; 3 * CHANNELS];
        a.take(&mut out, 3, false);
        b.take(&mut out, 3, true);
        assert_eq!(out, [11.0, -11.0, 13.0, -13.0, 15.0, -15.0]);
    }

    type Packets = Arc<Mutex<VecDeque<(Option<Vec<f32>>, usize, Option<i64>)>>>;

    struct Fake {
        packets: Packets,
        fail: Arc<AtomicBool>,
    }

    impl Capture for Fake {
        fn name(&self) -> String {
            "假裝置".into()
        }
        fn read(&mut self, visit: &mut crate::audio::Visit) -> Result<(), String> {
            if self.fail.load(Ordering::Relaxed) {
                return Err("拔除".into());
            }
            while let Some((d, n, q)) = self.packets.lock().unwrap().pop_front() {
                visit(d.as_deref(), n, q);
            }
            Ok(())
        }
    }

    fn fake_opener(packets: Packets, fail: Arc<AtomicBool>) -> Opener {
        Arc::new(move |_spec: &AudioSourceSpec| -> Result<Box<dyn Capture>, String> {
            if fail.load(Ordering::Relaxed) {
                return Err("找不到".into());
            }
            Ok(Box::new(Fake { packets: packets.clone(), fail: fail.clone() }))
        })
    }

    fn logs() -> (LogFn, Arc<Mutex<Vec<String>>>) {
        let store = Arc::new(Mutex::new(Vec::new()));
        let s = store.clone();
        (Arc::new(move |_l, m| s.lock().unwrap().push(m)), store)
    }

    fn floats(bytes: &[u8]) -> Vec<f32> {
        bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
    }

    const MS: i64 = 10_000;

    #[test]
    fn aligns_packets_to_video_time() {
        let packets: Packets = Default::default();
        let fail = Arc::new(AtomicBool::new(false));
        let (log, _) = logs();
        let spec = AudioSourceSpec { loopback: true, mic_id: String::new() };
        let mut tl = Timeline::new(&[spec], fake_opener(packets.clone(), fail), log, 0);
        assert_eq!(tl.describe(), "系統聲音（假裝置）");
        // 畫面零點 = 1000ms（第二張畫面延遲較大，不影響最小值）
        tl.on_video_frame(0.0, 1000 * MS);
        tl.on_video_frame(0.1, 1150 * MS);
        // 零點前 100ms 的封包（480 個取樣）與零點後 50ms 的封包
        packets.lock().unwrap().push_back((Some(seq(0, 480)), 480, Some(900 * MS)));
        packets.lock().unwrap().push_back((Some(seq(1000, 48)), 48, Some(1050 * MS)));
        assert!(tl.tick(1500 * MS).is_empty()); // 還在收集畫面時間
        assert!(!tl.synced());
        let out = floats(tl.tick(2000 * MS));
        assert!(tl.synced());
        // 送到 2000 − 150 − 1000 = 850ms
        assert_eq!(out.len(), 850 * 48 * CHANNELS);
        // 零點之前的聲音被裁掉，50ms 處補靜音後接上第二個封包
        assert!(out[..50 * 48 * CHANNELS].iter().all(|v| *v == 0.0));
        assert_eq!(out[50 * 48 * CHANNELS..(50 * 48 + 48) * CHANNELS], seq(1000, 48)[..]);
        // flush：立即補到現在
        let out = floats(tl.flush_to(2100 * MS));
        assert_eq!(out.len(), 250 * 48 * CHANNELS);
    }

    #[test]
    fn mixes_two_sources_and_skips_sleep() {
        let packets: Packets = Default::default();
        let fail = Arc::new(AtomicBool::new(false));
        let (log, messages) = logs();
        let specs = [AudioSourceSpec { loopback: true, mic_id: String::new() }, AudioSourceSpec { loopback: false, mic_id: String::new() }];
        let mut tl = Timeline::new(&specs, fake_opener(packets.clone(), fail), log, 0);
        tl.on_video_frame(0.0, 0);
        // 兩個來源共用同一個假封包佇列：第一個來源讀完全部，第二個只有靜音
        packets.lock().unwrap().push_back((Some(vec![0.25; 96]), 48, Some(0)));
        let out = floats(tl.flush_to(10 * MS));
        assert_eq!(out.len(), 480 * CHANNELS);
        assert_eq!(out[..96], vec![0.25; 96][..]);
        // 電腦睡眠 1 小時：時間軸跳過，不補一小時的靜音
        let out = tl.tick(3600 * 1000 * MS).len();
        assert!(out < (MAX_SILENCE_FRAMES as usize + 48_000) * CHANNELS * 4);
        assert!(messages.lock().unwrap().iter().any(|m| m.contains("電腦睡眠")));
    }

    #[test]
    fn missing_device_records_silence_and_reopens() {
        let packets: Packets = Default::default();
        let fail = Arc::new(AtomicBool::new(true));
        let (log, messages) = logs();
        let spec = AudioSourceSpec { loopback: false, mic_id: "x".into() };
        let mut tl = Timeline::new(&[spec], fake_opener(packets, fail.clone()), log, 0);
        assert_eq!(tl.describe(), "麥克風（無法使用，改錄靜音）");
        tl.on_video_frame(0.0, 0);
        let out = floats(tl.flush_to(100 * MS));
        assert_eq!(out.len(), 100 * 48 * CHANNELS);
        assert!(out.iter().all(|v| *v == 0.0));
        fail.store(false, Ordering::Relaxed);
        tl.tick(1000 * MS); // 3 秒後才重試
        assert_eq!(tl.describe(), "麥克風（無法使用，改錄靜音）");
        tl.tick(3000 * MS);
        assert_eq!(tl.describe(), "麥克風（假裝置）");
        assert!(messages.lock().unwrap().iter().any(|m| m.contains("已恢復")));
        // 裝置中斷：改以靜音代替
        fail.store(true, Ordering::Relaxed);
        tl.tick(3100 * MS);
        assert!(messages.lock().unwrap().iter().any(|m| m.contains("中斷")));
    }

    #[test]
    fn pipe_streams_over_tcp() {
        use std::io::Read;
        let packets: Packets = Default::default();
        let fail = Arc::new(AtomicBool::new(false));
        let (log, _) = logs();
        let pipe = AudioPipe::new(vec![AudioSourceSpec { loopback: true, mic_id: String::new() }], fake_opener(packets, fail), log).unwrap();
        assert_eq!(pipe.describe(), "系統聲音（假裝置）");
        let args = pipe.input_args();
        assert_eq!(args.last().unwrap(), &format!("tcp://127.0.0.1:{}", pipe.port()));
        let mut conn = TcpStream::connect(("127.0.0.1", pipe.port())).unwrap();
        pipe.on_video_frame(0.0, now_100ns());
        pipe.flush_to_now();
        assert!(pipe.synced());
        std::thread::sleep(Duration::from_millis(300));
        pipe.close();
        let mut got = Vec::new();
        conn.read_to_end(&mut got).unwrap();
        // 至少有 flush 之後約 0.15 秒的聲音
        assert!(got.len() >= 48 * 100 * BYTES, "{}", got.len());
        assert_eq!(got.len() % BYTES, 0);
    }

    const BYTES: usize = crate::audio::BYTES_PER_FRAME;
}
