//! 浮在畫面上的小提示（Windows）：分層視窗（圓角、半透明），不搶焦點，也不會被截圖、錄影拍進去。
//! 延遲截圖的倒數（點一下或按 Esc 取消）、長截圖進行中（滑鼠穿過去）、步驟截圖的小工具列（完成、取消）共用。
//! 每個提示有自己的執行緒與訊息迴圈；Bubble 丟掉時關閉。畫法在 bubble.rs。

use crate::bubble::{hit, place, render, Content, Layout};
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, TrackMouseEvent, UnregisterHotKey, MOD_NOREPEAT, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetCursor, SetWindowDisplayAffinity,
    ShowWindow, TranslateMessage, IDC_ARROW, IDC_HAND, MSG, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_CLOSE, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 點在按鈕以外的地方
pub const BODY: u32 = 0;
/// 按了 Esc
pub const ESC: u32 = u32::MAX;

const WM_UPDATE: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const MA_NOACTIVATE: isize = 3;

/// 點了按鈕（編號）、點在其他地方（BODY）或按 Esc（ESC）時呼叫；在提示自己的執行緒上
pub type OnEvent = Box<dyn Fn(u32) + Send>;

pub struct Options {
    /// 滑鼠可以點（否則穿過去，點到下面的視窗）
    pub clickable: bool,
    /// 按 Esc（不管焦點在哪個視窗）當成一個事件（ESC）
    pub esc: bool,
    /// 放在這個範圍（桌面座標：左, 上, 右, 下）的下方；None = 游標所在螢幕的上方（bottom = 下方）中間
    pub near: Option<(i32, i32, i32, i32)>,
    pub bottom: bool,
    pub on: Option<OnEvent>,
}

struct Shared {
    content: Content,
    events: Vec<u32>,
    closed: bool,
}

/// 顯示中的提示
pub struct Bubble {
    shared: Arc<Mutex<Shared>>,
    hwnd: Arc<AtomicIsize>,
}

struct State {
    shared: Arc<Mutex<Shared>>,
    shown: Content,
    layout: Layout,
    hover: Option<u32>,
    pos: (i32, i32),
    scale: f32,
    work: (i32, i32, i32, i32),
    near: Option<(i32, i32, i32, i32)>,
    bottom: bool,
    inside: bool,
    clickable: bool,
    on: Option<OnEvent>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

impl Bubble {
    pub fn show(content: Content, opts: Options) -> Bubble {
        let shared = Arc::new(Mutex::new(Shared { content, events: vec![], closed: false }));
        let hwnd = Arc::new(AtomicIsize::new(0));
        let (s, h) = (shared.clone(), hwnd.clone());
        let _ = std::thread::Builder::new().name("bubble".into()).spawn(move || unsafe { run(s, h, opts) });
        Bubble { shared, hwnd }
    }

    /// 換內容（一樣時不重畫）
    pub fn set(&self, content: Content) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.content == content {
                return;
            }
            s.content = content;
        }
        let h = self.hwnd.load(Ordering::Acquire);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_UPDATE, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// 取出到目前為止的事件（按鈕編號、BODY、ESC）
    pub fn events(&self) -> Vec<u32> {
        std::mem::take(&mut self.shared.lock().unwrap().events)
    }

    pub fn close(&self) {
        self.shared.lock().unwrap().closed = true;
        let h = self.hwnd.load(Ordering::Acquire);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

impl Drop for Bubble {
    fn drop(&mut self) {
        self.close();
    }
}

/// 點 p 所在螢幕的工作區與縮放比例
unsafe fn monitor_of(p: POINT) -> ((i32, i32, i32, i32), f32) {
    let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let w = if GetMonitorInfoW(mon, &mut mi).as_bool() { mi.rcWork } else { RECT { left: 0, top: 0, right: 1920, bottom: 1040 } };
    let mut dpi = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
    ((w.left, w.top, w.right, w.bottom), (dpi.0 as f32 / 96.0).max(1.0))
}

unsafe fn run(shared: Arc<Mutex<Shared>>, hwnd_out: Arc<AtomicIsize>, opts: Options) {
    let at = match opts.near {
        Some((l, t, r, b)) => POINT { x: (l + r) / 2, y: (t + b) / 2 },
        None => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            p
        }
    };
    let (work, scale) = monitor_of(at);
    let content = shared.lock().unwrap().content.clone();
    let Some((pm, layout)) = render(&content, scale, None) else { return };
    let gap = (12.0 * scale) as i32;
    let pos = place((layout.w as i32, layout.h as i32), work, opts.near, opts.bottom, gap);
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("ScreenRecorderBubble");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    let mut ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE;
    if !opts.clickable {
        ex |= WS_EX_TRANSPARENT;
    }
    let Ok(hwnd) = CreateWindowExW(ex, class, w!("提示"), WS_POPUP, pos.0, pos.1, layout.w as i32, layout.h as i32, None, None, Some(hinst.into()), None) else { return };
    // 不被截圖、錄影拍進去
    let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    if opts.esc {
        let _ = RegisterHotKey(Some(hwnd), 1, MOD_NOREPEAT, VK_ESCAPE.0 as u32);
    }
    STATE.with(|s| {
        *s.borrow_mut() =
            Some(State { shared: shared.clone(), shown: content, layout, hover: None, pos, scale, work, near: opts.near, bottom: opts.bottom, inside: false, clickable: opts.clickable, on: opts.on })
    });
    crate::shot_toast_win::present(hwnd, &pm, pos);
    hwnd_out.store(hwnd.0 as isize, Ordering::Release);
    // 建好視窗前內容可能已經換了、或已經要關了
    if shared.lock().unwrap().closed {
        let _ = DestroyWindow(hwnd);
    } else {
        update(hwnd, false);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    if opts.esc {
        let _ = UnregisterHotKey(Some(hwnd), 1);
    }
    hwnd_out.store(0, Ordering::Release);
    STATE.with(|s| *s.borrow_mut() = None);
}

/// 內容或滑鼠位置變了：重畫（大小變了時重新放好位置）
unsafe fn update(hwnd: HWND, force: bool) {
    let redraw = STATE.with(|s| {
        let mut b = s.borrow_mut();
        let st = b.as_mut()?;
        let content = st.shared.lock().unwrap().content.clone();
        if content == st.shown && !force {
            return None;
        }
        st.shown = content;
        let (pm, layout) = render(&st.shown, st.scale, st.hover)?;
        if (layout.w, layout.h) != (st.layout.w, st.layout.h) {
            st.pos = place((layout.w as i32, layout.h as i32), st.work, st.near, st.bottom, (12.0 * st.scale) as i32);
        }
        st.layout = layout;
        Some((pm, st.pos))
    });
    if let Some((pm, pos)) = redraw {
        crate::shot_toast_win::present(hwnd, &pm, pos);
    }
}

/// 發生事件：記下來並通知
fn event(id: u32) {
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            st.shared.lock().unwrap().events.push(id);
            if let Some(on) = &st.on {
                on(id);
            }
        }
    });
}

fn point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE),
        WM_SETCURSOR => {
            let hand = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.clickable && (st.hover.is_some() || st.layout.buttons.is_empty())));
            if let Ok(c) = LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            let changed = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                let first = !st.inside;
                st.inside = true;
                let h = hit(&st.layout, x, y);
                let changed = h != st.hover;
                st.hover = h;
                Some((first, changed))
            });
            if let Some((first, changed)) = changed {
                if first {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                }
                if changed {
                    update(hwnd, true);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let changed = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                st.inside = false;
                st.hover.take().map(|_| ())
            });
            if changed.is_some() {
                update(hwnd, true);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            let id = STATE.with(|s| s.borrow().as_ref().map(|st| hit(&st.layout, x, y).unwrap_or(BODY)));
            if let Some(id) = id {
                event(id);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            event(ESC);
            LRESULT(0)
        }
        WM_UPDATE => {
            update(hwnd, false);
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
