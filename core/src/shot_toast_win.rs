//! 截圖後的小縮圖（Windows）：分層視窗（圓角、半透明），放在游標所在螢幕的右下角，
//! 不搶焦點、不被擷取。點縮圖或「編輯」開啟編輯；滑鼠不在上面時 7 秒後自動消失；刪除後顯示「已移到資源回收筒」再消失。
//! 一次只有一個：新的截圖出現時，舊的先關掉。

use crate::shot_toast::{hit, render, thumbnail, Button, Hit, Layout};
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetCursor, SetTimer,
    SetWindowDisplayAffinity, ShowWindow, TranslateMessage, UpdateLayeredWindow, IDC_ARROW, IDC_HAND, MSG, SW_SHOWNOACTIVATE, ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 按下按鈕要做的事
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Edit,
    Pin,
    /// 在檔案總管顯示（選取這個檔案）
    Reveal,
    Delete,
}

const WM_MOUSELEAVE: u32 = 0x02A3;
const MA_NOACTIVATE: isize = 3;
/// 沒碰它多久後消失；刪除後的訊息顯示多久
const LIFETIME: Duration = Duration::from_secs(7);
const DELETED_LIFETIME: Duration = Duration::from_millis(2500);

/// 目前顯示中的（新的出現時把它關掉）
static CURRENT: Mutex<Option<isize>> = Mutex::new(None);

/// 按鈕的處理函式：(要做的事, 截圖的路徑)；回傳是否做到了（刪除成功才顯示「已移到資源回收筒」）
type OnCmd = std::rc::Rc<dyn Fn(Cmd, &str) -> bool>;

struct State {
    path: String,
    thumb: Pixmap,
    scale: f32,
    layout: Layout,
    hover: Option<Hit>,
    pos: (i32, i32),
    inside: bool,
    since: Instant,
    on: OnCmd,
    /// 圖下面的說明（「已存檔並複製到剪貼簿」）
    caption: String,
    /// 已經移到資源回收筒
    deleted: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 顯示這張截圖的小縮圖；按鈕交給 on 處理。thumb：已經做好的縮圖（沒有時讀取 path 再縮小）；caption：圖下面的說明
pub fn show(path: String, thumb: Option<Pixmap>, caption: String, on: impl Fn(Cmd, &str) -> bool + Send + 'static) {
    if let Some(h) = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).take() {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    let _ = std::thread::Builder::new().name("shot-toast".into()).spawn(move || unsafe { run(path, thumb, caption, std::rc::Rc::new(on)) });
}

unsafe fn run(path: String, thumb: Option<Pixmap>, caption: String, on: OnCmd) {
    let thumb = match thumb {
        Some(t) => t,
        None => {
            let Some(src) = std::fs::read(&path).ok().and_then(|b| Pixmap::decode_png(&b).ok()) else { return };
            let Some(t) = thumbnail(&src, 640) else { return };
            t
        }
    };
    // 游標所在螢幕的工作區右下角
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    let mon = MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut mi).as_bool() {
        return;
    }
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let scale = (dx as f32 / 96.0).max(1.0);
    let Some((pm, layout)) = render(&thumb, scale, None, &caption, false) else { return };
    let gap = (16.0 * scale) as i32;
    let pos = (mi.rcWork.right - layout.w as i32 - gap, mi.rcWork.bottom - layout.h as i32 - gap);
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderShotToast");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE;
    let Ok(hwnd) = CreateWindowExW(ex, class, w!("截圖"), WS_POPUP, pos.0, pos.1, layout.w as i32, layout.h as i32, None, None, Some(hinst.into()), None) else { return };
    // 不被下一次截圖、錄影拍進去
    let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
    present(hwnd, &pm, pos);
    STATE.with(|s| *s.borrow_mut() = Some(State { path, thumb, scale, layout, hover: None, pos, inside: false, since: Instant::now(), on, caption, deleted: false }));
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    SetTimer(Some(hwnd), 1, 200, None);
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    STATE.with(|s| *s.borrow_mut() = None);
}

/// 把圖（預乘 RGBA）放到分層視窗上
pub(crate) unsafe fn present(hwnd: HWND, pm: &Pixmap, (x, y): (i32, i32)) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let screen = GetDC(None);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !bits.is_null() {
            let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
                *d = [s[2], s[1], s[0], s[3]];
            }
            let dc = CreateCompatibleDC(Some(screen));
            let old = SelectObject(dc, bmp.into());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let (pt, size, src) = (POINT { x, y }, SIZE { cx: w, cy: h }, POINT { x: 0, y: 0 });
            let _ = UpdateLayeredWindow(hwnd, Some(screen), Some(&pt), Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
        }
        let _ = DeleteObject(bmp.into());
    }
    ReleaseDC(None, screen);
}

/// 滑鼠移到的地方變了（或刪掉了）：重畫
unsafe fn set_hover(hwnd: HWND, h: Option<Hit>, force: bool) {
    let redraw = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        if st.hover == h && !force {
            return None;
        }
        st.hover = h;
        let (pm, layout) = render(&st.thumb, st.scale, h, &st.caption, st.deleted)?;
        st.layout = layout;
        Some((pm, st.pos))
    });
    if let Some((pm, pos)) = redraw {
        present(hwnd, &pm, pos);
    }
}

fn point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE),
        WM_SETCURSOR => {
            let hand = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.hover.is_some()));
            if let Ok(c) = LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            let first = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let st = s.as_mut()?;
                let was = st.inside;
                st.inside = true;
                Some((!was, hit(&st.layout, x, y, st.deleted)))
            });
            if let Some((first, h)) = first {
                if first {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                }
                set_hover(hwnd, h, false);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.inside = false;
                    // 移開後再給幾秒
                    st.since = Instant::now();
                }
            });
            set_hover(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            let act = STATE.with(|s| s.borrow().as_ref().and_then(|st| hit(&st.layout, x, y, st.deleted)));
            let cmd = match act {
                Some(Hit::Image) | Some(Hit::Button(Button::Edit)) => Some(Cmd::Edit),
                Some(Hit::Button(Button::Pin)) => Some(Cmd::Pin),
                Some(Hit::Button(Button::Reveal)) => Some(Cmd::Reveal),
                Some(Hit::Button(Button::Delete)) => Some(Cmd::Delete),
                Some(Hit::Button(Button::Close)) => None,
                None => return LRESULT(0),
            };
            let mut done = false;
            if let Some(c) = cmd {
                // 先取出再呼叫：刪除可能在等的時候處理其他訊息（又進到這裡借用 STATE）
                let job = STATE.with(|s| s.borrow().as_ref().map(|st| (st.on.clone(), st.path.clone())));
                if let Some((on, path)) = job {
                    done = on(c, &path);
                }
            }
            // 刪掉了：留著顯示「已移到資源回收筒」，一下子後再關；其他都關掉
            if cmd == Some(Cmd::Delete) && done {
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        (st.deleted, st.since) = (true, Instant::now());
                    }
                });
                set_hover(hwnd, None, true);
            } else {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let expired = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| if st.deleted { st.since.elapsed() > DELETED_LIFETIME } else { !st.inside && st.since.elapsed() > LIFETIME }));
            if expired {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), 1);
            let mut cur = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
            if *cur == Some(hwnd.0 as isize) {
                *cur = None;
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
