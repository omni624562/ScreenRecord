//! 錄影範圍的外框（Windows）：錄自訂範圍時，在範圍外圍畫一圈細框，倒數、錄影、暫停期間一直顯示，
//! 讓人知道錄到哪裡。框畫在範圍外面（不會錄進去），也設成不被擷取（Windows 10 2004 以後）；
//! 滑鼠可以穿過去，不搶焦點。錄影中紅色，暫停時橘色。座標是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::types::Rect;
use std::cell::Cell;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, HDC, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, PeekMessageW, RegisterClassExW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage,
    HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WDA_EXCLUDEFROMCAPTURE, WM_ERASEBKGND, WM_NCHITTEST, WM_PAINT,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 框的粗細與離範圍的距離（像素）
const THICK: i32 = 3;
const GAP: i32 = 2;
/// #E5484D 與 #F5A524（COLORREF 是 BGR）
const RED: COLORREF = COLORREF(0x004D_48E5);
const AMBER: COLORREF = COLORREF(0x0024_A5F5);

thread_local! {
    static COLOR: Cell<COLORREF> = const { Cell::new(RED) };
}

/// 背景執行緒：每 0.1 秒問一次 area()（None = 不顯示；Some((範圍, 暫停中))），跟著顯示、移動或隱藏外框
pub fn spawn(area: impl Fn() -> Option<(Rect, bool)> + Send + 'static) {
    let _ = std::thread::Builder::new().name("rec-frame".into()).spawn(move || unsafe { run(area) });
}

unsafe fn run(area: impl Fn() -> Option<(Rect, bool)>) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderRecFrame");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    // 上、下、左、右四條（中間是空的，不擋範圍裡的畫面與滑鼠）
    let mut bars: Vec<HWND> = Vec::new();
    for _ in 0..4 {
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
        match CreateWindowExW(ex, class, w!("錄影範圍"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
            Ok(h) => {
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
                // 不被擷取（舊版 Windows 不支援時忽略；框本來就在範圍外）
                let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
                bars.push(h);
            }
            Err(e) => {
                crate::warn!("無法建立錄影範圍外框：{e}");
                return;
            }
        }
    }
    let mut shown: Option<(Rect, bool)> = None;
    let mut raised = Instant::now();
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let want = area();
        if want != shown {
            match want {
                Some((r, paused)) => {
                    COLOR.with(|c| c.set(if paused { AMBER } else { RED }));
                    let o = GAP + THICK;
                    let (l, t, rt, b) = (r.x - o, r.y - o, r.x + r.width + o, r.y + r.height + o);
                    let rects = [(l, t, rt - l, THICK), (l, b - THICK, rt - l, THICK), (l, t, THICK, b - t), (rt - THICK, t, THICK, b - t)];
                    for (h, (x, y, w, hh)) in bars.iter().zip(rects) {
                        let _ = SetWindowPos(*h, Some(HWND_TOPMOST), x, y, w, hh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                        let _ = InvalidateRect(Some(*h), None, true);
                    }
                }
                None => {
                    for h in &bars {
                        let _ = ShowWindow(*h, SW_HIDE);
                    }
                }
            }
            shown = want;
            raised = Instant::now();
        } else if shown.is_some() && raised.elapsed() > Duration::from_secs(2) {
            // 其他最上層視窗出現時，框可能被蓋住：定時移回最上層
            for h in &bars {
                let _ = SetWindowPos(*h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
            }
            raised = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

unsafe fn fill(hwnd: HWND, hdc: HDC) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let brush = CreateSolidBrush(COLOR.with(|c| c.get()));
    FillRect(hdc, &rc, brush);
    let _ = DeleteObject(brush.into());
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => {
            fill(hwnd, HDC(wparam.0 as *mut _));
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            fill(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
