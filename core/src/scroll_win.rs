//! 長截圖用的 Windows 功能：直接從螢幕截取一塊（GDI，比 FFmpeg 快很多）、等畫面停下來、
//! 把範圍裡的視窗切到前面、在範圍中間送出滾輪、看 Esc 有沒有按下。

use crate::types::Rect;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetWindowThreadProcessId, SetCursorPos, SetForegroundWindow, WindowFromPoint, GA_ROOT};

/// 截取螢幕上的一塊（RGBA，不透明，由上而下）；不含游標
pub fn grab(r: Rect) -> Option<Vec<u8>> {
    let (w, h) = (r.width, r.height);
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), r.x, r.y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        SelectObject(mem, old);
        let lines = if ok { GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS) } else { 0 };
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if lines != h {
            return None;
        }
        // BGRA → RGBA（不透明）
        for p in buf.as_chunks_mut::<4>().0 {
            *p = [p[2], p[1], p[0], 255];
        }
        Some(buf)
    }
}

/// 等畫面停下來再截（平滑捲動的動畫、框選畫面關掉時的淡出）：連續兩次截到一樣才算；最多等 max，
/// 一直在動（影片、動畫）時回傳最後一張
pub fn grab_settled(r: Rect, max: Duration) -> Option<Vec<u8>> {
    let start = Instant::now();
    let mut prev = grab(r)?;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let next = grab(r)?;
        if next == prev || start.elapsed() >= max {
            return Some(next);
        }
        prev = next;
    }
}

/// 把 (x, y) 那裡的視窗切到前面：關閉「捲動非使用中的視窗」時，滾輪才會送到它（自己的視窗不動）
pub fn focus_at(x: i32, y: i32) {
    unsafe {
        let h = WindowFromPoint(POINT { x, y });
        if h.is_invalid() {
            return;
        }
        let root = GetAncestor(h, GA_ROOT);
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, Some(&mut pid));
        if pid != 0 && pid != GetCurrentProcessId() {
            let _ = SetForegroundWindow(root);
        }
    }
}

/// 把游標移到範圍中間並往下捲 notches 格（一格 = 滾輪轉一下）
pub fn scroll_down(r: Rect, notches: i32) {
    unsafe {
        let _ = SetCursorPos(r.x + r.width / 2, r.y + r.height / 2);
        let input = INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: MOUSEEVENTF_WHEEL, mouseData: (-120 * notches) as u32, ..Default::default() } } };
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Esc 有沒有按著（或從上次問到現在按過）
pub fn esc_pressed() -> bool {
    unsafe { GetAsyncKeyState(0x1B) as u16 & 0x8001 != 0 }
}
