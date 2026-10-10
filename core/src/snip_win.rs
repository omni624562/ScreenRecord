//! 框選截圖的畫面（Windows 原生視窗，不靠操作視窗）：一個蓋住整個桌面的最上層視窗，
//! 顯示凍結的畫面並變暗；拖曳框選範圍，或點一下選游標下的視窗；Esc 或右鍵取消。截圖與錄影共用。
//! 座標都是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::types::Rect;
use std::cell::RefCell;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetDC,
    GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, MonitorFromPoint, ReleaseDC, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_SEMIBOLD, HBITMAP, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow, ShowWindow,
    TranslateMessage, IDC_CROSS, MSG, SW_SHOW, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

/// 小於這個大小（像素）的拖曳當成點一下
const MIN_DRAG: i32 = 4;
const ACCENT: COLORREF = COLORREF(0x00EB_6325); // #2563EB（BGR）

struct State {
    desk: Rect,
    windows: Vec<Rect>,
    /// 原始與變暗的畫面（記憶體 DC 上的 DIB）
    bright: HDC,
    dim: HDC,
    /// 畫面先畫在這裡再一次貼上（不閃爍）
    back: HDC,
    bitmaps: [HBITMAP; 3],
    font: HFONT,
    drag: Option<(POINT, POINT)>,
    hover: Option<Rect>,
    result: Option<Rect>,
    /// 選來做什麼（說明文字不同）
    record: Mode,
}

/// 框選的用途
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shot,
    Record,
    /// 長截圖（捲動截圖）
    Scroll,
    /// 讀取 QR 碼
    Qr,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 顯示框選畫面直到使用者選好或取消（在呼叫的執行緒上執行訊息迴圈）。
/// rgba：整個桌面的畫面（由上而下、不透明）；windows：看得到的視窗（上層在前）；record：選錄影範圍
pub fn select(rgba: &[u8], desk: Rect, windows: Vec<Rect>, record: Mode) -> Option<Rect> {
    let (w, h) = (desk.width, desk.height);
    if w <= 0 || h <= 0 || rgba.len() < (w * h * 4) as usize {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let (bright, b1) = dib(screen, w, h, |dst| {
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
                *d = [s[2], s[1], s[0], 255];
            }
        })?;
        let (dim, b2) = dib(screen, w, h, |dst| {
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
                let k = |v: u8| (v as u16 * 45 / 100) as u8;
                *d = [k(s[2]), k(s[1]), k(s[0]), 255];
            }
        })?;
        let back = CreateCompatibleDC(Some(screen));
        let b3 = CreateCompatibleBitmap(screen, w, h);
        SelectObject(back, b3.into());
        ReleaseDC(None, screen);
        let font = CreateFontW(-18, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Microsoft JhengHei UI"));
        STATE.with(|s| *s.borrow_mut() = Some(State { desk, windows, bright, dim, back, bitmaps: [b1, b2, b3], font, drag: None, hover: None, result: None, record }));

        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("ScreenRecorderSnip");
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            match record {
                Mode::Record => w!("框選錄影範圍"),
                Mode::Scroll => w!("長截圖"),
                Mode::Qr => w!("讀取 QR 碼"),
                Mode::Shot => w!("框選截圖"),
            },
            WS_POPUP,
            desk.x,
            desk.y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        );
        if let Ok(hwnd) = hwnd {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let st = STATE.with(|s| s.borrow_mut().take())?;
        let _ = DeleteDC(st.bright);
        let _ = DeleteDC(st.dim);
        let _ = DeleteDC(st.back);
        for b in st.bitmaps {
            let _ = DeleteObject(b.into());
        }
        let _ = DeleteObject(st.font.into());
        st.result
    }
}

/// 建立 w×h 的 32 位元 DIB 並選進新的記憶體 DC；fill 填入 BGRA 像素（由上而下）
unsafe fn dib(screen: HDC, w: i32, h: i32, fill: impl FnOnce(&mut [u8])) -> Option<(HDC, HBITMAP)> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bmp.into());
        return None;
    }
    fill(std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize));
    let dc = CreateCompatibleDC(Some(screen));
    SelectObject(dc, bmp.into());
    Some((dc, bmp))
}

fn point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 }
}

/// 拖曳範圍（視窗座標）
fn drag_rect(a: POINT, b: POINT) -> RECT {
    RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x), bottom: a.y.max(b.y) }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = point(lparam);
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    if let Some((a, _)) = st.drag {
                        st.drag = Some((a, p));
                    } else {
                        let (x, y) = (p.x + st.desk.x, p.y + st.desk.y);
                        st.hover = st.windows.iter().find(|w| x >= w.x && x < w.x + w.width && y >= w.y && y < w.y + w.height).copied();
                    }
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let p = point(lparam);
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.drag = Some((p, p));
                }
            });
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let p = point(lparam);
            let done = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                let (a, _) = st.drag.take()?;
                let r = drag_rect(a, p);
                st.result = if r.right - r.left >= MIN_DRAG && r.bottom - r.top >= MIN_DRAG {
                    Some(Rect { x: r.left + st.desk.x, y: r.top + st.desk.y, width: r.right - r.left, height: r.bottom - r.top })
                } else {
                    // 點一下：截取游標下的視窗（超出桌面的部分裁掉）
                    st.hover.map(|w| clamp_to(w, st.desk))
                };
                st.result.map(|_| ())
            });
            if done.is_some() {
                let _ = DestroyWindow(hwnd);
            } else {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == 0x1B => {
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

fn clamp_to(r: Rect, b: Rect) -> Rect {
    let x0 = r.x.max(b.x);
    let y0 = r.y.max(b.y);
    let x1 = (r.x + r.width).min(b.x + b.width);
    let y1 = (r.y + r.height).min(b.y + b.height);
    Rect { x: x0, y: y0, width: (x1 - x0).max(1), height: (y1 - y0).max(1) }
}

/// 先畫在 back 裡再一次貼上（不閃爍）
unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    STATE.with(|s| {
        let b = s.borrow();
        let Some(st) = b.as_ref() else { return };
        let (w, h) = (st.desk.width, st.desk.height);
        let mem = st.back;
        let _ = BitBlt(mem, 0, 0, w, h, Some(st.dim), 0, 0, SRCCOPY);
        // 選取範圍（或游標下的視窗）用原本的亮度，外框與尺寸
        let (sel, is_drag) = match st.drag {
            Some((a, b)) => (Some(drag_rect(a, b)), true),
            None => (st.hover.map(|r| RECT { left: r.x - st.desk.x, top: r.y - st.desk.y, right: r.x + r.width - st.desk.x, bottom: r.y + r.height - st.desk.y }), false),
        };
        SetBkMode(mem, TRANSPARENT);
        SelectObject(mem, st.font.into());
        let brush = CreateSolidBrush(ACCENT);
        if let Some(r) = sel.filter(|r| r.right > r.left && r.bottom > r.top) {
            let _ = BitBlt(mem, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(st.bright), r.left, r.top, SRCCOPY);
            for i in 0..2 {
                let f = RECT { left: r.left - 1 - i, top: r.top - 1 - i, right: r.right + 1 + i, bottom: r.bottom + 1 + i };
                FrameRect(mem, &f, brush);
            }
            let text = if is_drag {
                format!("{} × {}", r.right - r.left, r.bottom - r.top)
            } else {
                format!(
                    "視窗 {} × {}・點一下{}",
                    r.right - r.left,
                    r.bottom - r.top,
                    match st.record {
                        Mode::Record => "錄這個範圍",
                        Mode::Scroll => "捲動截取",
                        Mode::Qr => "讀取 QR 碼",
                        Mode::Shot => "截取",
                    }
                )
            };
            let top = if r.top >= 34 { r.top - 34 } else { r.top + 6 };
            label(mem, &text, r.left.max(0), top, brush);
        }
        // 游標所在螢幕的上方：說明
        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            let m = mi.rcMonitor;
            let tip = match st.record {
                Mode::Record => "拖曳框選要錄影的範圍，或點一下選視窗　　Esc 或右鍵取消",
                Mode::Scroll => "框選要捲動的內容（例如網頁中間的部分），或點一下選視窗；選好後自動往下捲並接成長圖，按 Esc 停止",
                Mode::Qr => "框選 QR 碼（大一點沒關係），或點一下選視窗　　Esc 或右鍵取消",
                Mode::Shot => "拖曳框選範圍，或點一下截取視窗　　Esc 或右鍵取消",
            };
            let mut t: Vec<u16> = tip.encode_utf16().collect();
            let mut sz = SIZE::default();
            let _ = GetTextExtentPoint32W(mem, &t, &mut sz);
            let cx = (m.left + m.right) / 2 - st.desk.x;
            let mut r = RECT { left: cx - sz.cx / 2 - 16, top: m.top - st.desk.y + 24, right: cx + sz.cx / 2 + 16, bottom: m.top - st.desk.y + 24 + sz.cy + 14 };
            let bg = CreateSolidBrush(COLORREF(0x0020_2020));
            FillRect(mem, &r, bg);
            let _ = DeleteObject(bg.into());
            SetTextColor(mem, COLORREF(0x00FF_FFFF));
            DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }
        let _ = DeleteObject(brush.into());
        let p = ps.rcPaint;
        let _ = BitBlt(hdc, p.left, p.top, p.right - p.left, p.bottom - p.top, Some(mem), p.left, p.top, SRCCOPY);
    });
    let _ = EndPaint(hwnd, &ps);
}

/// 藍底白字的小標籤
unsafe fn label(hdc: HDC, text: &str, x: i32, y: i32, brush: windows::Win32::Graphics::Gdi::HBRUSH) {
    let mut t: Vec<u16> = text.encode_utf16().collect();
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &t, &mut sz);
    let mut r = RECT { left: x, top: y, right: x + sz.cx + 16, bottom: y + sz.cy + 8 };
    FillRect(hdc, &r, brush);
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    DrawTextW(hdc, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}
