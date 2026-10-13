//! 框選截圖的畫面（Windows 原生視窗，不靠操作視窗）：一個蓋住整個桌面的最上層視窗，
//! 顯示凍結的畫面並變暗；拖曳框選範圍，或點一下選游標下的視窗；Esc 或右鍵取消。截圖與錄影共用。
//! 截圖、選錄影範圍時放開滑鼠後範圍留著：可以拖曳邊、角調整或整個移動，下方的工具列選要做什麼
//! （完成、編輯、複製、釘在桌面、取消；Enter 完成、在範圍裡點兩下也是完成）。設定「放開滑鼠就直接存檔」時放開就截。
//! 游標旁有放大鏡（看清楚像素、按 C 複製色碼、方向鍵微調位置）；也用來當取色器與尺規。
//! 座標都是實體像素（程式已宣告 Per-Monitor DPI aware）。

use crate::snip_tools::{drag_grab, grab_at, handles, hex, loupe_pos, render_loupe, ruler_text, ruler_ticks, snap, toolbar_pos, tools, Grab, SnipAction, Tool};
use crate::types::Rect;
use crate::{tr, trf};
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect,
    GetDC, GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD, HBITMAP, HBRUSH, HDC, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_SHIFT};
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
    /// 剛按 C 複製了色碼（放大鏡下方顯示「已複製」）
    copied: bool,
    /// 取色器選到的顏色
    color: Option<[u8; 3]>,
    /// 尺規：量的線（放開後留著，可以再量）
    ruler: Option<(POINT, POINT)>,
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
    /// 取色器：點一下複製色碼
    Color,
    /// 尺規：拖曳量距離
    Ruler,
}

impl Mode {
    /// 顯示放大鏡（可以按 C 複製色碼）
    fn loupe(self) -> bool {
        matches!(self, Mode::Shot | Mode::Record | Mode::Color | Mode::Ruler)
    }

    /// 不變暗、不選視窗（取色器、尺規）
    fn tool(self) -> bool {
        matches!(self, Mode::Color | Mode::Ruler)
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 顯示框選畫面直到使用者選好或取消（在呼叫的執行緒上執行訊息迴圈）。
/// rgba：整個桌面的畫面（由上而下、不透明）；windows：看得到的視窗（上層在前）；record：用途；
/// instant：截圖時放開滑鼠就直接存檔（不留下範圍調整）。回傳 (範圍, 工具列選的動作)
pub fn select(rgba: &[u8], desk: Rect, windows: Vec<Rect>, record: Mode, instant: bool) -> Option<(Rect, SnipAction)> {
    run(rgba, desk, windows, record, instant).and_then(|r| r.0.map(|rect| (rect, r.2)))
}

/// 取色器：在凍結的畫面上點一下（或按 C），回傳那個像素的顏色；取消時 None
pub fn pick_color(rgba: &[u8], desk: Rect) -> Option<[u8; 3]> {
    run(rgba, desk, vec![], Mode::Color, true).and_then(|r| r.1)
}

/// 尺規：在凍結的畫面上拖曳量距離，Esc 或右鍵結束
pub fn ruler(rgba: &[u8], desk: Rect) {
    run(rgba, desk, vec![], Mode::Ruler, true);
}

/// 顯示畫面直到結束：(框選的範圍, 取色器選的顏色, 工具列選的動作)
fn run(rgba: &[u8], desk: Rect, windows: Vec<Rect>, record: Mode, instant: bool) -> Option<(Option<Rect>, Option<[u8; 3]>, SnipAction)> {
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
            copied: false,
            color: None,
            ruler: None,
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));

        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("ScreenRecorderSnip");
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, style: CS_DBLCLKS, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassExW(&wc);
        let title: Vec<u16> = match record {
            Mode::Record => tr!("框選錄影範圍", "Select area to record"),
            Mode::Scroll => tr!("長截圖", "Scrolling screenshot"),
            Mode::Qr => tr!("讀取 QR 碼", "Read QR code"),
            Mode::Color => tr!("取色器", "Color picker"),
            Mode::Ruler => tr!("尺規", "Ruler"),
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
        Some((st.result, st.color, st.action))
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
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.cursor = Some(p);
                    st.copied = false;
                    if st.record == Mode::Ruler {
                        if let Some((a, _)) = st.drag {
                            let b = if key_down(VK_SHIFT.0) {
                                let (x, y) = snap((a.x, a.y), (p.x, p.y));
                                POINT { x, y }
                            } else {
                                p
                            };
                            st.drag = Some((a, b));
                            st.ruler = Some((a, b));
                        }
                    } else if let Some((g, p0, orig)) = st.grab {
                        let bounds = (0, 0, st.desk.width, st.desk.height);
                        st.sel = Some(rect_of(drag_grab(tuple(orig), g, (p.x - p0.x, p.y - p0.y), bounds)));
                    } else if let Some((a, _)) = st.drag {
                        st.drag = Some((a, p));
                    } else if st.sel.is_none() {
                        st.hover = window_at(st, p);
                    }
                    st.hot = bar_hit(st, p);
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
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
                if st.record == Mode::Color {
                    // 取色器：點一下選這個像素
                    st.drag = None;
                    st.color = pixel(st, p.x, p.y);
                    return st.color.map(|_| ());
                }
                if st.record == Mode::Ruler {
                    // 尺規：量好的線留著（點一下沒拖曳就清掉）
                    if let Some((a, b)) = st.drag.take() {
                        st.ruler = (a.x != b.x || a.y != b.y).then_some((a, b));
                    }
                    return None;
                }
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
        // Ctrl+C：框好範圍時只複製到剪貼簿；C：複製游標下的色碼（取色器直接結束）
        WM_KEYDOWN if wparam.0 == 0x43 => {
            let copy_area = key_down(VK_CONTROL.0)
                && STATE.with(|s| {
                    let mut b = s.borrow_mut();
                    let Some(st) = b.as_mut() else { return false };
                    st.record == Mode::Shot && st.sel.is_some_and(|sel| finish(st, sel, SnipAction::Copy))
                });
            if copy_area {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            // 剪貼簿在放開 STATE 之後才寫（寫的時候可能處理其他訊息，又進到這裡借用）
            let picked = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                if !st.record.loupe() {
                    return None;
                }
                let p = st.cursor?;
                let c = pixel(st, p.x, p.y)?;
                if st.record == Mode::Color {
                    st.color = Some(c);
                    return Some((true, c));
                }
                Some((false, c))
            });
            let done = match picked {
                Some((false, c)) => {
                    let ok = crate::clipboard::copy_text(&hex(c));
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.copied = ok;
                        }
                    });
                    Some(false)
                }
                other => other.map(|p| p.0),
            };
            if done == Some(true) {
                let _ = DestroyWindow(hwnd);
            } else {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        // 方向鍵：游標移動一個像素（取色、框選對齊用）
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
        Mode::Qr => tr!("框選 QR 碼（大一點沒關係），或點一下選視窗　　Esc 或右鍵取消", "Select the QR code (larger is fine), or click a window    Esc or right-click to cancel"),
        Mode::Color => tr!(
            "移到要取色的地方點一下（或按 C），色碼會複製到剪貼簿；方向鍵可以微調　　Esc 或右鍵取消",
            "Click a spot (or press C) to copy its color code; arrow keys fine-tune    Esc or right-click to cancel"
        ),
        Mode::Ruler => tr!(
            "拖曳量距離（按住 Shift 保持水平、垂直或 45°），可以重複量　　Esc 或右鍵結束",
            "Drag to measure (hold Shift for horizontal, vertical or 45°); measure as often as you like    Esc or right-click to finish"
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
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        let Some(st) = b.as_mut() else { return };
        let (w, h) = (st.desk.width, st.desk.height);
        let mem = st.back;
        // 取色器、尺規：不變暗
        let _ = BitBlt(mem, 0, 0, w, h, Some(if st.record.tool() { st.bright } else { st.dim }), 0, 0, SRCCOPY);
        // 選取範圍（或游標下的視窗）用原本的亮度，外框與尺寸
        let (sel, sized) = match (st.drag, st.sel) {
            _ if st.record.tool() => (None, false),
            (Some((a, b)), _) => (Some(drag_rect(a, b)), true),
            (None, Some(r)) => (Some(r), true),
            (None, None) => (st.hover.map(|r| win_rect(st, r)), false),
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
                        Mode::Qr => tr!("讀取 QR 碼", "read the QR code"),
                        Mode::Shot if st.adjust => tr!("選這個視窗", "select it"),
                        Mode::Shot | Mode::Color | Mode::Ruler => tr!("擷取", "capture"),
                    }
                )
            };
            let top = if r.top >= 34 { r.top - 34 } else { r.top + 6 };
            label(mem, &text, r.left.max(0), top, brush);
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
        if let Some((a, b)) = st.ruler {
            draw_ruler(mem, a, b, brush);
        }
        // 游標所在螢幕的上方：說明（游標移過去時改到下方，才不會擋住）
        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        let curw = POINT { x: cur.x - st.desk.x, y: cur.y - st.desk.y };
        let (m, scale) = monitor_at(st, curw);
        SelectObject(mem, st.font.into());
        let mut t: Vec<u16> = tip_text(st).encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(mem, &t, &mut sz);
        let cx = (m.left + m.right) / 2;
        let bar_h = sz.cy + 14;
        let top_at = m.top + 24;
        let near_top = curw.y < top_at + bar_h + 48 && curw.x > cx - sz.cx / 2 - 64 && curw.x < cx + sz.cx / 2 + 64;
        let y = if near_top { m.bottom - 24 - bar_h } else { top_at };
        let mut r = RECT { left: cx - sz.cx / 2 - 16, top: y, right: cx + sz.cx / 2 + 16, bottom: y + bar_h };
        let bg = CreateSolidBrush(COLORREF(0x0020_2020));
        FillRect(mem, &r, bg);
        let _ = DeleteObject(bg.into());
        SetTextColor(mem, COLORREF(0x00FF_FFFF));
        DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        // 放大鏡：在游標所在的螢幕裡（框好範圍後只在調整時顯示，不擋住工具列）
        let show_loupe = st.sel.is_none() || st.grab.is_some();
        if let Some(c) = st.cursor.filter(|_| st.record.loupe() && show_loupe) {
            let area = (m.left, m.top, m.right, m.bottom);
            let hint = if st.copied {
                tr!("已複製色碼", "Color code copied")
            } else if st.record == Mode::Color {
                tr!("點一下複製", "Click to copy")
            } else {
                tr!("按 C 複製色碼", "Press C to copy")
            };
            draw_loupe(mem, st, c, area, scale, hint);
        }
        let _ = DeleteObject(brush.into());
        let p = ps.rcPaint;
        let _ = BitBlt(hdc, p.left, p.top, p.right - p.left, p.bottom - p.top, Some(mem), p.left, p.top, SRCCOPY);
    });
    let _ = EndPaint(hwnd, &ps);
}

/// 框好範圍下方的工具列：整個螢幕｜編輯、複製、釘在桌面｜取消、完成（只選範圍時只有整個螢幕、取消、確定）。
/// 記下每個按鈕的位置（點的時候用）
unsafe fn draw_toolbar(hdc: HDC, st: &mut State, sel: RECT, scale: f32) {
    let record = st.record == Mode::Record;
    let px = |v: f32| (v * scale).round() as i32;
    let font = CreateFontW(-px(14.0), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Microsoft JhengHei UI"));
    let old = SelectObject(hdc, font.into());
    let (pad, h, gap, sep, icon) = (px(12.0), px(34.0), px(2.0), px(10.0), px(14.0));
    // 每個按鈕的寬度（取消、完成前面有圖示）
    let list: Vec<(Tool, Vec<u16>, i32)> = tools(record)
        .iter()
        .map(|t| {
            let text: Vec<u16> = t.label(record).encode_utf16().collect();
            let mut sz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &text, &mut sz);
            let with_icon = matches!(t, Tool::Done | Tool::Cancel);
            (*t, text, sz.cx + pad * 2 + if with_icon { icon + px(6.0) } else { 0 })
        })
        .collect();
    // 分組之間多留一點空：整個螢幕｜編輯、複製、釘在桌面｜取消、完成
    let group_end = |t: Tool| matches!(t, Tool::Full | Tool::Pin);
    let inner: i32 = list.iter().map(|(t, _, w)| w + if group_end(*t) { sep } else { gap }).sum::<i32>() - gap;
    let (bw, bh) = (inner + px(8.0), h + px(8.0));
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
unsafe fn draw_loupe(hdc: HDC, st: &State, c: POINT, area: (i32, i32, i32, i32), scale: f32, hint: &str) {
    let Some(pm) = render_loupe(|x, y| pixel(st, x, y), c.x, c.y, scale, hint) else { return };
    let (lw, lh) = (pm.width() as i32, pm.height() as i32);
    let (x, y) = loupe_pos((c.x, c.y), (lw, lh), area, (24.0 * scale) as i32);
    let Some((dc, bmp, _)) = dib(hdc, lw, lh, |dst| {
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], 255];
        }
    }) else {
        return;
    };
    let _ = BitBlt(hdc, x, y, lw, lh, Some(dc), 0, 0, SRCCOPY);
    let _ = DeleteDC(dc);
    let _ = DeleteObject(bmp.into());
}

/// 尺規：白底藍線、兩端的短槓，旁邊標上長度與角度
unsafe fn draw_ruler(hdc: HDC, a: POINT, b: POINT, brush: HBRUSH) {
    let ticks = ruler_ticks((a.x, a.y), (b.x, b.y), 7.0);
    for (width, color) in [(5, COLORREF(0x00FF_FFFF)), (2, ACCENT)] {
        let pen = CreatePen(PS_SOLID, width, color);
        let old = SelectObject(hdc, pen.into());
        let _ = MoveToEx(hdc, a.x, a.y, None);
        let _ = LineTo(hdc, b.x, b.y);
        for (p, q) in ticks {
            let _ = MoveToEx(hdc, p.0, p.1, None);
            let _ = LineTo(hdc, q.0, q.1);
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(pen.into());
    }
    let text = ruler_text((a.x, a.y), (b.x, b.y));
    let (mx, my) = ((a.x + b.x) / 2, (a.y + b.y) / 2);
    label(hdc, &text, mx + 12, my + 12, brush);
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
