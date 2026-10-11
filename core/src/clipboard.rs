//! 剪貼簿：圖片（截圖後可直接貼到 LINE、Word、信件）與檔案（影片、圖檔直接貼到 LINE、Teams、檔案總管）。

use std::path::Path;

/// 讀取 PNG 並複製到剪貼簿；回傳 (寬, 高, 是否成功)
pub fn copy_png(path: &Path) -> (u32, u32, bool) {
    let Ok(data) = std::fs::read(path) else { return (0, 0, false) };
    copy_png_bytes(&data)
}

/// 把 PNG 的內容複製到剪貼簿；回傳 (寬, 高, 是否成功)。
/// Windows：剪貼簿的 PNG 格式直接用這些位元組（不重新壓縮），另外解碼成點陣圖（CF_DIBV5）給只認點陣圖的程式；
/// 解碼直接寫進交給剪貼簿的記憶體，很長的長截圖也不會多佔一份
pub fn copy_png_bytes(png: &[u8]) -> (u32, u32, bool) {
    #[cfg(windows)]
    if let Some(r) = win::copy_png_bytes(png) {
        return r;
    }
    let Ok(pm) = tiny_skia::Pixmap::decode_png(png) else { return (0, 0, false) };
    let ok = copy_pixmap(&pm);
    (pm.width(), pm.height(), ok)
}

/// 解碼出來的 RGBA（ch = 4）或 RGB（ch = 3，放在前 w×h×3 個位元組）由上而下的像素，
/// 就地改成剪貼簿點陣圖要的 BGRA、由下而上（p 的長度要有 w×h×4）
#[cfg(any(windows, test))]
fn to_bgra_bottom_up(p: &mut [u8], w: usize, h: usize, ch: usize) {
    let n = w * h;
    if ch == 3 {
        // 從後面往前展開成 4 個位元組，不會蓋到還沒處理的像素
        for i in (0..n).rev() {
            let (r, g, b) = (p[i * 3], p[i * 3 + 1], p[i * 3 + 2]);
            p[i * 4..i * 4 + 4].copy_from_slice(&[b, g, r, 255]);
        }
    } else {
        for px in p[..n * 4].as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    let row = w * 4;
    for y in 0..h / 2 {
        let (top, bottom) = p.split_at_mut((h - 1 - y) * row);
        top[y * row..(y + 1) * row].swap_with_slice(&mut bottom[..row]);
    }
}

#[cfg(windows)]
mod win {
    use image::codecs::png::PngDecoder;
    use image::{ColorType, ImageDecoder};
    use windows::core::w;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::Graphics::Gdi::{BITMAPV5HEADER, BI_BITFIELDS};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    const CF_DIBV5: u32 = 17;

    /// 要交給剪貼簿的一塊記憶體；沒交出去時自動釋放
    struct Global(HGLOBAL, usize);

    impl Global {
        fn new(size: usize) -> Option<Global> {
            unsafe { GlobalAlloc(GMEM_MOVEABLE, size).ok().map(|h| Global(h, size)) }
        }

        /// 鎖住記憶體交給 f 填資料
        fn fill<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
            unsafe {
                let p = GlobalLock(self.0) as *mut u8;
                if p.is_null() {
                    return None;
                }
                let r = f(std::slice::from_raw_parts_mut(p, self.1));
                let _ = GlobalUnlock(self.0);
                Some(r)
            }
        }

        /// 交給剪貼簿；成功後記憶體歸剪貼簿管
        fn give(self, format: u32) -> bool {
            let ok = unsafe { SetClipboardData(format, Some(HANDLE(self.0 .0))).is_ok() };
            if ok {
                std::mem::forget(self);
            }
            ok
        }
    }

    impl Drop for Global {
        fn drop(&mut self) {
            unsafe {
                let _ = GlobalFree(Some(self.0));
            }
        }
    }

    /// 只處理 8 位元的 RGB / RGBA（本程式存的截圖都是）；其他格式回傳 None，交給一般的做法
    pub fn copy_png_bytes(png: &[u8]) -> Option<(u32, u32, bool)> {
        let dec = PngDecoder::new(std::io::Cursor::new(png)).ok()?;
        let (w, h) = dec.dimensions();
        let ch = match dec.color_type() {
            ColorType::Rgba8 => 4,
            ColorType::Rgb8 => 3,
            _ => return None,
        };
        let (wu, hu) = (w as usize, h as usize);
        let pixels = wu.checked_mul(hu)?.checked_mul(4)?;
        let head = std::mem::size_of::<BITMAPV5HEADER>();
        if pixels > u32::MAX as usize {
            return None;
        }
        let dib = Global::new(head + pixels)?;
        let decoded = dib.fill(|mem| {
            let (hdr, px) = mem.split_at_mut(head);
            let header = BITMAPV5HEADER {
                bV5Size: head as u32,
                bV5Width: w as i32,
                // 正的高度 = 由下而上（Word、小畫家只接受這種）
                bV5Height: h as i32,
                bV5Planes: 1,
                bV5BitCount: 32,
                bV5Compression: BI_BITFIELDS,
                bV5SizeImage: pixels as u32,
                bV5RedMask: 0x00ff_0000,
                bV5GreenMask: 0x0000_ff00,
                bV5BlueMask: 0x0000_00ff,
                bV5AlphaMask: 0xff00_0000,
                bV5CSType: 0x7352_4742, // LCS_sRGB
                bV5Intent: 4,           // LCS_GM_IMAGES
                ..Default::default()
            };
            unsafe { std::ptr::write_unaligned(hdr.as_mut_ptr() as *mut BITMAPV5HEADER, header) };
            if dec.read_image(&mut px[..wu * hu * ch]).is_err() {
                return false;
            }
            super::to_bgra_bottom_up(px, wu, hu, ch);
            true
        })?;
        if !decoded {
            return Some((0, 0, false));
        }
        let png_mem = Global::new(png.len())?;
        png_mem.fill(|m| m.copy_from_slice(png))?;
        // 剪貼簿可能正被其他程式使用：稍等再試
        let mut opened = false;
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            crate::info!("[截圖] 無法複製到剪貼簿：剪貼簿被其他程式占用");
            return Some((w, h, false));
        }
        let ok = unsafe {
            let _ = EmptyClipboard();
            // 先放 PNG：有些程式拿第一個看得懂的格式，PNG 的相容性較好（與 arboard 的順序相同）
            let fmt = RegisterClipboardFormatW(w!("PNG"));
            let a = fmt != 0 && png_mem.give(fmt);
            let b = dib.give(CF_DIBV5);
            let _ = CloseClipboard();
            a || b
        };
        Some((w, h, ok))
    }
}

/// 複製文字
pub fn copy_text(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).is_ok()
}

/// 把畫面（預乘 alpha）複製到剪貼簿；編輯後加了陰影的圖四周是透明的
pub fn copy_pixmap(pm: &tiny_skia::Pixmap) -> bool {
    let bytes = crate::shot_edit::straight_rgba(pm);
    let img = arboard::ImageData { width: pm.width() as usize, height: pm.height() as usize, bytes: std::borrow::Cow::Owned(bytes) };
    match arboard::Clipboard::new().and_then(|mut c| c.set_image(img)) {
        Ok(()) => true,
        Err(e) => {
            crate::info!("[截圖] 無法複製到剪貼簿：{e}");
            false
        }
    }
}

/// 把檔案放進剪貼簿（和在檔案總管按「複製」一樣）：可以直接貼到 LINE、Teams、信件或資料夾
#[cfg(windows)]
pub fn copy_files(paths: &[String]) -> Result<(), String> {
    use windows::core::w;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, POINT};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::UI::Shell::DROPFILES;
    if paths.is_empty() {
        return Err(crate::tr!("沒有要複製的檔案", "No files to copy").into());
    }
    // DROPFILES 後面接著以 \0 分隔、\0\0 結尾的 UTF-16 路徑
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let head = std::mem::size_of::<DROPFILES>();
    let fail = |what: &str| crate::trf!("無法複製到剪貼簿（{what}）", "Couldn't copy to the clipboard ({what})");
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, head + list.len() * 2).map_err(|_| fail(crate::tr!("記憶體", "out of memory")))?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            return Err(fail(crate::tr!("記憶體", "out of memory")));
        }
        let df = DROPFILES { pFiles: head as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
        std::ptr::write_unaligned(ptr as *mut DROPFILES, df);
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, ptr.add(head), list.len() * 2);
        let _ = GlobalUnlock(mem);
        // 「複製」而不是「剪下」：貼到資料夾時不會搬走原檔
        let effect = GlobalAlloc(GMEM_MOVEABLE, 4).map_err(|_| fail(crate::tr!("記憶體", "out of memory")))?;
        let ep = GlobalLock(effect) as *mut u32;
        if !ep.is_null() {
            *ep = 1; // DROPEFFECT_COPY
            let _ = GlobalUnlock(effect);
        }
        // 交給剪貼簿成功後記憶體歸剪貼簿管；沒交出去的要自己釋放
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(mem));
            let _ = GlobalFree(Some(effect));
            return Err(fail(crate::tr!("剪貼簿被其他程式占用", "the clipboard is in use by another app")));
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(15 /* CF_HDROP */, Some(HANDLE(mem.0))).is_ok();
        if !ok {
            let _ = GlobalFree(Some(mem));
        }
        let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
        if fmt == 0 || SetClipboardData(fmt, Some(HANDLE(effect.0))).is_err() {
            let _ = GlobalFree(Some(effect));
        }
        let _ = CloseClipboard();
        if ok {
            Ok(())
        } else {
            Err(fail(crate::tr!("寫入失敗", "write failed")))
        }
    }
}

#[cfg(not(windows))]
pub fn copy_files(_paths: &[String]) -> Result<(), String> {
    Err(crate::tr!("複製檔案只支援 Windows", "Copying files is only available on Windows").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_bottom_up_from_rgba_and_rgb() {
        // 2×2：第一列 紅、綠，第二列 藍、白
        let mut p = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 128, 255, 255, 255, 255];
        to_bgra_bottom_up(&mut p, 2, 2, 4);
        // 由下而上：先藍、白，再紅、綠；每個像素 BGRA（alpha 保留）
        assert_eq!(p, vec![255, 0, 0, 128, 255, 255, 255, 255, 0, 0, 255, 255, 0, 255, 0, 255]);
        // RGB 放在前 3/4
        let mut q = vec![0u8; 3 * 2 * 4];
        q[..9].copy_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90]);
        to_bgra_bottom_up(&mut q, 1, 3, 3);
        assert_eq!(q[..12], [90, 80, 70, 255, 60, 50, 40, 255, 30, 20, 10, 255]);
    }
}
