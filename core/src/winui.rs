//! 操作視窗的縮小 / 還原（對應 src/winui.ts）：開始擷取時把擋到擷取範圍的操作視窗縮到工作列（避免錄到它），
//! 停止後再還原，讓使用者馬上看到「錄影已儲存」。以視窗標題（「螢幕錄影 v…」）辨識。

use crate::types::Rect;

pub const TITLE_PREFIX: &str = "螢幕錄影 v";

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::args::intersects;
    use std::sync::Mutex;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowRect, InternalGetWindowText, IsIconic, IsWindowVisible, ShowWindow, SW_MINIMIZE, SW_SHOWNOACTIVATE};

    /// 先前縮小的視窗（HWND 以整數保存，才能跨執行緒）
    static MINIMIZED: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    struct Search<'a> {
        prefix: &'a str,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
            // InternalGetWindowText 不送 WM_GETTEXT：操作視窗就在本程式裡，不能等它的執行緒回應
            let mut buf = [0u16; 256];
            let n = InternalGetWindowText(hwnd, &mut buf);
            if n > 0 && String::from_utf16_lossy(&buf[..n as usize]).starts_with(search.prefix) {
                search.found.push(hwnd);
            }
        }
        BOOL(1) // 繼續列舉
    }

    /// 目前看得到、而且沒有縮小的操作視窗
    fn find_ui_windows(prefix: &str) -> Vec<HWND> {
        let mut search = Search { prefix, found: Vec::new() };
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        search.found
    }

    /// 視窗看得到的範圍（實體像素）。優先用 DWM 的外框：GetWindowRect 含不可見的縮放邊框，
    /// 最大化的視窗會多出約 8px 到隔壁螢幕上，被誤判成擋到擷取範圍。
    fn window_rect(hwnd: HWND) -> Option<Rect> {
        let mut r = RECT::default();
        let ok = unsafe {
            DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_ok() || GetWindowRect(hwnd, &mut r).is_ok()
        };
        ok.then(|| Rect { x: r.left, y: r.top, width: r.right - r.left, height: r.bottom - r.top })
    }

    /// 與 area 重疊的操作視窗（拿不到位置的視窗算重疊，寧可多縮也不要錄到它）
    fn windows_in(area: Option<&Rect>, prefix: &str) -> Vec<HWND> {
        find_ui_windows(prefix)
            .into_iter()
            .filter(|h| match area {
                None => true,
                Some(a) => window_rect(*h).is_none_or(|r| intersects(&r, a)),
            })
            .collect()
    }

    pub fn ui_in_area(area: &Rect, prefix: &str) -> bool {
        !windows_in(Some(area), prefix).is_empty()
    }

    pub fn minimize_ui(area: Option<&Rect>, prefix: &str) -> bool {
        let list = windows_in(area, prefix);
        for h in &list {
            unsafe {
                let _ = ShowWindow(*h, SW_MINIMIZE);
            }
        }
        let any = !list.is_empty();
        *MINIMIZED.lock().unwrap() = list.into_iter().map(|h| h.0 as isize).collect();
        any
    }

    pub fn restore_ui() {
        let list = std::mem::take(&mut *MINIMIZED.lock().unwrap());
        for h in list {
            let hwnd = HWND(h as *mut _);
            unsafe {
                // 使用者自己又打開的就不動；視窗已關閉時 IsIconic 回傳 false
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
        }
    }
}

/// 是否有看得到的操作視窗在 area 內（倒數時決定要不要蓋上全畫面倒數）；無法判斷時回傳 true
pub fn ui_in_area(area: &Rect) -> bool {
    #[cfg(windows)]
    return imp::ui_in_area(area, TITLE_PREFIX);
    #[cfg(not(windows))]
    {
        let _ = area;
        true
    }
}

/// 縮小操作視窗；回傳是否有縮小任何視窗。
/// 指定 area 時只縮小與它重疊的視窗（拿不到位置的視窗一律縮小，寧可多縮也不要錄到它）。
pub fn minimize_ui(area: Option<&Rect>) -> bool {
    #[cfg(windows)]
    return imp::minimize_ui(area, TITLE_PREFIX);
    #[cfg(not(windows))]
    {
        let _ = area;
        false
    }
}

/// 還原先前由 minimize_ui 縮小的視窗（使用者自己又打開的就不動）
pub fn restore_ui() {
    #[cfg(windows)]
    imp::restore_ui();
}
