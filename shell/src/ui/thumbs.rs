//! 錄影縮圖：由後端產生 PNG（快取在資料夾），這裡解碼成材質；以「路徑 + 修改時間」為鍵，檔案改了就重做。

use super::UiApp;
use eframe::egui::{ColorImage, TextureHandle, TextureOptions};
use screenrecorder_core::actions;
use std::collections::HashMap;

pub enum Thumb {
    Loading,
    Ready(TextureHandle),
    Failed,
}

#[derive(Default)]
pub struct Thumbs {
    map: HashMap<(String, u64), Thumb>,
}

fn decode(path: &std::path::Path) -> Option<ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    let pm = tiny_skia::Pixmap::decode_png(&bytes).ok()?;
    Some(ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
}

impl UiApp {
    /// 縮圖材質（還沒產生時開始產生，回傳 None）
    pub fn thumb(&mut self, path: &str, mtime: f64) -> Option<TextureHandle> {
        let key = (path.to_string(), mtime as u64);
        match self.thumbs.map.get(&key) {
            Some(Thumb::Ready(t)) => return Some(t.clone()),
            Some(_) => return None,
            None => {}
        }
        if self.thumbs.map.len() > 400 {
            self.thumbs.map.clear();
        }
        self.thumbs.map.insert(key.clone(), Thumb::Loading);
        let core = self.core.clone();
        let p = path.to_string();
        self.spawn(
            async move {
                let file = actions::thumb(&core, &p).await?;
                tokio::task::spawn_blocking(move || decode(&file)).await.ok().flatten()
            },
            move |app, img| {
                let t = match img {
                    Some(img) => Thumb::Ready(app.ctx.load_texture(format!("thumb:{}", key.0), img, TextureOptions::LINEAR)),
                    None => Thumb::Failed,
                };
                app.thumbs.map.insert(key, t);
            },
        );
        None
    }
}
