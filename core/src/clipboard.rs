//! 把圖片放進剪貼簿（截圖後可直接貼到 LINE、Word、信件）。

use std::path::Path;

/// 讀取 PNG 並複製到剪貼簿；回傳 (寬, 高, 是否成功)
pub fn copy_png(path: &Path) -> (u32, u32, bool) {
    let Ok(data) = std::fs::read(path) else { return (0, 0, false) };
    let Ok(pm) = tiny_skia::Pixmap::decode_png(&data) else { return (0, 0, false) };
    let (w, h) = (pm.width(), pm.height());
    // 截圖是不透明的：預乘 alpha 與一般 RGBA 相同
    let img = arboard::ImageData { width: w as usize, height: h as usize, bytes: std::borrow::Cow::Borrowed(pm.data()) };
    let ok = match arboard::Clipboard::new().and_then(|mut c| c.set_image(img)) {
        Ok(()) => true,
        Err(e) => {
            crate::info!("[截圖] 無法複製到剪貼簿：{e}");
            false
        }
    };
    (w, h, ok)
}
