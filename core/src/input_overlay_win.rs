//! 錄影時顯示滑鼠點擊與按鍵（Windows）：用低階滑鼠 / 鍵盤攔截（只在錄影中）得知點擊與按鍵，
//! 在點擊處顯示一圈擴散的波紋、在錄影範圍下方顯示按下的快捷鍵。這兩個小視窗會被錄進影片
//! （和錄影外框相反），而且讓滑鼠穿過、不搶焦點。
//! 點擊的位置也交給錄影器記下來（剪輯時「跟著點擊放大」用），所以不管有沒有顯示，錄影中都會攔截滑鼠。

use crate::input_overlay::{halo, key_pill, key_text, ripple, HALO_SIZE, KEY_FADE_MS, KEY_SHOW_MS, RIPPLE_MS, RIPPLE_SIZE};
use crate::recorder::OverlayInfo;
use crate::types::{shown_keys, Rect};
use std::cell::RefCell;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, MonitorFromRect, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, PeekMessageW, RegisterClassExW, SetWindowPos, SetWindowsHookExW, ShowWindow, TranslateMessage,
    UnhookWindowsHookEx, UpdateLayeredWindow, HHOOK, HWND_TOPMOST, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

enum Ev {
    Click { x: i32, y: i32, right: bool },
    Key(String),
}

thread_local! {
    /// 攔截到、還沒處理的事件（攔截函式要盡快返回，畫面在主迴圈更新）
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

/// 在背景執行緒顯示點擊與按鍵；info 取得錄影狀態（None = 沒在錄影），on_click 收到點擊的桌面座標
pub fn spawn(info: impl Fn() -> Option<OverlayInfo> + Send + 'static, on_click: impl Fn(i32, i32) + Send + 'static) {
    let _ = std::thread::Builder::new().name("input-overlay".into()).spawn(move || unsafe { run(info, on_click) });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let m = wparam.0 as u32;
        if m == WM_LBUTTONDOWN || m == WM_RBUTTONDOWN {
            let s = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            EVENTS.with(|e| e.borrow_mut().push(Ev::Click { x: s.pt.x, y: s.pt.y, right: m == WM_RBUTTONDOWN }));
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
        let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let down = |vk: i32| (GetAsyncKeyState(vk) as u16 & 0x8000) != 0;
        let (ctrl, alt, shift, win) = (down(0x11), down(0x12), down(0x10), down(0x5B) || down(0x5C));
        if let Some(label) = shown_keys(k.vkCode, ctrl, alt, shift, win) {
            EVENTS.with(|e| e.borrow_mut().push(Ev::Key(label)));
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// 顯示中的效果
struct Show {
    hwnd: HWND,
    /// 開始的時間；None = 沒在顯示
    since: Option<Instant>,
}

unsafe fn run(info: impl Fn() -> Option<OverlayInfo>, on_click: impl Fn(i32, i32)) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderInputOverlay");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    // 會被錄進影片（沒有設成不被擷取）；滑鼠穿過、不搶焦點
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
    let make = || CreateWindowExW(ex, class, w!("點擊與按鍵"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None).ok();
    let (Some(rw), Some(kw), Some(hw)) = (make(), make(), make()) else {
        crate::warn!("無法建立顯示點擊與按鍵的視窗");
        return;
    };
    let mut ripple_fx = Show { hwnd: rw, since: None };
    let mut key_fx = Show { hwnd: kw, since: None };
    // 游標光暈：(圖, 目前顯示的位置)
    let mut halo_fx = Show { hwnd: hw, since: None };
    let mut halo_img: Option<(u32, Pixmap)> = None;
    let mut halo_at: Option<POINT> = None;
    // 目前的按鍵提示：(文字, 次數, 圖)
    let mut key: Option<(String, u32, Pixmap)> = None;
    let mut click: Option<(i32, i32, bool)> = None;
    let mut hooks: Option<(HHOOK, Option<HHOOK>)> = None;
    let mut cur: Option<OverlayInfo> = None;
    let mut polled = Instant::now() - Duration::from_secs(1);
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // 錄影狀態（每 0.2 秒看一次）：開始錄影時開始攔截，結束時停止
        if polled.elapsed() >= Duration::from_millis(200) {
            polled = Instant::now();
            let next = info();
            if next.map(|n| n.show_keys) != cur.map(|c| c.show_keys) || next.is_some() != cur.is_some() {
                if let Some((m, k)) = hooks.take() {
                    let _ = UnhookWindowsHookEx(m);
                    if let Some(k) = k {
                        let _ = UnhookWindowsHookEx(k);
                    }
                }
                if let Some(n) = next {
                    match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst.into()), 0) {
                        Ok(m) => {
                            let k = if n.show_keys { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(hinst.into()), 0).ok() } else { None };
                            hooks = Some((m, k));
                        }
                        Err(e) => crate::warn!("無法攔截滑鼠點擊：{e}"),
                    }
                }
            }
            if next.is_none_or(|n| !n.cursor_halo) {
                hide(&mut halo_fx);
                halo_at = None;
            }
            if next.is_none() {
                hide(&mut ripple_fx);
                hide(&mut key_fx);
                key = None;
                EVENTS.with(|e| e.borrow_mut().clear());
            }
            cur = next;
        }
        let events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        if let Some(info) = cur {
            let scale = scale_of(&info.area);
            for ev in events {
                match ev {
                    Ev::Click { x, y, right } => {
                        on_click(x, y);
                        let a = info.area;
                        if info.show_clicks && x >= a.x && y >= a.y && x < a.x + a.width && y < a.y + a.height {
                            click = Some((x, y, right));
                            ripple_fx.since = Some(Instant::now());
                        }
                    }
                    Ev::Key(label) => {
                        // 連續按同一組（還在顯示時）：加上次數
                        let count = match &key {
                            Some((l, n, _)) if *l == label && key_fx.since.is_some() => n + 1,
                            _ => 1,
                        };
                        if let Some(pm) = key_pill(&key_text(&label, count), scale) {
                            let (w, h) = (pm.width() as i32, pm.height() as i32);
                            let a = info.area;
                            let x = a.x + (a.width - w) / 2;
                            let y = a.y + a.height - h - (a.height as f32 * 0.08) as i32;
                            present(kw, &pm, x, y, 255);
                            key = Some((label, count, pm));
                            key_fx.since = Some(Instant::now());
                        }
                    }
                }
            }
            // 波紋：由小變大、漸漸變淡
            if let (Some(t0), Some((x, y, right))) = (ripple_fx.since, click) {
                let p = t0.elapsed().as_millis() as f32 / RIPPLE_MS as f32;
                if p >= 1.0 {
                    hide(&mut ripple_fx);
                } else {
                    let size = (RIPPLE_SIZE * scale).round() as u32;
                    if let Some(pm) = ripple(size, p, right) {
                        present(rw, &pm, x - size as i32 / 2, y - size as i32 / 2, 255);
                    }
                }
            }
            // 按鍵提示：顯示一陣子後淡出
            if let (Some(t0), Some((_, _, pm))) = (key_fx.since, &key) {
                let ms = t0.elapsed().as_millis() as u64;
                if ms >= KEY_SHOW_MS + KEY_FADE_MS {
                    hide(&mut key_fx);
                } else if ms > KEY_SHOW_MS {
                    let alpha = 255 - ((ms - KEY_SHOW_MS) * 255 / KEY_FADE_MS) as u8;
                    let a = info.area;
                    let (w, h) = (pm.width() as i32, pm.height() as i32);
                    present(kw, pm, a.x + (a.width - w) / 2, a.y + a.height - h - (a.height as f32 * 0.08) as i32, alpha);
                }
            }
        }
        // 游標光暈：跟著游標（只在錄影範圍內顯示）
        if let Some(info) = cur.filter(|i| i.cursor_halo) {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let a = info.area;
            let inside = p.x >= a.x && p.y >= a.y && p.x < a.x + a.width && p.y < a.y + a.height;
            if !inside {
                hide(&mut halo_fx);
                halo_at = None;
            } else if halo_at.is_none_or(|q| q.x != p.x || q.y != p.y) {
                let size = (HALO_SIZE * scale_of(&a)).round() as u32;
                if halo_img.as_ref().is_none_or(|(s, _)| *s != size) {
                    halo_img = halo(size).map(|pm| (size, pm));
                }
                if let Some((s, pm)) = &halo_img {
                    present(hw, pm, p.x - *s as i32 / 2, p.y - *s as i32 / 2, 255);
                    halo_fx.since = Some(Instant::now());
                    halo_at = Some(p);
                }
            }
        }
        let animating = ripple_fx.since.is_some() || key_fx.since.is_some() || halo_fx.since.is_some();
        std::thread::sleep(Duration::from_millis(if animating { 15 } else { 30 }));
    }
}

unsafe fn hide(s: &mut Show) {
    if s.since.take().is_some() {
        let _ = ShowWindow(s.hwnd, SW_HIDE);
    }
}

/// 錄影範圍所在螢幕的縮放比例（DPI / 96）
unsafe fn scale_of(a: &Rect) -> f32 {
    let r = RECT { left: a.x, top: a.y, right: a.x + a.width, bottom: a.y + a.height };
    let mon = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    (dx as f32 / 96.0).max(1.0)
}

/// 把圖（預乘 RGBA）放到分層視窗上，左上角在 (x, y)；alpha = 整體透明度
unsafe fn present(hwnd: HWND, pm: &Pixmap, x: i32, y: i32, alpha: u8) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let screen = GetDC(None);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !bits.is_null() {
            // 預乘的 RGBA → 預乘的 BGRA
            let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
                *d = [s[2], s[1], s[0], s[3]];
            }
            let dc = CreateCompatibleDC(Some(screen));
            let old = SelectObject(dc, bmp.into());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
            let (pt, size, src) = (POINT { x, y }, SIZE { cx: w, cy: h }, POINT { x: 0, y: 0 });
            let _ = UpdateLayeredWindow(hwnd, Some(screen), Some(&pt), Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
        }
        let _ = DeleteObject(bmp.into());
    }
    ReleaseDC(None, screen);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    // 保持在最上層（其他最上層視窗之上）
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
}
