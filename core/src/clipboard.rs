//! 把圖片放進剪貼簿（截圖後可直接貼到 LINE、Word、信件）。

use std::path::Path;

/// 讀取 PNG 並複製到剪貼簿；回傳 (寬, 高, 是否成功)
pub fn copy_png(path: &Path) -> (u32, u32, bool) {
    let Ok(data) = std::fs::read(path) else { return (0, 0, false) };
    let Ok(pm) = tiny_skia::Pixmap::decode_png(&data) else { return (0, 0, false) };
    let ok = copy_pixmap(&pm);
    (pm.width(), pm.height(), ok)
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
