//! 錄影縮圖：由後端產生 PNG（快取在資料夾），這裡解碼成材質；以「路徑 + 修改時間」為鍵，檔案改了就重做。

use super::UiApp;
use eframe::egui::{ColorImage, TextureHandle, TextureOptions};
use screenrecorder_core::actions;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 產生失敗的縮圖隔多久再要一次（例如程式剛開、FFmpeg 還沒偵測完時要不到；之後就要得到）
const RETRY_FAILED: Duration = Duration::from_secs(20);

pub enum Thumb {
    Loading,
    Ready(TextureHandle),
    /// 失敗的時間：過了 RETRY_FAILED 再要一次，不會一直空白到重開程式
    Failed(Instant),
}

#[derive(Default)]
pub struct Thumbs {
    map: HashMap<(String, u64), Thumb>,
    /// 滑過預覽的長條圖（core::thumbs::strip）
    strips: HashMap<(String, u64), Thumb>,
}

fn decode(path: &std::path::Path) -> Option<ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    let pm = tiny_skia::Pixmap::decode_png(&bytes).ok()?;
    Some(ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
}

impl Thumbs {
    /// 這個檔案的縮圖重做（設了封面：修改時間保留，鍵不會變）
    pub fn forget(&mut self, path: &str) {
        self.map.retain(|k, _| k.0 != path);
    }
}

impl UiApp {
    /// 縮圖材質（還沒產生時開始產生，回傳 None）
    pub fn thumb(&mut self, path: &str, mtime: f64) -> Option<TextureHandle> {
        let key = (path.to_string(), mtime as u64);
        match self.thumbs.map.get(&key) {
            Some(Thumb::Ready(t)) => return Some(t.clone()),
            Some(Thumb::Failed(at)) if at.elapsed() >= RETRY_FAILED => {}
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
                    None => {
                        // 到時候再畫一次，才會重新要縮圖
                        app.ctx.request_repaint_after(RETRY_FAILED);
                        Thumb::Failed(Instant::now())
                    }
                };
                app.thumbs.map.insert(key, t);
            },
        );
        None
    }

    /// 滑過預覽的長條圖材質（STRIP_FRAMES 格左右接起來）；還沒產生時開始產生，回傳 None
    pub fn thumb_strip(&mut self, path: &str, mtime: f64, duration: f64) -> Option<TextureHandle> {
        let key = (path.to_string(), mtime as u64);
        match self.thumbs.strips.get(&key) {
            Some(Thumb::Ready(t)) => return Some(t.clone()),
            Some(Thumb::Failed(at)) if at.elapsed() >= RETRY_FAILED => {}
            Some(_) => return None,
            None => {}
        }
        if self.thumbs.strips.len() > 60 {
            self.thumbs.strips.clear();
        }
        self.thumbs.strips.insert(key.clone(), Thumb::Loading);
        let core = self.core.clone();
        let p = path.to_string();
        self.spawn(
            async move {
                let file = actions::thumb_strip(&core, &p, duration).await?;
                tokio::task::spawn_blocking(move || decode(&file)).await.ok().flatten()
            },
            move |app, img| {
                let t = match img {
                    Some(img) => Thumb::Ready(app.ctx.load_texture(format!("strip:{}", key.0), img, TextureOptions::LINEAR)),
                    None => Thumb::Failed(Instant::now()),
                };
                app.thumbs.strips.insert(key, t);
                app.ctx.request_repaint();
            },
        );
        None
    }
}

/// 滑過預覽：游標在 hover（縮圖）上左右移動時，在 near 旁邊浮出一張大一點的畫面、時間與進度；
/// 長條圖還沒好時顯示「正在準備預覽…」
pub fn scrub_preview(app: &mut UiApp, ui: &eframe::egui::Ui, hover: eframe::egui::Rect, near: eframe::egui::Rect, path: &str, mtime: f64, duration: f64) {
    use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, Id, Order, Rect, Stroke};
    use screenrecorder_core::thumbs::{strip_pick, STRIP_FRAMES};
    let Some(pos) = ui.ctx().pointer_hover_pos().filter(|p| hover.contains(*p)) else { return };
    // 只有 MP4 影片；產生失敗（損壞、還在寫入）的不顯示
    if !(duration.is_finite() && duration > 0.0) || !path.to_ascii_lowercase().ends_with(".mp4") {
        return;
    }
    let strip = app.thumb_strip(path, mtime, duration);
    if strip.is_none() && matches!(app.thumbs.strips.get(&(path.to_string(), mtime as u64)), Some(Thumb::Failed(_))) {
        return;
    }
    let frac = ((pos.x - hover.min.x) / hover.width().max(1.0)) as f64;
    let (i, t) = strip_pick(frac, duration);
    let p = super::theme::pal(ui);
    // 預覽寬 240（和長條圖一格一樣），依影片比例；放在縮圖右邊，放不下時放左邊
    let aspect = strip.as_ref().map(|s| s.size_vec2()).map(|s| s.y / (s.x / STRIP_FRAMES as f32)).filter(|a| a.is_finite() && *a > 0.1).unwrap_or(9.0 / 16.0);
    let size = vec2(240.0, (240.0 * aspect).clamp(60.0, 320.0));
    let screen = ui.ctx().content_rect();
    let mut at = pos2(near.max.x + 10.0, near.center().y - size.y / 2.0);
    if at.x + size.x > screen.max.x - 8.0 {
        at.x = near.min.x - 10.0 - size.x;
    }
    at.y = at.y.clamp(screen.min.y + 8.0, (screen.max.y - size.y - 30.0).max(screen.min.y + 8.0));
    egui::Area::new(Id::new("scrub-preview")).order(Order::Tooltip).fixed_pos(at).interactable(false).show(ui.ctx(), |ui| {
        let r = Rect::from_min_size(at, size);
        let painter = ui.painter();
        painter.rect_filled(r.expand(3.0), CornerRadius::same(8), p.surface);
        painter.rect_stroke(r.expand(3.0), CornerRadius::same(8), Stroke::new(1.0, p.border_strong), egui::StrokeKind::Outside);
        painter.rect_filled(r, CornerRadius::same(6), Color32::BLACK);
        match &strip {
            Some(tex) => {
                let n = STRIP_FRAMES as f32;
                let uv = Rect::from_min_max(pos2(i as f32 / n, 0.0), pos2((i + 1) as f32 / n, 1.0));
                painter.image(tex.id(), r, uv, Color32::WHITE);
            }
            None => {
                painter.text(r.center(), Align2::CENTER_CENTER, screenrecorder_core::tr!("正在準備預覽…", "Preparing preview…"), super::theme::font(12.5), Color32::from_gray(200));
            }
        }
        // 下緣的進度與時間
        let bar = Rect::from_min_max(pos2(r.min.x, r.max.y - 4.0), r.max);
        painter.rect_filled(bar, CornerRadius::ZERO, Color32::from_white_alpha(60));
        painter.rect_filled(Rect::from_min_max(bar.min, pos2(r.min.x + r.width() * (t / duration) as f32, bar.max.y)), CornerRadius::ZERO, p.accent);
        let label = screenrecorder_core::format::video_clock(t);
        let g = painter.layout_no_wrap(label, super::theme::mono(12.0), Color32::WHITE);
        let lr = Rect::from_min_size(pos2(r.max.x - g.size().x - 12.0, r.max.y - g.size().y - 12.0), g.size() + vec2(8.0, 4.0));
        painter.rect_filled(lr, CornerRadius::same(4), Color32::from_black_alpha(170));
        painter.galley(lr.min + vec2(4.0, 2.0), g, Color32::WHITE);
        ui.allocate_rect(r, egui::Sense::hover());
    });
    ui.ctx().request_repaint();
}
