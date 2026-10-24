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

pub struct Thumbs {
    map: Lru<(String, u64)>,
    /// 滑過預覽的長條圖（core::thumbs::strip；Jellyfin 的影片鍵是「jf:編號」與大小）
    strips: Lru<(String, u64)>,
    /// Jellyfin 的時間軸預覽縮圖（影片編號, 第幾張大圖）
    jf_tiles: Lru<(String, u32)>,
}

impl Default for Thumbs {
    fn default() -> Self {
        Thumbs { map: Lru::new(400), strips: Lru::new(60), jf_tiles: Lru::new(40) }
    }
}

/// 材質快取：滿了丟掉最久沒用到的（以前是整個清空，捲動清單時畫面上的縮圖也要全部重讀）
pub struct Lru<K> {
    map: HashMap<K, (Thumb, u64)>,
    tick: u64,
    cap: usize,
}

impl<K: std::hash::Hash + Eq + Clone> Lru<K> {
    pub fn new(cap: usize) -> Self {
        Lru { map: HashMap::new(), tick: 0, cap }
    }

    /// 拿來用（記下用過）
    pub fn get(&mut self, k: &K) -> Option<&Thumb> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(k).map(|e| {
            e.1 = tick;
            &e.0
        })
    }

    /// 只看不算用過
    pub fn peek(&self, k: &K) -> Option<&Thumb> {
        self.map.get(k).map(|e| &e.0)
    }

    pub fn insert(&mut self, k: K, t: Thumb) {
        self.tick += 1;
        self.map.insert(k, (t, self.tick));
        if self.map.len() > self.cap {
            // 一次丟掉最舊的四分之一（不用每次都排序）；正在讀的留著（讀好會再放進來）
            let mut old: Vec<(u64, K)> = self.map.iter().filter(|(_, (t, _))| !matches!(t, Thumb::Loading)).map(|(k, (_, at))| (*at, k.clone())).collect();
            old.sort_unstable_by_key(|(at, _)| *at);
            for (_, k) in old.into_iter().take(self.map.len() - self.cap * 3 / 4) {
                self.map.remove(&k);
            }
        }
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.map.retain(|k, _| keep(k));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

fn decode(path: &std::path::Path) -> Option<ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    let pm = tiny_skia::Pixmap::decode_png(&bytes).ok()?;
    Some(ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
}

impl Thumbs {
    /// 這個檔案的縮圖重做（設了封面：修改時間保留，鍵不會變）
    pub fn forget(&mut self, path: &str) {
        self.map.retain(|k| k.0 != path);
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

    /// Jellyfin 影片（沒有時間軸預覽縮圖時）的滑過預覽長條圖：FFmpeg 從串流網址讀，存在縮圖快取
    pub fn thumb_strip_remote(&mut self, id: &str, url: &str, size: u64, duration: f64) -> Option<TextureHandle> {
        let key = (format!("jf:{id}"), size);
        match self.thumbs.strips.get(&key) {
            Some(Thumb::Ready(t)) => return Some(t.clone()),
            Some(Thumb::Failed(at)) if at.elapsed() >= RETRY_FAILED => {}
            Some(_) => return None,
            None => {}
        }
        self.thumbs.strips.insert(key.clone(), Thumb::Loading);
        let core = self.core.clone();
        let (url, name) = (url.to_string(), format!("{id}|{size}"));
        self.spawn(
            async move {
                let file = core.thumbs.strip_remote(core.ffmpeg_path().as_deref(), &url, &name, duration).await?;
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

    /// Jellyfin 的時間軸預覽縮圖的第 index 張大圖（還沒讀時開始讀，回傳 None）
    fn jf_tile(&mut self, id: &str, tp: &screenrecorder_core::jellyfin::Trickplay, index: u32) -> Option<TextureHandle> {
        let key = (id.to_string(), index);
        match self.thumbs.jf_tiles.get(&key) {
            Some(Thumb::Ready(t)) => return Some(t.clone()),
            Some(Thumb::Failed(at)) if at.elapsed() >= RETRY_FAILED => {}
            Some(_) => return None,
            None => {}
        }
        let jf = self.jellyfin()?;
        self.thumbs.jf_tiles.insert(key.clone(), Thumb::Loading);
        let (id, tp) = (id.to_string(), tp.clone());
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    let bytes = jf.trickplay_tile(&id, &tp, index)?;
                    let pm = screenrecorder_core::picture::decode_pixmap(&bytes)?;
                    Some(ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
                })
                .await
                .ok()
                .flatten()
            },
            move |app, img| {
                let t = match img {
                    Some(img) => Thumb::Ready(app.ctx.load_texture("jf-trickplay", img, TextureOptions::LINEAR)),
                    None => Thumb::Failed(Instant::now()),
                };
                app.thumbs.jf_tiles.insert(key, t);
                app.ctx.request_repaint();
            },
        );
        None
    }
}

/// 滑過預覽：游標在 hover（縮圖）上左右移動時，在 near 旁邊浮出一張大一點的畫面、時間與進度；
/// 長條圖還沒好時顯示「正在準備預覽…」
pub fn scrub_preview(app: &mut UiApp, ui: &eframe::egui::Ui, hover: eframe::egui::Rect, near: eframe::egui::Rect, path: &str, mtime: f64, duration: f64) {
    let Some(pos) = ui.ctx().pointer_hover_pos().filter(|p| hover.contains(*p)) else { return };
    // 只有 MP4 影片；產生失敗（損壞、還在寫入）的不顯示
    if !(duration.is_finite() && duration > 0.0) || !path.to_ascii_lowercase().ends_with(".mp4") {
        return;
    }
    let strip = app.thumb_strip(path, mtime, duration);
    if strip.is_none() && matches!(app.thumbs.strips.peek(&(path.to_string(), mtime as u64)), Some(Thumb::Failed(_))) {
        return;
    }
    strip_box(ui.ctx(), near, strip, ((pos.x - hover.min.x) / hover.width().max(1.0)) as f64, duration);
}

/// 用長條圖（STRIP_FRAMES 格）浮出 frac（0～1）處的畫面
fn strip_box(ctx: &eframe::egui::Context, near: eframe::egui::Rect, strip: Option<TextureHandle>, frac: f64, duration: f64) {
    let (image, aspect, t) = strip_frame(strip.as_ref(), frac, duration);
    scrub_box(ctx, near, image, aspect, t, duration);
}

/// 長條圖（STRIP_FRAMES 格）裡 frac（0～1）處的那一格：（圖與範圍, 高 / 寬, 秒）
fn strip_frame(strip: Option<&TextureHandle>, frac: f64, duration: f64) -> (ScrubImage, f32, f64) {
    use eframe::egui::{pos2, Rect};
    use screenrecorder_core::thumbs::{strip_pick, STRIP_FRAMES};
    let (i, t) = strip_pick(frac, duration);
    let aspect = strip.map(|s| s.size_vec2()).map(|s| s.y / (s.x / STRIP_FRAMES as f32)).filter(|a| a.is_finite() && *a > 0.1).unwrap_or(9.0 / 16.0);
    let n = STRIP_FRAMES as f32;
    let image = strip.map(|tex| (tex.id(), Rect::from_min_max(pos2(i as f32 / n, 0.0), pos2((i + 1) as f32 / n, 1.0))));
    (image, aspect, t)
}

/// 預覽的一格：材質與在材質上的範圍（還沒讀好時 None）
type ScrubImage = Option<(eframe::egui::TextureId, eframe::egui::Rect)>;

/// Jellyfin 縮圖上的滑過預覽（直接畫在那張縮圖上，見 paint_scrub）：上一格算好的
struct Scrub {
    key: String,
    image: ScrubImage,
    aspect: f32,
    t: f64,
    duration: f64,
}

thread_local! {
    static SCRUB: std::cell::RefCell<Option<Scrub>> = const { std::cell::RefCell::new(None) };
}

/// 記下這支要畫哪一格；和上一次不同時重畫
fn set_scrub(ctx: &eframe::egui::Context, s: Scrub) {
    SCRUB.with_borrow_mut(|cur| {
        let changed = cur.as_ref().is_none_or(|c| c.key != s.key || (c.t - s.t).abs() > 1e-6 || c.image.map(|i| i.0) != s.image.map(|i| i.0) || c.image.is_some() != s.image.is_some());
        *cur = Some(s);
        if changed {
            ctx.request_repaint();
        }
    });
}

/// 滑鼠在 key 這支的縮圖上左右移動：把那個時間點的畫面直接畫在 rect（和縮圖一樣填滿），下緣是進度、右下角是時間；
/// 還沒讀好時暗下來寫「正在準備預覽…」。有畫時回傳 true
pub fn paint_scrub(painter: &eframe::egui::Painter, key: &str, rect: eframe::egui::Rect, rounding: u8) -> bool {
    use eframe::egui::{pos2, vec2, Align2, Color32, CornerRadius, Rect};
    SCRUB.with_borrow(|s| {
        let Some(s) = s.as_ref().filter(|s| s.key == key) else { return false };
        let p = super::theme::pal_ctx(painter.ctx());
        match s.image {
            Some((tex, uv)) => {
                // 和縮圖一樣填滿：比例不同時裁掉多的部分
                let (fa, ra) = (s.aspect, rect.height() / rect.width().max(1.0));
                let uv = if fa > ra {
                    let keep = ra / fa;
                    let cut = uv.height() * (1.0 - keep) / 2.0;
                    Rect::from_min_max(pos2(uv.min.x, uv.min.y + cut), pos2(uv.max.x, uv.max.y - cut))
                } else {
                    let keep = fa / ra;
                    let cut = uv.width() * (1.0 - keep) / 2.0;
                    Rect::from_min_max(pos2(uv.min.x + cut, uv.min.y), pos2(uv.max.x - cut, uv.max.y))
                };
                painter.add(eframe::egui::epaint::RectShape::filled(rect, CornerRadius::same(rounding), Color32::WHITE).with_texture(tex, uv));
            }
            None => {
                painter.rect_filled(rect, CornerRadius::same(rounding), Color32::from_black_alpha(140));
                painter.text(rect.center(), Align2::CENTER_CENTER, screenrecorder_core::tr!("正在準備預覽…", "Preparing preview…"), super::theme::font(12.0), Color32::from_gray(230));
            }
        }
        let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 3.0), rect.max);
        painter.rect_filled(bar, CornerRadius::ZERO, Color32::from_white_alpha(70));
        painter.rect_filled(Rect::from_min_max(bar.min, pos2(rect.min.x + rect.width() * (s.t / s.duration.max(0.001)) as f32, bar.max.y)), CornerRadius::ZERO, p.accent);
        let g = painter.layout_no_wrap(screenrecorder_core::format::video_clock(s.t), super::theme::mono(12.0), Color32::WHITE);
        // 時間放在下緣正中間（左下是刪除、下載、編輯，右下是 CC）
        let lr = Rect::from_center_size(pos2(rect.center().x, rect.max.y - 18.0 - g.size().y / 2.0), g.size() + vec2(8.0, 4.0));
        painter.rect_filled(lr, CornerRadius::same(4), Color32::from_black_alpha(170));
        painter.galley(lr.min + vec2(4.0, 2.0), g, Color32::WHITE);
        true
    })
}

/// Jellyfin 分頁的滑過預覽（畫在縮圖本身上，見 paint_scrub）：Jellyfin 做好時間軸預覽縮圖時用它
/// （每幾秒一張，跟著游標連續變），沒有時從串流讀一條長條圖（第一次要等幾秒）
pub fn jf_scrub_preview(app: &mut UiApp, ctx: &eframe::egui::Context, hover: eframe::egui::Rect, it: &screenrecorder_core::jellyfin::JfItem) {
    use eframe::egui::{pos2, Rect};
    let key = it.id.clone();
    let Some(pos) = ctx.pointer_hover_pos().filter(|p| hover.contains(*p)) else { return };
    if !(it.duration.is_finite() && it.duration > 0.0) {
        return;
    }
    let frac = (((pos.x - hover.min.x) / hover.width().max(1.0)) as f64).clamp(0.0, 1.0);
    if let Some(tp) = &it.trickplay {
        let t = frac * it.duration;
        let (index, uv) = tp.at(t);
        let tile = app.jf_tile(&it.id, tp, index);
        let aspect = if tp.width > 0 { tp.height as f32 / tp.width as f32 } else { 9.0 / 16.0 };
        let image = tile.as_ref().map(|tex| (tex.id(), Rect::from_min_max(pos2(uv[0], uv[1]), pos2(uv[2], uv[3]))));
        set_scrub(ctx, Scrub { key, image, aspect, t, duration: it.duration });
        return;
    }
    let Some(jf) = app.jellyfin() else { return };
    let strip = app.thumb_strip_remote(&it.id, &jf.stream_url(&it.id), it.size, it.duration);
    if strip.is_none() && matches!(app.thumbs.strips.peek(&(format!("jf:{}", it.id), it.size)), Some(Thumb::Failed(_))) {
        return;
    }
    let (image, aspect, t) = strip_frame(strip.as_ref(), frac, it.duration);
    set_scrub(ctx, Scrub { key, image, aspect, t, duration: it.duration });
}

/// 浮出的預覽框（寬 240，依影片比例）：放在 near 右邊，放不下時放左邊；image = None 時寫「正在準備預覽…」
fn scrub_box(ctx: &eframe::egui::Context, near: eframe::egui::Rect, image: Option<(eframe::egui::TextureId, eframe::egui::Rect)>, aspect: f32, t: f64, duration: f64) {
    use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, Id, Order, Rect, Stroke};
    let p = super::theme::pal_ctx(ctx);
    let size = vec2(240.0, (240.0 * aspect).clamp(60.0, 320.0));
    let screen = ctx.content_rect();
    let mut at = pos2(near.max.x + 10.0, near.center().y - size.y / 2.0);
    if at.x + size.x > screen.max.x - 8.0 {
        at.x = near.min.x - 10.0 - size.x;
    }
    at.y = at.y.clamp(screen.min.y + 8.0, (screen.max.y - size.y - 30.0).max(screen.min.y + 8.0));
    egui::Area::new(Id::new("scrub-preview")).order(Order::Tooltip).fixed_pos(at).interactable(false).show(ctx, |ui| {
        let r = Rect::from_min_size(at, size);
        let painter = ui.painter();
        painter.rect_filled(r.expand(3.0), CornerRadius::same(8), p.surface);
        painter.rect_stroke(r.expand(3.0), CornerRadius::same(8), Stroke::new(1.0, p.border_strong), egui::StrokeKind::Outside);
        painter.rect_filled(r, CornerRadius::same(6), Color32::BLACK);
        match image {
            Some((tex, uv)) => {
                painter.image(tex, r, uv, Color32::WHITE);
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
    // 不必每格都重畫：游標移動、縮圖讀好時（上面讀取完成會 request_repaint）才換畫面
}

/// 縮圖上停住就播放：用 FFmpeg 解成小畫面、不出聲音（input 是本機路徑或 Jellyfin 的串流網址）
pub fn hover_player(app: &UiApp, input: &str, duration: f64, fps: f64, size: (u32, u32)) -> Option<screenrecorder_core::player::Player> {
    use screenrecorder_core::player::{MediaSpec, Player};
    let ffmpeg = app.core.ffmpeg_path()?;
    let spec = MediaSpec { path: input.to_string(), duration, fps: if fps > 0.0 { fps } else { 30.0 }, has_audio: false };
    let c = app.ctx.clone();
    let (max_w, max_h) = super::hover_play::DECODE_MAX;
    let (w, h) = if size.0 > 0 && size.1 > 0 { size } else { (1280, 720) };
    Some(Player::new(ffmpeg, spec, w, h, max_w, max_h, std::sync::Arc::new(move || c.request_repaint())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_drops_the_least_recently_used() {
        let mut c: Lru<u32> = Lru::new(8);
        for i in 0..8 {
            c.insert(i, Thumb::Failed(Instant::now()));
        }
        // 0、1 剛用過；正在讀的 7 不丟
        c.insert(7, Thumb::Loading);
        assert!(c.get(&0).is_some() && c.get(&1).is_some());
        c.insert(8, Thumb::Failed(Instant::now()));
        assert_eq!(c.len(), 6);
        for k in [0, 1, 7, 8] {
            assert!(c.peek(&k).is_some(), "{k}");
        }
        assert!(c.peek(&2).is_none() && c.peek(&3).is_none());
    }
}
