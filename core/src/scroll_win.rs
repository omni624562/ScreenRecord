//! 長截圖用的 Windows 功能：直接從螢幕截取一塊（GDI，比 FFmpeg 快很多）、在範圍中間送出滾輪、看 Esc 有沒有按下。

use crate::types::Rect;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT};
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

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
