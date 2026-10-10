//! 螢幕畫筆（Windows）：快捷鍵（預設 Ctrl+Alt+D）或系統匣打開，在游標所在的螢幕蓋一層透明的分層視窗，
//! 拖曳就能畫線；錄影時會一起錄進去。再按一次快捷鍵或 Esc 結束（畫的線一起清掉）。
//! 上方的操作說明是另一個小視窗，設成不被擷取，不會錄進影片。

use crate::screen_pen::{help_text, render_help, Board, KeyResult};
use crate::shot_toast_win::present;
use std::cell::RefCell;
use std::sync::Mutex;
use tiny_skia::PixmapMut;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow,
    SetTimer, SetWindowDisplayAffinity, ShowWindow, TranslateMessage, UpdateLayeredWindow, IDC_CROSS, MSG, SW_SHOW, SW_SHOWNOACTIVATE, ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_DESTROY,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 整個螢幕大小的點陣圖（BGRA，預乘透明度）：開著時一直用同一張，直接畫進去、直接貼到視窗，
/// 不用每次配置、轉換顏色順序（4K 一張就 33 MB）
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: u32,
    h: u32,
}

impl Surface {
    unsafe fn new(w: u32, h: u32) -> Option<Surface> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
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
        let Some(bmp) = bmp.filter(|_| !bits.is_null()) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, bmp.into());
        let mut s = Surface { dc, bmp, old, bits: bits as *mut u8, w, h };
        s.clear();
        Some(s)
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }

    fn pixmap(&mut self) -> Option<PixmapMut<'_>> {
        let (w, h) = (self.w, self.h);
        PixmapMut::from_bytes(self.bytes(), w, h)
    }

    /// 幾乎看不見的底色：完全透明的地方滑鼠會穿過去，要有一點點才收得到拖曳
    fn clear(&mut self) {
        for px in self.bytes().as_chunks_mut::<4>().0 {
            *px = [0, 0, 0, 1];
        }
    }

    unsafe fn present(&self, hwnd: HWND, (x, y): (i32, i32)) {
        let screen = GetDC(None);
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (pt, size, src) = (POINT { x, y }, SIZE { cx: self.w as i32, cy: self.h as i32 }, POINT { x: 0, y: 0 });
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

/// 畫筆視窗（開著時）
static CURRENT: Mutex<Option<isize>> = Mutex::new(None);

struct State {
    board: Board,
    surface: Surface,
    /// 螢幕左上角（桌面座標）
    origin: (i32, i32),
    scale: f32,
    /// 要整個重畫（放開滑鼠、復原、清除）
    full: bool,
    /// 正在畫的那一筆有新的點；已經畫到第幾個點
    tail: bool,
    drawn: usize,
    help: Option<HWND>,
    help_pos: (i32, i32),
    /// 說明上顯示的顏色（變了才重畫說明）
    help_color: usize,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 打開或關掉螢幕畫筆
/// 正在開啟（視窗還沒建好）時 CURRENT 裡放這個：連按兩次快捷鍵不會開出兩個畫筆
const STARTING: isize = -1;

pub fn toggle() {
    let mut cur = CURRENT.lock().unwrap();
    match *cur {
        // 還在開啟中：不重複開，也不能關（還沒有視窗）
        Some(STARTING) => {}
        Some(h) => {
            *cur = None;
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        None => {
            *cur = Some(STARTING);
            drop(cur);
            let spawned = std::thread::Builder::new().name("screen-pen".into()).spawn(|| {
                unsafe { run() };
                // 開不起來（例如找不到螢幕）時把佔位拿掉，下次才能再開
                let mut c = CURRENT.lock().unwrap();
                if *c == Some(STARTING) {
                    *c = None;
                }
            });
            if spawned.is_err() {
                *CURRENT.lock().unwrap() = None;
            }
        }
    }
}

/// 畫筆開著
pub fn active() -> bool {
    CURRENT.lock().unwrap().is_some_and(|h| h != STARTING)
}

unsafe fn run() {
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    let mon = MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut mi).as_bool() {
        return;
    }
    let m = mi.rcMonitor;
    let (w, h) = (m.right - m.left, m.bottom - m.top);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let scale = (dx as f32 / 96.0).max(1.0);
    let Some(surface) = Surface::new(w.max(1) as u32, h.max(1) as u32) else { return };

    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderPen");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    let help_class = w!("ScreenRecorderPenHelp");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(help_proc), hInstance: hinst.into(), lpszClassName: help_class, ..Default::default() };
    RegisterClassExW(&wc);
    let Ok(hwnd) = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED, class, w!("螢幕畫筆"), WS_POPUP, m.left, m.top, w, h, None, None, Some(hinst.into()), None) else { return };
    *CURRENT.lock().unwrap() = Some(hwnd.0 as isize);
    surface.present(hwnd, (m.left, m.top));

    // 操作說明：螢幕上方中間，滑鼠穿過、不被擷取
    let help_img = render_help(&help_text(0), scale);
    let (help, help_pos) = match &help_img {
        Some(img) => {
            let pos = ((m.left + m.right - img.width() as i32) / 2, m.top + (12.0 * scale) as i32);
            let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT;
            match CreateWindowExW(ex, help_class, w!("螢幕畫筆說明"), WS_POPUP, pos.0, pos.1, img.width() as i32, img.height() as i32, Some(hwnd), None, Some(hinst.into()), None) {
                Ok(hh) => {
                    let _ = SetWindowDisplayAffinity(hh, WDA_EXCLUDEFROMCAPTURE);
                    present(hh, img, pos);
                    (Some(hh), pos)
                }
                Err(_) => (None, pos),
            }
        }
        None => (None, (0, 0)),
    };
    STATE.with(|s| *s.borrow_mut() = Some(State { board: Board::new(scale), surface, origin: (m.left, m.top), scale, full: false, tail: false, drawn: 0, help, help_pos, help_color: 0 }));
    let _ = ShowWindow(hwnd, SW_SHOW);
    if let Some(hh) = help {
        let _ = ShowWindow(hh, SW_SHOWNOACTIVATE);
    }
    // 要收到 Esc、數字鍵
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
    // 重畫最多每秒 60 次（拖曳時不用每個滑鼠事件都更新整個螢幕）
    SetTimer(Some(hwnd), 1, 16, None);
    crate::info!("[畫筆] 開啟（{w}×{h}）");
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    STATE.with(|s| *s.borrow_mut() = None);
    let mut c = CURRENT.lock().unwrap();
    if *c == Some(hwnd.0 as isize) {
        *c = None;
    }
    crate::info!("[畫筆] 關閉");
}

fn point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32)
}

/// 用 STATE 做事
fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lparam);
            with(|st| {
                st.board.begin(x + st.origin.0 as f32, y + st.origin.1 as f32);
                (st.tail, st.drawn) = (true, 0);
            });
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            with(|st| {
                if st.board.move_to(x + st.origin.0 as f32, y + st.origin.1 as f32) {
                    st.tail = true;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            // 放開：整個重畫成平滑、有外框的線
            with(|st| {
                st.board.end();
                st.full = true;
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            match with(|st| st.board.key(wparam.0 as u32, ctrl)) {
                Some(KeyResult::Exit) => {
                    let _ = DestroyWindow(hwnd);
                }
                Some(KeyResult::Redraw) => {
                    with(|st| st.full = true);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            redraw(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 有變動時更新畫面：拖曳中只畫新的一段；放開、復原、清除時才整個重畫。換了顏色時更新說明
unsafe fn redraw(hwnd: HWND) {
    let Some(Some(help)) = with(|st| {
        let off = (st.origin.0 as f32, st.origin.1 as f32);
        if st.full {
            (st.full, st.tail, st.drawn) = (false, false, 0);
            st.surface.clear();
            if let Some(mut pm) = st.surface.pixmap() {
                st.board.render_to(&mut pm, off, true);
            }
        } else if std::mem::take(&mut st.tail) {
            let n = st.board.drawing_len().unwrap_or(0);
            let from = st.drawn.saturating_sub(1);
            if let Some(mut pm) = st.surface.pixmap() {
                st.board.render_tail(&mut pm, off, true, from);
            }
            st.drawn = n;
        } else {
            return None;
        }
        st.surface.present(hwnd, st.origin);
        let help = (st.help_color != st.board.color).then(|| {
            st.help_color = st.board.color;
            (st.help, render_help(&help_text(st.board.color), st.scale), st.help_pos)
        });
        Some(help)
    }) else {
        return;
    };
    if let Some((Some(hh), Some(img), pos)) = help {
        present(hh, &img, pos);
    }
}

unsafe extern "system" fn help_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
