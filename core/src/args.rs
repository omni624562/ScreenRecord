//! 組 FFmpeg 參數（純函式，方便測試）。

use crate::error::{Error, Result};
use crate::format::{even, num, output_size, FPS_MAX, FPS_MIN, MAX_MINUTES_MAX, SPEED_MAX, SPEED_MIN};
use crate::types::{CaptureMethod, EncoderPreference, MonitorInfo, RecordConfig, Rect, SourceConfig, SCALE_OPTIONS};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSpec {
    pub name: &'static str,
    /// 送進編碼器的像素格式；全部都是 4:2:0 8-bit
    pub pix_fmt: &'static str,
    /// 即時錄影用（速度優先）
    live: &'static [&'static str],
    /// 製作加速版用（離線，畫質 / 壓縮率優先）
    offline: &'static [&'static str],
}

impl EncoderSpec {
    pub fn live(&self) -> Vec<String> {
        self.live.iter().map(|s| s.to_string()).collect()
    }
    pub fn offline(&self) -> Vec<String> {
        self.offline.iter().map(|s| s.to_string()).collect()
    }
}

pub const ENCODERS: [EncoderSpec; 5] = [
    EncoderSpec { name: "libx264", pix_fmt: "yuv420p", live: &["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"], offline: &["-c:v", "libx264", "-preset", "fast", "-crf", "20"] },
    EncoderSpec {
        name: "h264_nvenc",
        pix_fmt: "yuv420p",
        live: &["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "24", "-b:v", "0"],
        offline: &["-c:v", "h264_nvenc", "-preset", "p6", "-rc", "vbr", "-cq", "22", "-b:v", "0"],
    },
    EncoderSpec {
        name: "h264_qsv",
        pix_fmt: "nv12",
        live: &["-c:v", "h264_qsv", "-preset", "veryfast", "-global_quality", "24"],
        offline: &["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "22"],
    },
    EncoderSpec {
        name: "h264_amf",
        pix_fmt: "nv12",
        live: &["-c:v", "h264_amf", "-usage", "lowlatency", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24"],
        offline: &["-c:v", "h264_amf", "-quality", "quality", "-rc", "cqp", "-qp_i", "20", "-qp_p", "22"],
    },
    EncoderSpec {
        name: "h264_mf",
        pix_fmt: "nv12",
        live: &["-c:v", "h264_mf", "-rate_control", "quality", "-quality", "75"],
        offline: &["-c:v", "h264_mf", "-rate_control", "quality", "-quality", "80"],
    },
];

/// 硬體編碼器（實測可用才會列入選項）；h264_mf 只在沒有 libx264 時當軟體備援
pub const HARDWARE_ENCODERS: [&str; 3] = ["h264_nvenc", "h264_qsv", "h264_amf"];

pub fn encoder_spec(name: &str) -> Option<EncoderSpec> {
    ENCODERS.iter().copied().find(|e| e.name == name)
}

/// 「自動」模式：每秒處理的像素超過 1080p60 就改用 GPU 編碼
pub const AUTO_GPU_PIXELS_PER_SEC: f64 = 1920.0 * 1080.0 * 60.0;

/// 擷取端（ddagrab / gdigrab / 濾鏡圖）出錯的訊息
static CAPTURE_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)ddagrab|gdigrab|Desktop duplication|Error configuring filter graph|Failed to capture").unwrap());
/// 編碼器本身出錯的訊息。擷取端失敗時 FFmpeg 也會連帶印出「Could not open encoder before EOF」，那不代表編碼器有問題。
static ENCODER_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Error while opening encoder|Error initializing output stream|enc:(h264_\w+|libx264)[^\n]*(Error|fail|not (supported|available))|No (NVENC|capable) devices?|Cannot load nvEncodeAPI|MFX|\bAMF\b|Cannot load nvcuda|CUDA_ERROR").unwrap()
});
static EOF_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Could not open encoder before EOF").unwrap());

/// FFmpeg 錯誤訊息是編碼器而非擷取端造成的
pub fn is_encoder_fault(stderr: &str) -> bool {
    let lines: Vec<&str> = stderr.lines().filter(|l| !EOF_LINE.is_match(l)).collect();
    let text = lines.join("\n");
    ENCODER_ERROR.is_match(&text) && !CAPTURE_ERROR.is_match(&text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupFallback {
    CpuEncoder,
    Gdigrab,
    Fatal,
}

pub struct FallbackInput<'a> {
    pub stderr: &'a str,
    pub gpu_encoder_in_use: bool,
    pub encoder_auto: bool,
    pub has_cpu_encoder: bool,
    pub ddagrab_in_use: bool,
    pub method_auto: bool,
}

/// 第一張畫面之前就失敗時該退回哪一項：依錯誤訊息判斷是編碼器還是擷取（ddagrab）出問題。
/// 判斷不出來時，先退擷取方式（較常見），之後若仍失敗再退編碼器。
pub fn startup_fallback(o: &FallbackInput) -> StartupFallback {
    let can_cpu = o.gpu_encoder_in_use && o.encoder_auto && o.has_cpu_encoder;
    let can_gdi = o.ddagrab_in_use && o.method_auto;
    if is_encoder_fault(o.stderr) && can_cpu {
        return StartupFallback::CpuEncoder;
    }
    if can_gdi {
        return StartupFallback::Gdigrab;
    }
    if can_cpu {
        return StartupFallback::CpuEncoder;
    }
    StartupFallback::Fatal
}

/// 決定這次錄影的編碼器（learned：之前在「自動」模式偵測到 CPU 跟不上，之後直接用 GPU）
pub fn choose_encoder(pref: EncoderPreference, out_width: i32, out_height: i32, fps: f64, cpu: Option<EncoderSpec>, gpu: &[EncoderSpec], learned: bool) -> Result<(EncoderSpec, &'static str)> {
    if pref == EncoderPreference::Gpu {
        let Some(g) = gpu.first() else {
            return Err(Error::config("這台電腦沒有可用的 GPU 編碼器，請改用 CPU 或自動"));
        };
        return Ok((*g, "指定使用 GPU 編碼"));
    }
    if pref == EncoderPreference::Auto {
        if let Some(g) = gpu.first() {
            if out_width as f64 * out_height as f64 * fps > AUTO_GPU_PIXELS_PER_SEC {
                return Ok((*g, "畫面量超過 1080p60，自動改用 GPU 編碼"));
            }
            if learned {
                return Ok((*g, "先前偵測到 CPU 編碼跟不上，自動改用 GPU 編碼"));
            }
        }
    }
    match (cpu, gpu.first()) {
        (Some(c), _) => Ok((c, "CPU 編碼")),
        (None, Some(g)) => Ok((*g, "沒有 CPU 編碼器，改用 GPU 編碼")),
        (None, None) => Err(Error::config("FFmpeg 沒有可用的 H.264 編碼器")),
    }
}

const COLOR_TAGS: [&str; 8] = ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"];
const AUDIO_ENCODE: [&str; 4] = ["-c:a", "aac", "-b:a", "160k"];

/// ddagrab 擷取的一塊：某個螢幕與擷取範圍的交集
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub output: u32,
    /// 相對於該螢幕左上角
    pub offset_x: i32,
    pub offset_y: i32,
    pub width: i32,
    pub height: i32,
    /// 是否為整個螢幕（不需指定 offset / video_size）
    pub full: bool,
    /// 在擷取範圍（輸出畫面）內的位置
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dda {
    pub adapter: u32,
    pub tiles: Vec<Tile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapturePlan {
    /// 擷取範圍（虛擬桌面絕對座標，gdigrab 用）
    pub rect: Rect,
    /// ddagrab：範圍內每個螢幕各擷取一塊；跨越不同顯示卡時為 None（改用 gdigrab）
    pub dda: Option<Dda>,
    /// 範圍涵蓋到的螢幕
    pub monitors: Vec<MonitorInfo>,
    pub out_width: i32,
    pub out_height: i32,
}

fn monitor_rect(m: &MonitorInfo) -> Rect {
    Rect { x: m.x, y: m.y, width: m.width, height: m.height }
}

/// 兩個範圍是否重疊（只碰到邊不算）
pub fn intersects(a: &Rect, b: &Rect) -> bool {
    b.x < a.x + a.width && b.x + b.width > a.x && b.y < a.y + a.height && b.y + b.height > a.y
}

/// 所有螢幕的聯集（= Windows 虛擬桌面範圍）
pub fn desktop_rect(monitors: &[MonitorInfo]) -> Rect {
    if monitors.is_empty() {
        return Rect::default();
    }
    let x = monitors.iter().map(|m| m.x).min().unwrap();
    let y = monitors.iter().map(|m| m.y).min().unwrap();
    let r = monitors.iter().map(|m| m.x + m.width).max().unwrap();
    let b = monitors.iter().map(|m| m.y + m.height).max().unwrap();
    Rect { x, y, width: r - x, height: b - y }
}

/// 依擷取範圍切出每個螢幕的 ddagrab 區塊
pub fn plan_tiles(rect: &Rect, monitors: &[MonitorInfo]) -> (Vec<MonitorInfo>, Option<Dda>) {
    let involved: Vec<MonitorInfo> = monitors.iter().filter(|m| intersects(&monitor_rect(m), rect)).cloned().collect();
    let tiles: Vec<Tile> = involved
        .iter()
        .map(|m| {
            let ix = rect.x.max(m.x);
            let iy = rect.y.max(m.y);
            let w = (rect.x + rect.width).min(m.x + m.width) - ix;
            let h = (rect.y + rect.height).min(m.y + m.height) - iy;
            Tile { output: m.output, offset_x: ix - m.x, offset_y: iy - m.y, width: w, height: h, full: ix == m.x && iy == m.y && w == m.width && h == m.height, x: ix - rect.x, y: iy - rect.y }
        })
        .collect();
    let same_adapter = involved.windows(2).all(|p| p[0].adapter == p[1].adapter);
    let dda = (!involved.is_empty() && same_adapter).then(|| Dda { adapter: involved[0].adapter, tiles: tiles.into_iter().filter(|t| t.width >= 2 && t.height >= 2).collect() });
    (involved, dda)
}

fn is_int(x: f64) -> bool {
    x.is_finite() && x.fract() == 0.0
}

/// 驗證設定並算出擷取範圍。錯誤訊息直接顯示在介面上。
pub fn resolve_plan(config: &RecordConfig, monitors: &[MonitorInfo]) -> Result<CapturePlan> {
    let fps = config.fps;
    if !is_int(fps) || !(FPS_MIN..=FPS_MAX).contains(&fps) {
        return Err(Error::config(format!("錄影 FPS 需為 {}～{} 的整數", num(FPS_MIN), num(FPS_MAX))));
    }
    if !SCALE_OPTIONS.iter().any(|&s| s as f64 == config.scale) {
        return Err(Error::config("解析度縮放只能是 100 / 75 / 50 / 25%"));
    }
    if !config.max_minutes.is_finite() || config.max_minutes < 0.0 || config.max_minutes > MAX_MINUTES_MAX {
        return Err(Error::config("最長錄影時間設定不正確"));
    }
    if config.output_dir.trim().is_empty() {
        return Err(Error::config("請指定儲存位置"));
    }

    let rect = match &config.source {
        SourceConfig::Monitor { monitor_id } => {
            let Some(m) = monitors.iter().find(|m| &m.id == monitor_id) else {
                return Err(Error::config("找不到選擇的螢幕，請重新整理螢幕清單"));
            };
            monitor_rect(m)
        }
        SourceConfig::All => {
            if monitors.is_empty() {
                return Err(Error::config("找不到任何螢幕"));
            }
            desktop_rect(monitors)
        }
        SourceConfig::Region { x, y, width, height } => {
            if ![*x, *y, *width, *height].iter().all(|v| is_int(*v)) {
                return Err(Error::config("範圍座標必須是整數"));
            }
            if *width < 16.0 || *height < 16.0 {
                return Err(Error::config("範圍寬高至少 16 像素"));
            }
            Rect { x: *x as i32, y: *y as i32, width: *width as i32, height: *height as i32 }
        }
    };

    let (involved, dda) = plan_tiles(&rect, monitors);
    if !monitors.is_empty() && involved.is_empty() {
        return Err(Error::config("範圍不在任何螢幕上"));
    }
    let (out_width, out_height) = output_size(rect.width, rect.height, config.scale);
    Ok(CapturePlan { rect, dda, monitors: involved, out_width, out_height })
}

/// ddagrab 來源（GPU 擷取後下載回系統記憶體）。多個螢幕時以 xstack 依實際位置拼接，沒有畫面的區域補黑色。
fn ddagrab_chain(plan: &CapturePlan, fps: f64, draw_mouse: bool) -> String {
    let tiles = &plan.dda.as_ref().expect("ddagrab 需要 dda").tiles;
    let src = |t: &Tile| {
        let mut p = vec![format!("ddagrab=output_idx={}", t.output), format!("framerate={}", num(fps)), format!("draw_mouse={}", draw_mouse as u8)];
        if !t.full {
            p.push(format!("offset_x={}", t.offset_x));
            p.push(format!("offset_y={}", t.offset_y));
            p.push(format!("video_size={}x{}", t.width, t.height));
        }
        format!("{},hwdownload,format=bgra", p.join(":"))
    };
    if tiles.len() == 1 {
        return src(&tiles[0]);
    }
    let (w, h) = (plan.rect.width, plan.rect.height);
    let right = tiles.iter().map(|t| t.x + t.width).max().unwrap_or(0);
    let bottom = tiles.iter().map(|t| t.y + t.height).max().unwrap_or(0);
    let labels: Vec<String> = (0..tiles.len()).map(|i| format!("[t{i}]")).collect();
    let parts: Vec<String> = tiles.iter().zip(&labels).map(|(t, l)| format!("{}{l}", src(t))).collect();
    let layout: Vec<String> = tiles.iter().map(|t| format!("{}_{}", t.x, t.y)).collect();
    let mut stack = format!("{}xstack=inputs={}:layout={}:fill=black", labels.concat(), tiles.len(), layout.join("|"));
    if right < w || bottom < h {
        stack.push_str(&format!(",pad={w}:{h}:0:0:color=black"));
    }
    format!("{};{stack}", parts.join(";"))
}

/// 裁成偶數 → 縮放 + 色彩轉換（BT.709 limited）
fn convert_filters(plan: &CapturePlan, config: &RecordConfig, enc: &EncoderSpec) -> Vec<String> {
    let mut f = Vec::new();
    let Rect { width, height, .. } = plan.rect;
    if config.scale == 100.0 && (plan.out_width != width || plan.out_height != height) {
        f.push(format!("crop={}:{}:0:0", plan.out_width, plan.out_height)); // 奇數寬高時裁掉 1px，避免 100% 也重新取樣
    }
    f.push(format!("scale={}:{}:flags=bicubic:out_color_matrix=bt709:out_range=tv", plan.out_width, plan.out_height));
    f.push(format!("format={}", enc.pix_fmt));
    f
}

pub struct CaptureSpec {
    /// 放在所有輸入之前的參數（硬體裝置）
    pub pre: Vec<String>,
    /// 視訊輸入（gdigrab）；ddagrab 是濾鏡來源，沒有輸入檔
    pub inputs: Vec<String>,
    /// 視訊輸入檔數量（之後的音訊輸入編號從這裡開始）
    pub input_count: usize,
    /// -filter_complex，輸出標籤為 [vout]
    pub graph: String,
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// probe：在擷取後插入 showinfo，每張畫面印一行 pts，供聲音對齊畫面時間零點
pub fn capture_spec(plan: &CapturePlan, config: &RecordConfig, method: CaptureMethod, enc: &EncoderSpec, probe: bool) -> Result<CaptureSpec> {
    let mut tail_parts = Vec::new();
    if probe {
        tail_parts.push("showinfo=checksum=0".to_string());
    }
    tail_parts.extend(convert_filters(plan, config, enc));
    let tail = tail_parts.join(",");

    if method == CaptureMethod::Ddagrab {
        let Some(dda) = &plan.dda else {
            return Err(Error::config("此範圍涵蓋不同顯示卡上的螢幕，無法使用 ddagrab"));
        };
        return Ok(CaptureSpec {
            pre: vec!["-init_hw_device".into(), format!("d3d11va=dda:{}", dda.adapter), "-filter_hw_device".into(), "dda".into()],
            inputs: vec![],
            input_count: 0,
            graph: format!("{},{tail}[vout]", ddagrab_chain(plan, config.fps, config.draw_mouse)),
        });
    }

    let Rect { x, y, width, height } = plan.rect;
    Ok(CaptureSpec {
        pre: vec![],
        inputs: vec![
            "-f".into(),
            "gdigrab".into(),
            "-framerate".into(),
            num(config.fps),
            "-draw_mouse".into(),
            (config.draw_mouse as u8).to_string(),
            "-offset_x".into(),
            x.to_string(),
            "-offset_y".into(),
            y.to_string(),
            "-video_size".into(),
            format!("{width}x{height}"),
            "-i".into(),
            "desktop".into(),
        ],
        input_count: 1,
        graph: format!("[0:v]{tail}[vout]"),
    })
}

/// 單一分段的完整參數。stdin 送 q 收尾；-progress 從 stdout 回報張數與大小。
/// audio_input：錄聲音時 AudioPipe 提供的輸入參數（TCP raw PCM）
pub fn segment_args(plan: &CapturePlan, config: &RecordConfig, method: CaptureMethod, enc: &EncoderSpec, out_file: &str, audio_input: Option<&[String]>) -> Result<Vec<String>> {
    let spec = capture_spec(plan, config, method, enc, audio_input.is_some())?;
    let fps = num(config.fps);
    let mut a: Vec<String> = Vec::new();
    // 錄聲音時需要 showinfo（info 等級）的輸出；level 前綴用來分辨錯誤訊息
    a.extend(strs(&["-hide_banner", "-nostats", "-loglevel", if audio_input.is_some() { "level+info" } else { "error" }]));
    a.extend(spec.pre);
    a.extend(spec.inputs);
    if let Some(ai) = audio_input {
        a.extend(ai.iter().cloned());
    }
    a.extend(["-filter_complex".into(), spec.graph, "-map".into(), "[vout]".into()]);
    if audio_input.is_some() {
        a.extend(["-map".into(), format!("{}:a", spec.input_count)]);
        a.extend(strs(&AUDIO_ENCODE));
        a.extend(strs(&["-max_muxing_queue_size", "4096"]));
    }
    a.extend(enc.live());
    a.extend(["-r".into(), fps.clone(), "-fps_mode".into(), "cfr".into(), "-g".into(), num(config.fps * 2.0)]);
    a.extend(strs(&COLOR_TAGS));
    // 分段寫成每秒一個 fragment：即使 FFmpeg 被強制結束或當機，最多只損失最後 1 秒
    a.extend(strs(&["-movflags", "+empty_moov+default_base_moof", "-frag_duration", "1000000", "-flush_packets", "1"]));
    a.extend(strs(&["-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

/// concat demuxer 的清單。outpoints[i]：第 i 個分段在此時間（秒）結束（錄聲音時用聲音的結尾截掉多出的畫面）。
pub fn concat_list(files: &[String], outpoints: &[Option<f64>]) -> String {
    let lines: Vec<String> = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let line = format!("file '{}'", f.replace('\\', "/").replace('\'', "'\\''"));
            match outpoints.get(i).copied().flatten() {
                Some(out) if out > 0.0 => format!("{line}\noutpoint {out:.6}"),
                _ => line,
            }
        })
        .collect();
    lines.join("\n") + "\n"
}

/// 讀出某個分段的聲音結尾（秒）：只讀聲音封包，不解碼
pub fn audio_end_args(file: &str) -> Vec<String> {
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(file.into());
    a.extend(strs(&["-map", "0:a:0", "-c", "copy", "-f", "null", "-", "-progress", "pipe:1"]));
    a
}

pub fn concat_args(list_file: &str, out_file: &str) -> Vec<String> {
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"]);
    a.push(list_file.into());
    a.extend(strs(&["-map", "0", "-c", "copy", "-movflags", "+faststart", "-y"]));
    a.push(out_file.into());
    a
}

/// atempo 串接：每段不超過 2 倍，變速不變調且音質較好
pub fn atempo_chain(speed: f64) -> String {
    let mut f = Vec::new();
    let mut s = speed;
    while s > 2.0 {
        f.push("atempo=2".to_string());
        s /= 2.0;
    }
    if s > 1.0001 {
        f.push(format!("atempo={}", num(crate::format::js_round(s * 1e6) / 1e6)));
    }
    f.join(",")
}

#[allow(clippy::too_many_arguments)]
/// 製作加速版：setpts 壓縮時間軸，fps 維持原本的幀率（多出來的幀直接捨棄，不做混合，文字才不會有殘影）；
/// 聲音以 atempo 變速不變調。width：縮小到這個寬度（高度等比、取偶數）；不小於原寬時維持原尺寸。
pub fn export_args(source: &str, out_file: &str, speed: f64, fps: f64, enc: &EncoderSpec, with_audio: bool, width: Option<i32>, src_width: Option<i32>) -> Result<Vec<String>> {
    if !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
        return Err(Error::config(format!("倍率需介於 {}～{}", num(SPEED_MIN), num(SPEED_MAX))));
    }
    let scale = match (width, src_width) {
        (Some(w), Some(sw)) if w > 0 && sw > 0 && w < sw => format!("scale={}:-2:flags=bicubic,", even(w as f64)),
        _ => String::new(),
    };
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(source.into());
    a.extend(strs(&["-map", "0:v:0"]));
    a.extend(if with_audio { strs(&["-map", "0:a:0"]) } else { strs(&["-an"]) });
    a.extend(strs(&["-sn", "-dn", "-vf"]));
    a.push(format!("setpts=PTS/{},fps={},{scale}format={}", num(speed), num(fps), enc.pix_fmt));
    if with_audio {
        a.push("-af".into());
        a.push(atempo_chain(speed));
        a.extend(strs(&AUDIO_ENCODE));
    }
    a.extend(enc.offline());
    a.extend(["-g".into(), num(fps * 2.0)]);
    a.extend(strs(&COLOR_TAGS));
    a.extend(strs(&["-movflags", "+faststart", "-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

pub struct GifOptions {
    pub fps: f64,
    /// 輸出寬度（不會放大超過原片）
    pub width: f64,
    pub src_width: f64,
    pub src_height: f64,
    /// 輸出長度（秒），用來估算記憶體
    pub output_sec: f64,
}

/// 整段共用一個調色盤時，FFmpeg 要先暫存所有畫面；超過這個量改用每張畫面各自的調色盤
const GIF_GLOBAL_PALETTE_MAX_BYTES: f64 = 400.0 * 1048576.0;

/// GIF（無聲音）：短片整段共用調色盤＋只更新變動區域；長片每張畫面各自的調色盤，記憶體固定
pub fn gif_args(source: &str, out_file: &str, speed: f64, o: &GifOptions) -> Result<Vec<String>> {
    if !(1.0..=SPEED_MAX).contains(&speed) {
        return Err(Error::config(format!("倍率需介於 1～{}", num(SPEED_MAX))));
    }
    let width = o.width.min(if o.src_width > 0.0 { o.src_width } else { o.width }).max(2.0);
    let height = if o.src_width > 0.0 && o.src_height > 0.0 { crate::format::js_round(o.src_height * width / o.src_width) } else { width * 9.0 / 16.0 };
    let frames = (o.output_sec * o.fps).ceil();
    let global = frames * width * height * 4.0 <= GIF_GLOBAL_PALETTE_MAX_BYTES;
    let palette = if global {
        "palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
    } else {
        "palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=bayer:bayer_scale=5"
    };
    let time = if speed > 1.0 { format!("setpts=PTS/{},", num(speed)) } else { String::new() };
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(source.into());
    a.extend(strs(&["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"]));
    a.push(format!("{time}fps={},scale={}:-2:flags=lanczos,split[a][b];[a]{palette}", num(o.fps), num(width)));
    a.extend(strs(&["-loop", "0", "-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

/// 剪輯：只保留 keep 區段（select / aselect 精確到每張畫面），可再裁切畫面範圍；必須重新編碼。
/// 剪輯時加上的標註（已換算成影片像素、檔案已寫好）
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayInput {
    /// 透明 PNG：疊在 (x, y)
    Image { path: String, x: i32, y: i32, start: f64, end: f64 },
    /// 範圍模糊或馬賽克（寬高為偶數、至少 8）。mask = 形狀遮罩圖檔（沒有就是方形）；
    /// invert = 範圍外模糊、範圍內清楚（frame 為整個畫面的寬高）
    Blur { rect: Rect, start: f64, end: f64, mosaic: bool, mask: Option<String>, invert: bool, frame: (i32, i32) },
}

/// 模糊或馬賽克（w × h 的畫面）
fn blur_effect(w: i32, h: i32, mosaic: bool, outside: bool) -> String {
    if mosaic {
        // 每格約為範圍短邊的 1/6（範圍外時以整個畫面為準，格子小一點），至少 12px，用最近鄰放大回原尺寸
        let block = if outside { (w.min(h) / 45).max(12) } else { (w.min(h) / 6).max(12) };
        format!("scale={}:{}:flags=area,scale={w}:{h}:flags=neighbor", (w / block).max(1), (h / block).max(1))
    } else {
        // boxblur 的半徑不能超過色度平面短邊的一半
        let radius = if outside { (w.min(h) / 40).clamp(4, 30) } else { (w.min(h) / 4 - 1).clamp(1, 30) };
        format!("boxblur={radius}:2")
    }
}

fn enable(start: f64, end: f64) -> String {
    format!("enable='between(t,{},{})'", num(start), num(end))
}

#[allow(clippy::too_many_arguments)]
pub fn cut_args(source: &str, out_file: &str, keep: &[(f64, f64)], crop: Option<Rect>, fps: f64, enc: &EncoderSpec, with_audio: bool, overlays: &[OverlayInput]) -> Result<Vec<String>> {
    if keep.is_empty() {
        return Err(Error::config("剪輯後沒有留下任何片段"));
    }
    // 單引號內的逗號不會被當成濾鏡分隔
    let expr = keep.iter().map(|(a, b)| format!("gte(t,{})*lt(t,{})", num(*a), num(*b))).collect::<Vec<_>>().join("+");
    let mut vf = vec![format!("select='{expr}'"), format!("setpts=N/({}*TB)", num(fps))];
    if let Some(c) = crop {
        vf.push(format!("crop={}:{}:{}:{}", c.width, c.height, c.x, c.y));
    }
    vf.push(format!("format={}", enc.pix_fmt));
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(source.into());
    if overlays.is_empty() {
        a.extend(strs(&["-map", "0:v:0"]));
    } else {
        // 標註以原影片的時間顯示，所以先疊上標註，再挑選保留的片段
        let mut graph = Vec::new();
        let mut cur = "[0:v]".to_string();
        let mut next_input = 1;
        for (i, o) in overlays.iter().enumerate() {
            let out = format!("[o{i}]");
            match o {
                OverlayInput::Image { path, x, y, start, end } => {
                    a.extend(["-i".into(), path.clone()]);
                    graph.push(format!("{cur}[{next_input}:v]overlay={x}:{y}:{}{out}", enable(*start, *end)));
                    next_input += 1;
                }
                OverlayInput::Blur { rect: r, start, end, mosaic, mask, invert, frame } => {
                    let (w, h, x, y) = (r.width, r.height, r.x, r.y);
                    // 範圍內的畫面（有形狀時用遮罩變成透明背景）→ [b{i}c]
                    let mut region = format!("crop={w}:{h}:{x}:{y}");
                    if !invert {
                        region = format!("{region},{}", blur_effect(w, h, *mosaic, false));
                    }
                    let shaped = match mask {
                        Some(m) => {
                            a.extend(["-i".into(), m.clone()]);
                            let s = format!("{region},format=yuva420p[b{i}e];[{next_input}:v]scale={w}:{h},format=gray[b{i}m];[b{i}e][b{i}m]alphamerge[b{i}c]");
                            next_input += 1;
                            s
                        }
                        None => format!("{region}[b{i}c]"),
                    };
                    if *invert {
                        // 整個畫面模糊，再把範圍內清楚的畫面疊回去
                        let (fw, fh) = *frame;
                        graph.push(format!(
                            "{cur}split=3[b{i}a][b{i}b][b{i}d];[b{i}b]{}[b{i}f];[b{i}d]{shaped};[b{i}f][b{i}c]overlay={x}:{y}[b{i}g];[b{i}a][b{i}g]overlay=0:0:{}{out}",
                            blur_effect(fw, fh, *mosaic, true),
                            enable(*start, *end)
                        ));
                    } else {
                        graph.push(format!("{cur}split=2[b{i}a][b{i}b];[b{i}b]{shaped};[b{i}a][b{i}c]overlay={x}:{y}:{}{out}", enable(*start, *end)));
                    }
                }
            }
            cur = out;
        }
        graph.push(format!("{cur}{}[vout]", vf.join(",")));
        a.extend(["-filter_complex".into(), graph.join(";"), "-map".into(), "[vout]".into()]);
    }
    a.extend(if with_audio { strs(&["-map", "0:a:0"]) } else { strs(&["-an"]) });
    a.extend(strs(&["-sn", "-dn"]));
    if overlays.is_empty() {
        a.push("-vf".into());
        a.push(vf.join(","));
    }
    if with_audio {
        a.push("-af".into());
        a.push(format!("aselect='{expr}',asetpts=N/SR/TB"));
        a.extend(strs(&AUDIO_ENCODE));
    }
    a.extend(enc.offline());
    a.extend(["-r".into(), num(fps), "-fps_mode".into(), "cfr".into(), "-g".into(), num(fps * 2.0)]);
    a.extend(strs(&COLOR_TAGS));
    a.extend(strs(&["-movflags", "+faststart", "-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedMedia {
    pub duration_sec: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub has_audio: bool,
}

static DURATION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Duration:\s*(\d+):(\d{2}):(\d{2}(?:\.\d+)?)").unwrap());
static VIDEO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)Stream #\S+.*?Video:.*$").unwrap());
static SIZE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*(\d{2,5})x(\d{2,5})[\s,\[]").unwrap());
static FPS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*(\d+(?:\.\d+)?)\s*fps").unwrap());
static AUDIO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Stream #\S+.*?Audio:").unwrap());

/// 解析 `ffmpeg -i file` 的 stderr：長度、解析度、fps、有無聲音
pub fn parse_media_info(stderr: &str) -> ParsedMedia {
    let duration_sec = DURATION_RE.captures(stderr).map(|d| d[1].parse::<f64>().unwrap_or(0.0) * 3600.0 + d[2].parse::<f64>().unwrap_or(0.0) * 60.0 + d[3].parse::<f64>().unwrap_or(0.0));
    let video = VIDEO_RE.find(stderr).map(|m| m.as_str().trim_end_matches('\r')).unwrap_or("");
    let size = SIZE_RE.captures(video);
    let fps = FPS_RE.captures(video).and_then(|c| c[1].parse().ok());
    ParsedMedia { duration_sec, width: size.as_ref().and_then(|s| s[1].parse().ok()), height: size.as_ref().and_then(|s| s[2].parse().ok()), fps, has_audio: AUDIO_RE.is_match(stderr) }
}

/// 預覽畫面的大小：寬度不超過 max_width（偶數），高度依比例
pub fn preview_size(rect: &Rect, max_width: u32) -> (u32, u32) {
    let w = (rect.width.max(2) as u32).min(max_width.max(2)) / 2 * 2;
    let h = ((rect.height.max(2) as f64 * w as f64 / rect.width.max(2) as f64).round() as u32 / 2 * 2).max(2);
    (w, h)
}

/// 預覽畫面（RGBA 原始像素到 stdout，大小固定為 preview_size）：傳入的螢幕決定範圍。
/// 能用 ddagrab 時與錄影走同一條路徑（混合 DPI 時畫面一致）；否則 gdigrab。
/// live_fps：即時預覽，持續輸出；否則只輸出一張。
pub fn preview_args(monitors: &[MonitorInfo], use_ddagrab: bool, max_width: u32, live_fps: Option<f64>) -> Vec<String> {
    let mut out = match live_fps {
        Some(_) => vec![],
        None => strs(&["-frames:v", "1"]),
    };
    out.extend(strs(&["-f", "rawvideo", "-pix_fmt", "rgba", "-"]));
    let fps = live_fps.unwrap_or(10.0);
    let rect = desktop_rect(monitors);
    let (w, h) = preview_size(&rect, max_width);
    let fit = format!("scale={w}:{h}:flags=bilinear,format=rgba");
    let (_, dda) = plan_tiles(&rect, monitors);
    if use_ddagrab {
        if let Some(dda) = dda.filter(|d| !d.tiles.is_empty()) {
            let adapter = dda.adapter;
            let plan = CapturePlan { rect, dda: Some(dda), monitors: monitors.to_vec(), out_width: rect.width, out_height: rect.height };
            let mut a = strs(&["-hide_banner", "-loglevel", "error", "-init_hw_device"]);
            a.push(format!("d3d11va=dda:{adapter}"));
            a.extend(strs(&["-filter_hw_device", "dda", "-filter_complex"]));
            a.push(format!("{},{fit}[vout]", ddagrab_chain(&plan, fps, true)));
            a.extend(strs(&["-map", "[vout]"]));
            a.extend(out);
            return a;
        }
    }
    let mut a = strs(&["-hide_banner", "-loglevel", "error", "-f", "gdigrab", "-framerate"]);
    a.push(num(fps));
    a.extend(strs(&["-draw_mouse", "1"]));
    if monitors.len() == 1 {
        a.extend(["-offset_x".into(), rect.x.to_string(), "-offset_y".into(), rect.y.to_string(), "-video_size".into(), format!("{}x{}", rect.width, rect.height)]);
    }
    a.extend(strs(&["-i", "desktop", "-vf"]));
    a.push(fit);
    a.extend(out);
    a
}

/// 截圖：擷取範圍的一張原尺寸 PNG（不縮放）。ddagrab 取第三張（第一張有時是黑的），gdigrab 取一張
pub fn screenshot_args(plan: &CapturePlan, use_ddagrab: bool, draw_mouse: bool, out_file: &str) -> Result<Vec<String>> {
    let mut a = strs(&["-hide_banner", "-loglevel", "error"]);
    if use_ddagrab {
        let Some(dda) = &plan.dda else {
            return Err(Error::config("此範圍涵蓋不同顯示卡上的螢幕，無法使用 ddagrab"));
        };
        a.extend(["-init_hw_device".into(), format!("d3d11va=dda:{}", dda.adapter), "-filter_hw_device".into(), "dda".into(), "-filter_complex".into()]);
        a.push(format!("{},format=rgb24[vout]", ddagrab_chain(plan, 10.0, draw_mouse)));
        a.extend(strs(&["-map", "[vout]", "-frames:v", "3"]));
    } else {
        let Rect { x, y, width, height } = plan.rect;
        a.extend(strs(&["-f", "gdigrab", "-framerate", "10", "-draw_mouse"]));
        a.push((draw_mouse as u8).to_string());
        a.extend(["-offset_x".into(), x.to_string(), "-offset_y".into(), y.to_string(), "-video_size".into(), format!("{width}x{height}"), "-i".into(), "desktop".into()]);
        a.extend(strs(&["-frames:v", "1", "-pix_fmt", "rgb24"]));
    }
    a.extend(strs(&["-update", "1", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{cut_file_name, keep_ranges, normalize_crop, EditSpec};
    use crate::types::{AudioConfig, MethodPreference};

    #[test]
    fn screenshot_full_resolution() {
        let mons = vec![mon("0:0", 0, 0, 1920, 1080, true), mon("0:1", 1920, 0, 1920, 1080, false)];
        let (_, dda) = plan_tiles(&Rect { x: 0, y: 0, width: 3840, height: 1080 }, &mons);
        let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: 3840, height: 1080 }, dda, monitors: mons.clone(), out_width: 1920, out_height: 540 };
        let a = screenshot_args(&plan, true, false, "C:\\out\\Shot.png").unwrap().join(" ");
        assert!(a.contains("d3d11va=dda:0") && a.contains("xstack") && a.contains("draw_mouse=0") && a.contains("format=rgb24[vout]"), "{a}");
        assert!(!a.contains("scale="), "截圖不縮放：{a}");
        assert!(a.ends_with("-frames:v 3 -update 1 -y C:\\out\\Shot.png"), "{a}");
        let plan = CapturePlan { rect: Rect { x: 100, y: 50, width: 800, height: 600 }, dda: None, monitors: mons, out_width: 800, out_height: 600 };
        let a = screenshot_args(&plan, false, true, "s.png").unwrap().join(" ");
        assert!(a.contains("-f gdigrab -framerate 10 -draw_mouse 1 -offset_x 100 -offset_y 50 -video_size 800x600 -i desktop -frames:v 1 -pix_fmt rgb24"), "{a}");
        assert!(screenshot_args(&plan, true, true, "s.png").is_err());
    }

    fn mon(id: &str, x: i32, y: i32, w: i32, h: i32, primary: bool) -> MonitorInfo {
        let mut p = id.split(':').map(|n| n.parse::<u32>().unwrap());
        let (adapter, output) = (p.next().unwrap(), p.next().unwrap());
        MonitorInfo {
            id: id.into(),
            adapter,
            output,
            adapter_name: "GPU".into(),
            device_name: format!("\\\\.\\DISPLAY{}", adapter * 10 + output + 1),
            display_number: output + 1,
            x,
            y,
            width: w,
            height: h,
            primary,
            rotation: 1,
        }
    }
    // 同一張顯示卡：主螢幕 1920×1080，右邊接一台較高的 2560×1440（上緣比主螢幕高 180）
    fn same_gpu() -> Vec<MonitorInfo> {
        vec![mon("0:0", 0, 0, 1920, 1080, true), mon("0:1", 1920, -180, 2560, 1440, false)]
    }
    // 混合顯卡：外接螢幕在另一張卡上
    fn two_gpus() -> Vec<MonitorInfo> {
        vec![mon("0:0", 0, 0, 1920, 1080, true), mon("1:0", 1920, -180, 2560, 1440, false)]
    }
    fn x264() -> EncoderSpec {
        ENCODERS[0]
    }
    fn cfg() -> RecordConfig {
        RecordConfig {
            source: SourceConfig::Monitor { monitor_id: "0:0".into() },
            fps: 30.0,
            scale: 100.0,
            draw_mouse: true,
            max_minutes: 0.0,
            method: MethodPreference::Auto,
            output_dir: "C:\\out".into(),
            audio: AudioConfig::default(),
            encoder: None,
            countdown_sec: None,
            hide_ui: None,
        }
    }
    fn region(x: f64, y: f64, w: f64, h: f64) -> SourceConfig {
        SourceConfig::Region { x, y, width: w, height: h }
    }
    fn after<'a>(args: &'a [String], k: &str) -> &'a str {
        &args[args.iter().position(|a| a == k).unwrap() + 1]
    }
    fn graph_of(args: &[String]) -> &str {
        after(args, "-filter_complex")
    }

    #[test]
    fn plan_single_monitor_full_tile() {
        let p = resolve_plan(&RecordConfig { source: SourceConfig::Monitor { monitor_id: "0:1".into() }, ..cfg() }, &same_gpu()).unwrap();
        assert_eq!(p.rect, Rect { x: 1920, y: -180, width: 2560, height: 1440 });
        assert_eq!(p.dda, Some(Dda { adapter: 0, tiles: vec![Tile { output: 1, offset_x: 0, offset_y: 0, width: 2560, height: 1440, full: true, x: 0, y: 0 }] }));
    }

    #[test]
    fn plan_region_relative_coordinates() {
        let p = resolve_plan(&RecordConfig { source: region(2000.0, -100.0, 801.0, 601.0), scale: 50.0, ..cfg() }, &same_gpu()).unwrap();
        assert_eq!(p.dda.unwrap().tiles, vec![Tile { output: 1, offset_x: 80, offset_y: 80, width: 801, height: 601, full: false, x: 0, y: 0 }]);
        assert_eq!((p.out_width, p.out_height), (400, 300));
    }

    #[test]
    fn plan_all_monitors_same_gpu() {
        let p = resolve_plan(&RecordConfig { source: SourceConfig::All, ..cfg() }, &same_gpu()).unwrap();
        assert_eq!(p.rect, Rect { x: 0, y: -180, width: 4480, height: 1440 });
        let t: Vec<_> = p.dda.unwrap().tiles.iter().map(|t| (t.output, t.x, t.y, t.width, t.height, t.full)).collect();
        assert_eq!(t, vec![(0, 0, 180, 1920, 1080, true), (1, 1920, 0, 2560, 1440, true)]);
    }

    #[test]
    fn plan_region_across_monitors() {
        let p = resolve_plan(&RecordConfig { source: region(1800.0, 0.0, 400.0, 300.0), ..cfg() }, &same_gpu()).unwrap();
        let t: Vec<_> = p.dda.unwrap().tiles.iter().map(|t| (t.output, t.offset_x, t.offset_y, t.width, t.height, t.x, t.y)).collect();
        assert_eq!(t, vec![(0, 1800, 0, 120, 300, 0, 0), (1, 0, 180, 280, 300, 120, 0)]);
    }

    #[test]
    fn plan_two_gpus_needs_gdigrab() {
        let p = resolve_plan(&RecordConfig { source: SourceConfig::All, ..cfg() }, &two_gpus()).unwrap();
        assert!(p.dda.is_none());
        assert_eq!(p.monitors.len(), 2);
        assert!(segment_args(&p, &cfg(), CaptureMethod::Ddagrab, &x264(), "o.mp4", None).unwrap_err().is_config());
    }

    #[test]
    fn plan_invalid_config() {
        let bad = |c: RecordConfig| resolve_plan(&c, &same_gpu()).unwrap_err().is_config();
        assert!(bad(RecordConfig { fps: 0.0, ..cfg() }));
        assert!(bad(RecordConfig { fps: 61.0, ..cfg() }));
        assert!(bad(RecordConfig { scale: 60.0, ..cfg() }));
        assert!(bad(RecordConfig { source: SourceConfig::Monitor { monitor_id: "9:9".into() }, ..cfg() }));
        assert!(bad(RecordConfig { source: region(99999.0, 0.0, 100.0, 100.0), ..cfg() }));
        assert!(bad(RecordConfig { source: region(1.5, 0.0, 100.0, 100.0), ..cfg() }));
    }

    #[test]
    fn segment_ddagrab_single_tile() {
        let c = RecordConfig { source: region(2000.0, -100.0, 801.0, 601.0), ..cfg() };
        let args = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "C:\\p\\seg.mp4", None).unwrap();
        assert!(args.contains(&"d3d11va=dda:0".to_string()));
        assert_eq!(
            graph_of(&args),
            "ddagrab=output_idx=1:framerate=30:draw_mouse=1:offset_x=80:offset_y=80:video_size=801x601,hwdownload,format=bgra,\
             crop=800:600:0:0,scale=800:600:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]"
        );
        assert_eq!(after(&args, "-map"), "[vout]");
        assert!(!args.contains(&"-nostdin".to_string()));
        assert_eq!(&args[args.len() - 2..], &["-y".to_string(), "C:\\p\\seg.mp4".to_string()]);
    }

    #[test]
    fn segment_ddagrab_xstack() {
        let c = RecordConfig { source: SourceConfig::All, scale: 50.0, ..cfg() };
        let args = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None).unwrap();
        assert_eq!(
            graph_of(&args),
            "ddagrab=output_idx=0:framerate=30:draw_mouse=1,hwdownload,format=bgra[t0];\
             ddagrab=output_idx=1:framerate=30:draw_mouse=1,hwdownload,format=bgra[t1];\
             [t0][t1]xstack=inputs=2:layout=0_180|1920_0:fill=black,\
             scale=2240:720:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]"
        );
    }

    #[test]
    fn segment_ddagrab_pad() {
        let c = RecordConfig { source: region(1000.0, -180.0, 1500.0, 400.0), ..cfg() };
        let args = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None).unwrap();
        assert!(graph_of(&args).contains("xstack=inputs=2:layout=0_180|920_0:fill=black,scale=1500:400"));
        assert!(!graph_of(&args).contains("pad="));
        let c2 = RecordConfig { source: region(1000.0, 900.0, 1500.0, 400.0), ..cfg() };
        let args2 = segment_args(&resolve_plan(&c2, &same_gpu()).unwrap(), &c2, CaptureMethod::Ddagrab, &x264(), "o.mp4", None).unwrap();
        assert!(graph_of(&args2).contains("pad=1500:400:0:0:color=black"));
    }

    #[test]
    fn segment_gdigrab_absolute_coordinates() {
        let c = RecordConfig { source: SourceConfig::Monitor { monitor_id: "1:0".into() }, draw_mouse: false, scale: 25.0, ..cfg() };
        let args = segment_args(&resolve_plan(&c, &two_gpus()).unwrap(), &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", None).unwrap();
        let v: Vec<&str> = ["-offset_x", "-offset_y", "-video_size", "-draw_mouse", "-framerate"].iter().map(|k| after(&args, k)).collect();
        assert_eq!(v, vec!["1920", "-180", "2560x1440", "0", "30"]);
        assert_eq!(graph_of(&args), "[0:v]scale=640:360:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]");
    }

    #[test]
    fn segment_with_audio() {
        let audio_in = strs(&["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "tcp://127.0.0.1:5000"]);
        let c = cfg();
        let plan = resolve_plan(&c, &same_gpu()).unwrap();
        let dda = segment_args(&plan, &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", Some(&audio_in)).unwrap();
        assert_eq!(after(&dda, "-loglevel"), "level+info");
        assert!(graph_of(&dda).contains("format=bgra,showinfo=checksum=0,"));
        assert!(dda.join(" ").contains("-map [vout] -map 0:a -c:a aac"));
        let gdi = segment_args(&plan, &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", Some(&audio_in)).unwrap();
        assert!(gdi.join(" ").contains("-map [vout] -map 1:a"));
        assert!(graph_of(&gdi).starts_with("[0:v]showinfo=checksum=0,"));
    }

    #[test]
    fn preview() {
        assert!(graph_of(&preview_args(&same_gpu(), true, 1600, None)).contains("xstack=inputs=2"));
        assert!(preview_args(&two_gpus(), true, 1600, None).contains(&"gdigrab".to_string()));
        assert!(preview_args(&same_gpu(), false, 1600, None).contains(&"gdigrab".to_string()));
        let second = same_gpu()[1].clone();
        let dda = preview_args(std::slice::from_ref(&second), true, 1600, None);
        assert!(!graph_of(&dda).contains("xstack"));
        assert!(graph_of(&dda).contains(&format!("output_idx={}", second.output)));
        let gdi = preview_args(std::slice::from_ref(&second), false, 1600, None).join(" ");
        assert!(gdi.contains(&format!("-offset_x {} -offset_y {} -video_size {}x{}", second.x, second.y, second.width, second.height)));
        assert!(!preview_args(&same_gpu(), false, 1600, None).contains(&"-offset_x".to_string()));
        // 原始像素、大小固定：介面依大小切出每一張
        let raw = preview_args(std::slice::from_ref(&second), false, 1280, Some(5.0)).join(" ");
        assert!(raw.ends_with("-f rawvideo -pix_fmt rgba -"), "{raw}");
        assert!(raw.contains("scale=1280:720:flags=bilinear,format=rgba"), "{raw}");
        assert!(!raw.contains("-frames:v"));
        assert_eq!(preview_size(&Rect { x: 0, y: 0, width: 3840, height: 1080 }, 1280), (1280, 360));
        assert_eq!(preview_size(&Rect { x: 0, y: 0, width: 1366, height: 768 }, 1600), (1366, 768));
        assert_eq!(preview_size(&Rect { x: 0, y: 0, width: 1001, height: 501 }, 1600), (1000, 500));
        let ids: Vec<_> = plan_tiles(&Rect { x: 0, y: 0, width: 100, height: 100 }, &same_gpu()).0.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec!["0:0"]);
    }

    #[test]
    fn gif() {
        let o = |out: f64, src_w: f64, src_h: f64| GifOptions { fps: 10.0, width: 640.0, src_width: src_w, src_height: src_h, output_sec: out };
        let short = gif_args("in.mp4", "out.gif", 4.0, &o(10.0, 1920.0, 1080.0)).unwrap();
        assert!(after(&short, "-vf").contains("setpts=PTS/4,fps=10,scale=640:-2"));
        assert!(after(&short, "-vf").contains("stats_mode=diff"));
        assert!(short.contains(&"-an".to_string()));
        let long = gif_args("in.mp4", "out.gif", 4.0, &o(600.0, 1920.0, 1080.0)).unwrap();
        assert!(after(&long, "-vf").contains("new=1"));
        let same = gif_args("in.mp4", "out.gif", 1.0, &GifOptions { output_sec: 5.0, ..o(5.0, 320.0, 180.0) }).unwrap();
        assert!(after(&same, "-vf").starts_with("fps=10,scale=320:-2"));
    }

    #[test]
    fn export() {
        let args = export_args("in.mp4", "out.mp4", 4.0, 30.0, &x264(), false, None, None).unwrap();
        assert_eq!(after(&args, "-vf"), "setpts=PTS/4,fps=30,format=yuv420p");
        assert!(args.contains(&"-an".to_string()));
        assert!(export_args("in.mp4", "out.mp4", 1.0, 30.0, &x264(), false, None, None).unwrap_err().is_config());
        let vf = |w, sw| export_args("in.mp4", "out.mp4", 4.0, 30.0, &x264(), false, Some(w), Some(sw)).unwrap();
        assert_eq!(after(&vf(1920, 3840), "-vf"), "setpts=PTS/4,fps=30,scale=1920:-2:flags=bicubic,format=yuv420p");
        assert_eq!(after(&vf(1920, 1920), "-vf"), "setpts=PTS/4,fps=30,format=yuv420p");
        assert_eq!(after(&vf(0, 3840), "-vf"), "setpts=PTS/4,fps=30,format=yuv420p");
        let audio = export_args("in.mp4", "out.mp4", 16.0, 30.0, &x264(), true, None, None).unwrap();
        assert_eq!(after(&audio, "-af"), "atempo=2,atempo=2,atempo=2,atempo=2");
        assert!(audio.join(" ").contains("-map 0:v:0 -map 0:a:0"));
        assert_eq!(atempo_chain(1.5), "atempo=1.5");
        assert_eq!(atempo_chain(3.0), "atempo=2,atempo=1.5");
    }

    #[test]
    fn media_info() {
        let stderr = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x.mp4':\n  Duration: 00:01:02.50, start: 0.000000, bitrate: 1234 kb/s\n  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 1920x1080 [SAR 1:1 DAR 16:9], 1200 kb/s, 30 fps, 30 tbr, 15360 tbn (default)\n  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 160 kb/s (default)";
        assert_eq!(parse_media_info(stderr), ParsedMedia { duration_sec: Some(62.5), width: Some(1920), height: Some(1080), fps: Some(30.0), has_audio: true });
        let first3: Vec<&str> = stderr.lines().take(3).collect();
        assert!(!parse_media_info(&first3.join("\n")).has_audio);
    }

    #[test]
    fn intersection_edges() {
        let screen1 = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        assert!(!intersects(&Rect { x: 1920, y: 0, width: 1280, height: 800 }, &screen1));
        assert!(intersects(&Rect { x: 1900, y: 100, width: 800, height: 600 }, &screen1));
        assert!(!intersects(&Rect { x: -1280, y: 0, width: 1280, height: 1024 }, &screen1));
        assert!(!intersects(&Rect { x: 100, y: 1080, width: 800, height: 600 }, &screen1));
    }

    #[test]
    fn concat() {
        assert_eq!(concat_list(&["C:\\Users\\O'Neil\\seg_000.mp4".into()], &[]), "file 'C:/Users/O'\\''Neil/seg_000.mp4'\n");
        assert_eq!(concat_list(&["a.mp4".into(), "b.mp4".into()], &[Some(1.5), None]), "file 'a.mp4'\noutpoint 1.500000\nfile 'b.mp4'\n");
    }

    #[test]
    fn encoder_choice() {
        let cpu = encoder_spec("libx264");
        let qsv = encoder_spec("h264_qsv").unwrap();
        let pick = |pref, w, h, fps, gpu: &[EncoderSpec], learned| choose_encoder(pref, w, h, fps, cpu, gpu, learned).map(|(s, _)| s.name);
        use EncoderPreference::*;
        assert_eq!(pick(Auto, 1920, 1080, 30.0, &[qsv], false), Ok("libx264"));
        const { assert!(3840.0 * 2160.0 * 30.0 > AUTO_GPU_PIXELS_PER_SEC) };
        assert_eq!(pick(Auto, 3840, 2160, 30.0, &[qsv], false), Ok("h264_qsv"));
        assert_eq!(pick(Auto, 4480, 1440, 30.0, &[qsv], false), Ok("h264_qsv"));
        assert_eq!(pick(Auto, 1920, 1080, 60.0, &[qsv], false), Ok("libx264"));
        assert_eq!(pick(Auto, 1280, 720, 30.0, &[qsv], true), Ok("h264_qsv"));
        assert_eq!(pick(Auto, 3840, 2160, 60.0, &[], true), Ok("libx264"));
        assert_eq!(pick(Gpu, 640, 360, 30.0, &[qsv], false), Ok("h264_qsv"));
        assert_eq!(pick(Cpu, 3840, 2160, 60.0, &[qsv], true), Ok("libx264"));
        assert!(pick(Gpu, 640, 360, 30.0, &[], false).unwrap_err().is_config());
    }

    #[test]
    fn encoder_fault_detection() {
        assert!(is_encoder_fault("[h264_nvenc @ 0x1] No NVENC capable devices found\nError while opening encoder"));
        assert!(!is_encoder_fault("[ddagrab @ 0x1] Failed to capture\nCould not open encoder before EOF"));
        let f = |stderr: &str| startup_fallback(&FallbackInput { stderr, gpu_encoder_in_use: true, encoder_auto: true, has_cpu_encoder: true, ddagrab_in_use: true, method_auto: true });
        assert_eq!(f("Cannot load nvEncodeAPI64.dll"), StartupFallback::CpuEncoder);
        assert_eq!(f("[ddagrab] Desktop duplication failed"), StartupFallback::Gdigrab);
        assert_eq!(f("something else"), StartupFallback::Gdigrab);
    }

    const NVENC_FAIL: &str = "[h264_nvenc @ 0000023ef7e6d940] Cannot load nvEncodeAPI64.dll\n[h264_nvenc @ 0000023ef7e6d940] The minimum required Nvidia driver for nvenc is 610.00 or newer\n[vost#0:0/h264_nvenc @ 0000023ef7e6c6c0] [enc:h264_nvenc @ 0000023ef7a04e00] Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.\n[vost#0:0/h264_nvenc @ 0000023ef7e6c6c0] [enc:h264_nvenc @ 0000023ef7a04e00] Could not open encoder before EOF";
    const DDAGRAB_FAIL: &str = "[Parsed_ddagrab_0 @ 000002c19f1d4d80] Failed to enumerate DXGI output 7\n[Parsed_ddagrab_0 @ 000002c19f1d4d80] Failed to configure output pad on Parsed_ddagrab_0\n[fc#0 @ 000002c19f130980] Error configuring filter graph: Generic error in an external library\n[vost#0:0/h264_qsv @ 000002c19f1cba80] [enc:h264_qsv @ 000002c19f1822c0] Could not open encoder before EOF";

    #[test]
    fn startup_fallback_with_real_ffmpeg_messages() {
        assert!(is_encoder_fault(NVENC_FAIL));
        assert!(!is_encoder_fault(DDAGRAB_FAIL));
        let base = |stderr| FallbackInput { stderr, gpu_encoder_in_use: true, encoder_auto: true, has_cpu_encoder: true, ddagrab_in_use: true, method_auto: true };
        assert_eq!(startup_fallback(&base(NVENC_FAIL)), StartupFallback::CpuEncoder);
        assert_eq!(startup_fallback(&base(DDAGRAB_FAIL)), StartupFallback::Gdigrab);
        assert_eq!(startup_fallback(&base("something odd")), StartupFallback::Gdigrab);
        assert_eq!(startup_fallback(&FallbackInput { ddagrab_in_use: false, ..base("something odd") }), StartupFallback::CpuEncoder);
        assert_eq!(startup_fallback(&FallbackInput { encoder_auto: false, method_auto: false, ..base(NVENC_FAIL) }), StartupFallback::Fatal);
        assert_eq!(startup_fallback(&FallbackInput { gpu_encoder_in_use: false, ddagrab_in_use: false, ..base(NVENC_FAIL) }), StartupFallback::Fatal);
    }

    #[test]
    fn cut() {
        let args = cut_args("in.mp4", "out.mp4", &[(1.0, 2.0), (4.0, 5.5)], Some(Rect { x: 40, y: 40, width: 200, height: 160 }), 30.0, &x264(), true, &[]).unwrap();
        assert_eq!(after(&args, "-vf"), "select='gte(t,1)*lt(t,2)+gte(t,4)*lt(t,5.5)',setpts=N/(30*TB),crop=200:160:40:40,format=yuv420p");
        assert_eq!(after(&args, "-af"), "aselect='gte(t,1)*lt(t,2)+gte(t,4)*lt(t,5.5)',asetpts=N/SR/TB");
        let no_audio = cut_args("in.mp4", "out.mp4", &[(0.0, 3.0)], None, 30.0, &x264(), false, &[]).unwrap();
        assert!(no_audio.contains(&"-an".to_string()));
        assert!(!no_audio.contains(&"-af".to_string()));
        assert!(cut_args("in.mp4", "out.mp4", &[], None, 30.0, &x264(), false, &[]).unwrap_err().is_config());
        // 與 edit 模組串起來
        let keep = keep_ranges(10.0, &EditSpec { start: 1.0, end: 9.0, removed: vec![(3.0, 4.0)], crop: None, overlays: vec![] });
        assert_eq!(keep, vec![(1.0, 3.0), (4.0, 9.0)]);
        assert_eq!(normalize_crop(None, 100, 100), None);

        // 標註：先疊上（原影片時間），再挑選保留的片段與裁切
        let overlays = [
            OverlayInput::Image { path: "a.png".into(), x: 10, y: 20, start: 1.0, end: 3.5 },
            OverlayInput::Blur { rect: Rect { x: 100, y: 50, width: 120, height: 60 }, start: 0.0, end: 9.0, mosaic: false, mask: None, invert: false, frame: (1280, 720) },
            OverlayInput::Blur { rect: Rect { x: 0, y: 0, width: 240, height: 120 }, start: 2.0, end: 4.0, mosaic: true, mask: None, invert: false, frame: (1280, 720) },
            // 橢圓模糊：遮罩縮放成範圍大小，alphamerge 後疊上
            OverlayInput::Blur { rect: Rect { x: 200, y: 100, width: 400, height: 240 }, start: 1.0, end: 3.0, mosaic: false, mask: Some("m.png".into()), invert: false, frame: (1280, 720) },
            // 範圍外模糊（橢圓範圍內清楚）
            OverlayInput::Blur { rect: Rect { x: 700, y: 400, width: 400, height: 240 }, start: 2.0, end: 4.0, mosaic: false, mask: Some("m2.png".into()), invert: true, frame: (1280, 720) },
            // 範圍外馬賽克（方形）
            OverlayInput::Blur { rect: Rect { x: 10, y: 20, width: 100, height: 50 }, start: 0.0, end: 1.0, mosaic: true, mask: None, invert: true, frame: (1280, 720) },
        ];
        let args = cut_args("in.mp4", "out.mp4", &[(1.0, 5.0)], Some(Rect { x: 40, y: 40, width: 200, height: 160 }), 30.0, &x264(), true, &overlays).unwrap();
        assert!(!args.contains(&"-vf".to_string()));
        let inputs: Vec<&String> = args.iter().zip(args.iter().skip(1)).filter(|(k, _)| *k == "-i").map(|(_, v)| v).collect();
        assert_eq!(inputs, ["in.mp4", "a.png", "m.png", "m2.png"]);
        assert_eq!(
            after(&args, "-filter_complex"),
            "[0:v][1:v]overlay=10:20:enable='between(t,1,3.5)'[o0];\
             [o0]split=2[b1a][b1b];[b1b]crop=120:60:100:50,boxblur=14:2[b1c];[b1a][b1c]overlay=100:50:enable='between(t,0,9)'[o1];\
             [o1]split=2[b2a][b2b];[b2b]crop=240:120:0:0,scale=12:6:flags=area,scale=240:120:flags=neighbor[b2c];[b2a][b2c]overlay=0:0:enable='between(t,2,4)'[o2];\
             [o2]split=2[b3a][b3b];[b3b]crop=400:240:200:100,boxblur=30:2,format=yuva420p[b3e];[2:v]scale=400:240,format=gray[b3m];[b3e][b3m]alphamerge[b3c];[b3a][b3c]overlay=200:100:enable='between(t,1,3)'[o3];\
             [o3]split=3[b4a][b4b][b4d];[b4b]boxblur=18:2[b4f];[b4d]crop=400:240:700:400,format=yuva420p[b4e];[3:v]scale=400:240,format=gray[b4m];[b4e][b4m]alphamerge[b4c];[b4f][b4c]overlay=700:400[b4g];[b4a][b4g]overlay=0:0:enable='between(t,2,4)'[o4];\
             [o4]split=3[b5a][b5b][b5d];[b5b]scale=80:45:flags=area,scale=1280:720:flags=neighbor[b5f];[b5d]crop=100:50:10:20[b5c];[b5f][b5c]overlay=10:20[b5g];[b5a][b5g]overlay=0:0:enable='between(t,0,1)'[o5];\
             [o5]select='gte(t,1)*lt(t,5)',setpts=N/(30*TB),crop=200:160:40:40,format=yuv420p[vout]"
        );
        assert_eq!(after(&args, "-map"), "[vout]");
        assert_eq!(after(&args, "-af"), "aselect='gte(t,1)*lt(t,5)',asetpts=N/SR/TB");
        assert_eq!(cut_file_name("a.mp4"), "a_cut.mp4");
    }
}
