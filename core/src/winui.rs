//! 操作視窗的縮小 / 還原：開始擷取時把擋到擷取範圍的操作視窗縮到工作列（避免錄到它），
//! 停止後再還原，讓使用者馬上看到「錄影已儲存」。以視窗標題（「螢幕錄影 v…」）辨識。

use crate::types::Rect;
use serde::{Deserialize, Serialize};

/// 操作視窗的標題開頭（兩種語言；切換語言後也認得出自己的視窗）
pub const TITLE_PREFIXES: [&str; 2] = ["螢幕錄影 v", "Screen Recorder v"];

/// 操作視窗的標題（依介面語言）
pub fn app_title() -> String {
    format!("{}{}", crate::tr!(TITLE_PREFIXES[0], TITLE_PREFIXES[1]), crate::version::APP_VERSION)
}

#[cfg(windows)]
fn is_app_title(t: &str) -> bool {
    TITLE_PREFIXES.iter().any(|p| t.starts_with(p))
}

/// 講稿小視窗的標題（兩種語言）：不算操作視窗（開始錄影時不縮小），也不列在「選擇視窗」
pub const NOTES_TITLES: [&str; 2] = ["講稿（不會錄進影片）", "Script (not recorded)"];

pub fn notes_title() -> &'static str {
    crate::tr!(NOTES_TITLES[0], NOTES_TITLES[1])
}

/// 講稿小視窗：設成不被擷取（錄影、截圖都看不到它；Windows 10 2004 以後），並調整透明度（255 = 不透明）。
/// 回傳是否找到視窗（剛開啟時視窗可能還沒建立，下一格再試）
pub fn style_notes_window(alpha: u8) -> bool {
    #[cfg(windows)]
    return imp::style_notes_window(alpha);
    #[cfg(not(windows))]
    {
        let _ = alpha;
        false
    }
}

/// 可以錄的視窗（「只錄這個視窗」）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// 視窗代碼（HWND；只在這次開機、視窗還開著時有效）
    pub id: i64,
    pub title: String,
    pub rect: Rect,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::args::intersects;
    use std::sync::Mutex;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowRect, InternalGetWindowText, IsIconic, IsWindowVisible, ShowWindow, SW_MINIMIZE, SW_SHOWNOACTIVATE};

    /// 先前縮小的視窗（HWND 以整數保存，才能跨執行緒）
    static MINIMIZED: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    struct Search {
        found: Vec<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
            // InternalGetWindowText 不送 WM_GETTEXT：操作視窗就在本程式裡，不能等它的執行緒回應
            let mut buf = [0u16; 256];
            let n = InternalGetWindowText(hwnd, &mut buf);
            if n > 0 && super::is_app_title(&String::from_utf16_lossy(&buf[..n as usize])) {
                search.found.push(hwnd);
            }
        }
        BOOL(1) // 繼續列舉
    }

    /// 目前看得到、而且沒有縮小的操作視窗
    fn find_ui_windows() -> Vec<HWND> {
        let mut search = Search { found: Vec::new() };
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        search.found
    }

    /// 視窗看得到的範圍（實體像素）。優先用 DWM 的外框：GetWindowRect 含不可見的縮放邊框，
    /// 最大化的視窗會多出約 8px 到隔壁螢幕上，被誤判成擋到擷取範圍。
    fn window_rect(hwnd: HWND) -> Option<Rect> {
        let mut r = RECT::default();
        let ok = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_ok() || GetWindowRect(hwnd, &mut r).is_ok() };
        ok.then(|| Rect { x: r.left, y: r.top, width: r.right - r.left, height: r.bottom - r.top })
    }

    /// 看得到、有標題的一般視窗（不含桌面、隱藏的 UWP 視窗）
    unsafe fn titled_window(hwnd: HWND) -> Option<WindowInfo> {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        // 隱藏起來的 UWP 視窗（cloaked）看不到，跳過
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok() && cloaked != 0 {
            return None;
        }
        let mut buf = [0u16; 256];
        let n = InternalGetWindowText(hwnd, &mut buf);
        if n == 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        // 桌面本身不算視窗
        let n = GetClassNameW(hwnd, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if ["Progman", "WorkerW"].contains(&class.as_str()) {
            return None;
        }
        let rect = window_rect(hwnd).filter(|r| r.width > 0 && r.height > 0)?;
        Some(WindowInfo { id: hwnd.0 as i64, title, rect })
    }

    unsafe extern "system" fn visit_all(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<WindowInfo>);
        if let Some(w) = titled_window(hwnd) {
            list.push(w);
        }
        BOOL(1)
    }

    pub fn style_notes_window(alpha: u8) -> bool {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA, WDA_EXCLUDEFROMCAPTURE, WS_EX_LAYERED,
        };
        let mut found = false;
        for t in NOTES_TITLES {
            let w: Vec<u16> = t.encode_utf16().chain(std::iter::once(0)).collect();
            let Ok(hwnd) = (unsafe { FindWindowW(PCWSTR::null(), PCWSTR(w.as_ptr())) }) else { continue };
            found = true;
            unsafe {
                let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                if ex & WS_EX_LAYERED.0 as isize == 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
                }
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
            }
        }
        found
    }

    pub fn app_windows() -> Vec<WindowInfo> {
        let mut list: Vec<WindowInfo> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(visit_all), LPARAM(&mut list as *mut Vec<WindowInfo> as isize));
        }
        list
    }

    pub fn window_bounds(id: i64) -> Option<Rect> {
        let hwnd = HWND(id as isize as *mut _);
        unsafe {
            if !windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
                return None;
            }
        }
        window_rect(hwnd)
    }

    unsafe extern "system" fn visit_monitor(h: windows::Win32::Graphics::Gdi::HMONITOR, _: windows::Win32::Graphics::Gdi::HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFOEXW};
        let list = &mut *(lparam.0 as *mut Vec<String>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let name = if GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut _).as_bool() {
            let n = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
            String::from_utf16_lossy(&info.szDevice[..n])
        } else {
            String::new()
        };
        list.push(name);
        BOOL(1)
    }

    pub fn display_order() -> Vec<String> {
        let mut list: Vec<String> = Vec::new();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::EnumDisplayMonitors(None, None, Some(visit_monitor), LPARAM(&mut list as *mut Vec<String> as isize));
        }
        list
    }

    pub fn visible_windows() -> Vec<Rect> {
        app_windows().into_iter().map(|w| w.rect).collect()
    }

    /// 與 area 重疊的操作視窗（拿不到位置的視窗算重疊，寧可多縮也不要錄到它）
    fn windows_in(area: Option<&Rect>) -> Vec<HWND> {
        find_ui_windows()
            .into_iter()
            .filter(|h| match area {
                None => true,
                Some(a) => window_rect(*h).is_none_or(|r| intersects(&r, a)),
            })
            .collect()
    }

    pub fn ui_in_area(area: &Rect) -> bool {
        !windows_in(Some(area)).is_empty()
    }

    /// 錄影開始時縮小的視窗（和截圖分開記：錄影中截圖收尾時不會把操作視窗叫回來，停止錄影時也不會漏還原）
    static REC_MINIMIZED: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    pub fn minimize_ui(area: Option<&Rect>, recording: bool) -> bool {
        let list = windows_in(area);
        for h in &list {
            unsafe {
                let _ = ShowWindow(*h, SW_MINIMIZE);
            }
        }
        let any = !list.is_empty();
        let ids = list.into_iter().map(|h| h.0 as isize).collect();
        *if recording { &REC_MINIMIZED } else { &MINIMIZED }.lock().unwrap() = ids;
        any
    }

    /// 桌面圖示是我們藏起來的（停止後要還原）
    static ICONS_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// 桌面圖示的清單視窗（Progman 或 WorkerW 底下的 SHELLDLL_DefView → SysListView32）
    fn desktop_list_view() -> Option<HWND> {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW};
        unsafe {
            let find_in = |parent: HWND| -> Option<HWND> {
                let def = FindWindowExW(Some(parent), None, w!("SHELLDLL_DefView"), None).ok()?;
                FindWindowExW(Some(def), None, w!("SysListView32"), None).ok()
            };
            if let Some(h) = FindWindowW(w!("Progman"), None).ok().and_then(find_in) {
                return Some(h);
            }
            // 換過桌布輪播等情況：在某個 WorkerW 底下
            let mut after: Option<HWND> = None;
            loop {
                let w = FindWindowExW(None, after, w!("WorkerW"), None).ok()?;
                if let Some(h) = find_in(w) {
                    return Some(h);
                }
                after = Some(w);
            }
        }
    }

    pub fn set_desktop_icons(visible: bool) {
        use std::sync::atomic::Ordering;
        if visible && !ICONS_HIDDEN.load(Ordering::Relaxed) {
            return;
        }
        let Some(h) = desktop_list_view() else { return };
        unsafe {
            let _ = ShowWindow(h, if visible { windows::Win32::UI::WindowsAndMessaging::SW_SHOW } else { windows::Win32::UI::WindowsAndMessaging::SW_HIDE });
        }
        ICONS_HIDDEN.store(!visible, Ordering::Relaxed);
    }

    pub fn restore_ui(recording: bool) {
        let list = std::mem::take(&mut *if recording { &REC_MINIMIZED } else { &MINIMIZED }.lock().unwrap());
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
    return imp::ui_in_area(area);
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
    return imp::minimize_ui(area, false);
    #[cfg(not(windows))]
    {
        let _ = area;
        false
    }
}

/// 開始錄影時縮小擋到範圍的操作視窗（停止後用 restore_ui_after_recording 還原）
pub fn minimize_ui_for_recording(area: &Rect) -> bool {
    #[cfg(windows)]
    return imp::minimize_ui(Some(area), true);
    #[cfg(not(windows))]
    {
        let _ = area;
        false
    }
}

/// 停止錄影：還原開始時縮小的操作視窗
pub fn restore_ui_after_recording() {
    #[cfg(windows)]
    imp::restore_ui(true);
}

/// 看得到的視窗範圍（實體像素），由上層到下層（框選截圖時點一下截整個視窗）
pub fn visible_windows() -> Vec<Rect> {
    #[cfg(windows)]
    return imp::visible_windows();
    #[cfg(not(windows))]
    Vec::new()
}

/// 可以錄的視窗（看得到、有標題），由上層到下層；不含本程式的操作視窗
pub fn app_windows() -> Vec<WindowInfo> {
    #[cfg(windows)]
    return imp::app_windows().into_iter().filter(|w| !is_app_title(&w.title) && !NOTES_TITLES.contains(&w.title.as_str()) && w.rect.width >= 80 && w.rect.height >= 40).collect();
    #[cfg(not(windows))]
    Vec::new()
}

/// 視窗目前的範圍（實體像素）；視窗已關閉或縮到工作列時為 None
pub fn window_bounds(id: i64) -> Option<Rect> {
    #[cfg(windows)]
    return imp::window_bounds(id);
    #[cfg(not(windows))]
    {
        let _ = id;
        None
    }
}

/// 螢幕的裝置名稱（\\.\DISPLAY1…），依 EnumDisplayMonitors 的順序（與 winit 的螢幕編號相同）
pub fn display_order() -> Vec<String> {
    #[cfg(windows)]
    return imp::display_order();
    #[cfg(not(windows))]
    Vec::new()
}

/// 顯示 / 隱藏桌面圖示（錄影時讓畫面乾淨）；只會還原自己藏起來的
pub fn set_desktop_icons(visible: bool) {
    #[cfg(windows)]
    imp::set_desktop_icons(visible);
    #[cfg(not(windows))]
    let _ = visible;
}

/// 還原先前由 minimize_ui 縮小的視窗（使用者自己又打開的就不動）
pub fn restore_ui() {
    #[cfg(windows)]
    imp::restore_ui(false);
}
