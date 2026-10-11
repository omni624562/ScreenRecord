//! 釘在桌面（Windows）：把圖變成浮在最上層的小視窗，方便對照著打字或填表。
//! 每張圖一個視窗（各自的執行緒與訊息迴圈），不靠操作視窗，操作視窗關到系統匣也還在。
//! 拖曳移動、滾輪縮放、點兩下或 Esc 關閉；右鍵選單：複製、原尺寸、關閉。
//! 圖的像素是實體像素（截圖就是實體像素），原尺寸顯示時和截圖時一樣大。

use crate::tr;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FrameRect, GetMonitorInfoW, MonitorFromPoint, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HALFTONE, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, GetWindowRect, LoadCursorW, PostQuitMessage,
    RegisterClassExW, SetForegroundWindow, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage, CS_DBLCLKS, CS_DROPSHADOW, HTCAPTION, HWND_TOPMOST, IDC_SIZEALL, MF_SEPARATOR, MF_STRING, MSG,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEWHEEL, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONUP, WM_PAINT,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

struct Pin {
    /// BGRA（預乘、由上而下）
    bgra: Vec<u8>,
    /// 原圖（RGBA，複製用）
    rgba: Vec<u8>,
    w: i32,
    h: i32,
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
    PIN.with(|s| *s.borrow_mut() = Some(Pin { bgra, rgba, w, h }));
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
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let work = if GetMonitorInfoW(MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() { mi.rcWork } else { RECT { left: 0, top: 0, right: 1920, bottom: 1080 } };
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

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 整個視窗都當成標題列：按住就能拖曳移動
        WM_NCHITTEST => LRESULT(HTCAPTION as isize),
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
