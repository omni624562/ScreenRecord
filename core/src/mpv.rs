//! 用 mpv（libmpv）播放影片：解碼（顯示卡）、聲音畫面同步、縮放都由 mpv 做，畫面直接畫進介面的 OpenGL，
//! 跟 Windows 的播放器一樣順。
//!
//! libmpv 不是必要的元件：執行時才載入（Windows 是下載的 libmpv-2.dll，見 mpv_download.rs；
//! 其他平台用系統的 libmpv.so.2），沒有或載入失敗時檢視器改用 player.rs（FFmpeg 解碼）。
//!
//! 用法：在介面的 OpenGL 執行緒 MpvPlayer::new 開始播放（先建立 render context 才開檔，
//! 不然 mpv 開檔時沒有畫面輸出會放棄）→ 每一格 poll()，wants_render() 時 render() 畫到材質的 FBO。
//! render context 一定要在 OpenGL 執行緒建立與釋放（Drop 也是）。
//!
//! 介面執行緒絕對不能等 mpv：mpv 的主執行緒有時在等介面畫下一張畫面，這時同步讀寫屬性（mpv_get_property、
//! mpv_command）會互相等，整個程式卡住。所以播放中的狀態都用 mpv_observe_property 訂閱、在 poll() 裡從事件收下，
//! 播放、暫停、跳轉、速度、音量用非同步的 mpv_set_property_async、mpv_command_async 送出（不等回應）。

use crate::player::Wake;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type Handle = *mut c_void;
type RenderCtx = *mut c_void;
type GetProc = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type UpdateCb = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct GlInitParams {
    get_proc_address: GetProc,
    ctx: *mut c_void,
}

#[repr(C)]
struct GlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

#[repr(C)]
struct Event {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct EventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}

// client.h / render.h 的常數
const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_DOUBLE: c_int = 5;
const EVENT_NONE: c_int = 0;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;
const EVENT_PROPERTY_CHANGE: c_int = 22;
const END_FILE_REASON_ERROR: c_int = 4;
const PARAM_API_TYPE: c_int = 1;
const PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const PARAM_OPENGL_FBO: c_int = 3;
const PARAM_FLIP_Y: c_int = 4;
const PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
const UPDATE_FRAME: u64 = 1;
/// 需要的 client API 版本：2.0（mpv 0.35）以上
const MIN_API: u64 = 2 << 16;

/// libmpv 的函式（執行時載入）
struct Lib {
    _lib: libloading::Library,
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    terminate_destroy: unsafe extern "C" fn(Handle),
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    set_property_async: unsafe extern "C" fn(Handle, u64, *const c_char, c_int, *mut c_void) -> c_int,
    observe_property: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    command_async: unsafe extern "C" fn(Handle, u64, *mut *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, f64) -> *mut Event,
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    render_create: unsafe extern "C" fn(*mut RenderCtx, Handle, *mut RenderParam) -> c_int,
    render_set_update_callback: unsafe extern "C" fn(RenderCtx, Option<UpdateCb>, *mut c_void),
    render_update: unsafe extern "C" fn(RenderCtx) -> u64,
    render: unsafe extern "C" fn(RenderCtx, *mut RenderParam) -> c_int,
    render_free: unsafe extern "C" fn(RenderCtx),
}

impl Lib {
    fn open(path: &std::path::Path) -> Result<Lib, String> {
        // SAFETY: 載入 libmpv 並取出公開的 C 函式；簽名照 mpv/client.h、mpv/render.h
        unsafe {
            let lib = libloading::Library::new(path).map_err(|e| e.to_string())?;
            let version: unsafe extern "C" fn() -> std::ffi::c_ulong = *lib.get(b"mpv_client_api_version\0").map_err(|e| e.to_string())?;
            // c_ulong 在 Windows 是 32 位元、Linux 是 64 位元
            #[allow(clippy::unnecessary_cast)]
            let v = version() as u64;
            if v < MIN_API {
                return Err(format!("libmpv 太舊（client API {}.{}）", v >> 16, v & 0xffff));
            }
            macro_rules! f {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| e.to_string())?
                };
            }
            Ok(Lib {
                create: f!("mpv_create"),
                initialize: f!("mpv_initialize"),
                terminate_destroy: f!("mpv_terminate_destroy"),
                set_option_string: f!("mpv_set_option_string"),
                set_property_async: f!("mpv_set_property_async"),
                observe_property: f!("mpv_observe_property"),
                command_async: f!("mpv_command_async"),
                wait_event: f!("mpv_wait_event"),
                error_string: f!("mpv_error_string"),
                render_create: f!("mpv_render_context_create"),
                render_set_update_callback: f!("mpv_render_context_set_update_callback"),
                render_update: f!("mpv_render_context_update"),
                render: f!("mpv_render_context_render"),
                render_free: f!("mpv_render_context_free"),
                _lib: lib,
            })
        }
    }

    fn err(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string 回傳靜態字串
        unsafe { CStr::from_ptr((self.error_string)(code)).to_string_lossy().into_owned() }
    }
}

/// 載入過的 libmpv（成功才記住；下載好之後下一次就載得到）
static LIB: Mutex<Option<Arc<Lib>>> = Mutex::new(None);

/// libmpv 的檔名
pub const LIB_NAME: &str = if cfg!(windows) { "libmpv-2.dll" } else { "libmpv.so.2" };

/// 下載的 libmpv 放在哪裡：exe 旁邊或資料夾（跟 FFmpeg 一樣，見 downloader::download_target）
pub fn candidates() -> Vec<PathBuf> {
    let mut v = vec![crate::paths::app_dir().join(LIB_NAME), crate::paths::data_dir().join(LIB_NAME)];
    if !cfg!(windows) {
        // 系統安裝的（apt install libmpv2）
        v.push(PathBuf::from(LIB_NAME));
    }
    v
}

fn lib() -> Result<Arc<Lib>, String> {
    // 測試沒有 libmpv 的情況（改用 FFmpeg 播放、提示下載）
    if std::env::var_os("SCREENRECORDER_NO_MPV").is_some() {
        return Err("SCREENRECORDER_NO_MPV".into());
    }
    let mut g = LIB.lock().unwrap();
    if let Some(l) = g.as_ref() {
        return Ok(l.clone());
    }
    let mut errors: Vec<String> = Vec::new();
    for p in candidates() {
        // 絕對路徑要檔案存在才試（不讓系統去別的地方找同名的 DLL）
        if p.is_absolute() && !p.is_file() {
            continue;
        }
        match Lib::open(&p) {
            Ok(l) => {
                crate::info!("[mpv] 已載入 {}", p.display());
                let l = Arc::new(l);
                *g = Some(l.clone());
                return Ok(l);
            }
            Err(e) => errors.push(format!("{}：{e}", p.display())),
        }
    }
    let last = if errors.is_empty() { format!("找不到 {LIB_NAME}") } else { errors.join("；") };
    // 載入失敗的原因寫進記錄檔（同樣的原因只寫一次）：例如缺少其他 DLL、版本太舊
    let mut logged = LAST_ERROR.lock().unwrap();
    if *logged != last {
        crate::info!("[mpv] 無法載入播放元件（改用 FFmpeg 播放）：{last}");
        logged.clone_from(&last);
    }
    Err(last)
}

/// 上一次寫進記錄檔的載入失敗原因
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// 程式開啟時在背景先載入（DLL 約 100 MB，第一次載入要將近 1 秒，不要等到播放時才卡住畫面）
pub fn preload() {
    std::thread::spawn(|| {
        let _ = lib();
    });
}

/// 有 libmpv 可以用嗎（會試著載入）
pub fn available() -> bool {
    lib().is_ok()
}

/// 給 mpv 用的 OpenGL 函式查詢（介面的 OpenGL context 提供）
pub type ProcAddress = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

unsafe extern "C" fn get_proc(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: ctx 是 init_gl 裡借出的 &ProcAddress，只在 mpv_render_context_create 期間使用
    let f = unsafe { &*(ctx as *const ProcAddress) };
    f(unsafe { CStr::from_ptr(name) }) as *mut c_void
}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    // SAFETY: ctx 是 Box<Wake>，在 render context 釋放之後才回收
    let wake = unsafe { &*(ctx as *const Wake) };
    wake();
}

/// 從哪裡開始播、速度、音量（0～1）
#[derive(Debug, Clone, Copy)]
pub struct Start {
    pub at: f64,
    pub rate: f64,
    pub volume: f32,
}

/// 播放的狀態（從 mpv 的屬性變更事件收下來；播放、暫停、跳轉時先照要求改，不等 mpv 回應）
#[derive(Debug, Default)]
struct State {
    time: f64,
    paused: bool,
    speed: f64,
    /// 播到最後了（mpv 的 eof-reached）
    ended: bool,
    /// 播完已經回報過（take_ended）
    reported: bool,
}

/// 一支正在播放的影片
pub struct MpvPlayer {
    lib: Arc<Lib>,
    h: Handle,
    rc: RenderCtx,
    /// 有新畫面時叫醒介面（render context 的更新通知用）
    wake: *mut Wake,
    duration: f64,
    st: Mutex<State>,
    /// 開檔失敗（格式不支援…）：介面改用 FFmpeg 播放
    failed: Option<String>,
    loaded: bool,
    /// 已經記下用哪一種解碼（記錄檔，查播放不順用）
    logged_hwdec: bool,
    hwdec: String,
    codec: String,
}

// SAFETY: mpv 的 handle 可以從任何執行緒呼叫；render context 只在介面執行緒使用（見模組說明）
unsafe impl Send for MpvPlayer {}

impl MpvPlayer {
    /// 開始播放 path。gl：介面的 OpenGL（目前的 context）；
    /// software_gl：OpenGL 是軟體算的（沒有顯示卡的虛擬機器、Mesa llvmpipe）
    pub fn new(path: &str, duration: f64, start: Start, wake: Wake, gl: &ProcAddress, software_gl: bool) -> Result<MpvPlayer, String> {
        Self::with_vo(path, duration, start, wake, Some((gl, software_gl)))
    }

    /// gl 是 None 時（測試沒有 OpenGL）畫面輸出用 null
    fn with_vo(path: &str, duration: f64, start: Start, wake: Wake, gl: Option<(&ProcAddress, bool)>) -> Result<MpvPlayer, String> {
        let vo = if gl.is_some() { "libmpv" } else { "null" };
        let Start { at: start, rate, volume } = start;
        let lib = lib()?;
        // SAFETY: 照 libmpv 的用法建立、設定、初始化
        let h = unsafe { (lib.create)() };
        if h.is_null() {
            return Err("mpv_create 失敗".into());
        }
        let mut p = MpvPlayer {
            lib,
            h,
            rc: std::ptr::null_mut(),
            wake: Box::into_raw(Box::new(wake)),
            duration,
            st: Mutex::new(State { time: start, speed: rate, ..Default::default() }),
            failed: None,
            loaded: false,
            logged_hwdec: false,
            hwdec: String::new(),
            codec: String::new(),
        };
        let opts: &[(&str, &str)] = &[
            // 畫進介面（render API），不開自己的視窗
            ("vo", vo),
            // 播完停在最後一張（不關掉檔案）
            ("keep-open", "yes"),
            ("idle", "yes"),
            // 不讀使用者的 mpv 設定、腳本，不接鍵盤滑鼠、不輸出到主控台
            ("config", "no"),
            ("load-scripts", "no"),
            ("ytdl", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("osc", "no"),
            // 字幕不疊在畫面上（跟 FFmpeg 播放一樣）
            ("sub-auto", "no"),
            ("sid", "no"),
            // 跳轉到準確的時間（不是最近的關鍵影格）
            ("hr-seek", "yes"),
            ("audio-client-name", "ScreenRecorder"),
            ("start", &format!("{start:.3}")),
            ("speed", &format!("{rate}")),
            ("volume", &format!("{}", (volume.clamp(0.0, 1.0) * 100.0).round())),
        ];
        for (k, v) in opts {
            p.option(k, v);
        }
        // 顯示卡解碼，解好的畫面複製回來再交給 OpenGL（不用顯示卡和 OpenGL 之間直接共用畫面：
        // 那要看驅動程式支援，嵌在別的程式裡容易出問題）；不支援時自動改用 CPU。舊版 mpv 沒有 auto-copy-safe
        if !p.option("hwdec", "auto-copy-safe") {
            p.option("hwdec", "auto-safe");
        }
        if gl.is_some_and(|(_, software)| software) {
            // 軟體算的 OpenGL（Mesa llvmpipe）用 mpv 預設的高品質縮放會畫出全黑或只剩綠色：改用最簡單的畫法（也最省力）。
            // 有顯示卡時照 mpv 預設
            p.option("gpu-dumb-mode", "yes");
        }
        // 查問題用：SCREENRECORDER_MPV_LOG=v（或 debug）時 mpv 的訊息印到主控台
        if let Ok(level) = std::env::var("SCREENRECORDER_MPV_LOG") {
            p.option("terminal", "yes");
            p.option("msg-level", &format!("all={level}"));
            p.option("gpu-debug", "yes");
        }
        if let Ok(extra) = std::env::var("SCREENRECORDER_MPV_OPTS") {
            for kv in extra.split(',') {
                if let Some((k, v)) = kv.split_once('=') {
                    p.option(k, v);
                }
            }
        }
        // SAFETY: h 是剛建立的 handle
        let r = unsafe { (p.lib.initialize)(p.h) };
        if r < 0 {
            return Err(format!("mpv_initialize：{}", p.lib.err(r)));
        }
        if let Some((gl, _)) = gl {
            p.init_gl(gl)?;
        }
        // 播放中要知道的狀態：變了就送事件過來（不用每一格去問、等 mpv 回答）
        for (name, format) in
            [("time-pos", FORMAT_DOUBLE), ("pause", FORMAT_FLAG), ("eof-reached", FORMAT_FLAG), ("speed", FORMAT_DOUBLE), ("hwdec-current", FORMAT_STRING), ("video-codec", FORMAT_STRING)]
        {
            let n = cstr(name);
            // SAFETY: 字串在呼叫期間有效
            unsafe { (p.lib.observe_property)(p.h, 0, n.as_ptr(), format) };
        }
        p.command(&["loadfile", path])?;
        Ok(p)
    }

    /// 設定選項（開始播放前）；回傳有沒有設定成功
    fn option(&self, k: &str, v: &str) -> bool {
        let (k, v) = (cstr(k), cstr(v));
        // SAFETY: 字串在呼叫期間有效
        let r = unsafe { (self.lib.set_option_string)(self.h, k.as_ptr(), v.as_ptr()) };
        if r < 0 {
            crate::info!("[mpv] 設定 {:?}={:?} 失敗：{}", k, v, self.lib.err(r));
        }
        r >= 0
    }

    /// 送出指令，不等執行完
    fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: 以 NULL 結尾的字串陣列，mpv 立刻複製
        let r = unsafe { (self.lib.command_async)(self.h, 0, ptrs.as_mut_ptr()) };
        if r < 0 {
            return Err(format!("{}：{}", args.first().unwrap_or(&""), self.lib.err(r)));
        }
        Ok(())
    }

    fn set_flag(&self, name: &str, v: bool) {
        let n = cstr(name);
        let mut f: c_int = v.into();
        // SAFETY: MPV_FORMAT_FLAG 用 int；mpv 立刻複製，不等設定完
        unsafe { (self.lib.set_property_async)(self.h, 0, n.as_ptr(), FORMAT_FLAG, &mut f as *mut c_int as *mut c_void) };
    }

    fn set_double(&self, name: &str, v: f64) {
        let n = cstr(name);
        let mut d = v;
        // SAFETY: MPV_FORMAT_DOUBLE 用 double；同上
        unsafe { (self.lib.set_property_async)(self.h, 0, n.as_ptr(), FORMAT_DOUBLE, &mut d as *mut f64 as *mut c_void) };
    }

    /// 在介面的 OpenGL 執行緒（context 是目前的）建立 render context
    fn init_gl(&mut self, proc_address: &ProcAddress) -> Result<(), String> {
        if !self.rc.is_null() {
            return Ok(());
        }
        let api = cstr("opengl");
        let mut init = GlInitParams { get_proc_address: get_proc, ctx: proc_address as *const ProcAddress as *mut c_void };
        let mut params = [
            RenderParam { kind: PARAM_API_TYPE, data: api.as_ptr() as *mut c_void },
            RenderParam { kind: PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut GlInitParams as *mut c_void },
            RenderParam { kind: 0, data: std::ptr::null_mut() },
        ];
        let mut rc: RenderCtx = std::ptr::null_mut();
        // SAFETY: 參數照 render.h；get_proc 只在這個呼叫期間用到 proc_address
        let r = unsafe { (self.lib.render_create)(&mut rc, self.h, params.as_mut_ptr()) };
        if r < 0 {
            return Err(format!("mpv_render_context_create：{}", self.lib.err(r)));
        }
        self.rc = rc;
        // SAFETY: wake 在 render context 釋放後才回收
        unsafe { (self.lib.render_set_update_callback)(rc, Some(on_update), self.wake as *mut c_void) };
        Ok(())
    }

    /// 處理 mpv 的事件（每一格呼叫，不等待）：屬性變更、開檔失敗、播完
    pub fn poll(&mut self) {
        let mut pause_now = false;
        loop {
            // SAFETY: 不等待；回傳的事件在下一次呼叫前有效
            let ev = unsafe { &*(self.lib.wait_event)(self.h, 0.0) };
            match ev.event_id {
                EVENT_NONE => break,
                EVENT_FILE_LOADED => self.loaded = true,
                EVENT_PROPERTY_CHANGE if !ev.data.is_null() => {
                    // SAFETY: PROPERTY_CHANGE 的 data 是 mpv_event_property；值的型別照 format
                    let prop = unsafe { &*(ev.data as *const EventProperty) };
                    let name = unsafe { CStr::from_ptr(prop.name) }.to_bytes();
                    if prop.data.is_null() {
                        continue;
                    }
                    let mut st = self.st.lock().unwrap();
                    match (name, prop.format) {
                        (b"time-pos", FORMAT_DOUBLE) => st.time = unsafe { *(prop.data as *const f64) },
                        (b"speed", FORMAT_DOUBLE) => st.speed = unsafe { *(prop.data as *const f64) },
                        (b"pause", FORMAT_FLAG) => st.paused = unsafe { *(prop.data as *const c_int) } != 0,
                        (b"eof-reached", FORMAT_FLAG) => {
                            let eof = unsafe { *(prop.data as *const c_int) } != 0;
                            if eof && !st.ended {
                                // 播完：跟 FFmpeg 播放一樣停在最後、暫停
                                st.ended = true;
                                st.paused = true;
                                pause_now = true;
                            }
                        }
                        (b"video-codec", FORMAT_STRING) => self.codec = unsafe { CStr::from_ptr(*(prop.data as *const *const c_char)) }.to_string_lossy().into_owned(),
                        (b"hwdec-current", FORMAT_STRING) => self.hwdec = unsafe { CStr::from_ptr(*(prop.data as *const *const c_char)) }.to_string_lossy().into_owned(),
                        _ => {}
                    }
                }
                EVENT_END_FILE => {
                    // SAFETY: END_FILE 的 data 是 mpv_event_end_file
                    let ef = unsafe { &*(ev.data as *const EndFile) };
                    if ef.reason == END_FILE_REASON_ERROR {
                        let e = self.lib.err(ef.error);
                        crate::info!("[mpv] 無法播放：{e}");
                        self.failed = Some(e);
                    }
                }
                _ => {}
            }
        }
        if pause_now {
            self.set_flag("pause", true);
        }
        // 用哪一種解碼（兩個屬性的事件先後不一定）
        if !self.logged_hwdec && !self.hwdec.is_empty() && !self.codec.is_empty() {
            self.logged_hwdec = true;
            let how = if self.hwdec == "no" { "CPU".to_string() } else { format!("顯示卡 {}", self.hwdec) };
            crate::info!("[mpv] 解碼：{how}（{}）", self.codec);
        }
    }

    /// 開檔失敗的原因（介面改用 FFmpeg 播放）
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// 已經開始播放（讀到檔案了）
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// 有新畫面要畫
    pub fn wants_render(&self) -> bool {
        // SAFETY: 介面執行緒呼叫
        !self.rc.is_null() && unsafe { (self.lib.render_update)(self.rc) } & UPDATE_FRAME != 0
    }

    /// 畫到 FBO（w × h 像素）；flip：上下顛倒
    pub fn render(&self, fbo: u32, w: u32, h: u32, flip: bool) {
        if self.rc.is_null() {
            return;
        }
        let mut target = GlFbo { fbo: fbo as c_int, w: w as c_int, h: h as c_int, internal_format: 0 };
        let mut flip_y: c_int = flip.into();
        // 不要等到畫面該顯示的時間（會卡住介面），mpv 已經照時間給畫面
        let mut block: c_int = 0;
        let mut params = [
            RenderParam { kind: PARAM_OPENGL_FBO, data: &mut target as *mut GlFbo as *mut c_void },
            RenderParam { kind: PARAM_FLIP_Y, data: &mut flip_y as *mut c_int as *mut c_void },
            RenderParam { kind: PARAM_BLOCK_FOR_TARGET_TIME, data: &mut block as *mut c_int as *mut c_void },
            RenderParam { kind: 0, data: std::ptr::null_mut() },
        ];
        // SAFETY: OpenGL context 是目前的（介面執行緒）
        let r = unsafe { (self.lib.render)(self.rc, params.as_mut_ptr()) };
        if r < 0 {
            crate::info!("[mpv] 畫面輸出失敗：{}", self.lib.err(r));
        }
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// 目前播到幾秒
    pub fn time(&self) -> f64 {
        self.st.lock().unwrap().time.clamp(0.0, self.duration.max(0.0))
    }

    pub fn is_playing(&self) -> bool {
        let st = self.st.lock().unwrap();
        !st.paused && !st.ended
    }

    /// 播完了（只回報一次）
    pub fn take_ended(&self) -> bool {
        let mut st = self.st.lock().unwrap();
        let first = st.ended && !st.reported;
        st.reported |= st.ended;
        first
    }

    pub fn play(&self) {
        let ended = {
            let mut st = self.st.lock().unwrap();
            let ended = st.ended;
            if ended {
                // 播完之後再按播放：從頭開始
                st.ended = false;
                st.reported = false;
                st.time = 0.0;
            }
            st.paused = false;
            ended
        };
        if ended {
            let _ = self.command(&["seek", "0", "absolute+exact"]);
        }
        self.set_flag("pause", false);
    }

    pub fn pause(&self) {
        self.st.lock().unwrap().paused = true;
        self.set_flag("pause", true);
    }

    pub fn seek(&self, t: f64) {
        let t = t.clamp(0.0, self.duration.max(0.0));
        {
            let mut st = self.st.lock().unwrap();
            st.time = t;
            st.ended = false;
            st.reported = false;
        }
        let _ = self.command(&["seek", &format!("{t:.3}"), "absolute+exact"]);
    }

    pub fn set_rate(&self, rate: f64) {
        self.st.lock().unwrap().speed = rate;
        self.set_double("speed", rate);
    }

    pub fn rate(&self) -> f64 {
        self.st.lock().unwrap().speed
    }

    pub fn set_volume(&self, v: f32) {
        self.set_double("volume", (v.clamp(0.0, 1.0) * 100.0) as f64);
    }
}

/// 在背景結束 mpv 用（handle 可以在任何執行緒使用）
struct SendHandle(Handle);
// SAFETY: 同上
unsafe impl Send for SendHandle {}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        // SAFETY: 先釋放 render context（OpenGL 執行緒），之後不會再呼叫 on_update，才回收 wake
        unsafe {
            if !self.rc.is_null() {
                (self.lib.render_free)(self.rc);
            }
            drop(Box::from_raw(self.wake));
        }
        // 結束 mpv 要等它關掉檔案、聲音：在背景做，不卡住介面
        let (lib, h) = (self.lib.clone(), SendHandle(self.h));
        std::thread::spawn(move || {
            let h = h;
            // SAFETY: handle 之後不再使用
            unsafe { (lib.terminate_destroy)(h.0) };
        });
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 測試影片：2 秒、30 fps、有聲音
    fn test_video(dir: &std::path::Path) -> Option<String> {
        let ffmpeg = ["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"].iter().map(PathBuf::from).find(|p| p.exists())?;
        let out = dir.join("t.mp4");
        let ok = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=160x90:r=30:d=2", "-f", "lavfi", "-i", "sine=f=440:d=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", "-y"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        ok.then(|| out.display().to_string())
    }

    fn wait_until(p: &mut MpvPlayer, secs: f64, f: impl Fn(&MpvPlayer) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f64() < secs {
            p.poll();
            if f(p) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// 沒有 libmpv（apt install libmpv2）時略過
    #[test]
    fn plays_seeks_and_ends() {
        if !available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = test_video(dir.path()) else { return };
        let mut p = MpvPlayer::with_vo(&path, 2.0, Start { at: 0.5, rate: 1.0, volume: 0.0 }, Arc::new(|| {}), None).unwrap();
        assert!(wait_until(&mut p, 5.0, |p| p.loaded()), "讀不到檔案：{:?}", p.failed());
        assert!(wait_until(&mut p, 5.0, |p| p.time() > 0.7), "沒有往前播：{}", p.time());
        assert!(p.is_playing());
        p.pause();
        assert!(wait_until(&mut p, 2.0, |p| !p.is_playing()));
        p.seek(1.2);
        assert!(wait_until(&mut p, 3.0, |p| (p.time() - 1.2).abs() < 0.05), "跳轉：{}", p.time());
        p.set_rate(2.0);
        assert!((p.rate() - 2.0).abs() < 1e-9);
        p.play();
        assert!(wait_until(&mut p, 5.0, |p| p.take_ended()), "沒有播完：{}", p.time());
        assert!(!p.is_playing() && !p.take_ended(), "播完只回報一次");
        // 播完再按播放：從頭開始
        p.play();
        assert!(wait_until(&mut p, 3.0, |p| p.is_playing() && p.time() < 1.0), "{}", p.time());
    }

    #[test]
    fn reports_files_it_cannot_play() {
        if !available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mp4");
        std::fs::write(&bad, b"not a video").unwrap();
        let mut p = MpvPlayer::with_vo(&bad.display().to_string(), 2.0, Start { at: 0.0, rate: 1.0, volume: 0.0 }, Arc::new(|| {}), None).unwrap();
        assert!(wait_until(&mut p, 5.0, |p| p.failed().is_some()));
    }
}
