//! 框選截圖的畫面（Windows 原生視窗，不靠操作視窗）：一個蓋住整個桌面的最上層視窗，
//! 顯示凍結的畫面並變暗；拖曳框選範圍，或點一下選游標下的視窗；Esc 或右鍵取消。截圖與錄影共用。
//! 截圖、選錄影範圍時放開滑鼠後範圍留著：可以拖曳邊、角調整或整個移動，下方的工具列選要做什麼
//! （完成、編輯、複製、釘在桌面、取消；Enter 完成、在範圍裡點兩下也是完成）。設定「放開滑鼠就直接存檔」時放開就截。
//! 游標旁有放大鏡（看清楚像素、方向鍵微調位置）。
//! 座標都是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::snip_tools::{drag_grab, grab_at, handles, loupe_pos, render_loupe, toolbar_pos, tools, Grab, SnipAction, Tool};
use crate::types::Rect;
use crate::{tr, trf};
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect,
    FrameRect, GetDC, GetMonitorInfoW, GetTextExtentPoint32W, GetUpdateRgn, InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, ReleaseDC, RoundRect, SelectClipRgn, SelectObject, SetBkMode,
    SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD, HBITMAP,
    HBRUSH, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, PostQuitMessage, RegisterClassExW, SetCursor, SetCursorPos, SetForegroundWindow,
    ShowWindow, TranslateMessage, CS_DBLCLKS, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, MSG, SW_SHOW, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 小於這個大小（像素）的拖曳當成點一下
const MIN_DRAG: i32 = 4;
const ACCENT: COLORREF = COLORREF(0x00EB_6325); // #2563EB（BGR）
const ACCENT_HOT: COLORREF = COLORREF(0x00F6_823B); // #3B82F6
const BAR_BG: COLORREF = COLORREF(0x0024_2220);
const BAR_HOT: COLORREF = COLORREF(0x0048_4440);

struct State {
    desk: Rect,
    windows: Vec<Rect>,
    /// 原始與變暗的畫面（記憶體 DC 上的 DIB）
    bright: HDC,
    /// 原始畫面的像素（BGRA、由上而下；取色用，跟著 bright 的 DIB 一起釋放）
    bits: *const u8,
    dim: HDC,
    /// 畫面先畫在這裡再一次貼上（不閃爍）
    back: HDC,
    bitmaps: [HBITMAP; 3],
    font: HFONT,
    drag: Option<(POINT, POINT)>,
    hover: Option<Rect>,
    result: Option<Rect>,
    /// 工具列選的動作（完成、編輯、複製、釘在桌面）
    action: SnipAction,
    /// 選來做什麼（說明文字不同）
    record: Mode,
    /// 放開滑鼠後範圍留著，可以調整並從工具列選要做什麼（截圖、選錄影範圍）
    adjust: bool,
    /// 框好、可以調整的範圍（視窗座標）
    sel: Option<RECT>,
    /// 正在拖曳範圍的邊、角或整個範圍：(哪裡, 按下的位置, 原本的範圍)
    grab: Option<(Grab, POINT, RECT)>,
    /// 工具列按鈕的位置（畫的時候算好，點的時候用）、滑鼠在哪個按鈕上、按下的按鈕
    bar: Vec<(Tool, RECT)>,
    hot: Option<Tool>,
    pressed: Option<Tool>,
    /// 游標位置（視窗座標）
    cursor: Option<POINT>,
    /// 放大鏡的大小（第一次畫的時候量到；算要重畫的地方用）
    loupe_size: Option<(i32, i32)>,
    /// 上次畫在變暗畫面上的東西在哪裡（下次只要擦掉這些、畫新的）
    drawn: Option<Vec<RECT>>,
}

/// 框選的用途
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shot,
    Record,
    /// 長截圖（捲動截圖）
    Scroll,
    /// 只抓文字（辨識後複製到剪貼簿）
    Text,
}

impl Mode {
    /// 顯示放大鏡（框選時對準用）
    fn loupe(self) -> bool {
        matches!(self, Mode::Shot | Mode::Record)
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 顯示框選畫面直到使用者選好或取消（在呼叫的執行緒上執行訊息迴圈）。
/// rgba：整個桌面的畫面（由上而下、不透明）；windows：看得到的視窗（上層在前）；record：用途；
/// instant：截圖時放開滑鼠就直接存檔（不留下範圍調整）。回傳 (範圍, 工具列選的動作)
pub fn select(rgba: &[u8], desk: Rect, windows: Vec<Rect>, record: Mode, instant: bool) -> Option<(Rect, SnipAction)> {
    run(rgba, desk, windows, record, instant).and_then(|r| r.0.map(|rect| (rect, r.1)))
}

/// 顯示畫面直到結束：(框選的範圍, 工具列選的動作)
fn run(rgba: &[u8], desk: Rect, windows: Vec<Rect>, record: Mode, instant: bool) -> Option<(Option<Rect>, SnipAction)> {
    let (w, h) = (desk.width, desk.height);
    if w <= 0 || h <= 0 || rgba.len() < (w * h * 4) as usize {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        // 記憶體不夠時（多個 4K 螢幕）可能建不出來：已經建好的要釋放
        let Some((bright, b1, bits)) = dib(screen, w, h, |dst| {
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
                *d = [s[2], s[1], s[0], 255];
            }
        }) else {
            ReleaseDC(None, screen);
            return None;
        };
        let Some((dim, b2, _)) = dib(screen, w, h, |dst| {
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
                let k = |v: u8| (v as u16 * 45 / 100) as u8;
                *d = [k(s[2]), k(s[1]), k(s[0]), 255];
            }
        }) else {
            let _ = DeleteDC(bright);
            let _ = DeleteObject(b1.into());
            ReleaseDC(None, screen);
            return None;
        };
        let back = CreateCompatibleDC(Some(screen));
        let b3 = CreateCompatibleBitmap(screen, w, h);
        SelectObject(back, b3.into());
        ReleaseDC(None, screen);
        let font = CreateFontW(-18, 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Microsoft JhengHei UI"));
        let adjust = match record {
            Mode::Shot => !instant,
            Mode::Record => true,
            _ => false,
        };
        let state = State {
            desk,
            windows,
            bright,
            bits,
            dim,
            back,
            bitmaps: [b1, b2, b3],
            font,
            drag: None,
            hover: None,
            result: None,
            action: SnipAction::Save,
            record,
            adjust,
            sel: None,
            grab: None,
            bar: vec![],
            hot: None,
            pressed: None,
            cursor: None,
            loupe_size: None,
            drawn: None,
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));

        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("ScreenRecorderSnip");
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, style: CS_DBLCLKS, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassExW(&wc);
        let title: Vec<u16> = match record {
            Mode::Record => tr!("框選錄影範圍", "Select area to record"),
            Mode::Scroll => tr!("長截圖", "Scrolling screenshot"),
            Mode::Text => tr!("只抓文字", "Grab text"),
            Mode::Shot => tr!("框選截圖", "Screenshot of selected area"),
        }
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class, PCWSTR(title.as_ptr()), WS_POPUP, desk.x, desk.y, w, h, None, None, Some(hinst.into()), None);
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
        Some((st.result, st.action))
    }
}

/// 建立 w×h 的 32 位元 DIB 並選進新的記憶體 DC；fill 填入 BGRA 像素（由上而下）。也回傳像素的位置
unsafe fn dib(screen: HDC, w: i32, h: i32, fill: impl FnOnce(&mut [u8])) -> Option<(HDC, HBITMAP, *const u8)> {
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
    Some((dc, bmp, bits as *const u8))
}

/// 凍結畫面上 (x, y)（視窗座標）的顏色
fn pixel(st: &State, x: i32, y: i32) -> Option<[u8; 3]> {
    if x < 0 || y < 0 || x >= st.desk.width || y >= st.desk.height || st.bits.is_null() {
        return None;
    }
    let i = ((y * st.desk.width + x) * 4) as usize;
    // SAFETY：bits 是 bright 的 DIB（w×h×4），視窗關閉前不會釋放
    let p = unsafe { std::slice::from_raw_parts(st.bits.add(i), 3) };
    Some([p[2], p[1], p[0]])
}

/// 按住 Shift / Ctrl
fn key_down(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

fn point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 }
}

/// 拖曳範圍（視窗座標）
fn drag_rect(a: POINT, b: POINT) -> RECT {
    RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x), bottom: a.y.max(b.y) }
}

fn tuple(r: RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right, r.bottom)
}

fn rect_of((left, top, right, bottom): (i32, i32, i32, i32)) -> RECT {
    RECT { left, top, right, bottom }
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// 視窗座標的範圍換成桌面座標
fn desk_rect(st: &State, r: RECT) -> Rect {
    Rect { x: r.left + st.desk.x, y: r.top + st.desk.y, width: r.right - r.left, height: r.bottom - r.top }
}

/// 桌面座標換成視窗座標
fn win_rect(st: &State, r: Rect) -> RECT {
    RECT { left: r.x - st.desk.x, top: r.y - st.desk.y, right: r.x + r.width - st.desk.x, bottom: r.y + r.height - st.desk.y }
}

/// 點 p（視窗座標）所在的螢幕：範圍（視窗座標）與縮放比例（DPI / 96）
fn monitor_at(st: &State, p: POINT) -> (RECT, f32) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: p.x + st.desk.x, y: p.y + st.desk.y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let r = if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let m = mi.rcMonitor;
            RECT { left: m.left - st.desk.x, top: m.top - st.desk.y, right: m.right - st.desk.x, bottom: m.bottom - st.desk.y }
        } else {
            RECT { left: 0, top: 0, right: st.desk.width, bottom: st.desk.height }
        };
        let mut dpi = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
        (r, (dpi.0 as f32 / 96.0).clamp(1.0, 3.0))
    }
}

/// 游標下的視窗（視窗座標的點）
fn window_at(st: &State, p: POINT) -> Option<Rect> {
    let (x, y) = (p.x + st.desk.x, p.y + st.desk.y);
    st.windows.iter().find(|w| x >= w.x && x < w.x + w.width && y >= w.y && y < w.y + w.height).copied()
}

/// 工具列上的哪個按鈕
fn bar_hit(st: &State, p: POINT) -> Option<Tool> {
    if st.sel.is_none() || st.grab.is_some() || st.drag.is_some() {
        return None;
    }
    st.bar.iter().find(|(_, r)| contains(r, p)).map(|(t, _)| *t)
}

/// 範圍的邊、角可以按到的距離
fn grab_tol(st: &State) -> i32 {
    let p = st.sel.map(|s| POINT { x: s.left, y: s.top }).unwrap_or_default();
    (7.0 * monitor_at(st, p).1) as i32
}

/// 結束並回傳這個範圍與動作（超出桌面的部分裁掉）
fn finish(st: &mut State, r: RECT, action: SnipAction) -> bool {
    let r = RECT { left: r.left.max(0), top: r.top.max(0), right: r.right.min(st.desk.width), bottom: r.bottom.min(st.desk.height) };
    if r.right - r.left < 1 || r.bottom - r.top < 1 {
        return false;
    }
    st.result = Some(desk_rect(st, r));
    st.action = action;
    true
}

/// 按下工具列的按鈕；回傳 true = 結束
fn run_tool(st: &mut State, t: Tool) -> bool {
    let Some(sel) = st.sel else { return false };
    match t {
        Tool::Cancel => {
            st.result = None;
            true
        }
        Tool::Full => {
            // 改成範圍中間所在的整個螢幕（還可以再選要做什麼）
            st.sel = Some(monitor_at(st, POINT { x: (sel.left + sel.right) / 2, y: (sel.top + sel.bottom) / 2 }).0);
            false
        }
        t => t.action().is_some_and(|a| finish(st, sel, a)),
    }
}

/// 游標的樣子
fn cursor_for(st: &State, p: POINT) -> PCWSTR {
    if bar_hit(st, p).is_some() {
        return IDC_HAND;
    }
    if st.sel.is_some() && st.bar.iter().any(|(_, r)| contains(&RECT { left: r.left - 6, top: r.top - 6, right: r.right + 6, bottom: r.bottom + 6 }, p)) {
        return IDC_ARROW;
    }
    let g = match (st.grab, st.sel) {
        (Some((g, ..)), _) => Some(g),
        (None, Some(sel)) if st.adjust && st.drag.is_none() => grab_at(tuple(sel), (p.x, p.y), grab_tol(st)),
        _ => None,
    };
    match g {
        Some(Grab::Move) => IDC_SIZEALL,
        Some(Grab::NW | Grab::SE) => IDC_SIZENWSE,
        Some(Grab::NE | Grab::SW) => IDC_SIZENESW,
        Some(Grab::N | Grab::S) => IDC_SIZENS,
        Some(Grab::E | Grab::W) => IDC_SIZEWE,
        None => IDC_CROSS,
    }
}

/// 按下左鍵（在範圍裡點兩下以外的第二下也到這裡）
unsafe fn press(hwnd: HWND, p: POINT) {
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return };
        if let Some(t) = bar_hit(st, p) {
            st.pressed = Some(t);
            return;
        }
        if let Some(sel) = st.sel.filter(|_| st.adjust) {
            if let Some(g) = grab_at(tuple(sel), (p.x, p.y), grab_tol(st)) {
                st.grab = Some((g, p, sel));
                return;
            }
        }
        // 在範圍外按下：重新框選
        st.sel = None;
        st.drag = Some((p, p));
    });
    SetCapture(hwnd);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            let mut cur = POINT::default();
            let _ = GetCursorPos(&mut cur);
            let id = STATE.with(|s| s.borrow().as_ref().map(|st| cursor_for(st, POINT { x: cur.x - st.desk.x, y: cur.y - st.desk.y }))).unwrap_or(IDC_CROSS);
            if let Ok(c) = LoadCursorW(None, id) {
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
            // 只重畫有變的地方（之前和現在的選取範圍、標籤、工具列、放大鏡、說明）：
            // 整個桌面（4K、多螢幕）每動一下都重畫會跟不上滑鼠
            let dirty = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                let before = st.drawn.clone();
                let old_cursor = st.cursor;
                {
                    st.cursor = Some(p);
                    if let Some((g, p0, orig)) = st.grab {
                        let bounds = (0, 0, st.desk.width, st.desk.height);
                        st.sel = Some(rect_of(drag_grab(tuple(orig), g, (p.x - p0.x, p.y - p0.y), bounds)));
                    } else if let Some((a, _)) = st.drag {
                        st.drag = Some((a, p));
                    } else if st.sel.is_none() {
                        st.hover = window_at(st, p);
                    }
                    st.hot = bar_hit(st, p);
                }
                let after = frame_rects(st);
                // 換到另一台螢幕（縮放可能不同，放大鏡、說明的大小跟著變）：整個重畫一次
                let mon = |c: Option<POINT>| c.map(|c| tuple(monitor_at(st, c).0));
                if mon(old_cursor) != mon(st.cursor) {
                    return None;
                }
                Some([before?, after?].concat())
            });
            match dirty {
                Some(list) => {
                    for r in list.iter().filter(|r| r.right > r.left && r.bottom > r.top) {
                        let _ = InvalidateRect(Some(hwnd), Some(r), false);
                    }
                }
                None => {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            press(hwnd, point(lparam));
            LRESULT(0)
        }
        // 在範圍裡點兩下：完成
        WM_LBUTTONDBLCLK => {
            let p = point(lparam);
            let done = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                let sel = st.sel.filter(|r| st.adjust && contains(r, p) && bar_hit(st, p).is_none())?;
                finish(st, sel, SnipAction::Save).then_some(())
            });
            if done.is_some() {
                let _ = DestroyWindow(hwnd);
            } else {
                press(hwnd, p);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let p = point(lparam);
            let done = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                if let Some(t) = st.pressed.take() {
                    return (bar_hit(st, p) == Some(t) && run_tool(st, t)).then_some(());
                }
                if st.grab.take().is_some() {
                    return None;
                }
                let (a, _) = st.drag.take()?;
                let r = drag_rect(a, p);
                let picked = if r.right - r.left >= MIN_DRAG && r.bottom - r.top >= MIN_DRAG {
                    r
                } else {
                    // 點一下：選游標下的視窗（超出桌面的部分裁掉）
                    win_rect(st, clamp_to(window_at(st, p)?, st.desk))
                };
                if st.adjust {
                    // 範圍留著：可以調整，再從工具列選要做什麼
                    st.sel = Some(picked);
                    st.hover = None;
                    return None;
                }
                finish(st, picked, SnipAction::Save).then_some(())
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
        // Enter：完成；還沒框選時截游標所在的整個螢幕
        WM_KEYDOWN if wparam.0 == 0x0D => {
            let done = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                if !matches!(st.record, Mode::Shot | Mode::Record) || st.drag.is_some() || st.grab.is_some() {
                    return None;
                }
                let r = match st.sel {
                    Some(r) => r,
                    None => {
                        let mut cur = POINT::default();
                        let _ = GetCursorPos(&mut cur);
                        monitor_at(st, POINT { x: cur.x - st.desk.x, y: cur.y - st.desk.y }).0
                    }
                };
                finish(st, r, SnipAction::Save).then_some(())
            });
            if done.is_some() {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        // Ctrl+C：框好範圍時只複製到剪貼簿
        WM_KEYDOWN if wparam.0 == 0x43 => {
            let copy_area = key_down(VK_CONTROL.0)
                && STATE.with(|s| {
                    let mut b = s.borrow_mut();
                    let Some(st) = b.as_mut() else { return false };
                    st.record == Mode::Shot && st.sel.is_some_and(|sel| finish(st, sel, SnipAction::Copy))
                });
            if copy_area {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        // 方向鍵：游標移動一個像素（框選對齊用）
        WM_KEYDOWN if (0x25..=0x28).contains(&wparam.0) => {
            let mut cur = POINT::default();
            if GetCursorPos(&mut cur).is_ok() {
                let (dx, dy) = match wparam.0 {
                    0x25 => (-1, 0),
                    0x26 => (0, -1),
                    0x27 => (1, 0),
                    _ => (0, 1),
                };
                let _ = SetCursorPos(cur.x + dx, cur.y + dy);
            }
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

/// 上方的說明
fn tip_text(st: &State) -> &'static str {
    let adjusting = st.sel.is_some() && st.drag.is_none();
    match st.record {
        Mode::Record if adjusting => tr!("拖曳邊框調整範圍，按 Enter 確定　　Esc 取消", "Drag the edges to adjust, then press Enter    Esc to cancel"),
        Mode::Record => tr!(
            "拖曳框選要錄影的範圍，或點一下選視窗；按 Enter 選整個螢幕　　Esc 或右鍵取消",
            "Drag to select the area to record, or click a window; Enter for the whole screen    Esc or right-click to cancel"
        ),
        Mode::Scroll => tr!(
            "框選要捲動的內容（例如網頁中間的部分），或點一下選視窗；選好後自動往下捲並接成長圖，按 Esc 停止",
            "Select what to scroll (e.g. part of a web page) or click a window; it then scrolls down and stitches a long image. Esc stops"
        ),
        Mode::Text => tr!(
            "框選要抓的文字（圖片、影片裡的字也可以），或點一下選視窗；字會直接複製到剪貼簿　　Esc 或右鍵取消",
            "Select the text to grab (text in images and videos works too), or click a window; it's copied straight to the clipboard    Esc or right-click to cancel"
        ),
        Mode::Shot if adjusting => {
            tr!("拖曳邊框調整範圍，在範圍裡點兩下或按 Enter 完成　　Esc 取消", "Drag the edges to adjust; double-click inside or press Enter to finish    Esc to cancel")
        }
        Mode::Shot => {
            tr!("拖曳框選範圍，或點一下選視窗；按 Enter 截整個螢幕　　Esc 或右鍵取消", "Drag to select an area or click a window; Enter for the whole screen    Esc or right-click to cancel")
        }
    }
}

/// 先畫在 back 裡再一次貼上（不閃爍）
unsafe fn paint(hwnd: HWND) {
    // 只畫要更新的地方：先取得更新範圍（BeginPaint 之後就清掉了），畫在記憶體 DC 上時也只畫那裡
    let rgn = CreateRectRgn(0, 0, 0, 0);
    let _ = GetUpdateRgn(hwnd, rgn, false);
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return };
        let (w, h) = (st.desk.width, st.desk.height);
        let mem = st.back;
        SelectClipRgn(mem, Some(rgn));
        let _ = BitBlt(mem, 0, 0, w, h, Some(st.dim), 0, 0, SRCCOPY);
        // 選取範圍（或游標下的視窗）用原本的亮度，外框與尺寸
        SetBkMode(mem, TRANSPARENT);
        SelectObject(mem, st.font.into());
        let brush = CreateSolidBrush(ACCENT);
        if let Some((r, text)) = sel_label(st) {
            let _ = BitBlt(mem, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(st.bright), r.left, r.top, SRCCOPY);
            for i in 0..2 {
                let f = RECT { left: r.left - 1 - i, top: r.top - 1 - i, right: r.right + 1 + i, bottom: r.bottom + 1 + i };
                FrameRect(mem, &f, brush);
            }
            let (x, y) = label_pos(r);
            label(mem, &text, x, y, brush);
        }
        // 框好的範圍：八個把手與工具列
        st.bar.clear();
        if let Some(r) = st.sel.filter(|_| st.adjust && st.drag.is_none()) {
            let scale = monitor_at(st, POINT { x: r.left, y: r.top }).1;
            let hs = (4.0 * scale).round() as i32;
            let white = CreateSolidBrush(COLORREF(0x00FF_FFFF));
            for (x, y) in handles(tuple(r)) {
                let hr = RECT { left: x - hs, top: y - hs, right: x + hs + 1, bottom: y + hs + 1 };
                FillRect(mem, &hr, white);
                FrameRect(mem, &hr, brush);
            }
            let _ = DeleteObject(white.into());
            if st.grab.is_none() {
                draw_toolbar(mem, st, r, scale);
            }
        }
        // 游標所在螢幕的上方：說明（游標移過去時改到下方，才不會擋住）
        SelectObject(mem, st.font.into());
        let (mut r, m, scale) = tip_rect(st, st.cursor.unwrap_or_else(|| cursor_now(st)));
        let mut t: Vec<u16> = tip_text(st).encode_utf16().collect();
        let bg = CreateSolidBrush(COLORREF(0x0020_2020));
        FillRect(mem, &r, bg);
        let _ = DeleteObject(bg.into());
        SetTextColor(mem, COLORREF(0x00FF_FFFF));
        DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        // 放大鏡：在游標所在的螢幕裡（框好範圍後只在調整時顯示，不擋住工具列）
        let show_loupe = st.sel.is_none() || st.grab.is_some();
        if let Some(c) = st.cursor.filter(|_| st.record.loupe() && show_loupe) {
            let area = (m.left, m.top, m.right, m.bottom);
            let hint = tr!("方向鍵可以微調", "Arrow keys fine-tune");
            st.loupe_size = draw_loupe(mem, st, c, area, scale, hint);
        }
        let _ = DeleteObject(brush.into());
        SelectClipRgn(mem, None);
        st.drawn = frame_rects(st);
        let p = ps.rcPaint;
        let _ = BitBlt(hdc, p.left, p.top, p.right - p.left, p.bottom - p.top, Some(mem), p.left, p.top, SRCCOPY);
    });
    let _ = EndPaint(hwnd, &ps);
    let _ = DeleteObject(rgn.into());
}

/// 亮起來的範圍（拖曳中、框好的範圍，或游標下的視窗）與它旁邊標籤的文字
fn sel_label(st: &State) -> Option<(RECT, String)> {
    let (sel, sized) = match (st.drag, st.sel) {
        (Some((a, b)), _) => (Some(drag_rect(a, b)), true),
        (None, Some(r)) => (Some(r), true),
        (None, None) => (st.hover.map(|r| win_rect(st, r)), false),
    };
    let r = sel.filter(|r| r.right > r.left && r.bottom > r.top)?;
    let text = if sized {
        format!("{} × {}", r.right - r.left, r.bottom - r.top)
    } else {
        trf!(
            "視窗 {} × {}・點一下{}",
            "Window {} × {} · Click to {}",
            r.right - r.left,
            r.bottom - r.top,
            match st.record {
                Mode::Record => tr!("選這個視窗", "select it"),
                Mode::Scroll => tr!("捲動擷取", "scroll and capture"),
                Mode::Text => tr!("抓裡面的文字", "grab its text"),
                Mode::Shot if st.adjust => tr!("選這個視窗", "select it"),
                Mode::Shot => tr!("擷取", "capture"),
            }
        )
    };
    Some((r, text))
}

/// 尺寸標籤的位置：範圍上方，放不下時放在範圍裡面
fn label_pos(r: RECT) -> (i32, i32) {
    (r.left.max(0), if r.top >= 34 { r.top - 34 } else { r.top + 6 })
}

/// 游標現在的位置（視窗座標）
unsafe fn cursor_now(st: &State) -> POINT {
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    POINT { x: cur.x - st.desk.x, y: cur.y - st.desk.y }
}

/// 說明的位置（游標所在螢幕的上方；游標移過去時改到下方），與那台螢幕和它的縮放
unsafe fn tip_rect(st: &State, curw: POINT) -> (RECT, RECT, f32) {
    let (m, scale) = monitor_at(st, curw);
    SelectObject(st.back, st.font.into());
    let t: Vec<u16> = tip_text(st).encode_utf16().collect();
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(st.back, &t, &mut sz);
    let cx = (m.left + m.right) / 2;
    let bar_h = sz.cy + 14;
    let top_at = m.top + 24;
    let near_top = curw.y < top_at + bar_h + 48 && curw.x > cx - sz.cx / 2 - 64 && curw.x < cx + sz.cx / 2 + 64;
    let y = if near_top { m.bottom - 24 - bar_h } else { top_at };
    (RECT { left: cx - sz.cx / 2 - 16, top: y, right: cx + sz.cx / 2 + 16, bottom: y + bar_h }, m, scale)
}

/// 這個狀態下畫在變暗畫面上的東西各在哪裡（滑鼠移動前後各算一次，只重畫這些地方）。
/// 還不知道放大鏡多大（還沒畫過）時回傳 None：整個重畫
unsafe fn frame_rects(st: &State) -> Option<Vec<RECT>> {
    let grow = |r: RECT, d: i32| RECT { left: r.left - d, top: r.top - d, right: r.right + d, bottom: r.bottom + d };
    let mut out = vec![];
    if let Some((r, text)) = sel_label(st) {
        let scale = monitor_at(st, POINT { x: r.left, y: r.top }).1;
        // 外框、八個把手
        out.push(grow(r, (6.0 * scale).ceil() as i32 + 3));
        SelectObject(st.back, st.font.into());
        let t: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(st.back, &t, &mut sz);
        let (x, y) = label_pos(r);
        out.push(RECT { left: x, top: y, right: x + sz.cx + 17, bottom: y + sz.cy + 9 });
    }
    if let Some(r) = st.sel.filter(|_| st.adjust && st.drag.is_none() && st.grab.is_none()) {
        out.push(grow(toolbar_rect(st, r), 2));
    }
    let (tip, m, scale) = tip_rect(st, st.cursor.unwrap_or_else(|| cursor_now(st)));
    out.push(grow(tip, 1));
    if let Some(c) = st.cursor.filter(|_| st.record.loupe() && (st.sel.is_none() || st.grab.is_some())) {
        let (lw, lh) = st.loupe_size?;
        let (x, y) = loupe_pos((c.x, c.y), (lw, lh), (m.left, m.top, m.right, m.bottom), (24.0 * scale) as i32);
        out.push(RECT { left: x - 1, top: y - 1, right: x + lw + 1, bottom: y + lh + 1 });
    }
    Some(out)
}

/// 框好範圍下方的工具列：整個螢幕｜編輯、複製、釘在桌面｜取消、完成（只選範圍時只有整個螢幕、取消、確定）。
/// 記下每個按鈕的位置（點的時候用）
/// 工具列的按鈕：哪一個、文字（UTF-16）、寬度
type BarItem = (Tool, Vec<u16>, i32);

/// 工具列各按鈕的文字與寬度、整條的大小（用 hdc 目前的字型量）
unsafe fn toolbar_layout(hdc: HDC, record: bool, scale: f32) -> (Vec<BarItem>, i32, i32) {
    let px = |v: f32| (v * scale).round() as i32;
    let (pad, h, gap, sep, icon) = (px(12.0), px(34.0), px(2.0), px(10.0), px(14.0));
    let list: Vec<BarItem> = tools(record)
        .iter()
        .map(|t| {
            let text: Vec<u16> = t.label(record).encode_utf16().collect();
            let mut sz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &text, &mut sz);
            let with_icon = matches!(t, Tool::Done | Tool::Cancel);
            (*t, text, sz.cx + pad * 2 + if with_icon { icon + px(6.0) } else { 0 })
        })
        .collect();
    let group_end = |t: Tool| matches!(t, Tool::Full | Tool::Pin);
    let inner: i32 = list.iter().map(|(t, _, w)| w + if group_end(*t) { sep } else { gap }).sum::<i32>() - gap;
    (list, inner + px(8.0), h + px(8.0))
}

fn toolbar_font(scale: f32) -> HFONT {
    unsafe {
        CreateFontW(-((14.0 * scale).round() as i32), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Microsoft JhengHei UI"))
    }
}

/// 工具列在哪裡（算要重畫的地方用）
unsafe fn toolbar_rect(st: &State, sel: RECT) -> RECT {
    let scale = monitor_at(st, POINT { x: sel.left, y: sel.top }).1;
    let font = toolbar_font(scale);
    let old = SelectObject(st.back, font.into());
    let (_, bw, bh) = toolbar_layout(st.back, st.record == Mode::Record, scale);
    SelectObject(st.back, old);
    let _ = DeleteObject(font.into());
    let (mon, _) = monitor_at(st, POINT { x: (sel.left + sel.right) / 2, y: (sel.top + sel.bottom) / 2 });
    let (x, y) = toolbar_pos(tuple(sel), (bw, bh), tuple(mon), (8.0 * scale).round() as i32);
    RECT { left: x, top: y, right: x + bw, bottom: y + bh }
}

unsafe fn draw_toolbar(hdc: HDC, st: &mut State, sel: RECT, scale: f32) {
    let record = st.record == Mode::Record;
    let px = |v: f32| (v * scale).round() as i32;
    let font = toolbar_font(scale);
    let old = SelectObject(hdc, font.into());
    let (pad, h, gap, sep, icon) = (px(12.0), px(34.0), px(2.0), px(10.0), px(14.0));
    // 每個按鈕的寬度（取消、完成前面有圖示）；分組之間多留一點空：整個螢幕｜編輯、複製、釘在桌面｜取消、完成
    let (list, bw, bh) = toolbar_layout(hdc, record, scale);
    let group_end = |t: Tool| matches!(t, Tool::Full | Tool::Pin);
    let (mon, _) = monitor_at(st, POINT { x: (sel.left + sel.right) / 2, y: (sel.top + sel.bottom) / 2 });
    let (x, y) = toolbar_pos(tuple(sel), (bw, bh), tuple(mon), px(8.0));
    // 底：深色圓角
    let round = |c: COLORREF, edge: COLORREF, r: RECT, radius: i32| {
        let br = CreateSolidBrush(c);
        let pen = CreatePen(PS_SOLID, 1, edge);
        let (ob, op) = (SelectObject(hdc, br.into()), SelectObject(hdc, pen.into()));
        let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(br.into());
        let _ = DeleteObject(pen.into());
    };
    round(BAR_BG, COLORREF(0x0060_5C58), RECT { left: x, top: y, right: x + bw, bottom: y + bh }, px(10.0));
    let mut bx = x + px(4.0);
    let by = y + px(4.0);
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    for (t, mut text, w) in list {
        let r = RECT { left: bx, top: by, right: bx + w, bottom: by + h };
        let hot = st.hot == Some(t) || st.pressed == Some(t);
        match (t, hot) {
            (Tool::Done, true) => round(ACCENT_HOT, ACCENT_HOT, r, px(8.0)),
            (Tool::Done, false) => round(ACCENT, ACCENT, r, px(8.0)),
            (_, true) => round(BAR_HOT, BAR_HOT, r, px(8.0)),
            _ => {}
        }
        // ✓ / ✕ 用線畫（字型不一定有這兩個符號）
        let mut text_rect = r;
        if matches!(t, Tool::Done | Tool::Cancel) {
            let (ix, iy) = (r.left + pad, (r.top + r.bottom) / 2);
            let pen = CreatePen(PS_SOLID, px(2.0).max(2), COLORREF(0x00FF_FFFF));
            let op = SelectObject(hdc, pen.into());
            if t == Tool::Done {
                let _ = MoveToEx(hdc, ix, iy, None);
                let _ = LineTo(hdc, ix + icon * 2 / 5, iy + icon * 2 / 5);
                let _ = LineTo(hdc, ix + icon, iy - icon / 2);
            } else {
                let (c, d) = (ix + icon / 2, icon * 2 / 5);
                let _ = MoveToEx(hdc, c - d, iy - d, None);
                let _ = LineTo(hdc, c + d, iy + d);
                let _ = MoveToEx(hdc, c + d, iy - d, None);
                let _ = LineTo(hdc, c - d, iy + d);
            }
            SelectObject(hdc, op);
            let _ = DeleteObject(pen.into());
            text_rect.left += icon + px(6.0);
        }
        DrawTextW(hdc, &mut text, &mut text_rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        st.bar.push((t, r));
        bx += w + if group_end(t) { sep } else { gap };
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(font.into());
}

/// 放大鏡：畫好（tiny-skia）後貼到 hdc 上游標的旁邊
unsafe fn draw_loupe(hdc: HDC, st: &State, c: POINT, area: (i32, i32, i32, i32), scale: f32, hint: &str) -> Option<(i32, i32)> {
    let pm = render_loupe(|x, y| pixel(st, x, y), c.x, c.y, scale, hint)?;
    let (lw, lh) = (pm.width() as i32, pm.height() as i32);
    let (x, y) = loupe_pos((c.x, c.y), (lw, lh), area, (24.0 * scale) as i32);
    let Some((dc, bmp, _)) = dib(hdc, lw, lh, |dst| {
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], 255];
        }
    }) else {
        return Some((lw, lh));
    };
    let _ = BitBlt(hdc, x, y, lw, lh, Some(dc), 0, 0, SRCCOPY);
    let _ = DeleteDC(dc);
    let _ = DeleteObject(bmp.into());
    Some((lw, lh))
}

/// 藍底白字的小標籤
unsafe fn label(hdc: HDC, text: &str, x: i32, y: i32, brush: HBRUSH) {
    let mut t: Vec<u16> = text.encode_utf16().collect();
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &t, &mut sz);
    let mut r = RECT { left: x, top: y, right: x + sz.cx + 16, bottom: y + sz.cy + 8 };
    FillRect(hdc, &r, brush);
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    DrawTextW(hdc, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}
