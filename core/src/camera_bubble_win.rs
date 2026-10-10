//! 攝影機小窗（Windows）：倒數、錄影、暫停期間，在擷取範圍的角落顯示攝影機畫面（圓形或圓角方形、白框），
//! 可以拖曳移動（只能在擷取範圍內，才會被錄到）。小窗不設成「不被擷取」，直接錄進影片。
//! 攝影機畫面由另一個 FFmpeg 讀進來（BGRA 原始像素），每張畫面套上形狀後用 UpdateLayeredWindow 顯示；
//! 透明的角落點得穿。不搶焦點。座標是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::camera_bubble::{bubble_rect, clamp_into, reader_args, BubbleInfo, Mask};
use crate::types::Rect;
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
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, LoadCursorW, PeekMessageW, RegisterClassExW, SetCursor, SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow,
    HWND_TOPMOST, IDC_SIZEALL, MA_NOACTIVATE, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, ULW_ALPHA, WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 用 FFmpeg 的測試畫面代替攝影機（沒有攝影機的電腦上測試小窗用）
const TEST_ENV: &str = "SCREENRECORDER_TEST_CAMERA";

thread_local! {
    /// 拖曳中：按下時的游標位置、小窗位置
    static DRAG: Cell<Option<(POINT, (i32, i32))>> = const { Cell::new(None) };
}

/// 背景執行緒：每 0.1 秒問一次 info()（None = 不顯示），跟著顯示、移動或關掉小窗與讀攝影機的 FFmpeg。
/// 攝影機打不開（被其他程式使用、拔掉了）時呼叫 failed()，這次錄影不再試
pub fn spawn(info: impl Fn() -> Option<BubbleInfo> + Send + 'static, ffmpeg: impl Fn() -> Option<PathBuf> + Send + 'static, failed: impl Fn(String) + Send + 'static) {
    let _ = std::thread::Builder::new().name("camera-bubble".into()).spawn(move || unsafe { run(info, ffmpeg, failed) });
}

/// 小窗大小的點陣圖（BGRA，預乘透明度）
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

/// 目前顯示中的小窗
struct Shown {
    info: BubbleInfo,
    pos: (i32, i32),
    surface: Surface,
    mask: Mask,
    reader: Option<Reader>,
    /// 已經畫到第幾張
    seq: u64,
}

unsafe fn run(info: impl Fn() -> Option<BubbleInfo>, ffmpeg: impl Fn() -> Option<PathBuf>, failed: impl Fn(String)) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderCameraBubble");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc_), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    // 標題不用「螢幕錄影 v」開頭：開始錄影時縮小操作視窗不會縮到它
    let hwnd = match CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, w!("攝影機"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
        Ok(h) => h,
        Err(e) => {
            crate::warn!("無法建立攝影機小窗：{e}");
            return;
        }
    };
    let mut shown: Option<Shown> = None;
    // 這次錄影的攝影機打不開：錄影結束前不再試
    let mut gave_up = false;
    let mut raised = Instant::now();
    // 讀攝影機的 FFmpeg 結束了：(已經有過畫面, 錯誤訊息)
    let mut dead: Option<(bool, String)> = None;
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let want = info().filter(|i| !i.camera.device.trim().is_empty());
        let dragging = DRAG.with(|d| d.get());
        match (want, shown.as_mut()) {
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
                let Some(surface) = Surface::new(rect.width) else {
                    crate::warn!("無法建立攝影機小窗的畫面");
                    gave_up = true;
                    continue;
                };
                let reader = match ffmpeg().map(|p| Reader::start(&p, &i.camera.device, rect.width)) {
                    Some(Ok(r)) => Some(r),
                    Some(Err(e)) => {
                        failed(format!("無法讀取攝影機：{e}"));
                        gave_up = true;
                        continue;
                    }
                    None => {
                        gave_up = true;
                        continue;
                    }
                };
                let mut s = Shown { mask: Mask::new(rect.width as usize, i.camera.circle), info: i, pos: (rect.x, rect.y), surface, reader, seq: 0 };
                // 攝影機開好之前先顯示灰色的形狀，知道小窗在哪裡
                let gray = vec![0x40u8; (rect.width * rect.width * 4) as usize];
                s.mask.apply(&gray, s.surface.pixels());
                s.surface.present(hwnd, s.pos);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                shown = Some(s);
                raised = Instant::now();
            }
            (Some(i), Some(s)) => {
                // 範圍移動（只錄某個視窗、拖曳錄影範圍）：小窗跟著移，相對位置不變
                if i.area != s.info.area && dragging.is_none() {
                    let (dx, dy) = (i.area.x - s.info.area.x, i.area.y - s.info.area.y);
                    let r = clamp_into(Rect { x: s.pos.0 + dx, y: s.pos.1 + dy, width: s.surface.size, height: s.surface.size }, &i.area);
                    s.pos = (r.x, r.y);
                    s.surface.present(hwnd, s.pos);
                    s.info.area = i.area;
                }
                // 拖曳中：跟著游標，限制在範圍內
                if let Some((start, origin)) = dragging {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    let r = clamp_into(Rect { x: origin.0 + p.x - start.x, y: origin.1 + p.y - start.y, width: s.surface.size, height: s.surface.size }, &s.info.area);
                    if (r.x, r.y) != s.pos {
                        s.pos = (r.x, r.y);
                        s.surface.present(hwnd, s.pos);
                    }
                }
                POS.with(|c| c.set(s.pos));
                // 新的攝影機畫面：直接套上形狀寫進點陣圖
                let mut fresh = false;
                if let Some(r) = &s.reader {
                    let f = r.frame.lock().unwrap();
                    if f.0 != s.seq && f.1.len() == (s.surface.size * s.surface.size * 4) as usize {
                        s.seq = f.0;
                        s.mask.apply(&f.1, s.surface.pixels());
                        fresh = true;
                    }
                }
                if fresh {
                    s.surface.present(hwnd, s.pos);
                }
                // 讀攝影機的 FFmpeg 結束了：打不開（還沒有任何畫面），或錄到一半中斷（拔掉了）。
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
            let detail = if detail.is_empty() { String::new() } else { format!("（{detail}）") };
            failed(if started {
                format!("攝影機中斷{detail}，之後的錄影不含攝影機")
            } else if detail.is_empty() {
                "攝影機無法開啟（可能被其他程式使用中），這次錄影不含攝影機".into()
            } else {
                format!("攝影機無法開啟{detail}，這次錄影不含攝影機")
            });
        }
        // 拖曳中 15ms；顯示中 33ms（攝影機每秒 30 張）；沒有小窗時 250ms 看一次就好
        std::thread::sleep(Duration::from_millis(if dragging.is_some() {
            15
        } else if shown.is_some() {
            33
        } else {
            250
        }));
    }
}

thread_local! {
    /// 小窗目前的位置（開始拖曳時的起點）
    static POS: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 點了也不搶焦點（正在打字的視窗不受影響）
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            if let Ok(c) = LoadCursorW(None, IDC_SIZEALL) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            DRAG.with(|d| d.set(Some((p, POS.with(|c| c.get())))));
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => LRESULT(0),
        WM_LBUTTONUP => {
            DRAG.with(|d| d.set(None));
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            DRAG.with(|d| d.set(None));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
