//! 攝影機小視窗（Windows）：選了攝影機時，錄影前（主畫面開著）與倒數、錄影、暫停期間，在擷取範圍裡顯示攝影機畫面
//! （圓形或圓角方形、白框）。小視窗不設成「不被擷取」，直接錄進影片，看到的就是錄到的。
//! - 拖曳中間移動、拖曳外圍一圈或轉動滑鼠滾輪調整大小（都限制在擷取範圍內，才會被錄到）；放開後記住位置與大小
//! - 攝影機畫面由另一個 FFmpeg 讀進來（BGRA，這個範圍裡小視窗最大時的大小），程式裡縮放成小視窗的大小、
//!   套上形狀後用 UpdateLayeredWindow 顯示；調整大小時不用重開攝影機。透明的角落點得穿，不搶焦點
//! - 錄影前後同一個小視窗、同一個攝影機連線接著用，不會關了又開
//!
//! 座標是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::camera_bubble::{bubble_rect, clamp_into, grab_at, reader_args, relative_pos, resample, resize_pct, size_px, source_size, BubbleInfo, Grab, Mask};
use crate::types::Rect;
use crate::{tr, trf};
use std::cell::Cell;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessagePos, LoadCursorW, PeekMessageW, RegisterClassExW, SetCursor, SetWindowPos, ShowWindow, TranslateMessage,
    UpdateLayeredWindow, HWND_TOPMOST, IDC_SIZEALL, IDC_SIZENWSE, MA_NOACTIVATE, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, ULW_ALPHA, WM_CAPTURECHANGED,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 用 FFmpeg 的測試畫面代替攝影機（沒有攝影機的電腦上測試小視窗用）
const TEST_ENV: &str = "SCREENRECORDER_TEST_CAMERA";

/// 滑鼠操作（視窗程序寫入、迴圈讀取）
#[derive(Clone, Copy)]
enum Drag {
    /// 移動：按下時的游標位置與小視窗左上角
    Move { start: POINT, origin: (i32, i32) },
    /// 調整大小：小視窗中心不動
    Resize { center: (i32, i32) },
}

thread_local! {
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };
    /// 剛放開的操作與放開時的游標位置（按下、放開在同一輪訊息裡處理完也不會漏掉）
    static RELEASED: Cell<Option<(Drag, POINT)>> = const { Cell::new(None) };
    /// 小視窗目前的左上角、邊長、是否圓形（視窗程序判斷按在哪裡用）
    static GEOM: Cell<(i32, i32, i32, bool)> = const { Cell::new((0, 0, 0, true)) };
    /// 滾輪累積的格數（正 = 放大）
    static WHEEL: Cell<i32> = const { Cell::new(0) };
}

/// 背景執行緒：每 0.1 秒問一次 info()（None = 不顯示），跟著顯示、移動、調整或關掉小視窗與讀攝影機的 FFmpeg。
/// 攝影機打不開或中斷時呼叫 failed()，錄影結束（或小視窗收起）前不再試。
/// 拖曳、調整大小後呼叫 moved(位置, 大小)：小視窗中心在擷取範圍內的相對位置（萬分比）與大小（短邊的百分比）
pub fn spawn(
    info: impl Fn() -> Option<BubbleInfo> + Send + 'static,
    ffmpeg: impl Fn() -> Option<PathBuf> + Send + 'static,
    failed: impl Fn(String) + Send + 'static,
    moved: impl Fn([u16; 2], u32) + Send + 'static,
) {
    let _ = std::thread::Builder::new().name("camera-bubble".into()).spawn(move || unsafe { run(info, ffmpeg, failed, moved) });
}

/// 小視窗大小的點陣圖（BGRA，預乘透明度）
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    size: i32,
}

impl Surface {
    unsafe fn new(size: i32) -> Option<Surface> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen = GetDC(None);
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let bmp = bmp.filter(|_| !bits.is_null())?;
        let old = SelectObject(dc, bmp.into());
        Some(Surface { dc, bmp, old, bits: bits as *mut u8, size })
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.size * self.size * 4) as usize) }
    }

    unsafe fn present(&self, hwnd: HWND, (x, y): (i32, i32)) {
        let screen = GetDC(None);
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (pt, size, src) = (POINT { x, y }, SIZE { cx: self.size, cy: self.size }, POINT { x: 0, y: 0 });
        let _ = UpdateLayeredWindow(hwnd, Some(screen), Some(&pt), Some(&size), Some(self.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        ReleaseDC(None, screen);
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// 讀攝影機的 FFmpeg：另一條執行緒一直讀，最新一張放在 frame（編號, 像素）
struct Reader {
    child: Child,
    frame: Arc<Mutex<(u64, Vec<u8>)>>,
    /// stdout 讀完了（FFmpeg 結束）
    ended: Arc<Mutex<bool>>,
    /// 最後幾行錯誤訊息
    errors: Arc<Mutex<String>>,
}

impl Reader {
    fn start(ffmpeg: &PathBuf, device: &str, size: i32) -> std::io::Result<Reader> {
        let test = std::env::var_os(TEST_ENV).is_some();
        let mut child = crate::process::std_command(ffmpeg).args(reader_args(device, size, test)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let frame = Arc::new(Mutex::new((0u64, Vec::new())));
        let ended = Arc::new(Mutex::new(false));
        let errors = Arc::new(Mutex::new(String::new()));
        if let Some(mut out) = child.stdout.take() {
            let (frame, ended) = (frame.clone(), ended.clone());
            let n = (size * size * 4) as usize;
            let _ = std::thread::Builder::new().name("camera-read".into()).spawn(move || {
                let mut buf = vec![0u8; n];
                while out.read_exact(&mut buf).is_ok() {
                    let mut f = frame.lock().unwrap();
                    f.0 += 1;
                    std::mem::swap(&mut f.1, &mut buf);
                    if buf.len() != n {
                        buf = vec![0u8; n];
                    }
                }
                *ended.lock().unwrap() = true;
            });
        }
        if let Some(err) = child.stderr.take() {
            let errors = errors.clone();
            let _ = std::thread::Builder::new().name("camera-err".into()).spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let mut e = errors.lock().unwrap();
                    e.push_str(line.trim());
                    e.push('\n');
                    if e.len() > 600 {
                        let cut = (e.len() - 600..e.len()).find(|i| e.is_char_boundary(*i)).unwrap_or(e.len());
                        e.drain(..cut);
                    }
                }
            });
        }
        Ok(Reader { child, frame, ended, errors })
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 目前顯示中的小視窗
struct Shown {
    /// 最後套用的範圍與攝影機設定
    info: BubbleInfo,
    pos: (i32, i32),
    /// 大小（短邊的百分比）與邊長
    pct: u32,
    size: i32,
    surface: Surface,
    mask: Mask,
    reader: Option<Reader>,
    /// 攝影機畫面的邊長、最新一張（還沒有時是空的）、縮放用的暫存
    src: i32,
    raw: Vec<u8>,
    scaled: Vec<u8>,
    seq: u64,
}

impl Shown {
    fn rect(&self) -> Rect {
        Rect { x: self.pos.0, y: self.pos.1, width: self.size, height: self.size }
    }

    /// 把目前的攝影機畫面（還沒有時是灰色）縮放、套上形狀，畫到小視窗
    unsafe fn render(&mut self, hwnd: HWND) {
        let n = (self.size * self.size * 4) as usize;
        self.scaled.resize(n, 0);
        if self.raw.is_empty() {
            self.scaled.fill(0x40);
        } else {
            resample(&self.raw, self.src as usize, &mut self.scaled, self.size as usize);
        }
        self.mask.apply(&self.scaled, self.surface.pixels());
        self.surface.present(hwnd, self.pos);
    }

    /// 拖曳到游標位置 p
    unsafe fn drag_to(&mut self, hwnd: HWND, drag: Drag, p: POINT) {
        match drag {
            // 移動：跟著游標，限制在範圍內
            Drag::Move { start, origin } => {
                let r = clamp_into(Rect { x: origin.0 + p.x - start.x, y: origin.1 + p.y - start.y, width: self.size, height: self.size }, &self.info.area);
                if (r.x, r.y) != self.pos {
                    self.pos = (r.x, r.y);
                    self.surface.present(hwnd, self.pos);
                }
            }
            // 調整大小：中心不動，游標離中心多遠就多大（短邊的整數百分比）
            Drag::Resize { center } => {
                let pct = resize_pct(&self.info.area, center, (p.x, p.y), self.info.camera.circle);
                if pct != self.pct {
                    let d = size_px(&self.info.area, pct);
                    if self.place(hwnd, clamp_into(Rect { x: center.0 - d / 2, y: center.1 - d / 2, width: d, height: d }, &self.info.area)) {
                        self.pct = pct;
                    }
                }
            }
        }
    }

    /// 換位置或大小（大小變了要換點陣圖與形狀）
    unsafe fn place(&mut self, hwnd: HWND, r: Rect) -> bool {
        if r.width != self.size {
            let Some(surface) = Surface::new(r.width) else { return false };
            self.surface = surface;
            self.mask = Mask::new(r.width as usize, self.info.camera.circle);
            self.size = r.width;
        }
        self.pos = (r.x, r.y);
        self.render(hwnd);
        true
    }
}

unsafe fn run(info: impl Fn() -> Option<BubbleInfo>, ffmpeg: impl Fn() -> Option<PathBuf>, failed: impl Fn(String), moved: impl Fn([u16; 2], u32)) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderCameraBubble");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc_), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    // 標題不用「螢幕錄影 v」開頭：開始錄影時縮小操作視窗不會縮到它
    let hwnd = match CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, w!("攝影機"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
        Ok(h) => h,
        Err(e) => {
            crate::warn!("無法建立攝影機小視窗：{e}");
            return;
        }
    };
    let mut shown: Option<Shown> = None;
    // 攝影機打不開或中斷：小視窗收起（錄影結束、主畫面關掉）前不再試
    let mut gave_up = false;
    let mut raised = Instant::now();
    // 讀攝影機的 FFmpeg 結束了：(已經有過畫面, 錯誤訊息)
    let mut dead: Option<(bool, String)> = None;
    // 最後一次存位置、大小的時間
    let mut saved_at: Option<Instant> = None;
    // info() 要讀設定，每 0.1 秒問一次就好（畫面每 33ms 更新）
    let mut want: Option<BubbleInfo> = None;
    let mut polled: Option<Instant> = None;
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let dragging = DRAG.with(|d| d.get());
        let released = RELEASED.with(|r| r.take());
        if polled.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
            want = info().filter(|i| !i.camera.device.trim().is_empty());
            polled = Some(Instant::now());
        }
        // 剛放開，或滾輪調了大小：記住位置與大小（設定裡的值也改成這樣，下一次 info() 不會把小視窗拉回去）
        let wheel = WHEEL.with(|w| w.replace(0));
        if let Some(s) = shown.as_mut() {
            // 放開時的位置：最後一次套用拖曳
            let mut changed = match released {
                Some((drag, p)) => {
                    s.drag_to(hwnd, drag, p);
                    true
                }
                None => false,
            };
            if wheel != 0 && dragging.is_none() {
                let pct = (s.pct as i32 + wheel).clamp(crate::camera_bubble::SIZE_MIN as i32, crate::camera_bubble::SIZE_MAX as i32) as u32;
                if pct != s.pct {
                    let (cx, cy) = (s.pos.0 + s.size / 2, s.pos.1 + s.size / 2);
                    let d = size_px(&s.info.area, pct);
                    if s.place(hwnd, clamp_into(Rect { x: cx - d / 2, y: cy - d / 2, width: d, height: d }, &s.info.area)) {
                        s.pct = pct;
                        changed = true;
                    }
                }
            }
            if changed {
                let pos = relative_pos(&s.rect(), &s.info.area);
                s.info.camera.pos = Some(pos);
                s.info.camera.size = s.pct;
                moved(pos, s.pct);
                saved_at = Some(Instant::now());
            }
        }
        // 剛存的位置、大小傳回介面要一點時間：這 1 秒內 info() 的位置、大小還是舊的，不要把小視窗拉回去
        if let (Some(w), Some(s), Some(t)) = (want.as_mut(), shown.as_ref(), saved_at) {
            if t.elapsed() < Duration::from_secs(1) && w.camera.device == s.info.camera.device {
                w.camera.pos = s.info.camera.pos;
                w.camera.size = s.info.camera.size;
            } else {
                saved_at = None;
            }
        }
        match (want.clone(), shown.as_mut()) {
            (None, _) => {
                if let Some(s) = shown.take() {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    if let Some(r) = s.reader {
                        r.stop();
                    }
                }
                gave_up = false;
            }
            (Some(_), None) if gave_up => {}
            (Some(i), None) => {
                let rect = bubble_rect(&i);
                let src = source_size(&i.area);
                let Some(surface) = Surface::new(rect.width) else {
                    crate::warn!("無法建立攝影機小視窗的畫面");
                    gave_up = true;
                    continue;
                };
                let reader = match ffmpeg().map(|p| Reader::start(&p, &i.camera.device, src)) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => {
                        failed(trf!("無法讀取攝影機：{e}", "Couldn't read the camera: {e}"));
                        gave_up = true;
                        continue;
                    }
                    None => {
                        gave_up = true;
                        continue;
                    }
                };
                let mut s = Shown {
                    mask: Mask::new(rect.width as usize, i.camera.circle),
                    pct: i.camera.size,
                    info: i,
                    pos: (rect.x, rect.y),
                    size: rect.width,
                    surface,
                    reader: Some(reader),
                    src,
                    raw: Vec::new(),
                    scaled: Vec::new(),
                    seq: 0,
                };
                // 攝影機開好之前先顯示灰色的形狀，知道小視窗在哪裡
                s.render(hwnd);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                shown = Some(s);
                raised = Instant::now();
            }
            (Some(i), Some(s)) => {
                if i.camera.device != s.info.camera.device {
                    // 換了攝影機：關掉重開（下一輪）
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    if let Some(r) = shown.take().and_then(|s| s.reader) {
                        r.stop();
                    }
                    continue;
                }
                if dragging.is_none() && (i.area != s.info.area || i.camera != s.info.camera) {
                    if i.camera == s.info.camera {
                        // 只有範圍移動（只錄某個視窗、拖曳錄影範圍）：小視窗跟著移，相對位置不變
                        let (dx, dy) = (i.area.x - s.info.area.x, i.area.y - s.info.area.y);
                        let r = clamp_into(Rect { x: s.pos.0 + dx, y: s.pos.1 + dy, width: s.size, height: s.size }, &i.area);
                        s.info.area = i.area;
                        s.place(hwnd, r);
                    } else {
                        // 設定改了（角落、大小、形狀），或錄影開始後範圍不同：照設定重新擺
                        let circle_changed = i.camera.circle != s.info.camera.circle;
                        let r = bubble_rect(&i);
                        s.pct = i.camera.size;
                        s.info = i;
                        if circle_changed {
                            s.mask = Mask::new(s.size as usize, s.info.camera.circle);
                        }
                        s.place(hwnd, r);
                    }
                }
                if let Some(drag) = dragging {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    s.drag_to(hwnd, drag, p);
                }
                // 新的攝影機畫面
                let mut fresh = false;
                if let Some(r) = &s.reader {
                    let mut f = r.frame.lock().unwrap();
                    if f.0 != s.seq && f.1.len() == (s.src * s.src * 4) as usize {
                        s.seq = f.0;
                        std::mem::swap(&mut s.raw, &mut f.1);
                        fresh = true;
                    }
                }
                if fresh {
                    s.render(hwnd);
                }
                // 讀攝影機的 FFmpeg 結束了：打不開（還沒有任何畫面），或中途中斷（拔掉了）。
                // 等這一輪結束再關（s 還借用著 shown）
                if s.reader.as_ref().is_some_and(|r| *r.ended.lock().unwrap()) {
                    let detail = s.reader.as_ref().map(|r| r.errors.lock().unwrap().lines().last().unwrap_or("").to_string()).unwrap_or_default();
                    dead = Some((s.seq > 0, detail));
                } else if raised.elapsed() > Duration::from_secs(2) {
                    // 其他最上層視窗出現時可能被蓋住：定時移回最上層
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
                    raised = Instant::now();
                }
            }
        }
        if let Some((started, detail)) = dead.take() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some(r) = shown.take().and_then(|s| s.reader) {
                r.stop();
            }
            gave_up = true;
            let detail = if detail.is_empty() { String::new() } else { trf!("（{detail}）", " ({detail})") };
            failed(if started {
                trf!("攝影機中斷{detail}，之後的錄影不含攝影機", "The camera disconnected{detail}. The rest of the recording won't include the camera.")
            } else if detail.is_empty() {
                tr!("攝影機無法開啟（可能被其他程式使用中），錄影不含攝影機", "Couldn't open the camera (another app may be using it). The recording won't include the camera.").into()
            } else {
                trf!("攝影機無法開啟{detail}，錄影不含攝影機", "Couldn't open the camera{detail}. The recording won't include the camera.")
            });
        }
        if let Some(s) = &shown {
            GEOM.with(|g| g.set((s.pos.0, s.pos.1, s.size, s.info.camera.circle)));
        }
        // 拖曳中 15ms；顯示中 33ms（攝影機每秒 30 張）；沒有小視窗時 250ms 看一次就好
        std::thread::sleep(Duration::from_millis(if dragging.is_some() {
            15
        } else if shown.is_some() {
            33
        } else {
            250
        }));
    }
}

/// 放開滑鼠（或失去滑鼠擷取）：記下放開時的位置，迴圈最後套用一次再存起來
fn release() {
    if let Some(drag) = DRAG.with(|d| d.take()) {
        RELEASED.with(|r| r.set(Some((drag, message_pos()))));
    }
}

/// 目前處理的滑鼠訊息發生時的游標位置（迴圈每 33ms 才處理一次訊息，游標可能已經移開）
fn message_pos() -> POINT {
    let pos = unsafe { GetMessagePos() };
    POINT { x: (pos & 0xFFFF) as u16 as i16 as i32, y: (pos >> 16) as u16 as i16 as i32 }
}

/// 游標在小視窗的哪裡（螢幕座標）
fn grab_screen(p: POINT) -> Option<Grab> {
    let (x, y, size, circle) = GEOM.with(|g| g.get());
    grab_at(p.x - x, p.y - y, size, circle)
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 點了也不搶焦點（正在打字的視窗不受影響）
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let resize = match DRAG.with(|d| d.get()) {
                Some(Drag::Resize { .. }) => true,
                Some(Drag::Move { .. }) => false,
                None => grab_screen(p) == Some(Grab::Resize),
            };
            if let Ok(c) = LoadCursorW(None, if resize { IDC_SIZENWSE } else { IDC_SIZEALL }) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let p = message_pos();
            let (x, y, size, _) = GEOM.with(|g| g.get());
            let drag = match grab_screen(p) {
                Some(Grab::Resize) => Some(Drag::Resize { center: (x + size / 2, y + size / 2) }),
                Some(Grab::Move) => Some(Drag::Move { start: p, origin: (x, y) }),
                None => None,
            };
            if drag.is_some() {
                DRAG.with(|d| d.set(drag));
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => LRESULT(0),
        WM_MOUSEWHEEL => {
            // 滾輪往前放大、往後縮小；每格 1%
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            WHEEL.with(|w| w.set(w.get() + if delta > 0 { 1 } else { -1 }));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            release();
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            release();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
