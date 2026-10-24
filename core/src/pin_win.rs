//! 釘在桌面（Windows）：把圖變成浮在最上層的小視窗，方便對照著打字或填表。
//! 每張圖一個視窗（各自的執行緒與訊息迴圈），不靠操作視窗，操作視窗關到系統匣也還在。
//! 拖曳移動、滾輪縮放、按兩下或 Esc 關閉；滑鼠移上去時右上角有 ✕；右鍵選單：複製、原尺寸、關閉。
//! 剛釘上時下方顯示 3 秒「拖曳移動・滾輪縮放・按兩下關閉」。
//! 圖的像素是實體像素（截圖就是實體像素），原尺寸顯示時和截圖時一樣大。

use crate::tr;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, EndPaint, FrameRect, GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, LineTo, MonitorFromPoint,
    MoveToEx, RoundRect, ScreenToClient, SelectObject, SetBkMode, SetStretchBltMode, SetTextColor, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HALFTONE, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, GetWindowRect, KillTimer, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage, CS_DBLCLKS, CS_DROPSHADOW, HTCAPTION, HTCLIENT,
    HWND_TOPMOST, IDC_HAND, IDC_SIZEALL, MF_SEPARATOR, MF_STRING, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCMOUSEMOVE, WM_NCRBUTTONUP, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;
/// 剛釘上時的說明顯示多久（毫秒）
const HINT_MS: u32 = 3000;

struct Pin {
    /// BGRA（預乘、由上而下）
    bgra: Vec<u8>,
    /// 原圖（RGBA，複製用）
    rgba: Vec<u8>,
    w: i32,
    h: i32,
    /// 螢幕的縮放比例（DPI / 96）
    scale: f32,
    /// 滑鼠在視窗上（顯示 ✕）、在 ✕ 上
    hover: bool,
    on_close: bool,
    /// 還在顯示說明
    hint: bool,
}

thread_local! {
    static PIN: RefCell<Option<Pin>> = const { RefCell::new(None) };
}

const ID_COPY: usize = 1;
const ID_ACTUAL: usize = 2;
const ID_CLOSE: usize = 3;

/// 開一個釘選視窗（rgba：一般 RGBA，w × h）
pub fn show(rgba: Vec<u8>, w: u32, h: u32) {
    let _ = std::thread::Builder::new().name("pin".into()).spawn(move || unsafe { run(rgba, w as i32, h as i32) });
}

unsafe fn run(rgba: Vec<u8>, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    // 透明處（陰影）預乘成白底上的顏色：GDI 不處理透明
    let bgra: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3] as u32;
            let mix = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
            [mix(p[2]), mix(p[1]), mix(p[0]), 255]
        })
        .collect();
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    let mon = MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST);
    let mut dpi = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
    let scale = (dpi.0 as f32 / 96.0).clamp(1.0, 3.0);
    PIN.with(|s| *s.borrow_mut() = Some(Pin { bgra, rgba, w, h, scale, hover: false, on_close: false, hint: true }));
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderPin");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DROPSHADOW | CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst.into(),
        hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    // 放在游標旁邊；太大時縮到工作區的八成
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let work = if GetMonitorInfoW(mon, &mut mi).as_bool() { mi.rcWork } else { RECT { left: 0, top: 0, right: 1920, bottom: 1080 } };
    let (ww, wh) = (work.right - work.left, work.bottom - work.top);
    let k = (ww as f64 * 0.8 / w as f64).min(wh as f64 * 0.8 / h as f64).min(1.0);
    let (vw, vh) = (((w as f64 * k) as i32).max(40), ((h as f64 * k) as i32).max(40));
    let x = (cur.x + 24).clamp(work.left, (work.right - vw).max(work.left));
    let y = (cur.y + 24).clamp(work.top, (work.bottom - vh).max(work.top));
    let Ok(hwnd) = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class, w!("釘選的截圖"), WS_POPUP, x, y, vw, vh, None, None, Some(hinst.into()), None) else {
        return;
    };
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    // 3 秒後收起說明
    SetTimer(Some(hwnd), 1, HINT_MS, None);
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// 以游標為中心縮放（factor > 1 放大）；size = None 時回到原尺寸
unsafe fn zoom(hwnd: HWND, factor: Option<f64>) {
    let Some((w, h)) = PIN.with(|s| s.borrow().as_ref().map(|p| (p.w, p.h))) else { return };
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    let (cw, ch) = (r.right - r.left, r.bottom - r.top);
    let (nw, nh) = match factor {
        Some(f) => {
            let k = (cw as f64 * f / w as f64).clamp(0.1, 4.0);
            ((w as f64 * k).round() as i32, (h as f64 * k).round() as i32)
        }
        None => (w, h),
    };
    let (nw, nh) = (nw.max(40), nh.max(40));
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    // 游標下的那一點縮放後還在游標下
    let (fx, fy) = ((cur.x - r.left) as f64 / cw.max(1) as f64, (cur.y - r.top) as f64 / ch.max(1) as f64);
    let (fx, fy) = (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0));
    let x = cur.x - (nw as f64 * fx) as i32;
    let y = cur.y - (nh as f64 * fy) as i32;
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, nw, nh, SWP_NOACTIVATE | SWP_NOZORDER);
}

unsafe fn menu(hwnd: HWND) {
    let Ok(m) = CreatePopupMenu() else { return };
    let item = |id: usize, text: &str| {
        let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = AppendMenuW(m, MF_STRING, id, PCWSTR(t.as_ptr()));
    };
    item(ID_COPY, tr!("複製", "Copy"));
    item(ID_ACTUAL, tr!("原尺寸", "Actual size"));
    let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
    item(ID_CLOSE, tr!("關閉", "Close"));
    let mut p = POINT::default();
    let _ = GetCursorPos(&mut p);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None).0 as usize;
    let _ = DestroyMenu(m);
    match cmd {
        ID_COPY => {
            if let Some((rgba, w, h)) = PIN.with(|s| s.borrow().as_ref().map(|p| (p.rgba.clone(), p.w, p.h))) {
                if let Some(mut pm) = tiny_skia::Pixmap::new(w as u32, h as u32) {
                    // 一般 RGBA → 預乘（copy_pixmap 會再轉回來）
                    for (d, s) in pm.pixels_mut().iter_mut().zip(rgba.as_chunks::<4>().0) {
                        *d = tiny_skia::ColorU8::from_rgba(s[0], s[1], s[2], s[3]).premultiply();
                    }
                    crate::clipboard::copy_pixmap(&pm);
                }
            }
        }
        ID_ACTUAL => zoom(hwnd, None),
        ID_CLOSE => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

/// 右上角 ✕ 的範圍（視窗座標）
fn close_rect(rc: &RECT, scale: f32) -> RECT {
    let d = ((22.0 * scale) as i32).min((rc.right - rc.left).min(rc.bottom - rc.top) / 2).max(12);
    let m = (6.0 * scale) as i32;
    RECT { left: rc.right - m - d, top: rc.top + m, right: rc.right - m, bottom: rc.top + m + d }
}

fn inside(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// 游標在 ✕ 上
unsafe fn cursor_on_close(hwnd: HWND) -> bool {
    let mut p = POINT::default();
    let _ = GetCursorPos(&mut p);
    let _ = ScreenToClient(hwnd, &mut p);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let scale = PIN.with(|s| s.borrow().as_ref().map(|p| p.scale)).unwrap_or(1.0);
    inside(&close_rect(&rc, scale), p)
}

/// 滑鼠移進來（或移到 ✕ 上）：開始追蹤移出，重畫
unsafe fn set_hover(hwnd: HWND, hover: bool, nonclient: bool) {
    let on_close = hover && cursor_on_close(hwnd);
    let changed = PIN.with(|s| {
        let mut b = s.borrow_mut();
        let p = b.as_mut()?;
        let changed = p.hover != hover || p.on_close != on_close;
        (p.hover, p.on_close) = (hover, on_close);
        Some(changed)
    });
    if hover {
        let flags = if nonclient { TME_LEAVE | TME_NONCLIENT } else { TME_LEAVE };
        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: flags, hwndTrack: hwnd, dwHoverTime: 0 };
        let _ = TrackMouseEvent(&mut tme);
    }
    if changed == Some(true) {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// 滑鼠在上面時右上角的 ✕；剛釘上時下方的說明
unsafe fn overlay(hdc: HDC, rc: &RECT, p: &Pin) {
    let s = p.scale;
    if p.hover {
        let c = close_rect(rc, s);
        let brush = CreateSolidBrush(if p.on_close { COLORREF(0x004D_48E5) } else { COLORREF(0x0020_2020) });
        let pen = CreatePen(PS_SOLID, 1, COLORREF(0x00FF_FFFF));
        let (ob, op) = (SelectObject(hdc, brush.into()), SelectObject(hdc, pen.into()));
        let _ = Ellipse(hdc, c.left, c.top, c.right, c.bottom);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
        // ✕ 用兩條線畫
        let d = (c.right - c.left) * 3 / 10;
        let (cx, cy) = ((c.left + c.right) / 2, (c.top + c.bottom) / 2);
        let pen = CreatePen(PS_SOLID, (2.0 * s) as i32, COLORREF(0x00FF_FFFF));
        let op = SelectObject(hdc, pen.into());
        let _ = MoveToEx(hdc, cx - d, cy - d, None);
        let _ = LineTo(hdc, cx + d, cy + d);
        let _ = MoveToEx(hdc, cx + d, cy - d, None);
        let _ = LineTo(hdc, cx - d, cy + d);
        SelectObject(hdc, op);
        let _ = DeleteObject(pen.into());
    }
    if p.hint {
        let font = CreateFontW(-((13.0 * s) as i32), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Microsoft JhengHei UI"));
        let old = SelectObject(hdc, font.into());
        let mut text: Vec<u16> = tr!("拖曳移動・滾輪縮放・按兩下關閉", "Drag to move · Scroll to zoom · Double-click to close").encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &text, &mut sz);
        let (pw, ph) = (sz.cx + (24.0 * s) as i32, sz.cy + (12.0 * s) as i32);
        // 圖太小放不下就不顯示
        if pw <= rc.right - rc.left - 8 {
            let x = (rc.left + rc.right - pw) / 2;
            let y = rc.bottom - ph - (10.0 * s) as i32;
            let mut r = RECT { left: x, top: y, right: x + pw, bottom: y + ph };
            let brush = CreateSolidBrush(COLORREF(0x0020_2020));
            let pen = CreatePen(PS_SOLID, 1, COLORREF(0x0020_2020));
            let (ob, op) = (SelectObject(hdc, brush.into()), SelectObject(hdc, pen.into()));
            let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, ph, ph);
            SelectObject(hdc, ob);
            SelectObject(hdc, op);
            let _ = DeleteObject(brush.into());
            let _ = DeleteObject(pen.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FF_FFFF));
            DrawTextW(hdc, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 整個視窗都當成標題列：按住就能拖曳移動；右上角的 ✕ 是一般的區域（點了關閉）
        WM_NCHITTEST => {
            let on_close = PIN.with(|s| s.borrow().as_ref().is_some_and(|p| p.hover)) && cursor_on_close(hwnd);
            LRESULT(if on_close { HTCLIENT } else { HTCAPTION } as isize)
        }
        WM_NCMOUSEMOVE => {
            set_hover(hwnd, true, true);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            set_hover(hwnd, true, false);
            LRESULT(0)
        }
        WM_NCMOUSELEAVE | WM_MOUSELEAVE => {
            // 從標題列區移到 ✕（或反過來）時也會收到：游標還在視窗裡就不收起 ✕
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            set_hover(hwnd, inside(&r, p), msg == WM_NCMOUSELEAVE);
            LRESULT(0)
        }
        WM_SETCURSOR if cursor_on_close(hwnd) => {
            if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => {
            if cursor_on_close(hwnd) {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), 1);
            PIN.with(|s| {
                if let Some(p) = s.borrow_mut().as_mut() {
                    p.hint = false;
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_NCLBUTTONDBLCLK => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_NCRBUTTONUP => {
            menu(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == 0x1B => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            zoom(hwnd, Some(if delta > 0 { 1.1 } else { 1.0 / 1.1 }));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            PIN.with(|s| {
                if let Some(p) = s.borrow().as_ref() {
                    let bi = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: p.w,
                            biHeight: -p.h,
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    SetStretchBltMode(hdc, HALFTONE);
                    StretchDIBits(hdc, 0, 0, rc.right, rc.bottom, 0, 0, p.w, p.h, Some(p.bgra.as_ptr() as *const _), &bi, DIB_RGB_COLORS, SRCCOPY);
                    overlay(hdc, &rc, p);
                }
            });
            // 細的灰色外框，在白色背景上也看得出邊界
            let b = CreateSolidBrush(COLORREF(0x0080_8080));
            FrameRect(hdc, &rc, b);
            let _ = DeleteObject(b.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PIN.with(|s| *s.borrow_mut() = None);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
