//! 剪貼簿：圖片（截圖後可直接貼到 LINE、Word、信件）與檔案（影片、圖檔直接貼到 LINE、Teams、檔案總管）。

use std::path::Path;

/// 讀取 PNG 並複製到剪貼簿；回傳 (寬, 高, 是否成功)
pub fn copy_png(path: &Path) -> (u32, u32, bool) {
    let Ok(data) = std::fs::read(path) else { return (0, 0, false) };
    let Ok(pm) = tiny_skia::Pixmap::decode_png(&data) else { return (0, 0, false) };
    let ok = copy_pixmap(&pm);
    (pm.width(), pm.height(), ok)
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
        return Err("沒有要複製的檔案".into());
    }
    // DROPFILES 後面接著以 \0 分隔、\0\0 結尾的 UTF-16 路徑
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let head = std::mem::size_of::<DROPFILES>();
    let fail = |what: &str| format!("無法複製到剪貼簿（{what}）");
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, head + list.len() * 2).map_err(|_| fail("記憶體"))?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            return Err(fail("記憶體"));
        }
        let df = DROPFILES { pFiles: head as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
        std::ptr::write_unaligned(ptr as *mut DROPFILES, df);
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, ptr.add(head), list.len() * 2);
        let _ = GlobalUnlock(mem);
        // 「複製」而不是「剪下」：貼到資料夾時不會搬走原檔
        let effect = GlobalAlloc(GMEM_MOVEABLE, 4).map_err(|_| fail("記憶體"))?;
        let ep = GlobalLock(effect) as *mut u32;
        if !ep.is_null() {
            *ep = 1; // DROPEFFECT_COPY
            let _ = GlobalUnlock(effect);
        }
        // 交給剪貼簿成功後記憶體歸剪貼簿管；沒交出去的要自己釋放
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(mem));
            let _ = GlobalFree(Some(effect));
            return Err(fail("剪貼簿被其他程式占用"));
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
            Err(fail("寫入失敗"))
        }
    }
}

#[cfg(not(windows))]
pub fn copy_files(_paths: &[String]) -> Result<(), String> {
    Err("複製檔案只支援 Windows".into())
}
