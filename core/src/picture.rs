//! 圖片標註（Logo、浮水印、貼上的圖）：讀取圖片檔並快取解碼結果。
//!
//! 預覽與匯出時同一張圖會畫很多次（拖曳調整大小時每一格都重畫），所以解碼一次後保留
//! 一組逐次縮小一半的圖，畫的時候挑最接近目標大小的那張，縮小時也不會鋸齒。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::{ColorU8, Pixmap};

/// 可以加進來的圖片格式
pub const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "webp"];

/// 太大的圖先縮小（標註用不到這麼大，也避免吃光記憶體）
const MAX_SIDE: u32 = 4096;

/// 解碼後的圖：levels[0] 是原尺寸，之後每張縮小一半
pub struct Picture {
    pub levels: Vec<Pixmap>,
}

impl Picture {
    pub fn width(&self) -> u32 {
        self.levels[0].width()
    }

    pub fn height(&self) -> u32 {
        self.levels[0].height()
    }

    /// 畫成 w 寬時用哪一張：不小於目標的最小一張
    pub fn level_for(&self, w: f32) -> &Pixmap {
        self.levels.iter().rev().find(|p| p.width() as f32 >= w).unwrap_or(&self.levels[0])
    }
}

pub fn is_picture(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

type Cache = Mutex<HashMap<String, (f64, Option<Arc<Picture>>)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// 讀取圖片（檔案改過就重新讀取）；讀不到時回傳 None
pub fn load(path: &str) -> Option<Arc<Picture>> {
    let mtime = std::fs::metadata(path).map(|m| crate::paths::mtime_ms(&m)).unwrap_or(-1.0);
    if let Some((t, p)) = cache().lock().unwrap().get(path) {
        if *t == mtime {
            return p.clone();
        }
    }
    let pic = std::fs::read(path).ok().and_then(|b| decode(&b)).map(Arc::new);
    cache().lock().unwrap().insert(path.to_string(), (mtime, pic.clone()));
    pic
}

/// 解碼圖片並轉成預乘 alpha 的畫布
pub fn decode(bytes: &[u8]) -> Option<Picture> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > MAX_SIDE || img.height() > MAX_SIDE { img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut pm = Pixmap::new(w, h)?;
    for (d, s) in pm.pixels_mut().iter_mut().zip(rgba.pixels()) {
        *d = ColorU8::from_rgba(s[0], s[1], s[2], s[3]).premultiply();
    }
    Some(Picture { levels: mipmaps(pm) })
}

/// 逐次縮小一半（2×2 平均），縮到 16 像素為止
fn mipmaps(first: Pixmap) -> Vec<Pixmap> {
    let mut out = vec![first];
    loop {
        let src = out.last().unwrap();
        let (w, h) = (src.width() / 2, src.height() / 2);
        if w < 16 || h < 16 {
            return out;
        }
        let Some(mut dst) = Pixmap::new(w, h) else { return out };
        let (sw, data) = (src.width() as usize, src.data());
        let px = dst.data_mut();
        for y in 0..h as usize {
            for x in 0..w as usize {
                for c in 0..4 {
                    let at = |xx: usize, yy: usize| data[(yy * sw + xx) * 4 + c] as u32;
                    let sum = at(x * 2, y * 2) + at(x * 2 + 1, y * 2) + at(x * 2, y * 2 + 1) + at(x * 2 + 1, y * 2 + 1);
                    px[(y * w as usize + x) * 4 + c] = ((sum + 2) / 4) as u8;
                }
            }
        }
        out.push(dst);
    }
}

/// 加進來的圖預設的大小（影片像素）：寬為畫面的 frac，高不超過畫面的 40%，保持比例
pub fn fit_size(pw: u32, ph: u32, vw: f64, vh: f64, frac: f64) -> (f64, f64) {
    let (pw, ph) = (pw.max(1) as f64, ph.max(1) as f64);
    let mut w = (vw * frac).min(pw.max(32.0));
    let mut h = w * ph / pw;
    if h > vh * 0.4 {
        h = vh * 0.4;
        w = h * pw / ph;
    }
    (w.round().max(8.0), h.round().max(8.0))
}

/// 存放貼上的圖片：資料夾\pictures
pub fn pictures_dir() -> PathBuf {
    crate::paths::data_dir().join("pictures")
}

/// 把剪貼簿裡的圖存成 PNG（貼上的圖沒有檔案，存下來標註才能記住它）
pub fn save_clipboard_image() -> Result<PathBuf, String> {
    let img = arboard::Clipboard::new().and_then(|mut c| c.get_image()).map_err(|_| "剪貼簿裡沒有圖片".to_string())?;
    let (w, h) = (img.width as u32, img.height as u32);
    let mut pm = Pixmap::new(w, h).ok_or("剪貼簿裡的圖片是空的")?;
    for (d, s) in pm.pixels_mut().iter_mut().zip(img.bytes.as_chunks::<4>().0) {
        *d = ColorU8::from_rgba(s[0], s[1], s[2], s[3]).premultiply();
    }
    let dir = pictures_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("無法建立資料夾：{e}"))?;
    let path = crate::paths::unique_path(&dir, &format!("貼上_{}", crate::paths::timestamp()), ".png");
    pm.save_png(&path).map_err(|e| format!("無法儲存圖片：{e}"))?;
    Ok(path)
}

/// 用 Windows 的開啟檔案視窗選一張圖片（會等到使用者選好或取消）
pub fn pick_file() -> Result<Option<PathBuf>, String> {
    crate::filepick::pick("選擇要加上的圖片或 Logo", &[("圖片（PNG、JPG、BMP、WebP）", "*.png;*.jpg;*.jpeg;*.bmp;*.webp"), ("所有檔案", "*.*")]).map_err(|e| {
        if cfg!(windows) {
            e
        } else {
            "這個系統不支援選擇檔案的視窗，請把圖片檔拖曳到編輯視窗".into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut pm = Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
        pm.encode_png().unwrap()
    }

    #[test]
    fn decodes_png_with_mipmaps() {
        let p = decode(&png(100, 40, [255, 0, 0, 255])).unwrap();
        assert_eq!((p.width(), p.height()), (100, 40));
        // 100×40 → 50×20 → 停（25×10 太小）
        assert_eq!(p.levels.len(), 2);
        assert_eq!((p.levels[1].width(), p.levels[1].height()), (50, 20));
        assert_eq!(p.levels[1].pixel(3, 3).unwrap().red(), 255);
        assert_eq!(p.level_for(60.0).width(), 100);
        assert_eq!(p.level_for(30.0).width(), 50);
        assert_eq!(p.level_for(10.0).width(), 50);
    }

    #[test]
    fn transparency_is_premultiplied() {
        let p = decode(&png(32, 32, [200, 100, 0, 128])).unwrap();
        let c = p.levels[0].pixel(5, 5).unwrap();
        assert_eq!(c.alpha(), 128);
        assert!(c.red() <= 101 && c.red() >= 99, "{}", c.red());
    }

    #[test]
    fn rejects_non_images() {
        assert!(decode(b"not an image").is_none());
        assert!(load("/nonexistent/logo.png").is_none());
    }

    #[test]
    fn caches_until_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logo.png");
        std::fs::write(&path, png(20, 20, [0, 0, 255, 255])).unwrap();
        let p = path.to_string_lossy().to_string();
        let a = load(&p).unwrap();
        let b = load(&p).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, png(30, 10, [0, 0, 255, 255])).unwrap();
        let c = load(&p).unwrap();
        assert_eq!(c.width(), 30);
    }

    #[test]
    fn fit_size_keeps_ratio() {
        assert_eq!(fit_size(400, 100, 1920.0, 1080.0, 0.2), (384.0, 96.0));
        // 很高的圖：高度限制在 40%
        assert_eq!(fit_size(100, 1000, 1920.0, 1080.0, 0.2), (43.0, 432.0));
        // 小圖不放大
        assert_eq!(fit_size(64, 64, 1920.0, 1080.0, 0.2), (64.0, 64.0));
        assert!(is_picture(Path::new("a/Logo.PNG")));
        assert!(!is_picture(Path::new("a/clip.mp4")));
    }
}
