//! 組 FFmpeg 參數（純函式，方便測試）。

use crate::edit::{AudioFx, Part};
use crate::error::{Error, Result};
use crate::format::{even, num, output_size, FPS_MAX, FPS_MIN, MAX_MINUTES_MAX, SPEED_MAX, SPEED_MIN};
use crate::types::{CaptureMethod, EncoderPreference, MonitorInfo, RecordConfig, Rect, SourceConfig, SCALE_OPTIONS};
use crate::{tr, trf};
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
    /// 指定位元率（kbps）的參數：壓縮到指定大小用
    pub fn bitrate(&self, kbps: u32) -> Vec<String> {
        let (b, max, buf) = (format!("{kbps}k"), format!("{}k", kbps * 3 / 2), format!("{}k", kbps * 2));
        let head: &[&str] = match self.name {
            "h264_nvenc" => &["-c:v", "h264_nvenc", "-preset", "p6", "-rc", "vbr"],
            "h264_qsv" => &["-c:v", "h264_qsv", "-preset", "medium"],
            "h264_amf" => &["-c:v", "h264_amf", "-quality", "quality", "-rc", "vbr_peak"],
            "h264_mf" => &["-c:v", "h264_mf", "-rate_control", "cbr"],
            _ => &["-c:v", "libx264", "-preset", "medium"],
        };
        let mut a: Vec<String> = head.iter().map(|s| s.to_string()).collect();
        a.extend(["-b:v".into(), b, "-maxrate".into(), max, "-bufsize".into(), buf]);
        a
    }
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
static CAPTURE_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)ddagrab|gdigrab|gfxcapture|Desktop duplication|Error configuring filter graph|Failed to capture").unwrap());
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
    /// 不在顯示卡上處理畫面，改回下載後用 CPU 轉
    CpuConvert,
    CpuEncoder,
    Gdigrab,
    /// 只錄視窗內容（gfxcapture）失敗：改用跟著視窗的範圍
    RegionCapture,
    Fatal,
}

pub struct FallbackInput<'a> {
    pub stderr: &'a str,
    pub gpu_encoder_in_use: bool,
    pub encoder_auto: bool,
    pub has_cpu_encoder: bool,
    pub ddagrab_in_use: bool,
    pub method_auto: bool,
    /// 這次在顯示卡上處理畫面
    pub gpu_convert_in_use: bool,
    /// 這次用 gfxcapture 只錄視窗內容
    pub gfx_in_use: bool,
}

/// 第一張畫面之前就失敗時該退回哪一項：依錯誤訊息判斷是編碼器還是擷取（ddagrab）出問題。
/// 判斷不出來時，先退擷取方式（較常見），之後若仍失敗再退編碼器。
pub fn startup_fallback(o: &FallbackInput) -> StartupFallback {
    // 顯示卡處理是最新、最依賴驅動的一環：先拿掉它再試，其他設定不變
    if o.gpu_convert_in_use {
        return StartupFallback::CpuConvert;
    }
    let can_cpu = o.gpu_encoder_in_use && o.encoder_auto && o.has_cpu_encoder;
    let can_gdi = o.ddagrab_in_use && o.method_auto;
    if is_encoder_fault(o.stderr) && can_cpu {
        return StartupFallback::CpuEncoder;
    }
    // Windows Graphics Capture 是最新、限制最多的擷取方式（舊版 Windows、某些視窗不能擷取）
    if o.gfx_in_use {
        return StartupFallback::RegionCapture;
    }
    if can_gdi {
        return StartupFallback::Gdigrab;
    }
    if can_cpu {
        return StartupFallback::CpuEncoder;
    }
    StartupFallback::Fatal
}

/// 「自動」優先用的顯示卡編碼器：Intel QSV（內顯幾乎每台都有，壓縮時 CPU 負擔最小；
/// 擷取在 Intel 顯示卡上時，畫面還能全程留在顯示卡）
pub const AUTO_PREFERRED_GPU: &str = "h264_qsv";

/// 決定這次錄影的編碼器（learned：之前在「自動」模式偵測到 CPU 跟不上，之後直接用 GPU）
pub fn choose_encoder(pref: EncoderPreference, out_width: i32, out_height: i32, fps: f64, cpu: Option<EncoderSpec>, gpu: &[EncoderSpec], learned: bool) -> Result<(EncoderSpec, &'static str)> {
    if pref == EncoderPreference::Gpu {
        let Some(g) = gpu.first() else {
            return Err(Error::config(tr!("這台電腦沒有可用的 GPU 編碼器，請改用 CPU 或自動", "No GPU encoder is available on this computer. Choose CPU or Auto instead.")));
        };
        return Ok((*g, tr!("指定使用 GPU 編碼", "Using GPU encoding (as set)")));
    }
    if pref == EncoderPreference::Auto {
        if let Some(q) = gpu.iter().find(|e| e.name == AUTO_PREFERRED_GPU) {
            return Ok((*q, tr!("優先使用 Intel 顯示卡編碼（QSV），減輕 CPU 負擔", "Using Intel GPU encoding (QSV) to reduce CPU load")));
        }
        if let Some(g) = gpu.first() {
            if out_width as f64 * out_height as f64 * fps > AUTO_GPU_PIXELS_PER_SEC {
                return Ok((*g, tr!("畫面量超過 1080p60，自動改用 GPU 編碼", "More than 1080p60 worth of pixels; switched to GPU encoding automatically")));
            }
            if learned {
                return Ok((*g, tr!("先前偵測到 CPU 編碼跟不上，自動改用 GPU 編碼", "CPU encoding couldn't keep up before; switched to GPU encoding automatically")));
            }
        }
    }
    match (cpu, gpu.first()) {
        (Some(c), _) => Ok((c, tr!("CPU 編碼", "CPU encoding"))),
        (None, Some(g)) => Ok((*g, tr!("沒有 CPU 編碼器，改用 GPU 編碼", "No CPU encoder; using GPU encoding"))),
        (None, None) => Err(Error::config(tr!("FFmpeg 沒有可用的 H.264 編碼器", "FFmpeg has no usable H.264 encoder"))),
    }
}

/// 在顯示卡上縮放並轉成 BT.709（limited）的 NV12：ddagrab 的畫面不用整張（BGRA）下載回 CPU 再轉。
/// 只用在能指定 BT.709 的濾鏡：其他處理（加速版、合併、剪輯匯出）都直接沿用影片的 YUV 並標成 BT.709，
/// 錄影本身必須是 BT.709。NVIDIA 沒有這樣的濾鏡（scale_d3d11 與 NVENC 收 RGB 時都是 BT.601），維持 CPU 轉換
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuConvert {
    /// Intel：對應到 QSV，以 vpp_qsv 處理
    Qsv,
    /// AMD：vpp_amf（FFmpeg 8.0 起）
    Amf,
}

impl GpuConvert {
    /// 依顯示卡名稱判斷
    pub fn for_adapter(name: &str) -> Option<GpuConvert> {
        let n = name.to_ascii_lowercase();
        if n.contains("intel") {
            Some(GpuConvert::Qsv)
        } else if n.contains("amd") || n.contains("radeon") {
            Some(GpuConvert::Amf)
        } else {
            None
        }
    }
    /// 需要的濾鏡（`ffmpeg -filters` 要列得出來）
    pub fn filter(self) -> &'static str {
        match self {
            GpuConvert::Qsv => "vpp_qsv",
            GpuConvert::Amf => "vpp_amf",
        }
    }
    /// 同一張顯示卡的編碼器：處理好的畫面可以直接交給它
    pub fn encoder(self) -> &'static str {
        match self {
            GpuConvert::Qsv => "h264_qsv",
            GpuConvert::Amf => "h264_amf",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            GpuConvert::Qsv => "Intel QSV",
            GpuConvert::Amf => "AMD AMF",
        }
    }
    /// 接在 ddagrab（D3D11 畫面）之後：縮放到 w × h 並轉成 BT.709 limited 的 NV12，畫面仍在顯示卡上。
    /// vpp_amf 的 color_profile=bt709 即 limited（FFmpeg 8 與 9 的範圍選項名稱不同，不另外指定）
    pub fn chain(self, w: i32, h: i32) -> String {
        match self {
            GpuConvert::Qsv => {
                format!("hwmap=derive_device=qsv,format=qsv,vpp_qsv=w={w}:h={h}:format=nv12:out_color_matrix=bt709:out_color_primaries=bt709:out_color_transfer=bt709:out_range=limited")
            }
            GpuConvert::Amf => format!("vpp_amf=w={w}:h={h}:format=nv12:scale_type=bicubic:color_profile=bt709"),
        }
    }
}

/// 實測通過的顯示卡處理（app 啟動時測試）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuSupport {
    pub adapter: u32,
    pub convert: GpuConvert,
    /// 處理好的畫面可以直接交給 convert.encoder()（實測可用）
    pub zero_copy: bool,
}

/// 這次錄影在顯示卡上處理畫面的方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuPath {
    /// 實測過的顯示卡：範圍移到其他顯示卡的螢幕時改回 CPU 轉換
    pub adapter: u32,
    pub convert: GpuConvert,
    /// 直接交給同一張顯示卡的編碼器；否則下載轉好的 NV12（比 BGRA 少 62%）給其他編碼器
    pub zero_copy: bool,
}

impl GpuPath {
    /// 這次錄影能不能在顯示卡上處理：ddagrab、單一螢幕且在實測過的顯示卡上
    pub fn choose(support: Option<GpuSupport>, plan: &CapturePlan, config: &RecordConfig, method: CaptureMethod, enc: &EncoderSpec) -> Option<GpuPath> {
        let s = support?;
        let g = GpuPath { adapter: s.adapter, convert: s.convert, zero_copy: s.zero_copy }.for_encoder(enc);
        (method == CaptureMethod::Ddagrab && g.applies(plan, config)).then_some(g)
    }
    /// 這個範圍、設定能不能用（錄影中移動範圍到別的螢幕時會變）
    pub fn applies(&self, plan: &CapturePlan, config: &RecordConfig) -> bool {
        // 要打馬賽克時畫面要回到 CPU 處理
        plan.dda.as_ref().is_some_and(|d| d.tiles.len() == 1 && d.adapter == self.adapter) && !config.audio_only && mask_rects(plan, config).is_empty()
    }
    /// 編碼器換了（例如 GPU 編碼器失敗改用 CPU）：只有同一張顯示卡的編碼器能直接接
    pub fn for_encoder(self, enc: &EncoderSpec) -> GpuPath {
        GpuPath { zero_copy: self.zero_copy && enc.name == self.convert.encoder(), ..self }
    }
    pub fn describe(&self) -> String {
        if self.zero_copy {
            trf!("畫面在顯示卡上縮放、轉色彩後直接壓縮（{}）", "Frames are scaled, color-converted and encoded on the GPU ({})", self.convert.label())
        } else {
            trf!("畫面在顯示卡上縮放、轉色彩（{}）", "Frames are scaled and color-converted on the GPU ({})", self.convert.label())
        }
    }
}

/// 顯示卡處理的實測：同一張畫面分別在顯示卡上轉、和用 CPU 轉成 BT.709 與 BT.601，比較亮度（Y）的 PSNR。
/// 顏色矩陣或範圍（limited / full）不對時亮度差很多；色度的取樣方式本來就不同，不拿來比
pub fn gpu_convert_test_args(m: &MonitorInfo, convert: GpuConvert) -> Vec<String> {
    let (w, h) = (m.width & !1, m.height & !1);
    // psnr 不收 NV12，FFmpeg 會自動插入轉換，並依標記把 full range 轉回 limited，把錯誤蓋掉：
    // 先把兩邊都標成一樣，比的才是實際的數值
    const SAME: &str = "setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709";
    let cpu = |matrix: &str| format!("hwdownload,format=bgra,scale={w}:{h}:flags=bicubic:out_color_matrix={matrix}:out_range=tv,format=nv12,{SAME}");
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "info", "-init_hw_device"]);
    a.push(format!("d3d11va=dda:{}", m.adapter));
    a.extend(strs(&["-filter_hw_device", "dda", "-filter_complex"]));
    a.push(format!(
        "ddagrab=output_idx={}:framerate=10:draw_mouse=0:video_size={w}x{h},showinfo=checksum=0,split=3[a][b][c];[a]{},hwdownload,format=nv12,{SAME},split=2[g1][g2];[b]{}[r709];[c]{}[r601];[g1][r709]psnr[o1];[g2][r601]psnr[o2]",
        m.output,
        convert.chain(w, h),
        cpu("bt709"),
        cpu("bt601"),
    ));
    a.extend(strs(&["-map", "[o1]", "-map", "[o2]", "-frames:v", "6", "-f", "null", "-"]));
    a
}

static PSNR_Y: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Parsed_psnr_(\d+)[^\n]*PSNR y:(inf|[\d.]+)").unwrap());

/// 從實測的輸出讀出 (BT.709 的 Y PSNR, BT.601 的 Y PSNR)；inf 表示完全相同
pub fn parse_gpu_psnr(stderr: &str) -> Option<(f64, f64)> {
    let mut v: Vec<(u32, f64)> = PSNR_Y.captures_iter(stderr).filter_map(|c| Some((c[1].parse().ok()?, if &c[2] == "inf" { f64::INFINITY } else { c[2].parse().ok()? }))).collect();
    v.sort_by_key(|p| p.0);
    (v.len() == 2).then(|| (v[0].1, v[1].1))
}

/// 顯示卡轉出來的顏色是否正確：亮度和 CPU 轉的 BT.709 幾乎相同，而且不比 BT.601 更不像
/// （畫面全是灰階時兩者一樣，這時相信濾鏡指定的 BT.709）
pub fn gpu_psnr_ok(bt709: f64, bt601: f64) -> bool {
    bt709 >= 40.0 && bt709 + 0.1 >= bt601
}

/// 直接交給編碼器的實測：與錄影相同的濾鏡串（含 showinfo）實際壓縮幾張
pub fn gpu_encode_test_args(m: &MonitorInfo, convert: GpuConvert, enc: &EncoderSpec) -> Vec<String> {
    let (w, h) = (m.width & !1, m.height & !1);
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-init_hw_device"]);
    a.push(format!("d3d11va=dda:{}", m.adapter));
    a.extend(strs(&["-filter_hw_device", "dda", "-filter_complex"]));
    a.push(format!("ddagrab=output_idx={}:framerate=30:draw_mouse=0:video_size={w}x{h},showinfo=checksum=0,{}[vout]", m.output, convert.chain(w, h)));
    a.extend(strs(&["-map", "[vout]"]));
    a.extend(enc.live());
    a.extend(strs(&["-frames:v", "10", "-f", "null", "-"]));
    a
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
    /// 只錄聲音：畫面改用這張圖（以實際速度送進 FFmpeg）
    pub card: Option<String>,
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
        return Err(Error::config(trf!("錄影 FPS 需為 {}～{} 的整數", "Recording FPS must be a whole number from {} to {}", num(FPS_MIN), num(FPS_MAX))));
    }
    if !SCALE_OPTIONS.iter().any(|&s| s as f64 == config.scale) {
        return Err(Error::config(tr!("解析度縮放只能是 100 / 75 / 50 / 25%", "Resolution scale must be 100 / 75 / 50 / 25%")));
    }
    if !config.max_minutes.is_finite() || config.max_minutes < 0.0 || config.max_minutes > MAX_MINUTES_MAX {
        return Err(Error::config(tr!("最長錄影時間設定不正確", "Invalid maximum recording length")));
    }
    if config.output_dir.trim().is_empty() {
        return Err(Error::config(tr!("請指定儲存位置", "Choose a save location")));
    }

    // 只錄聲音：畫面是固定大小的卡片，與螢幕無關
    if config.audio_only {
        let (w, h) = (crate::audio_card::SIZE.0 as i32, crate::audio_card::SIZE.1 as i32);
        return Ok(CapturePlan { rect: Rect { x: 0, y: 0, width: w, height: h }, dda: None, monitors: vec![], out_width: w, out_height: h, card: None });
    }
    let rect = match &config.source {
        SourceConfig::Monitor { monitor_id } => {
            let Some(m) = monitors.iter().find(|m| &m.id == monitor_id) else {
                return Err(Error::config(tr!("找不到選擇的螢幕，請重新整理螢幕清單", "The selected screen wasn't found. Refresh the screen list.")));
            };
            monitor_rect(m)
        }
        SourceConfig::All => {
            if monitors.is_empty() {
                return Err(Error::config(tr!("找不到任何螢幕", "No screens found")));
            }
            desktop_rect(monitors)
        }
        SourceConfig::Region { x, y, width, height } => {
            if ![*x, *y, *width, *height].iter().all(|v| is_int(*v)) {
                return Err(Error::config(tr!("範圍座標必須是整數", "Area coordinates must be whole numbers")));
            }
            if *width < 16.0 || *height < 16.0 {
                return Err(Error::config(tr!("範圍寬高至少 16 像素", "The area must be at least 16 pixels wide and tall")));
            }
            Rect { x: *x as i32, y: *y as i32, width: *width as i32, height: *height as i32 }
        }
    };

    let (involved, dda) = plan_tiles(&rect, monitors);
    if !monitors.is_empty() && involved.is_empty() {
        return Err(Error::config(tr!("範圍不在任何螢幕上", "The area isn't on any screen")));
    }
    let (out_width, out_height) = output_size(rect.width, rect.height, config.scale);
    Ok(CapturePlan { rect, dda, monitors: involved, out_width, out_height, card: None })
}

/// ddagrab 來源（GPU 擷取後下載回系統記憶體）。多個螢幕時以 xstack 依實際位置拼接，沒有畫面的區域補黑色。
fn ddagrab_chain(plan: &CapturePlan, fps: f64, draw_mouse: bool) -> String {
    ddagrab_chain_opts(plan, fps, draw_mouse, false)
}

/// ddagrab 的來源濾鏡（畫面還在顯示卡上，D3D11 BGRA）。
/// skip_static：畫面沒有變化時不送出（dup_frames=0，FFmpeg 7.0 起）；沒有變化時整個濾鏡圖會停著等，
/// 只能用在即時預覽這種不需要固定張數的地方，錄影（聲音交錯、每秒寫入、停頓偵測）不能用
fn ddagrab_source(t: &Tile, fps: f64, draw_mouse: bool, skip_static: bool) -> String {
    let mut p = vec![format!("ddagrab=output_idx={}", t.output), format!("framerate={}", num(fps)), format!("draw_mouse={}", draw_mouse as u8)];
    if !t.full {
        p.push(format!("offset_x={}", t.offset_x));
        p.push(format!("offset_y={}", t.offset_y));
        p.push(format!("video_size={}x{}", t.width, t.height));
    }
    if skip_static {
        p.push("dup_frames=0".into());
    }
    p.join(":")
}

/// skip_static 只對單一螢幕有效：多個螢幕時 xstack 要每塊都有新畫面才會輸出，一塊靜止就會整個停住
fn ddagrab_chain_opts(plan: &CapturePlan, fps: f64, draw_mouse: bool, skip_static: bool) -> String {
    let tiles = &plan.dda.as_ref().expect("ddagrab 需要 dda").tiles;
    let skip_static = skip_static && tiles.len() == 1;
    let src = |t: &Tile| format!("{},hwdownload,format=bgra", ddagrab_source(t, fps, draw_mouse, skip_static));
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

/// 錄影時要打馬賽克的範圍（相對於擷取範圍的左上角、裁到範圍內）；太小的不算
pub fn mask_rects(plan: &CapturePlan, config: &RecordConfig) -> Vec<Rect> {
    if config.audio_only || config.window_capture {
        return vec![];
    }
    let r = plan.rect;
    config
        .masks
        .iter()
        .filter_map(|m| {
            let x0 = m.x.max(r.x);
            let y0 = m.y.max(r.y);
            let x1 = (m.x + m.width).min(r.x + r.width);
            let y1 = (m.y + m.height).min(r.y + r.height);
            (x1 - x0 >= 4 && y1 - y0 >= 4).then(|| Rect { x: x0 - r.x, y: y0 - r.y, width: x1 - x0, height: y1 - y0 })
        })
        .collect()
}

/// 畫面來源 src（濾鏡鏈或輸入標籤 [0:v]）之後打上馬賽克，再接 tail，輸出 [vout]
fn masked_graph(src: &str, tail: &str, masks: &[Rect]) -> String {
    let label = src.ends_with(']');
    if masks.is_empty() {
        return if label { format!("{src}{tail}[vout]") } else { format!("{src},{tail}[vout]") };
    }
    let mut g = vec![if label { format!("{src}null[mk0]") } else { format!("{src}[mk0]") }];
    for (i, m) in masks.iter().enumerate() {
        let (w, h, x, y) = (m.width, m.height, m.x, m.y);
        g.push(format!("[mk{i}]split=2[mka{i}][mkb{i}];[mkb{i}]crop={w}:{h}:{x}:{y},{}[mkc{i}];[mka{i}][mkc{i}]overlay={x}:{y}[mk{}]", blur_effect(w, h, true, false), i + 1));
    }
    g.push(format!("[mk{}]{tail}[vout]", masks.len()));
    g.join(";")
}

/// 錄影時的麥克風降噪（FFT 降噪、追蹤雜音的變化）
const MIC_DENOISE: &str = "afftdn=nr=12:nf=-40:tn=1";

/// 錄影的聲音：分軌、降噪時的濾鏡（另一個 -filter_complex）與要 map 的輸出；都不用時 None（直接 map 輸入）
fn record_audio_graph(i: usize, config: &RecordConfig) -> Option<(String, Vec<&'static str>)> {
    let mic = |extra: &str| format!("pan=stereo|c0=c4|c1=c5{extra}");
    let dn = format!(",{MIC_DENOISE}");
    match (config.separate_tracks(), config.denoise_mic(), config.pipe_separate()) {
        // 分軌、不降噪：拆成 混音、系統聲音、麥克風 三條
        (true, false, _) => {
            Some((format!("[{i}:a]asplit=3[sa0][sa1][sa2];[sa0]pan=stereo|c0=c0|c1=c1[amix];[sa1]pan=stereo|c0=c2|c1=c3[asys];[sa2]{}[amic]", mic("")), vec!["[amix]", "[asys]", "[amic]"]))
        }
        // 分軌又降噪：混音改用降噪後的麥克風重新混
        (true, true, _) => Some((
            format!("[{i}:a]asplit=2[sa1][sa2];[sa1]pan=stereo|c0=c2|c1=c3,asplit=2[asys][as2];[sa2]{},asplit=2[amic][am2];[as2][am2]amix=inputs=2:normalize=0[amix]", mic(&dn)),
            vec!["[amix]", "[asys]", "[amic]"],
        )),
        // 系統聲音＋麥克風、只降噪：分開送進來，麥克風降噪後再混成一條
        (false, true, true) => Some((format!("[{i}:a]asplit=2[sa1][sa2];[sa1]pan=stereo|c0=c2|c1=c3[as];[sa2]{}[am];[as][am]amix=inputs=2:normalize=0[amix]", mic(&dn)), vec!["[amix]"])),
        // 只有麥克風：整條降噪
        (false, true, false) => Some((format!("[{i}:a]{MIC_DENOISE}[amix]"), vec!["[amix]"])),
        (false, false, _) => None,
    }
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

/// 列出攝影機的參數（結果在 stderr）
pub fn list_cameras_args() -> Vec<String> {
    strs(&["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
}

/// 從 `-list_devices` 的輸出讀出攝影機名稱。新版每行標 (video) / (audio)；舊版分成「DirectShow video devices」與 audio 兩段
pub fn parse_cameras(stderr: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut section_video = false;
    for line in stderr.lines() {
        if line.contains("DirectShow video devices") {
            section_video = true;
            continue;
        }
        if line.contains("DirectShow audio devices") {
            section_video = false;
            continue;
        }
        if line.contains("Alternative name") {
            continue;
        }
        let (Some(a), Some(b)) = (line.find('"'), line.rfind('"')) else { continue };
        if b <= a + 1 {
            continue;
        }
        let name = &line[a + 1..b];
        let tail = &line[b + 1..];
        let video = if tail.contains("(video)") {
            true
        } else if tail.contains("(audio)") || tail.contains("(none)") {
            false
        } else {
            section_video
        };
        if video && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// probe：在擷取後插入 showinfo，每張畫面印一行 pts，供聲音對齊畫面時間零點
/// gpu：在顯示卡上縮放、轉色彩（GpuPath::choose 決定，只會是單一螢幕的 ddagrab）。
/// 攝影機不在這裡：錄影時是螢幕上的小視窗（camera_bubble），跟著畫面一起被錄進去
pub fn capture_spec(plan: &CapturePlan, config: &RecordConfig, method: CaptureMethod, enc: &EncoderSpec, probe: bool, gpu: Option<GpuPath>) -> Result<CaptureSpec> {
    let mut tail_parts = Vec::new();
    if probe {
        tail_parts.push("showinfo=checksum=0".to_string());
    }
    tail_parts.extend(convert_filters(plan, config, enc));
    let tail = tail_parts.join(",");

    if method == CaptureMethod::Ddagrab {
        let Some(dda) = &plan.dda else {
            return Err(Error::config(tr!("此範圍涵蓋不同顯示卡上的螢幕，無法使用 ddagrab", "This area spans screens on different graphics cards, so ddagrab can't be used")));
        };
        let graph = match gpu.filter(|g| g.applies(plan, config)) {
            // 顯示卡上處理：showinfo 只讀時間，不碰畫面內容，放在顯示卡上的畫面也可以
            Some(g) => {
                let mut f = vec![ddagrab_source(&dda.tiles[0], config.fps, config.draw_mouse, false)];
                if probe {
                    f.push("showinfo=checksum=0".into());
                }
                f.push(g.convert.chain(plan.out_width, plan.out_height));
                if !g.zero_copy {
                    f.push("hwdownload,format=nv12".into());
                }
                format!("{}[vout]", f.join(","))
            }
            None => masked_graph(&ddagrab_chain(plan, config.fps, config.draw_mouse), &tail, &mask_rects(plan, config)),
        };
        return Ok(CaptureSpec { pre: vec!["-init_hw_device".into(), format!("d3d11va=dda:{}", dda.adapter), "-filter_hw_device".into(), "dda".into()], inputs: vec![], input_count: 0, graph });
    }

    // 只錄視窗內容：Windows Graphics Capture 直接擷取那個視窗（顯示卡上的畫面，下載後照常縮放、轉色彩）；
    // 視窗大小改變時維持開始時的大小（等比例縮放、補黑邊）
    if method == CaptureMethod::Gfxcapture {
        let Some(hwnd) = config.follow_window.filter(|h| *h > 0) else {
            return Err(Error::config(tr!("找不到要錄的視窗", "The window to record wasn't found")));
        };
        let src = format!("gfxcapture=hwnd={hwnd}:max_framerate={}:capture_cursor={}:width=-2:height=-2:resize_mode=scale_aspect,hwdownload,format=bgra", num(config.fps), config.draw_mouse as u8);
        // 擷取到的畫面大小由視窗決定（可能和範圍差幾個像素）：直接縮放，不從左上角裁切
        let tail: Vec<&String> = tail_parts.iter().filter(|f| !f.starts_with("crop=")).collect();
        let tail = tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
        return Ok(CaptureSpec { pre: vec![], inputs: vec![], input_count: 0, graph: format!("{src},{tail}[vout]") });
    }

    // 只錄聲音：卡片圖片一直重複，以實際速度送進來（-re），showinfo 的時間才能拿來對齊聲音
    if let Some(card) = plan.card.as_ref().filter(|_| config.audio_only) {
        return Ok(CaptureSpec {
            pre: vec![],
            inputs: vec!["-re".into(), "-loop".into(), "1".into(), "-framerate".into(), num(config.fps), "-i".into(), card.clone()],
            input_count: 1,
            graph: format!("[0:v]{tail}[vout]"),
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
        graph: masked_graph("[0:v]", &tail, &mask_rects(plan, config)),
    })
}

/// 分軌錄影的三條音軌：名稱（播放器、剪輯軟體看得到；MP4 存在 handler_name）與預設播放第一條（混音）
pub fn track_tags() -> Vec<String> {
    let mut a = vec![];
    for (i, title) in [tr!("混音", "Mix"), tr!("系統聲音", "System audio"), tr!("麥克風", "Microphone")].iter().enumerate() {
        a.push(format!("-metadata:s:a:{i}"));
        a.push(format!("handler_name={title}"));
        a.push(format!("-metadata:s:a:{i}"));
        a.push(format!("title={title}"));
        a.push(format!("-disposition:a:{i}"));
        a.push(if i == 0 { "default".into() } else { "0".into() });
    }
    a
}

/// 單一分段的完整參數。stdin 送 q 收尾；-progress 從 stdout 回報張數與大小。
/// audio_input：錄聲音時 AudioPipe 提供的輸入參數（TCP raw PCM）
/// 重播緩衝每個檔案的長度（秒）：存下來的影片最多比指定的長這麼多
pub const REPLAY_CHUNK_SEC: u32 = 10;

pub fn segment_args(plan: &CapturePlan, config: &RecordConfig, method: CaptureMethod, enc: &EncoderSpec, out_file: &str, audio_input: Option<&[String]>, gpu: Option<GpuPath>) -> Result<Vec<String>> {
    let spec = capture_spec(plan, config, method, enc, audio_input.is_some(), gpu)?;
    let fps = num(config.fps);
    let mut a: Vec<String> = Vec::new();
    // 錄聲音時需要 showinfo（info 等級）的輸出；level 前綴用來分辨錯誤訊息
    a.extend(strs(&["-hide_banner", "-nostats", "-loglevel", if audio_input.is_some() { "level+info" } else { "error" }]));
    a.extend(spec.pre);
    a.extend(spec.inputs);
    if let Some(ai) = audio_input {
        a.extend(ai.iter().cloned());
    }
    // 分軌：送進來的 6 個聲道是 混音、系統聲音、麥克風 各兩個，拆成三條立體聲音軌。
    // 拆聲音要放在另一個濾鏡圖：和畫面放在同一個時，FFmpeg 7 以後要等聲音進來才處理畫面，
    // 而聲音管線要看到第一張畫面才開始送（對齊時間），兩邊互等、一張畫面都錄不到
    // 麥克風降噪也一樣放在聲音自己的濾鏡圖
    let audio_graph = audio_input.and_then(|_| record_audio_graph(spec.input_count, config));
    a.extend(["-filter_complex".into(), spec.graph]);
    if let Some((g, _)) = &audio_graph {
        a.extend(["-filter_complex".into(), g.clone()]);
    }
    a.extend(strs(&["-map", "[vout]"]));
    if let Some((_, outs)) = &audio_graph {
        for o in outs {
            a.extend(["-map".into(), o.to_string()]);
        }
        a.extend(strs(&AUDIO_ENCODE));
        if outs.len() == 3 {
            a.extend(track_tags());
        }
        a.extend(strs(&["-max_muxing_queue_size", "4096"]));
    } else if audio_input.is_some() {
        a.extend(["-map".into(), format!("{}:a", spec.input_count)]);
        a.extend(strs(&AUDIO_ENCODE));
        a.extend(strs(&["-max_muxing_queue_size", "4096"]));
    }
    a.extend(enc.live());
    // 只錄聲音每秒只有幾張：x264 平常會先收一批畫面才輸出（預讀 + 多執行緒），
    // 每秒 5 張時要好幾秒才寫出第一段，當掉就全沒了。卡片不會動，不預讀也不會變大
    if config.audio_only && enc.name == "libx264" {
        a.extend(strs(&["-tune", "zerolatency"]));
    }
    a.extend(["-r".into(), fps.clone(), "-fps_mode".into(), "cfr".into(), "-g".into(), num(config.fps * 2.0)]);
    a.extend(strs(&COLOR_TAGS));
    if config.replay_sec > 0 {
        // 重播緩衝：每 REPLAY_CHUNK_SEC 秒換一個檔案（out_file 是含 %05d 的檔名樣式；新檔案從關鍵畫面開始），
        // 太舊的由錄影器刪掉。每個檔案一樣每秒寫入一次，存下來時連正在寫的那個也讀得到
        a.extend(strs(&["-f", "segment", "-segment_time", &REPLAY_CHUNK_SEC.to_string(), "-reset_timestamps", "1", "-segment_format", "mp4"]));
        a.extend(strs(&["-segment_format_options", "movflags=+empty_moov+default_base_moof:frag_duration=1000000:flush_packets=1", "-flush_packets", "1"]));
    } else {
        // 分段寫成每秒一個 fragment：即使 FFmpeg 被強制結束或當機，只損失最後 1 秒與編碼器還沒送出的畫面
        a.extend(strs(&["-movflags", "+empty_moov+default_base_moof", "-frag_duration", "1000000", "-flush_packets", "1"]));
    }
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

/// 只留下聲音（.m4a）：直接複製 AAC，不重新壓縮
pub fn audio_file_args(src: &str, out_file: &str) -> Vec<String> {
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(src.into());
    a.extend(strs(&["-map", "0:a:0", "-vn", "-c:a", "copy", "-movflags", "+faststart", "-y"]));
    a.push(out_file.into());
    a
}

/// chapters：章節檔（FFMETADATA，錄影時加的標記）
pub fn concat_args(list_file: &str, out_file: &str, chapters: Option<&str>) -> Vec<String> {
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"]);
    a.push(list_file.into());
    a.extend(strs(&["-map", "0", "-c", "copy", "-movflags", "+faststart", "-y"]));
    a.push(out_file.into());
    if let Some(c) = chapters {
        add_chapters(&mut a, c);
    }
    a
}

/// 輸出用這個章節檔（FFMETADATA）的章節：放在最後一個輸入之後（前面的輸入編號不變）。
/// args 的最後一個值是輸出檔
pub fn add_chapters(args: &mut Vec<String>, meta_file: &str) {
    let inputs = args.iter().filter(|a| *a == "-i").count();
    let Some(last) = args.iter().rposition(|a| a == "-i") else { return };
    args.splice(last + 2..last + 2, ["-i".to_string(), meta_file.to_string()]);
    let out = args.pop().unwrap_or_default();
    args.extend(["-map_chapters".into(), inputs.to_string()]);
    args.push(out);
}

/// 不要沿用來源的章節（FFmpeg 預設會照抄第一個有章節的輸入，時間會對不上）
pub fn drop_chapters(args: &mut Vec<String>) {
    let out = args.pop().unwrap_or_default();
    args.extend(strs(&["-map_chapters", "-1"]));
    args.push(out);
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

/// 壓縮到 max_bytes 以內的影片位元率（kbps）：扣掉聲音與 3% 的檔案結構；太小時以 80 kbps 為下限
pub fn size_bitrate(max_bytes: f64, seconds: f64, audio_kbps: u32) -> u32 {
    if seconds <= 0.0 || max_bytes <= 0.0 {
        return 80;
    }
    let total = max_bytes * 8.0 * 0.97 / seconds / 1000.0;
    ((total - audio_kbps as f64).floor() as i64).max(80) as u32
}

/// 壓縮到指定大小時聲音用的位元率
pub const SMALL_AUDIO_KBPS: u32 = 96;

/// 合併多支影片：每支縮放到 w × h（等比，不足補黑邊）、統一張數；沒有聲音的補一段靜音，再用 concat 接起來
pub fn merge_args(inputs: &[(String, bool, f64)], out_file: &str, w: i32, h: i32, fps: f64, enc: &EncoderSpec) -> Vec<String> {
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error"]);
    for (p, _, _) in inputs {
        a.extend(["-i".into(), p.clone()]);
    }
    let mut graph = vec![];
    let mut pairs = String::new();
    for (i, (_, audio, dur)) in inputs.iter().enumerate() {
        graph.push(format!("[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black,setsar=1,fps={},format={}[v{i}]", num(fps), enc.pix_fmt));
        if *audio {
            graph.push(format!("[{i}:a]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo[a{i}]"));
        } else {
            graph.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={},aformat=sample_fmts=fltp[a{i}]", num(dur.max(0.04))));
        }
        pairs.push_str(&format!("[v{i}][a{i}]"));
    }
    graph.push(format!("{pairs}concat=n={}:v=1:a=1[vout][aout]", inputs.len()));
    a.extend(["-filter_complex".into(), graph.join(";"), "-map".into(), "[vout]".into(), "-map".into(), "[aout]".into()]);
    a.extend(strs(&AUDIO_ENCODE));
    a.extend(enc.offline());
    a.extend(["-r".into(), num(fps)]);
    a.extend(strs(&COLOR_TAGS));
    a.extend(strs(&["-movflags", "+faststart", "-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    a
}

/// 製作加速版：setpts 壓縮時間軸，fps 維持原本的影格率（多出來的影格直接捨棄，不做混合，文字才不會有殘影）；
/// 聲音以 atempo 變速不變調。width：縮小到這個寬度（高度等比、取偶數）；不小於原寬時維持原尺寸。
/// limit_kbps：壓縮到指定大小（影片位元率）；這時可以是原速（1×，只壓縮）
#[allow(clippy::too_many_arguments)]
pub fn export_args(
    source: &str,
    out_file: &str,
    speed: f64,
    fps: f64,
    enc: &EncoderSpec,
    with_audio: bool,
    width: Option<i32>,
    src_width: Option<i32>,
    limit_kbps: Option<u32>,
) -> Result<Vec<String>> {
    let min = if limit_kbps.is_some() { 1.0 } else { SPEED_MIN };
    if !(min..=SPEED_MAX).contains(&speed) {
        return Err(Error::config(trf!("倍率需介於 {}～{}", "Speed must be between {} and {}", num(SPEED_MIN), num(SPEED_MAX))));
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
        if speed > 1.0 {
            a.push("-af".into());
            a.push(atempo_chain(speed));
        }
        match limit_kbps {
            Some(_) => a.extend(["-c:a".into(), "aac".into(), "-b:a".into(), format!("{SMALL_AUDIO_KBPS}k")]),
            None => a.extend(strs(&AUDIO_ENCODE)),
        }
    }
    match limit_kbps {
        Some(k) => a.extend(enc.bitrate(k)),
        None => a.extend(enc.offline()),
    }
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
    /// 來回播放：播到底再倒著播回來（循環時不會跳一下；長度變兩倍）
    pub bounce: bool,
}

/// 來回播放最長幾秒（單程）：倒著播要先把整段畫面放在記憶體
pub const BOUNCE_MAX_SEC: f64 = 30.0;

/// 來回播放：接上倒過來的畫面（去掉重複的轉折那一張）
fn bounce_chain(on: bool) -> &'static str {
    if on {
        ",split[bf][br];[br]reverse,trim=start_frame=1,setpts=PTS-STARTPTS[brv];[bf][brv]concat=n=2:v=1:a=0"
    } else {
        ""
    }
}

/// 整段共用一個調色盤時，FFmpeg 要先暫存所有畫面；超過這個量改用每張畫面各自的調色盤
const GIF_GLOBAL_PALETTE_MAX_BYTES: f64 = 400.0 * 1048576.0;

/// GIF（無聲音）：短片整段共用調色盤＋只更新變動區域；長片每張畫面各自的調色盤，記憶體固定
pub fn gif_args(source: &str, out_file: &str, speed: f64, o: &GifOptions) -> Result<Vec<String>> {
    if !(1.0..=SPEED_MAX).contains(&speed) {
        return Err(Error::config(trf!("倍率需介於 1～{}", "Speed must be between 1 and {}", num(SPEED_MAX))));
    }
    let width = o.width.min(if o.src_width > 0.0 { o.src_width } else { o.width }).max(2.0);
    let height = if o.src_width > 0.0 && o.src_height > 0.0 { crate::format::js_round(o.src_height * width / o.src_width) } else { width * 9.0 / 16.0 };
    let frames = (o.output_sec * o.fps * if o.bounce { 2.0 } else { 1.0 }).ceil();
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
    a.push(format!("{time}fps={},scale={}:-2:flags=lanczos{},split[a][b];[a]{palette}", num(o.fps), num(width), bounce_chain(o.bounce)));
    a.extend(strs(&["-loop", "0", "-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

/// WebP 動圖（libwebp_anim）：與 GIF 相同的寬度與張數，有損壓縮（品質 80）；畫面一樣時檔案約為 GIF 的 1/6，也沒有 256 色的色帶
pub fn webp_args(source: &str, out_file: &str, speed: f64, o: &GifOptions) -> Result<Vec<String>> {
    if !(1.0..=SPEED_MAX).contains(&speed) {
        return Err(Error::config(trf!("倍率需介於 1～{}", "Speed must be between 1 and {}", num(SPEED_MAX))));
    }
    let width = o.width.min(if o.src_width > 0.0 { o.src_width } else { o.width }).max(2.0);
    let time = if speed > 1.0 { format!("setpts=PTS/{},", num(speed)) } else { String::new() };
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(source.into());
    a.extend(strs(&["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"]));
    a.push(format!("{time}fps={},scale={}:-2:flags=lanczos{}", num(o.fps), num(width), bounce_chain(o.bounce)));
    a.extend(strs(&["-c:v", "libwebp_anim", "-lossless", "0", "-quality", "80", "-compression_level", "4", "-loop", "0"]));
    a.extend(strs(&["-progress", "pipe:1", "-stats_period", "0.5", "-y"]));
    a.push(out_file.into());
    Ok(a)
}

/// 剪輯：只保留 keep 區段（select / aselect 精確到每張畫面），可再裁切畫面範圍；必須重新編碼。
/// 剪輯時加上的標註（已換算成影片像素、檔案已寫好）
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayInput {
    /// 透明 PNG：疊在 (x, y)；anim：0 直接出現、1 淡入淡出、2 放大出現（size = 圖的大小，放大時以中心為準）
    Image { path: String, x: i32, y: i32, start: f64, end: f64, anim: u8, size: (i32, i32) },
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

/// 背景與圓角（video_frame.rs 畫好的兩張圖）：影片縮成 w × h 疊在底圖的 (x, y)，再蓋上遮罩
#[derive(Debug, Clone, PartialEq)]
pub struct FrameInput {
    pub base: String,
    pub cover: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 平滑、放大的游標：游標圖與移動它的 sendcmd 指令檔（檔名相對於 FFmpeg 的工作資料夾，見 exporter.rs），一開始的位置
#[derive(Debug, Clone, PartialEq)]
pub struct CursorInput {
    pub image: String,
    pub commands: String,
    pub x: i32,
    pub y: i32,
}

/// 直式影片（見 vertical.rs）：放在中間（輸出 w × h），或跟著游標裁切
/// （sendcmd 指令檔的檔名、裁切大小、一開始的位置、太大時縮小成多大）
#[derive(Debug, Clone, PartialEq)]
pub enum VerticalInput {
    Fit { w: i32, h: i32 },
    Follow { commands: String, w: i32, h: i32, x: i32, y: i32, scale: Option<(i32, i32)> },
}

/// 片頭、片尾卡片與背景音樂（cards.rs）：卡片圖檔與秒數、輸出影片的大小（卡片放大到一樣大才能接起來）、
/// 剪好的影片長度（沒有聲音時補靜音）、背景音樂
#[derive(Debug, Clone, PartialEq)]
pub struct Extras {
    pub intro: Option<(String, f64)>,
    pub outro: Option<(String, f64)>,
    pub size: (i32, i32),
    pub main_len: f64,
    pub music: Option<MusicInput>,
    /// 旁白與點擊音效（剪輯後的時間，片頭之前）
    pub voice: Option<VoiceInput>,
}

/// 旁白：WAV 檔與開始時間（剪輯後的秒數）、旁白音量、原本的聲音在旁白時的音量濾鏡；點擊音效的聲音檔（從 0 開始）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VoiceInput {
    pub clips: Vec<(String, f64)>,
    pub volume: f64,
    pub duck: Option<String>,
    pub clicks: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MusicInput {
    pub path: String,
    pub volume: f64,
    pub duck: bool,
}

impl Extras {
    /// 加上片頭片尾後的總長度
    pub fn total(&self) -> f64 {
        self.main_len + self.intro.as_ref().map_or(0.0, |c| c.1) + self.outro.as_ref().map_or(0.0, |c| c.1)
    }
}

/// 聲音統一成 48 kHz 立體聲，才能接起來、混在一起
const AFMT: &str = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";

/// 片頭片尾、背景音樂的輸入與濾鏡：接在 [vout]（與有聲音時的 [aout]）後面，輸出 [vfin]（與 [afin]）。
/// first：第一個輸入的編號；回傳 (輸入參數, 濾鏡, 有沒有聲音)
fn extras_graph(x: &Extras, first: usize, fps: f64, pix_fmt: &str, with_audio: bool) -> (Vec<String>, String, bool) {
    let mut inputs = vec![];
    let mut g = vec![];
    let mut idx = first;
    let has_voice = x.voice.as_ref().is_some_and(|v| !v.clips.is_empty() || v.clicks.is_some());
    let out_audio = with_audio || x.music.is_some() || has_voice;
    let (w, h) = x.size;
    let mut card = |png: &str, secs: f64, name: &str, inputs: &mut Vec<String>, g: &mut Vec<String>| {
        inputs.extend(["-loop".into(), "1".into(), "-framerate".into(), num(fps), "-t".into(), num(secs), "-i".into(), png.to_string()]);
        let fade_out = (secs - 0.4).max(0.0);
        g.push(format!("[{idx}:v]scale={w}:{h}:flags=bicubic,setsar=1,format={pix_fmt},fade=t=in:st=0:d=0.3,fade=t=out:st={}:d=0.4[v{name}]", num(fade_out)));
        if out_audio {
            g.push(format!("anullsrc=r=48000:cl=stereo:d={}[a{name}]", num(secs)));
        }
        idx += 1;
    };
    let mut segs: Vec<&str> = vec![];
    if let Some((png, secs)) = &x.intro {
        card(png, *secs, "in", &mut inputs, &mut g);
        segs.push("in");
    }
    g.push(format!("[vout]setsar=1,format={pix_fmt}[vmain]"));
    let main = if has_voice { "[amainx]" } else { "[amain]" };
    if with_audio {
        g.push(format!("[aout]{AFMT}{main}"));
    } else if out_audio {
        g.push(format!("anullsrc=r=48000:cl=stereo:d={}{main}", num(x.main_len)));
    }
    segs.push("main");
    if let Some((png, secs)) = &x.outro {
        card(png, *secs, "out", &mut inputs, &mut g);
        segs.push("out");
    }
    // 旁白、點擊音效：混進剪好的聲音（旁白時原本的聲音調小），長度不變
    if let Some(v) = x.voice.as_ref().filter(|_| has_voice) {
        let mut mix = vec![];
        let duck = v.duck.as_deref().filter(|_| with_audio).map(|d| format!(",{d}")).unwrap_or_default();
        g.push(format!("{main}anull{duck}[avbase]"));
        mix.push("[avbase]".to_string());
        for (i, (file, at)) in v.clips.iter().enumerate() {
            inputs.extend(["-i".into(), file.clone()]);
            let ms = (at * 1000.0).round().max(0.0) as u64;
            g.push(format!("[{idx}:a]{AFMT},volume={},adelay={ms}:all=1[vo{i}]", num(v.volume)));
            mix.push(format!("[vo{i}]"));
            idx += 1;
        }
        if let Some(c) = &v.clicks {
            inputs.extend(["-i".into(), c.clone()]);
            g.push(format!("[{idx}:a]{AFMT}[vclk]"));
            mix.push("[vclk]".into());
            idx += 1;
        }
        g.push(format!("{}amix=inputs={}:duration=first:normalize=0,atrim=0:{}[amain]", mix.join(""), mix.len(), num(x.main_len)));
    }
    let alabel = if x.music.is_some() { "[amixin]" } else { "[afin]" };
    if segs.len() > 1 {
        let pairs: String = segs.iter().map(|s| if out_audio { format!("[v{s}][a{s}]") } else { format!("[v{s}]") }).collect();
        if out_audio {
            g.push(format!("{pairs}concat=n={}:v=1:a=1[vfin]{alabel}", segs.len()));
        } else {
            g.push(format!("{pairs}concat=n={}:v=1:a=0[vfin]", segs.len()));
        }
    } else {
        g.push("[vmain]null[vfin]".into());
        if out_audio {
            g.push(format!("[amain]anull{alabel}"));
        }
    }
    // 背景音樂：重複播放到影片結束，最後 2 秒淡出；講話時（原本的聲音）自動調小
    if let Some(m) = &x.music {
        inputs.extend(["-stream_loop".into(), "-1".into(), "-i".into(), m.path.clone()]);
        let total = x.total();
        let fade = (total - 2.0).max(0.0);
        g.push(format!("[{idx}:a]{AFMT},volume={},atrim=0:{},afade=t=out:st={}:d=2[mus]", num(m.volume.clamp(0.0, 1.0)), num(total), num(fade)));
        if m.duck && with_audio {
            g.push("[amixin]asplit=2[aspk][akey];[mus][akey]sidechaincompress=threshold=0.02:ratio=10:attack=30:release=500[mduck];[aspk][mduck]amix=inputs=2:duration=first:normalize=0[afin]".into());
        } else {
            g.push("[amixin][mus]amix=inputs=2:duration=first:normalize=0[afin]".into());
        }
    }
    (inputs, g.join(";"), out_audio)
}

/// 要不要用時間對應：有定格、放慢，或加速不是整數倍
fn needs_retime(keep: &[Part]) -> bool {
    keep.iter().any(|p| p.is_hold() || p.rate < 1.0 || p.rate.fract().abs() > 1e-9 || (p.keep_audio && p.retimed()))
}

/// 聲音變快、音調不變：atempo 每個最多 2 倍，接起來
fn atempo(rate: f64) -> String {
    let mut r = rate.clamp(0.5, 100.0);
    let mut v = vec![];
    while r > 2.0 + 1e-9 {
        v.push("atempo=2".to_string());
        r /= 2.0;
    }
    v.push(format!("atempo={}", num((r * 10000.0).round() / 10000.0)));
    v.join(",")
}

/// 時間對應的畫面濾鏡：挑出保留的畫面 → 每張換成剪輯後的時間 → 固定張數（放慢、定格時重複前一張，加速時丟掉）；
/// 開頭的定格由 fps 的 start_time=0 補上，結尾的用 tpad 延長最後一張
fn retime_video(keep: &[Part], expr: &str, fps: f64) -> Vec<String> {
    let mut off = 0.0;
    let mut map = vec![];
    for p in keep {
        if !p.is_hold() {
            map.push(format!("gte(T,{})*lt(T,{})*({}+(T-{})/{})", num(p.a), num(p.b), num(off), num(p.a), num(p.rate)));
        }
        off += p.len();
    }
    let mut vf = vec![format!("select='{expr}'"), format!("setpts='({})/TB'", map.join("+")), format!("fps={}:start_time=0", num(fps))];
    let tail: f64 = keep.iter().rev().take_while(|p| p.is_hold()).map(Part::len).sum();
    if tail > 0.0 {
        vf.push(format!("tpad=stop_mode=clone:stop_duration={}", num(tail)));
    }
    // 長度剛好是剪好的長度
    vf.push(format!("trim=end={}", num(off)));
    vf
}

/// 片段改了順序：每一段各自剪下來（變速時改時間、定格時重複那一張）再照順序接起來 → [seg]
fn segment_graph(input: &str, keep: &[Part], fps: f64) -> String {
    let n = keep.len();
    let labels: Vec<String> = (0..n).map(|i| format!("[sg{i}]")).collect();
    let mut g = vec![if n == 1 { format!("{input}null{}", labels[0]) } else { format!("{input}split={n}{}", labels.join("")) }];
    for (i, p) in keep.iter().enumerate() {
        let chain = if p.is_hold() {
            // 定格：只留那一張，延長成 hold 秒
            format!("trim=start={}:duration={},setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop_duration={},trim=duration={}", num(p.a), num(1.0 / fps), num(p.hold), num(p.hold))
        } else {
            format!("trim={}:{},setpts=(PTS-STARTPTS)/{}", num(p.a), num(p.b), num(p.rate))
        };
        g.push(format!("{}{chain},fps={}[sc{i}]", labels[i], num(fps)));
    }
    let segs: String = (0..n).map(|i| format!("[sc{i}]")).collect();
    g.push(format!("{segs}concat=n={n}:v=1:a=0[seg]"));
    g.join(";")
}

/// 時間對應的聲音：原速的片段用原本的聲音，變速、定格的片段換成同樣長度的靜音，再接起來 → [acut]
fn retime_audio(input: &str, keep: &[Part]) -> String {
    let normal = keep.iter().filter(|p| !p.silent()).count();
    let mut g = vec![];
    let labels: Vec<String> = (0..normal).map(|i| format!("[an{i}]")).collect();
    match normal {
        0 => g.push(format!("{input}anullsink")),
        1 => g.push(format!("{input}anull{}", labels[0])),
        n => g.push(format!("{input}asplit={n}{}", labels.join(""))),
    }
    let mut segs = vec![];
    let mut k = 0;
    for (i, p) in keep.iter().enumerate() {
        if p.silent() {
            g.push(format!("anullsrc=r=48000:cl=stereo,atrim=end={}[as{i}]", num(p.len())));
        } else {
            // 整支加速保留聲音：變快、音調不變，長度剛好是這一段
            let tempo = if p.retimed() { format!(",{},atrim=end={}", atempo(p.rate), num(p.len())) } else { String::new() };
            g.push(format!("{}atrim={}:{},asetpts=PTS-STARTPTS{tempo},{AFMT}[as{i}]", labels[k], num(p.a), num(p.b)));
            k += 1;
        }
        segs.push(format!("[as{i}]"));
    }
    g.push(format!("{}concat=n={}:v=0:a=1[acut]", segs.join(""), segs.len()));
    g.join(";")
}

fn enable(start: f64, end: f64) -> String {
    format!("enable='between(t,{},{})'", num(start), num(end))
}

/// 剪輯的其他畫面處理：調色濾鏡（接在裁切後面）、日期時間浮水印的 drawtext（在原影片的時間畫上去）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CutMore {
    pub look: Option<String>,
    pub stamp: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn cut_args(
    source: &str,
    out_file: &str,
    keep: &[Part],
    crop: Option<Rect>,
    fps: f64,
    enc: &EncoderSpec,
    with_audio: bool,
    overlays: &[OverlayInput],
    audio: AudioFx,
    zoom: Option<&str>,
    frame: Option<&FrameInput>,
    cursor: Option<&CursorInput>,
    vertical: Option<&VerticalInput>,
    extras: Option<&Extras>,
    fade_cuts: bool,
) -> Result<Vec<String>> {
    cut_args_more(source, out_file, keep, crop, fps, enc, with_audio, overlays, audio, zoom, frame, cursor, vertical, extras, fade_cuts, &CutMore::default())
}

#[allow(clippy::too_many_arguments)]
pub fn cut_args_more(
    source: &str,
    out_file: &str,
    keep: &[Part],
    crop: Option<Rect>,
    fps: f64,
    enc: &EncoderSpec,
    with_audio: bool,
    overlays: &[OverlayInput],
    audio: AudioFx,
    zoom: Option<&str>,
    frame: Option<&FrameInput>,
    cursor: Option<&CursorInput>,
    vertical: Option<&VerticalInput>,
    extras: Option<&Extras>,
    fade_cuts: bool,
    more: &CutMore,
) -> Result<Vec<String>> {
    // 選了「不要聲音」
    let with_audio = with_audio && !audio.mute;
    if keep.is_empty() {
        return Err(Error::config(tr!("剪輯後沒有留下任何片段", "Nothing is left after editing")));
    }
    // 片段的順序改了：每一段分開剪下來再接起來（select 不能改變前後順序）
    let segments = crate::edit::reordered(keep);
    // 有放慢、定格（或不是整數倍的加速）時改用時間對應：挑出保留的畫面、換成剪輯後的時間，再補齊（重複）或丟掉畫面
    let retime = segments || needs_retime(keep);
    // 單引號內的逗號不會被當成濾鏡分隔；加速的片段每 N 張（聲音每 N 個音框）留一個
    let expr = keep
        .iter()
        .filter(|p| !p.is_hold())
        .map(|p| if p.rate > 1.0 && !retime { format!("gte(t,{})*lt(t,{})*not(mod(n,{}))", num(p.a), num(p.b), p.rate as u32) } else { format!("gte(t,{})*lt(t,{})", num(p.a), num(p.b)) })
        .collect::<Vec<_>>()
        .join("+");
    let mut vf = if segments {
        // 片段已經在 segment_graph 裡接好（[seg]），這裡只固定張數
        vec![format!("fps={}:start_time=0", num(fps))]
    } else if retime {
        retime_video(keep, &expr, fps)
    } else {
        vec![format!("select='{expr}'"), format!("setpts=N/({}*TB)", num(fps))]
    };
    // 跟著點擊放大（剪輯後的時間）
    if let Some(z) = zoom {
        vf.push(z.to_string());
    }
    if let Some(c) = crop {
        vf.push(format!("crop={}:{}:{}:{}", c.width, c.height, c.x, c.y));
    }
    // 要先在原影片的時間處理的：標註、游標、跟著游標的直式裁切、日期時間（片段改了順序時也走這條，才接得上 segment_graph）
    let follow = matches!(vertical, Some(VerticalInput::Follow { .. }));
    let pre = !overlays.is_empty() || cursor.is_some() || follow || segments || more.stamp.is_some();
    // 調色：有標註時在最前面先調（標註、游標、浮水印的顏色不受影響），否則接在裁切後面
    if let Some(l) = more.look.as_ref().filter(|_| !pre) {
        vf.push(l.clone());
    }
    // 直式影片（放在中間）：剪好的畫面放在直式畫面中間，上下是放大、模糊的同一個畫面
    let fit = matches!(vertical, Some(VerticalInput::Fit { .. }));
    if let Some(VerticalInput::Fit { w, h }) = vertical {
        vf.push(crate::vertical::fit_filter(*w, *h));
    }
    // 濾鏡有多個輸入（不能用 -vf）
    let complex = frame.is_some() || fit || extras.is_some();
    let mut a = strs(&["-hide_banner", "-nostats", "-loglevel", "error", "-i"]);
    a.push(source.into());
    // 背景與圓角：底圖與遮罩是一直重複的圖片（跟著影片結束）
    let mut next_input = 1;
    if let Some(f) = frame {
        for img in [&f.base, &f.cover] {
            a.extend(["-loop".into(), "1".into(), "-framerate".into(), num(fps), "-i".into(), img.clone()]);
        }
        vf.push(format!("scale={}:{}:flags=bicubic[fv];[1:v][fv]overlay={}:{}:shortest=1[fb];[fb][2:v]overlay=0:0:shortest=1", f.w, f.h, f.x, f.y));
        next_input = 3;
    }
    // 接縫淡入淡出：在剪好的時間，每個刪掉片段的接縫前淡出、後淡入（畫面變黑再亮起來）
    let fades = if fade_cuts { crate::edit::cut_fades(keep) } else { vec![] };
    for &(t, d) in &fades {
        vf.push(format!("fade=t=out:st={}:d={}:enable='between(t,{},{})'", num(t - d), num(d), num(t - d), num(t)));
        vf.push(format!("fade=t=in:st={}:d={}:enable='between(t,{},{})'", num(t), num(d), num(t), num(t + d)));
    }
    vf.push(format!("format={}", enc.pix_fmt));
    // 聲音：加速的片段沒有聲音（快轉的聲音聽不清楚），只留保留的片段，再做聲音處理
    let af: Vec<String> = if with_audio {
        let mut af: Vec<String> = vec![];
        if !retime {
            af.extend(keep.iter().filter(|p| p.rate > 1.0).map(|p| format!("volume=0:enable='between(t,{},{})'", num(p.a), num(p.b))));
            af.push(format!("aselect='{expr}',asetpts=N/SR/TB"));
        }
        for &(t, d) in &fades {
            af.push(format!("afade=t=out:st={}:d={}:enable='between(t,{},{})'", num(t - d), num(d), num(t - d), num(t)));
            af.push(format!("afade=t=in:st={}:d={}:enable='between(t,{},{})'", num(t), num(d), num(t), num(t + d)));
        }
        af.extend(audio.filters().into_iter().map(String::from));
        af
    } else {
        vec![]
    };
    let af_chain = if af.is_empty() { "anull".to_string() } else { af.join(",") };
    // 分軌錄影：系統聲音與麥克風分別調整後重新混合，接著同樣處理（放進 -filter_complex）
    let mut track_graph =
        audio.track_graph().filter(|_| with_audio).map(|g| if retime { format!("{g};{};[acut]{af_chain}[aout]", retime_audio("[amixed]", keep)) } else { format!("{g};[amixed]{af_chain}[aout]") });
    // 時間對應：聲音一段一段接起來（變速、定格的地方是靜音）
    if retime && with_audio && track_graph.is_none() {
        track_graph = Some(format!("{};[acut]{af_chain}[aout]", retime_audio("[0:a:0]", keep)));
    }
    // 片頭片尾、背景音樂：聲音也要在 -filter_complex 裡（[aout]），才能接起來、混音
    if extras.is_some() && with_audio && track_graph.is_none() {
        track_graph = Some(format!("[0:a:0]{af_chain}[aout]"));
    }
    if !pre && !complex {
        a.extend(strs(&["-map", "0:v:0"]));
        if let Some(g) = &track_graph {
            a.extend(["-filter_complex".into(), g.clone()]);
        }
    } else if !pre {
        let mut g = format!("[0:v]{}[vout]", vf.join(","));
        if let Some(t) = &track_graph {
            g = format!("{g};{t}");
        }
        a.extend(["-filter_complex".into(), g, "-map".into(), "[vout]".into()]);
    } else {
        // 標註以原影片的時間顯示，所以先疊上標註，再挑選保留的片段
        let mut graph = Vec::new();
        let mut cur = "[0:v]".to_string();
        if let Some(l) = &more.look {
            graph.push(format!("{cur}{l}[look]"));
            cur = "[look]".into();
        }
        // 游標在最底下（標註、馬賽克蓋在它上面）：sendcmd 依原影片的時間移動 overlay@cursor
        if let Some(c) = cursor {
            a.extend(["-i".into(), c.image.clone()]);
            graph.push(format!("{cur}sendcmd=f={}[cs];[cs][{next_input}:v]overlay@cursor=x={}:y={}[cur0]", c.commands, c.x, c.y));
            next_input += 1;
            cur = "[cur0]".into();
        }
        for (i, o) in overlays.iter().enumerate() {
            let out = format!("[o{i}]");
            match o {
                OverlayInput::Image { path, x, y, start, end, anim: 0, .. } => {
                    a.extend(["-i".into(), path.clone()]);
                    graph.push(format!("{cur}[{next_input}:v]overlay={x}:{y}:{}{out}", enable(*start, *end)));
                    next_input += 1;
                }
                OverlayInput::Image { path, x, y, start, end, anim, size } => {
                    // 動畫：圖片重複成影片（時間和原影片一樣從 0 開始），淡入淡出改透明度，放大出現再由一半大小變回原本
                    a.extend(["-loop".into(), "1".into(), "-framerate".into(), num(fps), "-t".into(), num(end + 0.1), "-i".into(), path.clone()]);
                    let d = crate::annotate::anim_secs(*start, *end);
                    let fade = format!("fade=t=in:st={}:d={}:alpha=1,fade=t=out:st={}:d={}:alpha=1", num(*start), num(d), num(end - d), num(d));
                    if *anim == 2 {
                        let k = format!("if(lt(t,{s}),0.5,min(1,0.5+0.5*(t-{s})/{d}))", s = num(*start), d = num(d));
                        let (cx, cy) = (*x as f64 + size.0 as f64 / 2.0, *y as f64 + size.1 as f64 / 2.0);
                        graph.push(format!(
                            "[{next_input}:v]format=rgba,scale=w='max(2,trunc(iw*{k}))':h='max(2,trunc(ih*{k}))':eval=frame,{fade}[a{i}];{cur}[a{i}]overlay=x='{}-overlay_w/2':y='{}-overlay_h/2':eval=frame:{}{out}",
                            num(cx),
                            num(cy),
                            enable(*start, *end)
                        ));
                    } else {
                        graph.push(format!("[{next_input}:v]format=rgba,{fade}[a{i}];{cur}[a{i}]overlay={x}:{y}:{}{out}", enable(*start, *end)));
                    }
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
        // 日期時間浮水印：在原影片的時間畫上去（顯示的是錄影當時的時間）
        if let Some(st) = &more.stamp {
            graph.push(format!("{cur}{st}[stamp]"));
            cur = "[stamp]".into();
        }
        // 跟著游標的直式裁切：在標註之後（標註跟著畫面一起裁），sendcmd 依原影片的時間移動 crop@vertical
        if let Some(VerticalInput::Follow { commands, w, h, x, y, scale }) = vertical {
            let sc = scale.map(|(sw, sh)| format!(",scale={sw}:{sh}:flags=bicubic")).unwrap_or_default();
            graph.push(format!("{cur}sendcmd=f={commands},crop@vertical=w={w}:h={h}:x={x}:y={y}{sc}[vert]"));
            cur = "[vert]".into();
        }
        if segments {
            graph.push(segment_graph(&cur, keep, fps));
            cur = "[seg]".into();
        }
        graph.push(format!("{cur}{}[vout]", vf.join(",")));
        if let Some(t) = &track_graph {
            graph.push(t.clone());
        }
        a.extend(["-filter_complex".into(), graph.join(";"), "-map".into(), "[vout]".into()]);
    }
    a.extend(match (with_audio, &track_graph) {
        (true, Some(_)) => strs(&["-map", "[aout]"]),
        (true, None) => strs(&["-map", "0:a:0"]),
        _ => strs(&["-an"]),
    });
    // 片頭片尾、背景音樂：輸入放在其他輸入後面，濾鏡接在 [vout] / [aout] 後面，改輸出 [vfin] / [afin]
    let mut out_audio = with_audio;
    if let Some(x) = extras {
        let (inputs, g, has_audio) = extras_graph(x, next_input, fps, enc.pix_fmt, with_audio);
        out_audio = has_audio;
        let at = a.iter().position(|s| s == "-filter_complex").unwrap_or(a.len());
        a.splice(at..at, inputs);
        if let Some(i) = a.iter().position(|s| s == "-filter_complex") {
            a[i + 1] = format!("{};{g}", a[i + 1]);
        }
        for s in a.iter_mut() {
            if s == "[vout]" {
                *s = "[vfin]".into();
            } else if s == "[aout]" {
                *s = "[afin]".into();
            }
        }
        if !with_audio && has_audio {
            if let Some(i) = a.iter().position(|s| s == "-an") {
                a.splice(i..i + 1, strs(&["-map", "[afin]"]));
            }
        }
    }
    a.extend(strs(&["-sn", "-dn"]));
    if !pre && !complex {
        a.push("-vf".into());
        a.push(vf.join(","));
    }
    if with_audio && track_graph.is_none() {
        a.push("-af".into());
        a.push(af.join(","));
    }
    if out_audio {
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
    pub chapters: Vec<crate::types::Chapter>,
    /// 幾條音軌
    pub audio_tracks: u32,
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
    ParsedMedia {
        duration_sec,
        width: size.as_ref().and_then(|s| s[1].parse().ok()),
        height: size.as_ref().and_then(|s| s[2].parse().ok()),
        fps,
        has_audio: AUDIO_RE.is_match(stderr),
        chapters: crate::chapters::parse(stderr),
        audio_tracks: AUDIO_RE.find_iter(stderr).count() as u32,
    }
}

/// 從第 t 秒開始讀 path：`-ss t -i path`（快速跳到附近的關鍵畫面）。
/// WebP 動圖不能這樣跳（什麼都讀不到），改成 `-i path -ss t`：從頭解碼、丟掉 t 秒以前的畫面（動圖都很短）
pub fn input_at(path: &str, t: &str) -> [String; 4] {
    if path.to_ascii_lowercase().ends_with(".webp") {
        ["-i".into(), path.into(), "-ss".into(), t.into()]
    } else {
        ["-ss".into(), t.into(), "-i".into(), path.into()]
    }
}

/// 量長度：有些格式（WebP 動圖）的檔頭沒有長度，只複製串流不解碼地讀一遍（很快）
pub fn measure_duration_args(path: &str) -> Vec<String> {
    let mut a = strs(&["-hide_banner", "-i"]);
    a.push(path.into());
    a.extend(strs(&["-map", "0:v:0", "-c", "copy", "-f", "null", "-"]));
    a
}

static TIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"time=\s*(\d+):(\d{2}):(\d{2}(?:\.\d+)?)").unwrap());

/// measure_duration_args 的 stderr：最後一個 time=
pub fn parse_measured_duration(stderr: &str) -> Option<f64> {
    let c = TIME_RE.captures_iter(stderr).last()?;
    let t = c[1].parse::<f64>().ok()? * 3600.0 + c[2].parse::<f64>().ok()? * 60.0 + c[3].parse::<f64>().ok()?;
    (t > 0.0).then_some(t)
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
/// skip_static：即時預覽在畫面沒有變化時不送出新畫面（ddagrab 支援 dup_frames 時）
pub fn preview_args(monitors: &[MonitorInfo], use_ddagrab: bool, max_width: u32, live_fps: Option<f64>, skip_static: bool) -> Vec<String> {
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
            let plan = CapturePlan { rect, dda: Some(dda), monitors: monitors.to_vec(), out_width: rect.width, out_height: rect.height, card: None };
            let mut a = strs(&["-hide_banner", "-loglevel", "error", "-init_hw_device"]);
            a.push(format!("d3d11va=dda:{adapter}"));
            a.extend(strs(&["-filter_hw_device", "dda", "-filter_complex"]));
            a.push(format!("{},{fit}[vout]", ddagrab_chain_opts(&plan, fps, true, skip_static && live_fps.is_some())));
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
            return Err(Error::config(tr!("此範圍涵蓋不同顯示卡上的螢幕，無法使用 ddagrab", "This area spans screens on different graphics cards, so ddagrab can't be used")));
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
    use crate::types::{AudioConfig, CameraConfig, MethodPreference};

    #[test]
    fn screenshot_full_resolution() {
        let mons = vec![mon("0:0", 0, 0, 1920, 1080, true), mon("0:1", 1920, 0, 1920, 1080, false)];
        let (_, dda) = plan_tiles(&Rect { x: 0, y: 0, width: 3840, height: 1080 }, &mons);
        let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: 3840, height: 1080 }, dda, monitors: mons.clone(), out_width: 1920, out_height: 540, card: None };
        let a = screenshot_args(&plan, true, false, "C:\\out\\Shot.png").unwrap().join(" ");
        assert!(a.contains("d3d11va=dda:0") && a.contains("xstack") && a.contains("draw_mouse=0") && a.contains("format=rgb24[vout]"), "{a}");
        assert!(!a.contains("scale="), "截圖不縮放：{a}");
        assert!(a.ends_with("-frames:v 3 -update 1 -y C:\\out\\Shot.png"), "{a}");
        let plan = CapturePlan { rect: Rect { x: 100, y: 50, width: 800, height: 600 }, dda: None, monitors: mons, out_width: 800, out_height: 600, card: None };
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
            key: String::new(),
            name: String::new(),
        }
    }
    // 同一張顯示卡：主螢幕 1920×1080，右邊接一台較高的 2560×1440（上緣比主螢幕高 180）
    fn same_gpu() -> Vec<MonitorInfo> {
        vec![mon("0:0", 0, 0, 1920, 1080, true), mon("0:1", 1920, -180, 2560, 1440, false)]
    }
    // 混合顯示卡：外接螢幕在另一張卡上
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
            shot_dir: String::new(),
            audio: AudioConfig::default(),
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
            window_capture: false,
            separate_audio: false,
            mic_denoise: false,
            quiet: false,
            masks: vec![],
            replay_sec: 0,
            monitor_key: None,
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
        assert!(segment_args(&p, &cfg(), CaptureMethod::Ddagrab, &x264(), "o.mp4", None, None).unwrap_err().is_config());
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
        let args = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "C:\\p\\seg.mp4", None, None).unwrap();
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
        let args = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, None).unwrap();
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
        let args = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, None).unwrap();
        assert!(graph_of(&args).contains("xstack=inputs=2:layout=0_180|920_0:fill=black,scale=1500:400"));
        assert!(!graph_of(&args).contains("pad="));
        let c2 = RecordConfig { source: region(1000.0, 900.0, 1500.0, 400.0), ..cfg() };
        let args2 = segment_args(&resolve_plan(&c2, &same_gpu()).unwrap(), &c2, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, None).unwrap();
        assert!(graph_of(&args2).contains("pad=1500:400:0:0:color=black"));
    }

    #[test]
    fn segment_gdigrab_absolute_coordinates() {
        let c = RecordConfig { source: SourceConfig::Monitor { monitor_id: "1:0".into() }, draw_mouse: false, scale: 25.0, ..cfg() };
        let args = segment_args(&resolve_plan(&c, &two_gpus()).unwrap(), &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", None, None).unwrap();
        let v: Vec<&str> = ["-offset_x", "-offset_y", "-video_size", "-draw_mouse", "-framerate"].iter().map(|k| after(&args, k)).collect();
        assert_eq!(v, vec!["1920", "-180", "2560x1440", "0", "30"]);
        assert_eq!(graph_of(&args), "[0:v]scale=640:360:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]");
    }

    #[test]
    fn segment_with_audio() {
        let audio_in = strs(&["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "tcp://127.0.0.1:5000"]);
        let c = cfg();
        let plan = resolve_plan(&c, &same_gpu()).unwrap();
        let dda = segment_args(&plan, &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
        assert_eq!(after(&dda, "-loglevel"), "level+info");
        assert!(graph_of(&dda).contains("format=bgra,showinfo=checksum=0,"));
        assert!(dda.join(" ").contains("-map [vout] -map 0:a -c:a aac"));
        let gdi = segment_args(&plan, &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
        assert!(gdi.join(" ").contains("-map [vout] -map 1:a"));
        assert!(graph_of(&gdi).starts_with("[0:v]showinfo=checksum=0,"));
    }

    #[test]
    fn cut_with_background_frame() {
        let f = FrameInput { base: "bg.png".into(), cover: "cover.png".into(), x: 134, y: 76, w: 1650, h: 928 };
        let a = cut_args("in.mp4", "out.mp4", &[Part::new(1.0, 5.0, 1.0)], None, 30.0, &x264(), true, &[], AudioFx::default(), None, Some(&f), None, None, None, false).unwrap();
        let j = a.join(" ");
        assert!(j.contains("-i in.mp4 -loop 1 -framerate 30 -i bg.png -loop 1 -framerate 30 -i cover.png"), "{j}");
        assert_eq!(
            graph_of(&a),
            "[0:v]select='gte(t,1)*lt(t,5)',setpts=N/(30*TB),scale=1650:928:flags=bicubic[fv];[1:v][fv]overlay=134:76:shortest=1[fb];[fb][2:v]overlay=0:0:shortest=1,format=yuv420p[vout]"
        );
        assert!(!j.contains(" -vf "));
        // 加上標註：標註的圖片接在底圖、遮罩之後
        let overlays = [OverlayInput::Image { path: "o.png".into(), x: 10, y: 20, start: 1.0, end: 2.0, anim: 0, size: (10, 10) }];
        let a = cut_args("in.mp4", "out.mp4", &[Part::new(1.0, 5.0, 1.0)], None, 30.0, &x264(), true, &overlays, AudioFx::default(), None, Some(&f), None, None, None, false).unwrap();
        let g = graph_of(&a);
        assert!(g.starts_with("[0:v][3:v]overlay=10:20:"), "{g}");
        assert!(g.ends_with("[fb][2:v]overlay=0:0:shortest=1,format=yuv420p[vout]"), "{g}");
    }

    #[test]
    fn audio_file() {
        assert_eq!(audio_file_args("a.mp4", "a.m4a").join(" "), "-hide_banner -nostats -loglevel error -i a.mp4 -map 0:a:0 -vn -c:a copy -movflags +faststart -y a.m4a");
    }

    #[test]
    fn audio_only_uses_the_card() {
        let audio_in = strs(&["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "tcp://127.0.0.1:5000"]);
        // 不管選了哪個螢幕：卡片的大小，沒有攝影機
        let cam = CameraConfig { device: "USB Camera".into(), corner: Default::default(), size: 20, circle: true, pos: None };
        let c = RecordConfig { audio_only: true, fps: 5.0, source: SourceConfig::Monitor { monitor_id: "missing".into() }, camera: Some(cam), ..cfg() };
        let mut plan = resolve_plan(&c, &same_gpu()).unwrap();
        assert_eq!((plan.out_width, plan.out_height, plan.monitors.len()), (640, 360, 0));
        plan.card = Some("C:\\out\\.parts\\card.png".into());
        let a = segment_args(&plan, &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
        let j = a.join(" ");
        assert!(j.contains("-re -loop 1 -framerate 5 -i C:\\out\\.parts\\card.png -f f32le"), "{j}");
        assert!(!j.contains("gdigrab") && !j.contains("dshow"), "{j}");
        assert_eq!(graph_of(&a), "[0:v]showinfo=checksum=0,scale=640:360:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]");
        assert!(j.contains("-map [vout] -map 1:a"));
        assert!(j.contains("-crf 23 -tune zerolatency -r 5"), "{j}");
        // 一般錄影維持預設（壓縮比較好）
        let screen = segment_args(&plan, &RecordConfig { audio_only: false, ..c.clone() }, CaptureMethod::Gdigrab, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
        assert!(!screen.join(" ").contains("zerolatency"));
    }

    #[test]
    fn merge() {
        let inputs = [("a.mp4".to_string(), true, 3.0), ("b.mp4".to_string(), false, 2.5)];
        let a = merge_args(&inputs, "out.mp4", 1920, 1080, 30.0, &x264());
        let j = a.join(" ");
        assert!(j.starts_with("-hide_banner -nostats -loglevel error -i a.mp4 -i b.mp4 -filter_complex"));
        let g = graph_of(&a);
        assert!(g.contains("[1:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:black,setsar=1,fps=30,format=yuv420p[v1]"), "{g}");
        assert!(g.contains("[0:a]aresample=48000") && g.contains("anullsrc=r=48000:cl=stereo,atrim=duration=2.5"), "{g}");
        assert!(g.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]"), "{g}");
        assert!(j.contains("-map [vout] -map [aout] -c:a aac") && j.ends_with("-y out.mp4"));
    }

    #[test]
    fn camera_list() {
        let new_style = "\
[dshow @ 000001] \"Integrated Camera\" (video)
[dshow @ 000001]   Alternative name \"@device_pnp_\\\\?\\usb#vid\"
[dshow @ 000001] \"OBS Virtual Camera\" (none)
[dshow @ 000001] \"Logi C270\" (video)
[dshow @ 000001] \"麥克風 (Realtek(R) Audio)\" (audio)
dummy: Immediate exit requested";
        assert_eq!(parse_cameras(new_style), vec!["Integrated Camera", "Logi C270"]);
        let old_style = "\
[dshow @ 0x1] DirectShow video devices (some may be both video and audio devices)
[dshow @ 0x1]  \"USB2.0 HD UVC WebCam\"
[dshow @ 0x1]     Alternative name \"@device_pnp_x\"
[dshow @ 0x1] DirectShow audio devices
[dshow @ 0x1]  \"Microphone Array\"";
        assert_eq!(parse_cameras(old_style), vec!["USB2.0 HD UVC WebCam"]);
        assert!(parse_cameras("").is_empty());
    }

    #[test]
    fn camera_is_not_in_the_recording_args() {
        // 攝影機是螢幕上的小視窗，跟著畫面被錄進去：錄影的 FFmpeg 不開攝影機、不疊畫面
        let audio_in = strs(&["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "tcp://127.0.0.1:5000"]);
        let c = RecordConfig { camera: Some(CameraConfig { device: "USB Camera".into(), corner: 0, size: 20, circle: true, pos: None }), ..cfg() };
        let plan = resolve_plan(&c, &same_gpu()).unwrap();
        for method in [CaptureMethod::Gdigrab, CaptureMethod::Ddagrab] {
            let a = segment_args(&plan, &c, method, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
            let j = a.join(" ");
            assert!(!j.contains("dshow") && !graph_of(&a).contains("overlay"), "{j}");
            assert!(j.contains("-map [vout] -map ") && j.contains(":a"), "{j}");
        }
    }

    #[test]
    fn preview_skips_static_frames() {
        let second = same_gpu()[1].clone();
        // 單一螢幕的即時預覽：畫面沒變化時不送出
        let live = preview_args(std::slice::from_ref(&second), true, 1280, Some(5.0), true);
        let live = graph_of(&live);
        assert!(live.contains("dup_frames=0"), "{live}");
        // 單張、多個螢幕（xstack 要每塊都有新畫面）、FFmpeg 不支援時都不用
        assert!(!graph_of(&preview_args(std::slice::from_ref(&second), true, 1600, None, true)).contains("dup_frames"));
        assert!(!graph_of(&preview_args(&same_gpu(), true, 1280, Some(5.0), true)).contains("dup_frames"));
        assert!(!graph_of(&preview_args(std::slice::from_ref(&second), true, 1280, Some(5.0), false)).contains("dup_frames"));
        // 錄影永遠不用（沒有變化時整個濾鏡圖會停住）
        let c = RecordConfig { source: SourceConfig::Monitor { monitor_id: second.id.clone() }, ..cfg() };
        let rec = segment_args(&resolve_plan(&c, &same_gpu()).unwrap(), &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, None).unwrap();
        assert!(!graph_of(&rec).contains("dup_frames"));
    }

    #[test]
    fn gpu_convert_by_vendor() {
        assert_eq!(GpuConvert::for_adapter("Intel(R) UHD Graphics 770"), Some(GpuConvert::Qsv));
        assert_eq!(GpuConvert::for_adapter("AMD Radeon(TM) Graphics"), Some(GpuConvert::Amf));
        assert_eq!(GpuConvert::for_adapter("Radeon RX 7600"), Some(GpuConvert::Amf));
        // NVIDIA 沒有能指定 BT.709 的顯示卡轉換，維持 CPU
        assert_eq!(GpuConvert::for_adapter("NVIDIA GeForce RTX 4060"), None);
        assert_eq!(GpuConvert::for_adapter("Microsoft Basic Render Driver"), None);
        assert!(GpuConvert::Qsv.chain(1920, 1080).contains("out_color_matrix=bt709") && GpuConvert::Qsv.chain(1920, 1080).contains("out_range=limited"));
        assert!(GpuConvert::Amf.chain(1280, 720).contains("w=1280:h=720:format=nv12") && GpuConvert::Amf.chain(1280, 720).contains("color_profile=bt709"));
    }

    #[test]
    fn gpu_path_choice() {
        let mons = same_gpu();
        let s = GpuSupport { adapter: mons[0].adapter, convert: GpuConvert::Qsv, zero_copy: true };
        let support = Some(s);
        let qsv = encoder_spec("h264_qsv").unwrap();
        let one = RecordConfig { source: SourceConfig::Monitor { monitor_id: mons[0].id.clone() }, ..cfg() };
        let plan = resolve_plan(&one, &mons).unwrap();
        // 同一張顯示卡的編碼器：直接交給它；其他編碼器：下載 NV12
        assert_eq!(GpuPath::choose(support, &plan, &one, CaptureMethod::Ddagrab, &qsv).map(|g| g.zero_copy), Some(true));
        assert_eq!(GpuPath::choose(support, &plan, &one, CaptureMethod::Ddagrab, &x264()).map(|g| g.zero_copy), Some(false));
        // 不能用的情況：gdigrab、沒有實測通過、兩個螢幕合成、只錄聲音、別張顯示卡
        assert!(GpuPath::choose(support, &plan, &one, CaptureMethod::Gdigrab, &x264()).is_none());
        assert!(GpuPath::choose(None, &plan, &one, CaptureMethod::Ddagrab, &x264()).is_none());
        let all = RecordConfig { source: SourceConfig::All, ..cfg() };
        assert!(GpuPath::choose(support, &resolve_plan(&all, &mons).unwrap(), &all, CaptureMethod::Ddagrab, &x264()).is_none());
        // 攝影機是螢幕上的小視窗，不影響
        let cam = RecordConfig { camera: Some(CameraConfig { device: "Cam".into(), size: 20, corner: 0, circle: false, pos: None }), ..one.clone() };
        assert!(GpuPath::choose(support, &plan, &cam, CaptureMethod::Ddagrab, &x264()).is_some());
        let other = Some(GpuSupport { adapter: mons[0].adapter + 1, ..s });
        assert!(GpuPath::choose(other, &plan, &one, CaptureMethod::Ddagrab, &x264()).is_none());
        // GPU 編碼器失敗改用 CPU：不再直接交給編碼器
        let g = GpuPath::choose(support, &plan, &one, CaptureMethod::Ddagrab, &qsv).unwrap();
        assert!(!g.for_encoder(&x264()).zero_copy);
    }

    #[test]
    fn gpu_convert_graph() {
        let mons = same_gpu();
        let c = RecordConfig { source: SourceConfig::Monitor { monitor_id: mons[0].id.clone() }, scale: 50.0, ..cfg() };
        let plan = resolve_plan(&c, &mons).unwrap();
        let g = GpuPath { adapter: mons[0].adapter, convert: GpuConvert::Qsv, zero_copy: true };
        // 直接交給編碼器：沒有下載、沒有 CPU 縮放；聲音對齊用的 showinfo 在轉換之前
        let audio_in = vec!["-f".to_string(), "s16le".into(), "-i".into(), "tcp://127.0.0.1:1".into()];
        let zc = segment_args(&plan, &c, CaptureMethod::Ddagrab, &encoder_spec("h264_qsv").unwrap(), "o.mp4", Some(&audio_in), Some(g)).unwrap();
        let zc = graph_of(&zc);
        assert!(zc.starts_with("ddagrab=") && zc.contains(&format!("showinfo=checksum=0,hwmap=derive_device=qsv,format=qsv,vpp_qsv=w={}:h={}:", plan.out_width, plan.out_height)), "{zc}");
        assert!(!zc.contains("hwdownload") && !zc.contains("scale=") && zc.ends_with("[vout]"), "{zc}");
        // 給 CPU 編碼器：下載轉好的 NV12
        let dl = segment_args(&plan, &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, Some(GpuPath { zero_copy: false, ..g })).unwrap();
        let dl = graph_of(&dl);
        assert!(dl.contains("out_range=limited,hwdownload,format=nv12[vout]") && !dl.contains("format=bgra") && !dl.contains("showinfo"), "{dl}");
        // 顏色標記不變（仍是 BT.709 limited），之後的處理照舊
        let args = segment_args(&plan, &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, Some(g)).unwrap().join(" ");
        assert!(args.contains("-colorspace bt709") && args.contains("-color_range tv"));
        // 範圍移到別張顯示卡、多個螢幕、gdigrab：回到原本的 CPU 轉換
        let other = GpuPath { adapter: g.adapter + 1, ..g };
        assert!(graph_of(&segment_args(&plan, &c, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, Some(other)).unwrap()).contains("hwdownload,format=bgra"));
        let all = RecordConfig { source: SourceConfig::All, ..c.clone() };
        assert!(graph_of(&segment_args(&resolve_plan(&all, &mons).unwrap(), &all, CaptureMethod::Ddagrab, &x264(), "o.mp4", None, Some(g)).unwrap()).contains("xstack"));
        assert!(!segment_args(&plan, &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", None, Some(g)).unwrap().join(" ").contains("vpp_qsv"));
    }

    #[test]
    fn gpu_convert_test_checks_colors() {
        let m = &same_gpu()[0];
        let a = gpu_convert_test_args(m, GpuConvert::Amf);
        let g = graph_of(&a);
        assert!(g.contains("split=3") && g.matches("psnr[").count() == 2 && g.matches("setparams=range=tv").count() == 3, "{g}");
        assert!(a.join(" ").contains("-map [o1] -map [o2]"));
        assert!(after(&gpu_encode_test_args(m, GpuConvert::Qsv, &encoder_spec("h264_qsv").unwrap()), "-filter_complex").contains("showinfo=checksum=0,hwmap=derive_device=qsv"));
        // 實際 FFmpeg 的輸出（以 CPU 模擬三種顯示卡轉換的結果；psnr 的編號依濾鏡圖的順序，印出來的順序相反）
        let ok = "[Parsed_psnr_15 @ 0x55] PSNR y:22.413359 u:28.659495 v:31.975017 average:23.812008 min:23.742304 max:23.851274\n[Parsed_psnr_14 @ 0x55] PSNR y:inf u:49.067202 v:45.117796 average:51.429437 min:51.113770 max:51.966583\n";
        let bt601 = "[Parsed_psnr_15 @ 0x55] PSNR y:inf u:49.636133 v:45.368733 average:51.769321\n[Parsed_psnr_14 @ 0x55] PSNR y:22.418426 u:28.602335 v:31.809216 average:23.809227\n";
        let full = "[Parsed_psnr_15 @ 0x55] PSNR y:18.767008 u:28.3 v:32.4 average:23.7\n[Parsed_psnr_14 @ 0x55] PSNR y:26.534293 u:47.6 v:44.5 average:47.3\n";
        let p = |s: &str| parse_gpu_psnr(s).unwrap();
        assert_eq!(p(ok), (f64::INFINITY, 22.413359));
        assert!(gpu_psnr_ok(p(ok).0, p(ok).1));
        assert!(!gpu_psnr_ok(p(bt601).0, p(bt601).1));
        assert!(!gpu_psnr_ok(p(full).0, p(full).1));
        // 畫面全是灰階：兩者一樣，相信濾鏡指定的 BT.709
        assert!(gpu_psnr_ok(f64::INFINITY, f64::INFINITY));
        assert!(gpu_psnr_ok(52.0, 52.0));
        assert!(parse_gpu_psnr("Error opening filters").is_none());
    }

    #[test]
    fn separate_audio_tracks_args() {
        let audio_in = strs(&["-f", "f32le", "-ar", "48000", "-ac", "6", "-i", "tcp://127.0.0.1:5000"]);
        let both = AudioConfig { system: true, mic: true, mic_id: String::new(), app: String::new() };
        let c = RecordConfig { audio: both.clone(), separate_audio: true, ..cfg() };
        let plan = resolve_plan(&c, &same_gpu()).unwrap();
        let a = segment_args(&plan, &c, CaptureMethod::Gdigrab, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
        let j = a.join(" ");
        // 畫面與拆聲音是兩個濾鏡圖（放在一起時 FFmpeg 要等聲音才處理畫面，而聲音要等第一張畫面才送）
        let graphs: Vec<&String> = a.iter().zip(a.iter().skip(1)).filter(|(k, _)| *k == "-filter_complex").map(|(_, v)| v).collect();
        assert_eq!(graphs.len(), 2, "{j}");
        assert!(!graphs[0].contains("[1:a]") && graphs[0].ends_with("[vout]"), "{j}");
        assert_eq!(graphs[1], "[1:a]asplit=3[sa0][sa1][sa2];[sa0]pan=stereo|c0=c0|c1=c1[amix];[sa1]pan=stereo|c0=c2|c1=c3[asys];[sa2]pan=stereo|c0=c4|c1=c5[amic]");
        assert!(j.contains("-map [vout] -map [amix] -map [asys] -map [amic] -c:a aac"), "{j}");
        assert!(j.contains("-metadata:s:a:2 handler_name=麥克風") && j.contains("-disposition:a:0 default -metadata:s:a:1"), "{j}");
        // 只錄一個來源、或沒打開分軌：一條音軌
        assert!(!RecordConfig { audio: AudioConfig { mic: false, ..both.clone() }, ..c.clone() }.separate_tracks());
        let one = segment_args(&plan, &RecordConfig { separate_audio: false, ..c }, CaptureMethod::Gdigrab, &x264(), "o.mp4", Some(&audio_in), None).unwrap();
        assert!(one.join(" ").contains("-map [vout] -map 1:a -c:a aac") && !one.join(" ").contains("asplit"));
    }

    #[test]
    fn replay_buffer_writes_rolling_files() {
        let c = RecordConfig { replay_sec: 120, ..cfg() };
        let plan = resolve_plan(&c, &same_gpu()).unwrap();
        let a = segment_args(&plan, &c, CaptureMethod::Gdigrab, &x264(), "rp_000_%05d.mp4", None, None).unwrap();
        let j = a.join(" ");
        assert!(j.contains("-f segment -segment_time 10 -reset_timestamps 1 -segment_format mp4"), "{j}");
        assert!(j.contains("frag_duration=1000000") && j.ends_with("-y rp_000_%05d.mp4"), "{j}");
        // 關鍵畫面每 2 秒一張：每個檔案都能從頭播放
        assert!(j.contains(&format!("-g {}", num(c.fps * 2.0))), "{j}");
        // 一般錄影：一個檔案
        let normal = segment_args(&plan, &cfg(), CaptureMethod::Gdigrab, &x264(), "seg_000.mp4", None, None).unwrap().join(" ");
        assert!(!normal.contains("segment_time") && normal.contains("-movflags +empty_moov+default_base_moof -frag_duration 1000000"), "{normal}");
    }

    #[test]
    fn window_capture_graph_and_fallback() {
        let c = RecordConfig { source: region(100.0, 100.0, 641.0, 361.0), follow_window: Some(4660), window_capture: true, fps: 30.0, ..cfg() };
        let plan = resolve_plan(&c, &same_gpu()).unwrap();
        let a = segment_args(&plan, &c, CaptureMethod::Gfxcapture, &x264(), "o.mp4", None, None).unwrap();
        assert_eq!(
            graph_of(&a),
            "gfxcapture=hwnd=4660:max_framerate=30:capture_cursor=1:width=-2:height=-2:resize_mode=scale_aspect,hwdownload,format=bgra,scale=640:360:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]"
        );
        // 沒有輸入檔、不需要另外建顯示卡裝置
        assert!(!a.contains(&"-i".to_string()) && !a.contains(&"-init_hw_device".to_string()), "{a:?}");
        let no_cursor = segment_args(&plan, &RecordConfig { draw_mouse: false, ..c.clone() }, CaptureMethod::Gfxcapture, &x264(), "o.mp4", None, None).unwrap();
        assert!(graph_of(&no_cursor).contains("capture_cursor=0"));
        assert!(segment_args(&plan, &RecordConfig { follow_window: None, ..c }, CaptureMethod::Gfxcapture, &x264(), "o.mp4", None, None).is_err());
        // 第一張畫面前就失敗：先改用範圍（編碼器的錯誤還是先退編碼器）
        let base = FallbackInput {
            stderr: "[gfxcapture @ 0x1] Failed",
            gpu_encoder_in_use: true,
            encoder_auto: true,
            has_cpu_encoder: true,
            ddagrab_in_use: false,
            method_auto: true,
            gpu_convert_in_use: false,
            gfx_in_use: true,
        };
        assert_eq!(startup_fallback(&base), StartupFallback::RegionCapture);
        assert_eq!(startup_fallback(&FallbackInput { stderr: "[h264_nvenc @ 0x1] No NVENC capable devices found\nError while opening encoder", ..base }), StartupFallback::CpuEncoder);
        // 擷取失敗時連帶印出的「Could not open encoder」不算編碼器的問題
        assert_eq!(startup_fallback(&FallbackInput { stderr: "[gfxcapture @ 0x1] Failed\nError while opening encoder", ..base }), StartupFallback::RegionCapture);
    }

    #[test]
    fn gpu_convert_falls_back_first() {
        let base = FallbackInput {
            stderr: "anything",
            gpu_encoder_in_use: true,
            encoder_auto: true,
            has_cpu_encoder: true,
            ddagrab_in_use: true,
            method_auto: true,
            gpu_convert_in_use: true,
            gfx_in_use: false,
        };
        assert_eq!(startup_fallback(&base), StartupFallback::CpuConvert);
        assert_eq!(startup_fallback(&FallbackInput { encoder_auto: false, method_auto: false, ..base }), StartupFallback::CpuConvert);
    }

    #[test]
    fn preview() {
        assert!(graph_of(&preview_args(&same_gpu(), true, 1600, None, false)).contains("xstack=inputs=2"));
        assert!(preview_args(&two_gpus(), true, 1600, None, false).contains(&"gdigrab".to_string()));
        assert!(preview_args(&same_gpu(), false, 1600, None, false).contains(&"gdigrab".to_string()));
        let second = same_gpu()[1].clone();
        let dda = preview_args(std::slice::from_ref(&second), true, 1600, None, false);
        assert!(!graph_of(&dda).contains("xstack"));
        assert!(graph_of(&dda).contains(&format!("output_idx={}", second.output)));
        let gdi = preview_args(std::slice::from_ref(&second), false, 1600, None, false).join(" ");
        assert!(gdi.contains(&format!("-offset_x {} -offset_y {} -video_size {}x{}", second.x, second.y, second.width, second.height)));
        assert!(!preview_args(&same_gpu(), false, 1600, None, false).contains(&"-offset_x".to_string()));
        // 原始像素、大小固定：介面依大小切出每一張
        let raw = preview_args(std::slice::from_ref(&second), false, 1280, Some(5.0), false).join(" ");
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
        let o = |out: f64, src_w: f64, src_h: f64| GifOptions { fps: 10.0, width: 640.0, src_width: src_w, src_height: src_h, output_sec: out, bounce: false };
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
        let args = export_args("in.mp4", "out.mp4", 4.0, 30.0, &x264(), false, None, None, None).unwrap();
        assert_eq!(after(&args, "-vf"), "setpts=PTS/4,fps=30,format=yuv420p");
        assert!(args.contains(&"-an".to_string()));
        assert!(export_args("in.mp4", "out.mp4", 1.0, 30.0, &x264(), false, None, None, None).unwrap_err().is_config());
        // 指定大小：原速也可以，改用位元率
        let small = export_args("in.mp4", "out.mp4", 1.0, 30.0, &x264(), true, None, None, Some(1500)).unwrap().join(" ");
        assert!(small.contains("-b:v 1500k -maxrate 2250k -bufsize 3000k") && !small.contains("-crf") && !small.contains("atempo") && small.contains("-b:a 96k"), "{small}");
        assert_eq!(size_bitrate(25.0 * 1024.0 * 1024.0, 100.0, 96), 1938);
        assert_eq!(size_bitrate(1.0, 100.0, 96), 80);
        let vf = |w, sw| export_args("in.mp4", "out.mp4", 4.0, 30.0, &x264(), false, Some(w), Some(sw), None).unwrap();
        assert_eq!(after(&vf(1920, 3840), "-vf"), "setpts=PTS/4,fps=30,scale=1920:-2:flags=bicubic,format=yuv420p");
        assert_eq!(after(&vf(1920, 1920), "-vf"), "setpts=PTS/4,fps=30,format=yuv420p");
        assert_eq!(after(&vf(0, 3840), "-vf"), "setpts=PTS/4,fps=30,format=yuv420p");
        let audio = export_args("in.mp4", "out.mp4", 16.0, 30.0, &x264(), true, None, None, None).unwrap();
        assert_eq!(after(&audio, "-af"), "atempo=2,atempo=2,atempo=2,atempo=2");
        assert!(audio.join(" ").contains("-map 0:v:0 -map 0:a:0"));
        assert_eq!(atempo_chain(1.5), "atempo=1.5");
        assert_eq!(atempo_chain(3.0), "atempo=2,atempo=1.5");
    }

    #[test]
    fn media_info() {
        let stderr = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x.mp4':\n  Duration: 00:01:02.50, start: 0.000000, bitrate: 1234 kb/s\n  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 1920x1080 [SAR 1:1 DAR 16:9], 1200 kb/s, 30 fps, 30 tbr, 15360 tbn (default)\n  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 160 kb/s (default)";
        assert_eq!(parse_media_info(stderr), ParsedMedia { duration_sec: Some(62.5), width: Some(1920), height: Some(1080), fps: Some(30.0), has_audio: true, chapters: vec![], audio_tracks: 1 });
        let first3: Vec<&str> = stderr.lines().take(3).collect();
        assert!(!parse_media_info(&first3.join("\n")).has_audio);
        // WebP 動圖：檔頭沒有長度，另外量
        let webp = "Input #0, webp_pipe, from 'a.webp':\n  Duration: N/A, start: 0.000000, bitrate: N/A\n  Stream #0:0: Video: webp_anim, argb, 480x270, 10 fps, 10 tbr, 1k tbn";
        assert_eq!(parse_media_info(webp).duration_sec, None);
        assert_eq!(measure_duration_args("a.webp").join(" "), "-hide_banner -i a.webp -map 0:v:0 -c copy -f null -");
        assert_eq!(input_at("x.mp4", "1.500").join(" "), "-ss 1.500 -i x.mp4");
        assert_eq!(input_at("x.WebP", "1.500").join(" "), "-i x.WebP -ss 1.500");
        let measured = "frame=   12 fps=0.0 q=-1.0 size=N/A time=00:00:01.20 bitrate=N/A\rframe=   30 fps=0.0 q=-1.0 Lsize=N/A time=00:00:03.00 bitrate=N/A speed=1.41e+03x";
        assert_eq!(parse_measured_duration(measured), Some(3.0));
        assert_eq!(parse_measured_duration("time=N/A"), None);
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
        let nvenc = encoder_spec("h264_nvenc").unwrap();
        let pick = |pref, w, h, fps, gpu: &[EncoderSpec], learned| choose_encoder(pref, w, h, fps, cpu, gpu, learned).map(|(s, _)| s.name);
        use EncoderPreference::*;
        // 「自動」：有 Intel QSV 就優先用，不論畫面大小（也比其他顯示卡優先）
        assert_eq!(pick(Auto, 1280, 720, 30.0, &[qsv], false), Ok("h264_qsv"));
        assert_eq!(pick(Auto, 1920, 1080, 30.0, &[qsv], false), Ok("h264_qsv"));
        assert_eq!(pick(Auto, 3840, 2160, 60.0, &[nvenc, qsv], false), Ok("h264_qsv"));
        // 沒有 QSV：照舊平常用 CPU，畫面量超過 1080p60 或先前跟不上才用顯示卡
        assert_eq!(pick(Auto, 1920, 1080, 30.0, &[nvenc], false), Ok("libx264"));
        assert_eq!(pick(Auto, 1920, 1080, 60.0, &[nvenc], false), Ok("libx264"));
        const { assert!(3840.0 * 2160.0 * 30.0 > AUTO_GPU_PIXELS_PER_SEC) };
        assert_eq!(pick(Auto, 3840, 2160, 30.0, &[nvenc], false), Ok("h264_nvenc"));
        assert_eq!(pick(Auto, 4480, 1440, 30.0, &[nvenc], false), Ok("h264_nvenc"));
        assert_eq!(pick(Auto, 1280, 720, 30.0, &[nvenc], true), Ok("h264_nvenc"));
        assert_eq!(pick(Auto, 3840, 2160, 60.0, &[], true), Ok("libx264"));
        // 指定 GPU / CPU 不受影響
        assert_eq!(pick(Gpu, 640, 360, 30.0, &[qsv], false), Ok("h264_qsv"));
        assert_eq!(pick(Cpu, 3840, 2160, 60.0, &[qsv], true), Ok("libx264"));
        assert!(pick(Gpu, 640, 360, 30.0, &[], false).unwrap_err().is_config());
    }

    #[test]
    fn encoder_fault_detection() {
        assert!(is_encoder_fault("[h264_nvenc @ 0x1] No NVENC capable devices found\nError while opening encoder"));
        assert!(!is_encoder_fault("[ddagrab @ 0x1] Failed to capture\nCould not open encoder before EOF"));
        let f = |stderr: &str| {
            startup_fallback(&FallbackInput {
                stderr,
                gpu_encoder_in_use: true,
                encoder_auto: true,
                has_cpu_encoder: true,
                ddagrab_in_use: true,
                method_auto: true,
                gpu_convert_in_use: false,
                gfx_in_use: false,
            })
        };
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
        let base = |stderr| FallbackInput {
            stderr,
            gpu_encoder_in_use: true,
            encoder_auto: true,
            has_cpu_encoder: true,
            ddagrab_in_use: true,
            method_auto: true,
            gpu_convert_in_use: false,
            gfx_in_use: false,
        };
        assert_eq!(startup_fallback(&base(NVENC_FAIL)), StartupFallback::CpuEncoder);
        assert_eq!(startup_fallback(&base(DDAGRAB_FAIL)), StartupFallback::Gdigrab);
        assert_eq!(startup_fallback(&base("something odd")), StartupFallback::Gdigrab);
        assert_eq!(startup_fallback(&FallbackInput { ddagrab_in_use: false, ..base("something odd") }), StartupFallback::CpuEncoder);
        assert_eq!(startup_fallback(&FallbackInput { encoder_auto: false, method_auto: false, ..base(NVENC_FAIL) }), StartupFallback::Fatal);
        assert_eq!(startup_fallback(&FallbackInput { gpu_encoder_in_use: false, ddagrab_in_use: false, ..base(NVENC_FAIL) }), StartupFallback::Fatal);
    }

    #[test]
    fn cut_with_fades() {
        let a = cut_args("in.mp4", "out.mp4", &[Part::new(1.0, 2.0, 1.0), Part::new(4.0, 5.5, 1.0)], None, 30.0, &x264(), true, &[], AudioFx::default(), None, None, None, None, None, true).unwrap();
        let vf = after(&a, "-vf");
        assert!(vf.contains("fade=t=out:st=0.8:d=0.2:enable='between(t,0.8,1)',fade=t=in:st=1:d=0.2:enable='between(t,1,1.2)',format=yuv420p"), "{vf}");
        let af = after(&a, "-af");
        assert!(af.ends_with("afade=t=out:st=0.8:d=0.2:enable='between(t,0.8,1)',afade=t=in:st=1:d=0.2:enable='between(t,1,1.2)'"), "{af}");
    }

    #[test]
    fn cut() {
        let args = cut_args(
            "in.mp4",
            "out.mp4",
            &[Part::new(1.0, 2.0, 1.0), Part::new(4.0, 5.5, 1.0)],
            Some(Rect { x: 40, y: 40, width: 200, height: 160 }),
            30.0,
            &x264(),
            true,
            &[],
            AudioFx::default(),
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(after(&args, "-vf"), "select='gte(t,1)*lt(t,2)+gte(t,4)*lt(t,5.5)',setpts=N/(30*TB),crop=200:160:40:40,format=yuv420p");
        assert_eq!(after(&args, "-af"), "aselect='gte(t,1)*lt(t,2)+gte(t,4)*lt(t,5.5)',asetpts=N/SR/TB");
        let no_audio = cut_args("in.mp4", "out.mp4", &[Part::new(0.0, 3.0, 1.0)], None, 30.0, &x264(), false, &[], AudioFx::default(), None, None, None, None, None, false).unwrap();
        // 降噪、音量平衡接在剪輯後面；不要聲音時整個拿掉
        let fx = cut_args(
            "in.mp4",
            "out.mp4",
            &[Part::new(0.0, 3.0, 1.0)],
            None,
            30.0,
            &x264(),
            true,
            &[],
            AudioFx { denoise: true, normalize: true, mute: false, voice: true, tracks: None },
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap()
        .join(" ");
        assert!(fx.contains(&format!("asetpts=N/SR/TB,afftdn=nf=-25,{},loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000", crate::edit::VOICE_FX)), "{fx}");
        let muted =
            cut_args("in.mp4", "out.mp4", &[Part::new(0.0, 3.0, 1.0)], None, 30.0, &x264(), true, &[], AudioFx { mute: true, ..Default::default() }, None, None, None, None, None, false).unwrap();
        assert!(muted.contains(&"-an".to_string()) && !muted.iter().any(|a| a.contains("aselect")));
        assert!(no_audio.contains(&"-an".to_string()));
        assert!(!no_audio.contains(&"-af".to_string()));
        assert!(cut_args("in.mp4", "out.mp4", &[], None, 30.0, &x264(), false, &[], AudioFx::default(), None, None, None, None, None, false).unwrap_err().is_config());
        // 與 edit 模組串起來
        let keep = keep_ranges(10.0, &EditSpec { start: 1.0, end: 9.0, removed: vec![(3.0, 4.0)], crop: None, overlays: vec![], ..Default::default() });
        assert_eq!(keep, vec![(1.0, 3.0), (4.0, 9.0)]);
        assert_eq!(normalize_crop(None, 100, 100), None);

        // 局部加速：每 4 張留一張，那段的聲音關掉
        let fast = cut_args(
            "in.mp4",
            "out.mp4",
            &[Part::new(0.0, 2.0, 1.0), Part::new(2.0, 6.0, 4.0), Part::new(6.0, 8.0, 1.0)],
            None,
            30.0,
            &x264(),
            true,
            &[],
            AudioFx::default(),
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        let vf = &fast[fast.iter().position(|a| a == "-vf").unwrap() + 1];
        assert!(vf.starts_with("select='gte(t,0)*lt(t,2)+gte(t,2)*lt(t,6)*not(mod(n,4))+gte(t,6)*lt(t,8)',setpts=N/(30*TB)"), "{vf}");
        let af = &fast[fast.iter().position(|a| a == "-af").unwrap() + 1];
        assert!(af.starts_with("volume=0:enable='between(t,2,6)',aselect='gte(t,0)*lt(t,2)+gte(t,2)*lt(t,6)*not(mod(n,4))+"), "{af}");

        // 標註：先疊上（原影片時間），再挑選保留的片段與裁切
        let overlays = [
            OverlayInput::Image { path: "a.png".into(), x: 10, y: 20, start: 1.0, end: 3.5, anim: 0, size: (10, 10) },
            OverlayInput::Blur { rect: Rect { x: 100, y: 50, width: 120, height: 60 }, start: 0.0, end: 9.0, mosaic: false, mask: None, invert: false, frame: (1280, 720) },
            OverlayInput::Blur { rect: Rect { x: 0, y: 0, width: 240, height: 120 }, start: 2.0, end: 4.0, mosaic: true, mask: None, invert: false, frame: (1280, 720) },
            // 橢圓模糊：遮罩縮放成範圍大小，alphamerge 後疊上
            OverlayInput::Blur { rect: Rect { x: 200, y: 100, width: 400, height: 240 }, start: 1.0, end: 3.0, mosaic: false, mask: Some("m.png".into()), invert: false, frame: (1280, 720) },
            // 範圍外模糊（橢圓範圍內清楚）
            OverlayInput::Blur { rect: Rect { x: 700, y: 400, width: 400, height: 240 }, start: 2.0, end: 4.0, mosaic: false, mask: Some("m2.png".into()), invert: true, frame: (1280, 720) },
            // 範圍外馬賽克（方形）
            OverlayInput::Blur { rect: Rect { x: 10, y: 20, width: 100, height: 50 }, start: 0.0, end: 1.0, mosaic: true, mask: None, invert: true, frame: (1280, 720) },
        ];
        let args = cut_args(
            "in.mp4",
            "out.mp4",
            &[Part::new(1.0, 5.0, 1.0)],
            Some(Rect { x: 40, y: 40, width: 200, height: 160 }),
            30.0,
            &x264(),
            true,
            &overlays,
            AudioFx::default(),
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
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
        // 平滑、放大的游標：在標註底下，sendcmd 依原影片的時間移動它；沒有其他標註時也走同一條路
        let c = CursorInput { image: "cursor.png".into(), commands: "cursor.txt".into(), x: 96, y: 50 };
        let args = cut_args("in.mp4", "out.mp4", &[Part::new(1.0, 5.0, 1.0)], None, 30.0, &x264(), false, &overlays[..1], AudioFx::default(), None, None, Some(&c), None, None, false).unwrap();
        let inputs: Vec<&String> = args.iter().zip(args.iter().skip(1)).filter(|(k, _)| *k == "-i").map(|(_, v)| v).collect();
        assert_eq!(inputs, ["in.mp4", "cursor.png", "a.png"]);
        assert!(
            after(&args, "-filter_complex").starts_with("[0:v]sendcmd=f=cursor.txt[cs];[cs][1:v]overlay@cursor=x=96:y=50[cur0];[cur0][2:v]overlay=10:20:enable='between(t,1,3.5)'[o0];[o0]select="),
            "{}",
            after(&args, "-filter_complex")
        );
        let only = cut_args("in.mp4", "out.mp4", &[Part::new(0.0, 5.0, 1.0)], None, 30.0, &x264(), false, &[], AudioFx::default(), None, None, Some(&c), None, None, false).unwrap();
        assert!(!only.contains(&"-vf".to_string()) && after(&only, "-map") == "[vout]");
        // 直式影片：放在中間（剪好的畫面再放進直式畫面）
        let fit = VerticalInput::Fit { w: 1080, h: 1920 };
        let a = cut_args(
            "in.mp4",
            "out.mp4",
            &[Part::new(0.0, 5.0, 1.0)],
            Some(Rect { x: 0, y: 0, width: 800, height: 600 }),
            30.0,
            &x264(),
            false,
            &[],
            AudioFx::default(),
            None,
            None,
            None,
            Some(&fit),
            None,
            false,
        )
        .unwrap();
        assert_eq!(after(&a, "-filter_complex"), format!("[0:v]select='gte(t,0)*lt(t,5)',setpts=N/(30*TB),crop=800:600:0:0,{},format=yuv420p[vout]", crate::vertical::fit_filter(1080, 1920)));
        // 跟著游標：在原影片的時間裁切（游標、標註之後），太大時縮小
        let follow = VerticalInput::Follow { commands: "vertical.txt".into(), w: 1214, h: 2158, x: 1300, y: 1, scale: Some((1080, 1920)) };
        let a = cut_args("in.mp4", "out.mp4", &[Part::new(0.0, 5.0, 1.0)], None, 30.0, &x264(), false, &[], AudioFx::default(), None, None, Some(&c), Some(&follow), None, false).unwrap();
        assert_eq!(
            after(&a, "-filter_complex"),
            "[0:v]sendcmd=f=cursor.txt[cs];[cs][1:v]overlay@cursor=x=96:y=50[cur0];[cur0]sendcmd=f=vertical.txt,crop@vertical=w=1214:h=2158:x=1300:y=1,scale=1080:1920:flags=bicubic[vert];\
             [vert]select='gte(t,0)*lt(t,5)',setpts=N/(30*TB),format=yuv420p[vout]"
        );
    }

    /// 片頭片尾與背景音樂：有 FFmpeg 時實際跑一次，確認濾鏡接得起來、長度對
    #[test]
    fn extras_run_in_ffmpeg() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.mp4");
        let music = dir.path().join("m.m4a");
        let card = dir.path().join("card.png");
        let run = |args: &[String]| std::process::Command::new(&ffmpeg).args(args).output().unwrap();
        let gen = |a: &[&str]| assert!(run(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).status.success());
        gen(&[
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30:d=3",
            "-f",
            "lavfi",
            "-i",
            "sine=f=300:d=3",
            "-shortest",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            src.to_str().unwrap(),
        ]);
        gen(&["-loglevel", "error", "-f", "lavfi", "-i", "sine=f=600:d=1", "-c:a", "aac", music.to_str().unwrap()]);
        crate::cards::save(&crate::cards::TitleCard::new("片頭"), 320, 240, &card).unwrap();
        let card = card.display().to_string();
        let probe = |f: &std::path::Path| String::from_utf8_lossy(&run(&["-hide_banner".into(), "-i".into(), f.display().to_string()]).stderr).to_string();
        for (with_audio, music_on) in [(true, true), (false, true), (true, false), (false, false)] {
            let out = dir.path().join(format!("out-{with_audio}-{music_on}.mp4"));
            let x = Extras {
                intro: Some((card.clone(), 1.0)),
                outro: Some((card.clone(), 1.5)),
                size: (320, 240),
                main_len: 2.0,
                music: music_on.then(|| MusicInput { path: music.display().to_string(), volume: 0.3, duck: true }),
                voice: None,
            };
            let a =
                cut_args(src.to_str().unwrap(), out.to_str().unwrap(), &[Part::new(0.5, 2.5, 1.0)], None, 30.0, &x264(), with_audio, &[], AudioFx::default(), None, None, None, None, Some(&x), false)
                    .unwrap();
            let r = run(&a);
            assert!(r.status.success(), "{with_audio} {music_on}: {}", String::from_utf8_lossy(&r.stderr));
            let info = parse_media_info(&probe(&out));
            let d = info.duration_sec.unwrap();
            assert!((d - 4.5).abs() < 0.25, "{with_audio} {music_on}: {d}");
            assert_eq!(info.has_audio, with_audio || music_on, "{with_audio} {music_on}");
        }
    }

    /// 放慢、定格、旁白與點擊音效：有 FFmpeg 時實際跑一次，確認長度、畫面張數與聲音
    #[test]
    fn look_and_stamp_run_in_ffmpeg() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let Some(font) = crate::look::stamp_font() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.mp4");
        let run = |args: &[String]| std::process::Command::new(&ffmpeg).current_dir(dir.path()).args(args).output().unwrap();
        let gen: Vec<String> =
            ["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:d=4", "-pix_fmt", "yuv420p"].iter().map(|s| s.to_string()).chain([src.display().to_string()]).collect();
        assert!(run(&gen).status.success());
        std::fs::copy(font, dir.path().join("stamp.ttf")).unwrap();
        std::fs::write(dir.path().join("stamp.txt"), crate::look::stamp_text(1_760_600_000_000)).unwrap();
        let look = crate::look::Look { brightness: 20, contrast: 10, saturation: -20, sharpen: 1 };
        let area = Rect { x: 40, y: 20, width: 240, height: 200 };
        let more = CutMore { look: look.filter(), stamp: Some(crate::look::stamp_filter(&crate::look::Stamp::default(), "stamp.ttf", "stamp.txt", area)) };
        // 刪掉中間、改順序（浮水印在原影片的時間畫上去）
        let keep = [Part::new(2.5, 4.0, 1.0), Part::new(0.0, 1.0, 1.0)];
        let out = dir.path().join("out.mp4");
        let a = cut_args_more(src.to_str().unwrap(), out.to_str().unwrap(), &keep, Some(area), 30.0, &x264(), false, &[], AudioFx::default(), None, None, None, None, None, false, &more).unwrap();
        assert!(a.iter().any(|s| s.contains("drawtext=fontfile=stamp.ttf") && s.contains("[stamp]")) && a.iter().any(|s| s.contains("[0:v]eq=") && s.contains("[look]")), "{a:?}");
        let r = run(&a);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let info = parse_media_info(&String::from_utf8_lossy(&run(&["-hide_banner".into(), "-i".into(), out.display().to_string()]).stderr));
        assert!((info.duration_sec.unwrap() - 2.5).abs() < 0.15 && info.width == Some(240), "{info:?}");
    }

    #[test]
    fn animated_overlays_run_in_ffmpeg() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.mp4");
        let run = |args: &[String]| std::process::Command::new(&ffmpeg).args(args).output().unwrap();
        let gen: Vec<String> =
            ["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:d=3", "-pix_fmt", "yuv420p"].iter().map(|s| s.to_string()).chain([src.display().to_string()]).collect();
        assert!(run(&gen).status.success());
        let png = dir.path().join("o.png");
        let mut pm = tiny_skia::Pixmap::new(60, 30).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 200));
        pm.save_png(&png).unwrap();
        let ov = |anim: u8, x: i32| OverlayInput::Image { path: png.display().to_string(), x, y: 20, start: 0.5, end: 2.5, anim, size: (60, 30) };
        let overlays = [ov(1, 10), ov(2, 120), ov(0, 220)];
        let out = dir.path().join("out.mp4");
        // 刪掉中間一段：動畫在原影片的時間
        let keep = [Part::new(0.0, 1.0, 1.0), Part::new(1.5, 3.0, 1.0)];
        let a = cut_args(src.to_str().unwrap(), out.to_str().unwrap(), &keep, None, 30.0, &x264(), false, &overlays, AudioFx::default(), None, None, None, None, None, false).unwrap();
        assert!(a.iter().any(|s| s.contains("fade=t=in:st=0.5:d=0.3:alpha=1") && s.contains("overlay_w/2") && s.contains("eval=frame")), "{a:?}");
        let r = run(&a);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let info = parse_media_info(&String::from_utf8_lossy(&run(&["-hide_banner".into(), "-i".into(), out.display().to_string()]).stderr));
        assert!((info.duration_sec.unwrap() - 2.5).abs() < 0.15, "{info:?}");
        assert_eq!(crate::annotate::anim_at(2, 0.5, 2.5, 0.5), (0.0, 0.5));
        assert_eq!(crate::annotate::anim_at(1, 0.5, 2.5, 1.5), (1.0, 1.0));
        assert_eq!(crate::annotate::anim_at(0, 0.5, 2.5, 0.5), (1.0, 1.0));
    }

    #[test]
    fn bounce_runs_in_ffmpeg() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.mp4");
        let run = |args: &[String]| std::process::Command::new(&ffmpeg).args(args).output().unwrap();
        let gen: Vec<String> =
            ["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:d=2", "-pix_fmt", "yuv420p"].iter().map(|s| s.to_string()).chain([src.display().to_string()]).collect();
        assert!(run(&gen).status.success());
        let o = GifOptions { fps: 10.0, width: 160.0, src_width: 320.0, src_height: 240.0, output_sec: 2.0, bounce: true };
        let out = dir.path().join("b.gif");
        let a = gif_args(src.to_str().unwrap(), out.to_str().unwrap(), 1.0, &o).unwrap();
        assert!(a.iter().any(|s| s.contains("reverse,trim=start_frame=1")));
        let r = run(&a);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        // 20 張往前 + 19 張倒回來
        let count = run(&["-v".into(), "error".into(), "-i".into(), out.display().to_string(), "-f".into(), "null".into(), "-progress".into(), "pipe:1".into(), "-".into()]);
        let text = String::from_utf8_lossy(&count.stdout);
        let frames: u32 = text.lines().filter_map(|l| l.strip_prefix("frame=")).filter_map(|v| v.trim().parse().ok()).next_back().unwrap();
        assert!((38..=40).contains(&frames), "{frames}");
        assert!(!gif_args("a", "b", 1.0, &GifOptions { bounce: false, ..o }).unwrap().iter().any(|s| s.contains("reverse")));
    }

    #[test]
    fn retime_and_voice_run_in_ffmpeg() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.mp4");
        let run = |args: &[String]| std::process::Command::new(&ffmpeg).args(args).output().unwrap();
        let ok = |a: &[String]| {
            let r = run(a);
            assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        };
        ok(&["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:d=6", "-f", "lavfi", "-i", "sine=f=300:d=6", "-shortest", "-pix_fmt", "yuv420p", "-c:a", "aac"]
            .iter()
            .map(|s| s.to_string())
            .chain([src.display().to_string()])
            .collect::<Vec<_>>());
        let voice = dir.path().join("v.wav");
        std::fs::write(&voice, crate::narration::wav_mono16(&[0.3; 48000], 48000)).unwrap();
        let clicks = dir.path().join("c.wav");
        std::fs::write(&clicks, crate::narration::click_track(&[0.5, 2.0], 7.0, 100)).unwrap();
        let probe = |f: &std::path::Path| String::from_utf8_lossy(&run(&["-hide_banner".into(), "-i".into(), f.display().to_string()]).stderr).to_string();
        let hold = |t: f64, h: f64| Part { a: t, b: t, rate: 1.0, hold: h, keep_audio: false };
        // 0～1 原速、1～2 放慢一半（2 秒）、2 秒處定格 1 秒、2～4 加速 1.5 倍、刪掉 4～5、5～6 原速、結尾定格 0.5 秒
        let keep = [Part::new(0.0, 1.0, 1.0), Part::new(1.0, 2.0, 0.5), hold(2.0, 1.0), Part::new(2.0, 4.0, 1.5), Part::new(5.0, 6.0, 1.0), hold(6.0, 0.5)];
        let total = crate::edit::output_length(&keep);
        assert!((total - (1.0 + 2.0 + 1.0 + 4.0 / 3.0 + 1.0 + 0.5)).abs() < 1e-9);
        for (with_audio, tracks) in [(true, false), (false, false)] {
            let out = dir.path().join(format!("out-{with_audio}-{tracks}.mp4"));
            let x = Extras {
                intro: None,
                outro: None,
                size: (320, 240),
                main_len: total,
                music: None,
                voice: Some(VoiceInput {
                    clips: vec![(voice.display().to_string(), 1.5)],
                    volume: 1.0,
                    duck: crate::narration::duck_filter(&[crate::narration::Placed { file: String::new(), at: 1.5, len: 1.0 }], 20),
                    clicks: Some(clicks.display().to_string()),
                }),
            };
            let a = cut_args(src.to_str().unwrap(), out.to_str().unwrap(), &keep, None, 30.0, &x264(), with_audio, &[], AudioFx::default(), None, None, None, None, Some(&x), false).unwrap();
            ok(&a);
            let info = parse_media_info(&probe(&out));
            let d = info.duration_sec.unwrap();
            assert!((d - total).abs() < 0.15, "{with_audio}: {d} vs {total}");
            assert!(info.has_audio, "{with_audio}");
        }
        // 整支 2 倍加速保留聲音（其中一段再局部 4 倍）：長度對、有聲音
        let fastw = [Part { keep_audio: true, ..Part::new(0.0, 2.0, 2.0) }, Part::new(2.0, 4.0, 8.0), Part { keep_audio: true, ..Part::new(4.0, 6.0, 2.0) }];
        assert!(atempo(8.0) == "atempo=2,atempo=2,atempo=2" && atempo(1.5) == "atempo=1.5");
        let out = dir.path().join("whole.mp4");
        let a = cut_args(src.to_str().unwrap(), out.to_str().unwrap(), &fastw, None, 30.0, &x264(), true, &[], AudioFx::default(), None, None, None, None, None, false).unwrap();
        assert!(a.iter().any(|s| s.contains("atempo=2")));
        ok(&a);
        let info = parse_media_info(&probe(&out));
        assert!((info.duration_sec.unwrap() - 2.25).abs() < 0.15, "{:?}", info.duration_sec);
        assert!(info.has_audio);
        // 片段改了順序（含放慢、定格）：長度、聲音都對
        let moved = [Part::new(4.0, 6.0, 1.0), Part::new(0.0, 1.0, 1.0), hold(1.0, 0.5), Part::new(1.0, 2.0, 0.5), Part::new(2.0, 4.0, 2.0)];
        let len = crate::edit::output_length(&moved);
        let out = dir.path().join("moved.mp4");
        let a = cut_args(src.to_str().unwrap(), out.to_str().unwrap(), &moved, None, 30.0, &x264(), true, &[], AudioFx::default(), None, None, None, None, None, true).unwrap();
        assert!(a.iter().any(|s| s.contains("concat=n=5:v=1:a=0[seg]")));
        ok(&a);
        let info = parse_media_info(&probe(&out));
        assert!((info.duration_sec.unwrap() - len).abs() < 0.15, "{:?} vs {len}", info.duration_sec);
        assert!(info.has_audio);
        // 沒有旁白、只有時間對應：畫面張數對
        let out = dir.path().join("plain.mp4");
        let a = cut_args(src.to_str().unwrap(), out.to_str().unwrap(), &keep, None, 30.0, &x264(), true, &[], AudioFx::default(), None, None, None, None, None, true).unwrap();
        assert!(a.iter().any(|s| s.contains("fps=30:start_time=0")) && a.iter().any(|s| s.contains("tpad=stop_mode=clone")));
        ok(&a);
        let frames = run(&["-hide_banner".into(), "-i".into(), out.display().to_string(), "-map".into(), "0:v:0".into(), "-f".into(), "null".into(), "-".into()]);
        let err = String::from_utf8_lossy(&frames.stderr);
        let n: f64 = err.rsplit("frame=").next().and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok()).unwrap();
        assert!((n - total * 30.0).abs() <= 3.0, "{n} frames vs {}", total * 30.0);
        let d = parse_media_info(&probe(&out)).duration_sec.unwrap();
        assert!((d - total).abs() < 0.15, "{d} vs {total}");
    }

    #[test]
    fn masks_are_cut_to_the_capture_area() {
        let mut c = cfg();
        c.masks = vec![Rect { x: 1800, y: 0, width: 300, height: 100 }, Rect { x: 5000, y: 0, width: 50, height: 50 }, Rect { x: 100, y: 100, width: 2, height: 2 }];
        let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: 1920, height: 1080 }, dda: None, monitors: vec![], out_width: 1920, out_height: 1080, card: None };
        // 超出範圍的裁掉、範圍外與太小的不算
        assert_eq!(mask_rects(&plan, &c), vec![Rect { x: 1800, y: 0, width: 120, height: 100 }]);
        let g = masked_graph("[0:v]null", "format=yuv420p", &mask_rects(&plan, &c));
        assert!(g.starts_with("[0:v]null[mk0];[mk0]split=2") && g.contains("crop=120:100:1800:0") && g.ends_with("[mk1]format=yuv420p[vout]"), "{g}");
        assert_eq!(masked_graph("[0:v]", "format=yuv420p", &[]), "[0:v]format=yuv420p[vout]");
        assert_eq!(masked_graph("ddagrab=0", "format=yuv420p", &[]), "ddagrab=0,format=yuv420p[vout]");
        // 只錄視窗內容時不打
        c.window_capture = true;
        assert!(mask_rects(&plan, &c).is_empty());
    }

    #[test]
    fn mic_denoise_graphs() {
        let both = AudioConfig { system: true, mic: true, mic_id: String::new(), app: String::new() };
        let c = |separate: bool, dn: bool, system: bool| RecordConfig { audio: AudioConfig { system, ..both.clone() }, separate_audio: separate, mic_denoise: dn, ..cfg() };
        assert!(record_audio_graph(2, &c(false, false, true)).is_none());
        let (g, outs) = record_audio_graph(2, &c(false, true, true)).unwrap();
        assert!(g.contains("c0=c4|c1=c5,afftdn") && g.contains("amix=inputs=2:normalize=0[amix]") && outs == vec!["[amix]"], "{g}");
        assert!(c(false, true, true).pipe_separate());
        let (g, outs) = record_audio_graph(2, &c(true, true, true)).unwrap();
        assert!(g.contains("afftdn") && outs.len() == 3, "{g}");
        let (g, _) = record_audio_graph(2, &c(true, false, true)).unwrap();
        assert!(!g.contains("afftdn"));
        let (g, _) = record_audio_graph(2, &c(false, true, false)).unwrap();
        assert_eq!(g, "[2:a]afftdn=nr=12:nf=-40:tn=1[amix]");
        assert!(!c(false, true, false).pipe_separate());
    }

    /// 馬賽克與降噪的濾鏡：有 FFmpeg 時實際跑一次
    #[test]
    fn mask_and_denoise_graphs_run_in_ffmpeg() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let masks = [Rect { x: 10, y: 10, width: 100, height: 60 }, Rect { x: 200, y: 100, width: 80, height: 80 }];
        let v = masked_graph("[0:v]", "format=yuv420p", &masks);
        let both = AudioConfig { system: true, mic: true, mic_id: String::new(), app: String::new() };
        for separate in [false, true] {
            let c = RecordConfig { audio: both.clone(), separate_audio: separate, mic_denoise: true, ..cfg() };
            let (a, outs) = record_audio_graph(1, &c).unwrap();
            let mut args: Vec<String> = [
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10:d=1",
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=d=1:r=48000,pan=6c|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0",
                "-filter_complex",
                &v,
                "-filter_complex",
                &a,
                "-map",
                "[vout]",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            for o in outs {
                args.extend(["-map".into(), o.to_string()]);
            }
            args.extend(["-f".into(), "null".into(), "-".into()]);
            let r = std::process::Command::new(&ffmpeg).args(&args).output().unwrap();
            assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        }
    }
}
