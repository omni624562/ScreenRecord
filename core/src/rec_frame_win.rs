//! 錄影範圍的外框與控制列（Windows）：錄自訂範圍時，在範圍外圍畫一圈細框，倒數、錄影、暫停期間一直顯示，
//! 讓人知道錄到哪裡；框的左上方有一個小控制列（已錄時間、暫停 / 繼續、停止；倒數中是剩幾秒與取消）。
//! 拖曳控制列（按鈕以外的地方）可以移動範圍：外框跟著游標走，放開後從新位置繼續錄（大小不變）。
//! 框與控制列都在範圍外面（放不下時控制列改到框下方或框內左上），也設成不被擷取（Windows 10 2004 以後），
//! 不會錄進影片。框讓滑鼠穿過；控制列可以點，但不搶焦點（正在打字的視窗不受影響）。
//! 錄影中紅色，暫停時橘色。座標是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::format::clock;
use crate::recorder::FrameInfo;
use crate::types::{RecorderState, Rect};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GetMonitorInfoW, GetStockObject,
    InvalidateRect, MonitorFromRect, Polygon, RoundRect, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    FW_SEMIBOLD, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetCursorPos, GetSystemMetrics, LoadCursorW, PeekMessageW, RegisterClassExW, SetCursor, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, IDC_HAND, IDC_SIZEALL, LWA_ALPHA, MA_NOACTIVATE, MSG, PM_REMOVE, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WDA_EXCLUDEFROMCAPTURE, WM_CAPTURECHANGED, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

/// 框的粗細與離範圍的距離（像素）
const THICK: i32 = 3;
const GAP: i32 = 2;
/// COLORREF 是 BGR：#E5484D、#F5A524、#9CA3AF
const RED: COLORREF = COLORREF(0x004D_48E5);
const AMBER: COLORREF = COLORREF(0x0024_A5F5);
const GREY: COLORREF = COLORREF(0x00AF_A39C);
/// 控制列：#202124 底、#3C4043 滑鼠移上去的按鈕、白字
const BAR_BG: COLORREF = COLORREF(0x0024_2120);
const BAR_HOVER: COLORREF = COLORREF(0x0043_403C);
const WHITE: COLORREF = COLORREF(0x00FF_FFFF);
/// 滑鼠離開視窗（windows crate 沒有列出）
const WM_MOUSELEAVE: u32 = 0x02A3;

/// 控制列的按鈕送出的指令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCmd {
    Pause,
    Resume,
    Stop,
    /// 打點
    Mark,
    /// 把範圍移到 (x, y)（左上角，虛擬桌面座標）
    Move(i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Mark,
    Pause,
    Resume,
    Stop,
}

/// 控制列目前的內容（只在控制列的執行緒上用）
#[derive(Clone, PartialEq)]
struct Bar {
    text: String,
    /// 打了幾個點（時間後面顯示）
    marks: u32,
    dot: COLORREF,
    buttons: Vec<Button>,
    /// DPI 縮放（1.0 = 96 DPI）
    scale: f32,
}

thread_local! {
    static FRAME_COLOR: Cell<COLORREF> = const { Cell::new(RED) };
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
    static HOVER: Cell<Option<usize>> = const { Cell::new(None) };
    /// 按下的按鈕（迴圈裡取出後送出指令）
    static CLICKED: Cell<Option<Button>> = const { Cell::new(None) };
    /// 正在拖曳控制列：按下時的游標位置與範圍左上角、目前的游標位置
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };
    /// 目前顯示的範圍左上角（拖曳的起點）
    static ORIGIN: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    /// 範圍的寬高
    static BAR_AREA: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    /// 放開後要移去的位置（錄影器移好之前，外框先停在這裡）
    static MOVE_TO: Cell<Option<(i32, i32)>> = const { Cell::new(None) };
    /// 錄整個螢幕：沒有外框、控制列不能拖曳
    static FULL: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
struct Drag {
    start: POINT,
    origin: (i32, i32),
    cur: POINT,
}

/// 拖曳中外框的左上角（限制在虛擬桌面內）
fn drag_origin(d: &Drag, size: (i32, i32)) -> (i32, i32) {
    let (vx, vy, vw, vh) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
    let x = d.origin.0 + d.cur.x - d.start.x;
    let y = d.origin.1 + d.cur.y - d.start.y;
    (x.clamp(vx, (vx + vw - size.0).max(vx)), y.clamp(vy, (vy + vh - size.1).max(vy)))
}

/// 背景執行緒：每 0.1 秒問一次 info()（None = 不顯示），跟著顯示、移動或隱藏外框與控制列；
/// 按下控制列的按鈕時呼叫 cmd()
pub fn spawn(info: impl Fn() -> Option<FrameInfo> + Send + 'static, cmd: impl Fn(FrameCmd) + Send + 'static) {
    let _ = std::thread::Builder::new().name("rec-frame".into()).spawn(move || unsafe { run(info, cmd) });
}

// ───────────── 版面（以 96 DPI 為準，乘上 scale） ─────────────

const BAR_H: f32 = 34.0;
const BAR_PAD: f32 = 12.0;
const DOT: f32 = 10.0;
/// 左邊的拖曳點（2×3 個小點）
const GRIP: f32 = 12.0;
const TEXT_W: f32 = 76.0;
/// 「・2 點」的寬度
const MARKS_W: f32 = 44.0;
const BTN_W: f32 = 32.0;
const BTN_GAP: f32 = 2.0;

fn px(v: f32, scale: f32) -> i32 {
    (v * scale).round() as i32
}

fn bar_size(b: &Bar) -> (i32, i32) {
    let s = b.scale;
    let w = BAR_PAD + GRIP + DOT + 8.0 + TEXT_W + if b.marks > 0 { MARKS_W } else { 0.0 } + 6.0 + b.buttons.len() as f32 * (BTN_W + BTN_GAP) + 4.0;
    (px(w, s), px(BAR_H, s))
}

/// 第 i 個按鈕的範圍（控制列的視窗座標）
fn button_rect(b: &Bar, i: usize) -> RECT {
    let s = b.scale;
    let (w, h) = bar_size(b);
    let n = b.buttons.len();
    let right = w - px(4.0, s) - ((n - 1 - i) as f32 * (BTN_W + BTN_GAP) * s).round() as i32;
    let m = px(4.0, s);
    RECT { left: right - px(BTN_W, s), top: m, right, bottom: h - m }
}

fn bar_for(f: &FrameInfo, scale: f32) -> Bar {
    match f.state {
        RecorderState::Countdown => Bar { text: format!("倒數 {}", f.countdown_ms.unwrap_or(0).div_ceil(1000)), marks: 0, dot: GREY, buttons: vec![Button::Stop], scale },
        RecorderState::Paused => Bar { text: clock(f.recorded_ms as f64), marks: f.markers, dot: AMBER, buttons: vec![Button::Mark, Button::Resume, Button::Stop], scale },
        _ => Bar { text: clock(f.recorded_ms as f64), marks: f.markers, dot: RED, buttons: vec![Button::Mark, Button::Pause, Button::Stop], scale },
    }
}

unsafe fn run(info: impl Fn() -> Option<FrameInfo>, cmd: impl Fn(FrameCmd)) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let frame_class = w!("ScreenRecorderRecFrame");
    let bar_class = w!("ScreenRecorderRecBar");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(frame_proc), hInstance: hinst.into(), lpszClassName: frame_class, ..Default::default() };
    RegisterClassExW(&wc);
    let wc = WNDCLASSEXW { lpfnWndProc: Some(bar_proc), lpszClassName: bar_class, ..wc };
    RegisterClassExW(&wc);
    // 上、下、左、右四條（中間是空的，不擋範圍裡的畫面與滑鼠）
    let mut edges: Vec<HWND> = Vec::new();
    for _ in 0..4 {
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
        match CreateWindowExW(ex, frame_class, w!("錄影範圍"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
            Ok(h) => {
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
                // 不被擷取（舊版 Windows 不支援時忽略；框本來就在範圍外）
                let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
                edges.push(h);
            }
            Err(e) => {
                crate::warn!("無法建立錄影範圍外框：{e}");
                return;
            }
        }
    }
    let bar = match CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, bar_class, w!("錄影控制"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
        Ok(h) => {
            let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, std::mem::size_of_val(&round) as u32);
            Some(h)
        }
        Err(e) => {
            crate::warn!("無法建立錄影控制列：{e}");
            None
        }
    };
    let mut shown: Option<(Rect, bool)> = None;
    let mut pending: Option<((i32, i32), Instant)> = None;
    let mut raised = Instant::now();
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(b) = CLICKED.with(|c| c.take()) {
            cmd(match b {
                Button::Pause => FrameCmd::Pause,
                Button::Resume => FrameCmd::Resume,
                Button::Stop => FrameCmd::Stop,
                Button::Mark => FrameCmd::Mark,
            });
        }
        let mut f = info();
        // 拖曳中：外框跟著游標；放開後到錄影器移好之前（最多 3 秒）先停在新位置
        let dragging = DRAG.with(|d| d.get());
        let idle = f.is_none() && dragging.is_none();
        if let Some(fi) = &mut f {
            let size = (fi.area.width, fi.area.height);
            if let Some(d) = dragging {
                (fi.area.x, fi.area.y) = drag_origin(&d, size);
            } else if let Some((to, since)) = pending {
                if (fi.area.x, fi.area.y) == to || since.elapsed() > Duration::from_secs(3) {
                    pending = None;
                } else {
                    (fi.area.x, fi.area.y) = to;
                }
            }
            ORIGIN.with(|o| o.set((fi.area.x, fi.area.y)));
            BAR_AREA.with(|a| a.set(size));
        } else {
            pending = None;
        }
        if let Some(to) = MOVE_TO.with(|m| m.take()) {
            pending = Some((to, Instant::now()));
            cmd(FrameCmd::Move(to.0, to.1));
        }
        let want = f.map(|f| (f.area, f.state == RecorderState::Paused));
        let full = f.is_some_and(|f| f.full);
        FULL.with(|c| c.set(full));
        if want != shown {
            match want {
                Some(_) if full => {
                    // 錄整個螢幕：只有控制列（外框會在螢幕外面）
                    for h in &edges {
                        let _ = ShowWindow(*h, SW_HIDE);
                    }
                }
                Some((r, paused)) => {
                    FRAME_COLOR.with(|c| c.set(if paused { AMBER } else { RED }));
                    let o = GAP + THICK;
                    let (l, t, rt, b) = (r.x - o, r.y - o, r.x + r.width + o, r.y + r.height + o);
                    let rects = [(l, t, rt - l, THICK), (l, b - THICK, rt - l, THICK), (l, t, THICK, b - t), (rt - THICK, t, THICK, b - t)];
                    for (h, (x, y, w, hh)) in edges.iter().zip(rects) {
                        let _ = SetWindowPos(*h, Some(HWND_TOPMOST), x, y, w, hh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                        let _ = InvalidateRect(Some(*h), None, true);
                    }
                }
                None => {
                    for h in edges.iter().chain(bar.as_ref()) {
                        let _ = ShowWindow(*h, SW_HIDE);
                    }
                    BAR.with(|b| *b.borrow_mut() = None);
                }
            }
            raised = Instant::now();
        } else if shown.is_some() && raised.elapsed() > Duration::from_secs(2) {
            // 其他最上層視窗出現時，框可能被蓋住：定時移回最上層
            for h in edges.iter().chain(bar.as_ref()) {
                let _ = SetWindowPos(*h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
            }
            raised = Instant::now();
        }
        if let (Some(h), Some(f)) = (bar, f) {
            update_bar(h, &f, want != shown);
        }
        shown = want;
        // 拖曳中更新得快一點，外框才跟得上游標
        // 拖曳中 15ms；顯示中 100ms；沒在錄影時 250ms 看一次就好（閒置時少喚醒）
        std::thread::sleep(Duration::from_millis(if dragging.is_some() {
            15
        } else if idle {
            250
        } else {
            100
        }));
    }
}

/// 更新控制列的內容；範圍改變（moved）或大小改變時重新擺放
unsafe fn update_bar(h: HWND, f: &FrameInfo, moved: bool) {
    let r = f.area;
    let o = GAP + THICK;
    let frame = RECT { left: r.x - o, top: r.y - o, right: r.x + r.width + o, bottom: r.y + r.height + o };
    let mon = MonitorFromRect(&frame, MONITOR_DEFAULTTONEAREST);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let next = bar_for(f, dx as f32 / 96.0);
    let prev = BAR.with(|b| b.borrow().clone());
    if prev.as_ref() == Some(&next) && !moved {
        return;
    }
    let resized = prev.as_ref().is_none_or(|p| bar_size(p) != bar_size(&next));
    BAR.with(|b| *b.borrow_mut() = Some(next.clone()));
    if moved || resized {
        let (w, bh) = bar_size(&next);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let m = if GetMonitorInfoW(mon, &mut mi).as_bool() { mi.rcMonitor } else { frame };
        let gap = px(4.0, next.scale);
        // 錄整個螢幕：螢幕上方中間（設成不被擷取，不會錄進影片）
        // 框的左上方；上面放不下改到框下方，都放不下時放在框內左上（不會被錄進去）
        let (x, y) = if f.full {
            ((m.left + m.right - w) / 2, m.top + gap * 2)
        } else if frame.top - gap - bh >= m.top {
            (frame.left, frame.top - gap - bh)
        } else if frame.bottom + gap + bh <= m.bottom {
            (frame.left, frame.bottom + gap)
        } else {
            (r.x + gap * 2, r.y + gap * 2)
        };
        let x = x.clamp(m.left, (m.right - w).max(m.left));
        let _ = SetWindowPos(h, Some(HWND_TOPMOST), x, y, w, bh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
    let _ = InvalidateRect(Some(h), None, false);
}

unsafe fn fill(hdc: HDC, rc: &RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    FillRect(hdc, rc, brush);
    let _ = DeleteObject(brush.into());
}

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            fill(HDC(wparam.0 as *mut _), &rc, FRAME_COLOR.with(|c| c.get()));
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            fill(hdc, &rc, FRAME_COLOR.with(|c| c.get()));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 滑鼠在第幾個按鈕上
fn hit(lparam: LPARAM) -> Option<usize> {
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    BAR.with(|b| {
        let b = b.borrow();
        let b = b.as_ref()?;
        (0..b.buttons.len()).find(|i| {
            let r = button_rect(b, *i);
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
    })
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 點了也不搶焦點
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            if let Some(mut d) = DRAG.with(|d| d.get()) {
                let _ = GetCursorPos(&mut d.cur);
                DRAG.with(|c| c.set(Some(d)));
                return LRESULT(0);
            }
            let h = hit(lparam);
            if HOVER.with(|c| c.replace(h)) != h {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            HOVER.with(|c| c.set(None));
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // 按鈕上是手指，其他地方可以拖曳
            let hand = HOVER.with(|c| c.get()).is_some() && DRAG.with(|d| d.get()).is_none();
            let cursor = if hand {
                IDC_HAND
            } else if FULL.with(|c| c.get()) {
                IDC_ARROW
            } else {
                IDC_SIZEALL
            };
            if let Ok(c) = LoadCursorW(None, cursor) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            if hit(lparam).is_none() && !FULL.with(|c| c.get()) {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                DRAG.with(|d| d.set(Some(Drag { start: p, origin: ORIGIN.with(|o| o.get()), cur: p })));
                SetCapture(hwnd);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // 拖曳被打斷（例如切換視窗）：放回原位
            DRAG.with(|d| d.set(None));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(mut d) = DRAG.with(|d| d.take()) {
                let _ = ReleaseCapture();
                let _ = GetCursorPos(&mut d.cur);
                // 只是點一下（沒移動）不算
                if (d.cur.x - d.start.x).abs() + (d.cur.y - d.start.y).abs() > 3 {
                    let size = BAR_AREA.with(|a| a.get());
                    MOVE_TO.with(|m| m.set(Some(drag_origin(&d, size))));
                }
                return LRESULT(0);
            }
            if let Some(i) = hit(lparam) {
                let b = BAR.with(|b| b.borrow().as_ref().and_then(|b| b.buttons.get(i).copied()));
                CLICKED.with(|c| c.set(b));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_bar(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 畫控制列（先畫在記憶體 DC 再一次貼上，不閃爍）
unsafe fn paint_bar(hwnd: HWND, hdc: HDC) {
    let Some(b) = BAR.with(|b| b.borrow().clone()) else { return };
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let mem = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem, bmp.into());
    let old_pen = SelectObject(mem, GetStockObject(NULL_PEN));
    let s = b.scale;
    fill(mem, &rc, BAR_BG);
    // 拖曳點（錄整個螢幕時不能拖，不畫）
    let cy = h / 2;
    let g = px(2.0, s).max(2);
    let grip: &[(i32, i32)] = if FULL.with(|c| c.get()) { &[] } else { &[(0, -1), (0, 0), (0, 1), (1, -1), (1, 0), (1, 1)] };
    for &(ix, iy) in grip {
        let (gx, gy) = (px(BAR_PAD - 2.0 + ix as f32 * 5.0, s), cy + iy * px(5.0, s) - g / 2);
        fill(mem, &RECT { left: gx, top: gy, right: gx + g, bottom: gy + g }, GREY);
    }
    // 狀態點
    let x0 = px(BAR_PAD + GRIP, s);
    let d = px(DOT, s);
    let brush = CreateSolidBrush(b.dot);
    let old_brush = SelectObject(mem, brush.into());
    let _ = Ellipse(mem, x0, cy - d / 2, x0 + d, cy - d / 2 + d);
    // 時間
    let font = CreateFontW(-px(14.0, s), 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Microsoft JhengHei UI"));
    let old_font = SelectObject(mem, font.into());
    SetBkMode(mem, TRANSPARENT);
    SetTextColor(mem, WHITE);
    let mut text: Vec<u16> = b.text.encode_utf16().collect();
    let mut tr = RECT { left: x0 + d + px(8.0, s), top: 0, right: x0 + d + px(8.0 + TEXT_W, s), bottom: h };
    DrawTextW(mem, &mut text, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    if b.marks > 0 {
        SetTextColor(mem, AMBER);
        let mut mt: Vec<u16> = format!("・{} 點", b.marks).encode_utf16().collect();
        let mut mr = RECT { left: tr.right, top: 0, right: tr.right + px(MARKS_W, s), bottom: h };
        DrawTextW(mem, &mut mt, &mut mr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
    // 按鈕
    let hover = HOVER.with(|c| c.get());
    for (i, btn) in b.buttons.iter().enumerate() {
        let r = button_rect(&b, i);
        if hover == Some(i) {
            let hb = CreateSolidBrush(BAR_HOVER);
            let prev = SelectObject(mem, hb.into());
            let rr = px(8.0, s);
            let _ = RoundRect(mem, r.left, r.top, r.right, r.bottom, rr, rr);
            SelectObject(mem, prev);
            let _ = DeleteObject(hb.into());
        }
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let k = px(6.0, s);
        match btn {
            Button::Pause => {
                let bw = px(3.5, s).max(2);
                fill(mem, &RECT { left: cx - k + px(1.0, s), top: cy - k, right: cx - k + px(1.0, s) + bw, bottom: cy + k }, WHITE);
                fill(mem, &RECT { left: cx + k - px(1.0, s) - bw, top: cy - k, right: cx + k - px(1.0, s), bottom: cy + k }, WHITE);
            }
            Button::Resume => {
                let wb = CreateSolidBrush(WHITE);
                let prev = SelectObject(mem, wb.into());
                let pts = [POINT { x: cx - k + px(2.0, s), y: cy - k }, POINT { x: cx + k, y: cy }, POINT { x: cx - k + px(2.0, s), y: cy + k }];
                let _ = Polygon(mem, &pts);
                SelectObject(mem, prev);
                let _ = DeleteObject(wb.into());
            }
            Button::Stop => {
                let q = px(5.5, s);
                fill(mem, &RECT { left: cx - q, top: cy - q, right: cx + q, bottom: cy + q }, RED);
            }
            Button::Mark => {
                // 小旗子：旗桿 + 三角旗
                let pole = px(2.0, s).max(1);
                fill(mem, &RECT { left: cx - k + px(1.0, s), top: cy - k, right: cx - k + px(1.0, s) + pole, bottom: cy + k }, WHITE);
                let wb = CreateSolidBrush(AMBER);
                let prev = SelectObject(mem, wb.into());
                let x0 = cx - k + px(1.0, s) + pole;
                let pts = [POINT { x: x0, y: cy - k }, POINT { x: cx + k, y: cy - k + px(3.5, s) }, POINT { x: x0, y: cy - k + px(7.0, s) }];
                let _ = Polygon(mem, &pts);
                SelectObject(mem, prev);
                let _ = DeleteObject(wb.into());
            }
        }
    }
    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
    SelectObject(mem, old_font);
    SelectObject(mem, old_brush);
    SelectObject(mem, old_pen);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(font.into());
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(mem);
}
